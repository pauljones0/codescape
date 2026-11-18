//! Git diff as an overlay: which lines of which files a change touches.
//!
//! One unified-diff parser serves both sources. `--diff <rev>` compares a
//! revision against the working tree, which is exactly what the map renders,
//! so line numbers always line up. `--diff <a>..<b>` and `--pr <n>` compare
//! two revisions instead. Any marks beyond the on-disk file are dropped and counted.

use crate::overlay::{FileOverlay, FileState, Overlay, M_ADDED, M_CHANGED, M_GONE, M_NONE};
use crate::scan::SourceFile;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Runs a command and returns stdout, or `None` if it could not be run or failed.
fn capture(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Reads the patch for `spec`, which is a revision, a `a..b` range, or empty
/// for the working tree against HEAD.
pub fn patch_for(root: &Path, spec: &str) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args([
        "--no-pager",
        "-c",
        "core.quotePath=false",
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--relative",
        "--output-indicator-new=+",
        "--output-indicator-old=-",
        "--output-indicator-context= ",
        "-M",
        "-U0",
    ]);
    if !spec.is_empty() {
        cmd.args(["--end-of-options", spec]);
    }
    cmd.arg("--");
    capture(&mut cmd)
}

/// `owner/repo` from a remote URL. The origin here is a proxy scheme rather
/// than a github.com URL, so `gh` cannot infer the repository on its own.
fn slug(remote: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    let mut parts = remote.rsplit(['/', ':']);
    let repo = parts.next()?;
    let owner = parts.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// Reads a pull request patch through `gh`, which does not need the branch locally.
pub fn patch_for_pr(root: &Path, pr: u32) -> Option<String> {
    let pr = pr.to_string();
    let remote = capture(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["remote", "get-url", "origin"]),
    )
    .unwrap_or_default();
    if let Some(slug) = slug(&remote) {
        if let Some(patch) = capture(
            Command::new("gh")
                .current_dir(root)
                .args(["pr", "diff", &pr, "-R", &slug]),
        ) {
            return Some(patch);
        }
    }
    capture(
        Command::new("gh")
            .current_dir(root)
            .args(["pr", "diff", &pr]),
    )
}

/// Git prints C-style escapes inside a quoted path, including octal bytes.
fn unquote_path(path: &str) -> Option<String> {
    if !path.starts_with('"') {
        return Some(path.to_owned());
    }
    let raw = path.strip_prefix('"')?.strip_suffix('"')?.as_bytes();
    let mut result = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter().copied();
    while let Some(b) = bytes.next() {
        let byte = if b != b'\\' {
            if b == b'"' {
                return None;
            }
            b
        } else {
            match bytes.next()? {
                b'a' => 7,
                b'b' => 8,
                b't' => b'\t',
                b'n' => b'\n',
                b'v' => 11,
                b'f' => 12,
                b'r' => b'\r',
                b'\\' => b'\\',
                b'"' => b'"',
                first @ b'0'..=b'3' => {
                    let second = bytes.next()?;
                    let third = bytes.next()?;
                    if !(b'0'..=b'7').contains(&second) || !(b'0'..=b'7').contains(&third) {
                        return None;
                    }
                    ((first - b'0') << 6) | ((second - b'0') << 3) | (third - b'0')
                }
                _ => return None,
            }
        };
        result.push(byte);
    }
    String::from_utf8(result).ok()
}

fn header_path(header: &str, prefix: &str) -> Option<String> {
    let header = header.split('\t').next()?;
    if header == "/dev/null" {
        return None;
    }
    let path = unquote_path(header)?;
    Some(path.strip_prefix(prefix).unwrap_or(&path).to_owned())
}

/// Content and rename headers replace these names when available. For a file
/// with metadata only, both names are identical and can disambiguate spaces.
fn diff_paths(header: &str) -> (Option<String>, Option<String>) {
    let mut paths = (None, None);
    for (i, _) in header.match_indices(' ') {
        let right = &header[i + 1..];
        if !right.starts_with("b/") && !right.starts_with("\"b/") {
            continue;
        }
        let old = header_path(&header[..i], "a/");
        let new = header_path(right, "b/");
        if old.is_some() && old == new {
            return (old, new);
        }
        if old.is_some() && new.is_some() {
            paths = (old, new);
        }
    }
    paths
}

fn hunk_head(line: &str) -> Option<(usize, usize)> {
    let mut fields = line.strip_prefix("@@ ")?.split_ascii_whitespace();
    fields.next()?.strip_prefix('-')?;
    let head = fields.next()?.strip_prefix('+')?;
    let (start, count) = head.split_once(',').unwrap_or((head, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

struct FilePatch {
    old: Option<String>,
    new: Option<String>,
    state: FileState,
    added: usize,
    removed: usize,
    /// Head-side line, 1-based, and its mark.
    marks: Vec<(usize, u8)>,
}

impl FilePatch {
    fn new(header: &str) -> Self {
        let (old, new) = diff_paths(header);
        Self {
            old,
            new,
            state: FileState::Modified,
            added: 0,
            removed: 0,
            marks: Vec::new(),
        }
    }
}

/// Context separates independent edits even when Git puts them in one hunk.
struct Hunk {
    next_line: usize,
    deletion_anchor: bool,
    first_added: usize,
    added: usize,
    removed: usize,
}

impl Hunk {
    fn new(start: usize, count: usize) -> Self {
        Self {
            next_line: start,
            deletion_anchor: count == 0,
            first_added: 0,
            added: 0,
            removed: 0,
        }
    }

    fn flush(&mut self, file: &mut FilePatch) {
        if self.added > 0 {
            let mark = if self.removed > 0 { M_CHANGED } else { M_ADDED };
            file.marks
                .extend((self.first_added..self.next_line).map(|line| (line, mark)));
        } else if self.removed > 0 {
            let previous = if self.deletion_anchor {
                self.next_line
            } else {
                self.next_line.saturating_sub(1)
            };
            file.marks.push((previous.max(1), M_GONE));
        }
        file.added += self.added;
        file.removed += self.removed;
        self.added = 0;
        self.removed = 0;
    }
}

fn parse(patch: &str) -> Vec<FilePatch> {
    let mut files: Vec<FilePatch> = Vec::new();
    let mut hunk: Option<Hunk> = None;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let (Some(mut previous), Some(file)) = (hunk.take(), files.last_mut()) {
                previous.flush(file);
            }
            files.push(FilePatch::new(rest));
        } else if let Some(file) = files.last_mut() {
            if let Some((start, count)) = hunk_head(line) {
                if let Some(previous) = &mut hunk {
                    previous.flush(file);
                }
                hunk = Some(Hunk::new(start, count));
            } else if let Some(hunk) = &mut hunk {
                match line.as_bytes().first() {
                    Some(b'+') => {
                        if hunk.added == 0 {
                            hunk.first_added = hunk.next_line;
                        }
                        hunk.added += 1;
                        hunk.next_line = hunk.next_line.saturating_add(1);
                    }
                    Some(b'-') => hunk.removed += 1,
                    Some(b' ') => {
                        hunk.flush(file);
                        hunk.next_line = hunk.next_line.saturating_add(1);
                    }
                    _ => {}
                }
            } else if line.starts_with("new file mode ") {
                file.state = FileState::Added;
            } else if line.starts_with("deleted file mode ") {
                file.state = FileState::Deleted;
            } else if let Some(old) = line.strip_prefix("rename from ") {
                file.old = unquote_path(old);
                file.state = FileState::Renamed;
            } else if let Some(new) = line.strip_prefix("rename to ") {
                file.new = unquote_path(new);
                file.state = FileState::Renamed;
            } else if let Some(old) = line.strip_prefix("--- ") {
                file.old = header_path(old, "a/");
            } else if let Some(new) = line.strip_prefix("+++ ") {
                file.new = header_path(new, "b/");
            }
        }
    }
    if let (Some(mut last), Some(file)) = (hunk, files.last_mut()) {
        last.flush(file);
    }
    files
}

/// Builds an overlay from a patch whose paths are relative to the scan root.
pub fn overlay(patch: &str, sources: &[SourceFile], label: String) -> (Overlay, usize) {
    overlay_in_scope(patch, sources, label, "")
}

/// PR patches use checkout-relative paths, even when the map starts in a subdirectory.
pub fn pr_overlay(
    root: &Path,
    patch: &str,
    sources: &[SourceFile],
    label: String,
) -> Option<(Overlay, usize)> {
    let prefix = capture(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "--show-prefix"]),
    )?;
    Some(overlay_in_scope(
        patch,
        sources,
        label,
        prefix.trim_end_matches(['\r', '\n']),
    ))
}

fn overlay_in_scope(
    patch: &str,
    sources: &[SourceFile],
    label: String,
    prefix: &str,
) -> (Overlay, usize) {
    let index: HashMap<&str, usize> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| (s.path.as_str(), i))
        .collect();
    let mut ov = Overlay::empty(sources.len());
    ov.label = label;
    let mut dropped = 0usize;
    for file in parse(patch) {
        let old = file.old.as_deref().and_then(|p| p.strip_prefix(prefix));
        let new = file.new.as_deref().and_then(|p| p.strip_prefix(prefix));
        if file.state == FileState::Deleted {
            if let Some(path) = old {
                ov.deleted.push((path.to_owned(), file.removed));
            }
            continue;
        }
        let head = new.and_then(|path| index.get(path).copied());
        let base = if file.state == FileState::Renamed {
            old.and_then(|path| index.get(path).copied())
        } else {
            None
        };
        let Some(i) = head.or(base) else { continue };
        let mut marks = vec![M_NONE; sources[i].line_count()];
        if head.is_some() {
            for (line, mark) in file.marks {
                match line.checked_sub(1).and_then(|i| marks.get_mut(i)) {
                    Some(slot) if *slot == M_NONE || mark != M_GONE => *slot = mark,
                    Some(_) => {}
                    None => dropped += 1,
                }
            }
        } else {
            // Head line numbers belong to the renamed file, not the older tile.
            dropped += file.marks.len();
        }
        let rename = if file.state == FileState::Renamed {
            file.old.zip(file.new)
        } else {
            None
        };
        ov.files[i] = Some(FileOverlay {
            state: file.state,
            rename,
            added: file.added,
            removed: file.removed,
            marks,
        });
    }
    ov.deleted
        .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    (ov, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{M_ADDED, M_CHANGED, M_GONE, M_NONE};

    /// A file of `n` blank-ish lines, so marks can be addressed by line number.
    fn source(path: &str, n: usize) -> SourceFile {
        let text = vec![b'x'; n];
        SourceFile {
            path: path.to_string(),
            class: vec![1u8; n],
            text,
            line_starts: (0..n as u32).collect(),
        }
    }

    const PATCH: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -2,0 +3,2 @@ fn x() {
+    let a = 1;
+    let b = 2;
@@ -10,2 +12,1 @@ fn y() {
-    old();
-    older();
+    new();
@@ -20,1 +21,0 @@ fn z() {
-    gone();
diff --git a/src/new.rs b/src/new.rs
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1,2 @@
+fn fresh() {}
+// added
diff --git a/src/old.rs b/src/old.rs
deleted file mode 100644
index 4444444..0000000
--- a/src/old.rs
+++ /dev/null
@@ -1,3 +0,0 @@
-fn a() {}
-fn b() {}
-fn c() {}
";

    #[test]
    fn remote_urls_reduce_to_a_gh_slug() {
        assert_eq!(
            slug("entire://aws-ap-southeast-2.entire.io/gh/flop-labs/flop-core").as_deref(),
            Some("flop-labs/flop-core")
        );
        assert_eq!(
            slug("git@github.com:flop-labs/flop-core.git").as_deref(),
            Some("flop-labs/flop-core")
        );
        assert_eq!(
            slug("https://github.com/flop-labs/flop-core/").as_deref(),
            Some("flop-labs/flop-core")
        );
    }

    #[test]
    fn marks_additions_changes_and_removals() {
        let sources = vec![source("src/a.rs", 40), source("src/new.rs", 2)];
        let (ov, dropped) = overlay(PATCH, &sources, "test".into());
        assert_eq!(dropped, 0);

        let a = ov.files[0].as_ref().expect("src/a.rs is marked");
        assert_eq!(a.state, FileState::Modified);
        assert_eq!((a.added, a.removed), (3, 3));
        // Hunk with no removals: plain additions on head lines 3 and 4.
        assert_eq!(a.marks[2], M_ADDED);
        assert_eq!(a.marks[3], M_ADDED);
        // Hunk replacing two lines with one: the surviving line is a change.
        assert_eq!(a.marks[11], M_CHANGED);
        // Pure deletion: the mark sits on the line that closed the gap.
        assert_eq!(a.marks[20], M_GONE);
        assert_eq!(a.marks[0], M_NONE);
    }

    #[test]
    fn new_files_are_added_and_deleted_files_have_no_tile() {
        let sources = vec![source("src/a.rs", 40), source("src/new.rs", 2)];
        let (ov, _) = overlay(PATCH, &sources, "test".into());

        let n = ov.files[1].as_ref().expect("src/new.rs is marked");
        assert_eq!(n.state, FileState::Added);
        assert_eq!(n.marks, vec![M_ADDED, M_ADDED]);

        // src/old.rs is gone from the working tree, so it gets no tile.
        assert_eq!(ov.deleted.len(), 1);
        assert_eq!(ov.deleted[0], ("src/old.rs".to_string(), 3));
    }

    #[test]
    fn marks_past_the_end_of_the_scanned_file_are_counted_not_lost_silently() {
        // Head revision is longer than the file on disk: a stale working tree.
        let sources = vec![source("src/a.rs", 5), source("src/new.rs", 2)];
        let (ov, dropped) = overlay(PATCH, &sources, "test".into());
        // One replacement line and one pure-deletion anchor are off the map.
        assert_eq!(dropped, 2);
        // The file still reads as changed, with its real counts.
        let a = ov.files[0].as_ref().expect("still marked");
        assert_eq!((a.added, a.removed), (3, 3));
    }

    #[test]
    fn context_splits_pure_additions_from_deletions() {
        let patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,4 +1,4 @@\n a\n-b\n c\n+x\n d\n";
        let (ov, dropped) = overlay(patch, &[source("a.rs", 4)], "PR".into());
        assert_eq!(dropped, 0);
        assert_eq!(ov.mark(0, 0), M_GONE);
        assert_eq!(ov.mark(0, 2), M_ADDED);
    }

    #[test]
    fn quoted_names_decode_octal_control_characters_and_quotes() {
        let patch = r#"diff --git "a/s p\t\"\303\251\n.rs" "b/s p\t\"\303\251\n.rs"
--- "a/s p\t\"\303\251\n.rs"
+++ "b/s p\t\"\303\251\n.rs"
@@ -1,0 +2 @@
+new
"#;
        let (ov, dropped) = overlay(patch, &[source("s p\t\"é\n.rs", 2)], "quoted".into());
        assert_eq!(dropped, 0);
        assert_eq!(ov.mark(0, 1), M_ADDED);
        let hidden = "diff --git a/a b/c.rs b/a b/c.rs\nold mode 100644\nnew mode 100755\n";
        assert_eq!(
            overlay(hidden, &[source("a b/c.rs", 1)], "mode".into())
                .0
                .touched(),
            1
        );
    }

    #[test]
    fn a_rename_prefers_the_head_and_can_fall_back_to_the_old_tile() {
        let patch = "diff --git a/src/old.rs b/src/new.rs\nsimilarity index 80%\nrename from src/old.rs\nrename to src/new.rs\n--- a/src/old.rs\n+++ b/src/new.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let (ov, dropped) = overlay_in_scope(
            patch,
            &[source("new.rs", 1), source("old.rs", 1)],
            "PR".into(),
            "src/",
        );
        assert_eq!(dropped, 0);
        assert_eq!(ov.files[0].as_ref().unwrap().state, FileState::Renamed);
        assert_eq!(ov.mark(0, 0), M_CHANGED);
        assert!(ov.files[1].is_none());

        let (ov, dropped) = overlay_in_scope(patch, &[source("old.rs", 1)], "PR".into(), "src/");
        let old = ov.files[0].as_ref().unwrap();
        assert_eq!(old.rename, Some(("src/old.rs".into(), "src/new.rs".into())));
        assert_eq!((ov.mark(0, 0), dropped), (M_NONE, 1));
        assert!(ov.deleted.is_empty());
    }

    #[test]
    fn real_git_uses_the_scan_root_even_with_mnemonic_prefixes_enabled() {
        let fixture = std::env::temp_dir().join(format!(
            "codescape-diff-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sub = fixture.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let git = |args: &[&str]| {
            assert!(
                capture(Command::new("git").arg("-C").arg(&fixture).args(args)).is_some(),
                "{args:?}"
            );
        };
        git(&["init", "-q"]);
        git(&["config", "diff.mnemonicPrefix", "true"]);
        git(&["config", "diff.relative", "false"]);
        let path = sub.join("a.rs");
        std::fs::write(&path, "fn original() {}\n").unwrap();
        git(&["add", "src/a.rs"]);
        std::fs::write(&path, "fn original() {}\nfn added() {}\n").unwrap();
        for root in [&fixture, &sub] {
            let sources = crate::scan::scan_repo(root).unwrap();
            let patch = patch_for(root, "").unwrap();
            let (ov, dropped) = overlay(&patch, &sources, "local".into());
            assert_eq!((ov.touched(), ov.mark(0, 1), dropped), (1, M_ADDED, 0));
        }
        let checkout_patch = patch_for(&fixture, "").unwrap();
        let sources = crate::scan::scan_repo(&sub).unwrap();
        let (ov, dropped) = pr_overlay(&sub, &checkout_patch, &sources, "PR".into()).unwrap();
        assert_eq!((ov.touched(), ov.mark(0, 1), dropped), (1, M_ADDED, 0));
        std::fs::remove_dir_all(fixture).unwrap();
    }
}
