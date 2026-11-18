//! Repository scan: list tracked source files and classify every character
//! into a small set of syntax classes used for colouring.

use std::path::Path;
use std::process::Command;

pub const C_SPACE: u8 = 0;
pub const C_IDENT: u8 = 1;
pub const C_KEYWORD: u8 = 2;
pub const C_TYPE: u8 = 3;
pub const C_STRING: u8 = 4;
pub const C_NUMBER: u8 = 5;
pub const C_COMMENT: u8 = 6;
pub const C_PUNCT: u8 = 7;
pub const C_HEADING: u8 = 8;
pub const C_CALL: u8 = 9;
pub const CLASS_COUNT: usize = 10;

/// Glyph index used for characters outside printable ASCII.
pub const GLYPH_OTHER: u8 = 127;

const EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "mjs", "cjs", "py", "jl", "qnt", "lean", "sol", "go", "sh", "toml",
    "md", "yml", "yaml", "css", "json", "tla", "just",
];
const MAX_LINES: usize = 20_000;
const MAX_AVG_LINE: usize = 240;

#[derive(Clone, Copy, PartialEq)]
pub enum Lang {
    CLike,
    Css,
    Quint,
    Rust,
    Hash,
    Yaml,
    Lean,
    Markdown,
    Json,
}

pub struct SourceFile {
    pub path: String,
    /// One byte per character: printable ASCII, or `GLYPH_OTHER`.
    pub text: Vec<u8>,
    pub class: Vec<u8>,
    /// Byte offset of each line start in `text`; lines exclude the newline.
    pub line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    pub fn line(&self, i: usize) -> (&[u8], &[u8]) {
        let s = self.line_starts[i] as usize;
        let e = if i + 1 < self.line_starts.len() {
            self.line_starts[i + 1] as usize - 1
        } else {
            self.text.len()
        };
        (&self.text[s..e], &self.class[s..e])
    }
}

pub fn scan_repo(root: &Path) -> Result<Vec<SourceFile>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-files failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut files = Vec::new();
    for rel in out.stdout.split(|b| *b == 0) {
        let Ok(rel) = std::str::from_utf8(rel) else {
            continue;
        };
        if rel.is_empty() || skip_path(rel) {
            continue;
        }
        let ext = rel.rsplit('.').next().unwrap_or("");
        let base = rel.rsplit('/').next().unwrap_or(rel);
        if !EXTENSIONS.contains(&ext) && base != "justfile" && base != "Dockerfile" {
            continue;
        }
        let Ok(bytes) = std::fs::read(root.join(rel)) else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let src = String::from_utf8_lossy(&bytes);
        if !within_line_limits(&src) {
            continue;
        }
        let lang = match ext {
            "rs" => Lang::Rust,
            "py" | "sh" | "toml" | "jl" | "just" => Lang::Hash,
            "yml" | "yaml" => Lang::Yaml,
            "css" => Lang::Css,
            "qnt" => Lang::Quint,
            "lean" => Lang::Lean,
            "md" => Lang::Markdown,
            "json" => Lang::Json,
            _ if base == "justfile" || base == "Dockerfile" => Lang::Hash,
            _ => Lang::CLike,
        };
        files.push(classify(rel.to_string(), &src, lang));
    }
    Ok(files)
}

fn within_line_limits(src: &str) -> bool {
    let mut chars = 0;
    let mut lines = 0;
    for line in src.lines() {
        lines += 1;
        if lines > MAX_LINES {
            return false;
        }
        chars += line.chars().count();
    }
    lines > 0 && chars <= lines * MAX_AVG_LINE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_limits_ignore_utf8_width_and_line_endings() {
        let line = "界".repeat(MAX_AVG_LINE);
        assert!(within_line_limits(&format!("{line}\r\n{line}\r\n")));
        assert!(!within_line_limits(&format!("{line}x\n{line}\n")));
        assert!(!within_line_limits(""));
    }

    fn token_class(text: &str, lang: Lang, token: &str) -> u8 {
        lex(text.as_bytes(), lang)[text.find(token).unwrap()]
    }

    #[test]
    fn vendor_filter_respects_directory_boundaries() {
        for path in [
            "vendor/pkg/a.rs",
            "src/vendor/pkg/a.rs",
            "node_modules/pkg/a.rs",
        ] {
            assert!(skip_path(path));
        }
        for path in ["vendor.rs", "my_node_modules/a.rs", "src/my_vendor/a.rs"] {
            assert!(!skip_path(path));
        }
    }

    #[test]
    fn rust_nested_comments_and_raw_strings_end_at_their_delimiters() {
        let comments = "/* outer /* inner */ still_outer */ fn live() {}";
        assert_eq!(token_class(comments, Lang::Rust, "still_outer"), C_COMMENT);
        assert_eq!(token_class(comments, Lang::Rust, "live"), C_CALL);
        for prefix in ["r", "br", "cr"] {
            let raw = format!(r###"{prefix}##"inside "# // still_inside"##; fn live() {{}}"###);
            assert_eq!(token_class(&raw, Lang::Rust, "still_inside"), C_STRING);
            assert_eq!(token_class(&raw, Lang::Rust, "live"), C_CALL);
        }
        assert_eq!(
            token_class("let x = \"incomplete\\", Lang::Rust, "incomplete"),
            C_STRING
        );
    }

    #[test]
    fn css_line_slashes_stay_code_and_block_comments_stay_comments() {
        let css = "a { background-image: url(https://example.org/p.png); color: red; } /* hint */";
        assert_ne!(token_class(css, Lang::Css, "color"), C_COMMENT);
        assert_eq!(token_class(css, Lang::Css, "hint"), C_COMMENT);
    }

    #[test]
    fn markdown_fences_require_the_same_marker_and_sufficient_length() {
        let markdown = "````text\n```\n~~~\n``` trailing\ninside\n`````\n# Heading\n~~~ text\nsecond\n~~~~\n# Again";
        assert_eq!(token_class(markdown, Lang::Markdown, "inside"), C_STRING);
        assert_eq!(token_class(markdown, Lang::Markdown, "Heading"), C_HEADING);
        assert_eq!(token_class(markdown, Lang::Markdown, "second"), C_STRING);
        assert_eq!(token_class(markdown, Lang::Markdown, "Again"), C_HEADING);
    }

    #[test]
    fn yaml_hash_comments_respect_scalar_content_and_indentation() {
        let yaml = "url: https://example.org/a#fragment\nscript: |2- # header\n  # literal\n\n  echo ok\nnote: can't stop # comment\nitems:\n  - body: >2\n      # folded\n    peer: 'quoted # value'\n";
        assert_ne!(token_class(yaml, Lang::Yaml, "fragment"), C_COMMENT);
        for token in ["literal", "echo", "folded", "value"] {
            assert_eq!(token_class(yaml, Lang::Yaml, token), C_STRING, "{token}");
        }
        for token in ["header", "comment"] {
            assert_eq!(token_class(yaml, Lang::Yaml, token), C_COMMENT, "{token}");
        }
        assert_eq!(token_class(yaml, Lang::Yaml, "peer"), C_IDENT);
    }
}

fn skip_path(rel: &str) -> bool {
    rel.rsplit_once('/').is_some_and(|(dirs, _)| {
        dirs.split('/')
            .any(|dir| matches!(dir, "vendor" | "node_modules"))
    }) || rel.ends_with(".lock")
        || rel.ends_with("-lock.yaml")
        || rel.ends_with("lock.json")
        || rel.contains(".min.")
        || rel.contains(".bundle.")
        || rel.ends_with("Manifest.toml")
}

fn map_char(c: char) -> u8 {
    match c {
        ' '..='~' => c as u8,
        '\u{2014}' | '\u{2013}' | '\u{2212}' => b'-',
        '\u{2192}' | '\u{21d2}' => b'>',
        '\u{2190}' => b'<',
        '\u{2018}' | '\u{2019}' => b'\'',
        '\u{201c}' | '\u{201d}' => b'"',
        '\u{2264}' => b'<',
        '\u{2265}' => b'>',
        '\u{00d7}' => b'x',
        _ => GLYPH_OTHER,
    }
}

const KEYWORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "const",
    "continue",
    "crate",
    "dyn",
    "else",
    "enum",
    "extern",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "trait",
    "true",
    "type",
    "unsafe",
    "use",
    "where",
    "while",
    "import",
    "export",
    "from",
    "function",
    "class",
    "interface",
    "extends",
    "implements",
    "new",
    "this",
    "var",
    "null",
    "undefined",
    "void",
    "typeof",
    "instanceof",
    "try",
    "catch",
    "finally",
    "throw",
    "switch",
    "case",
    "default",
    "def",
    "lambda",
    "None",
    "True",
    "False",
    "and",
    "or",
    "not",
    "is",
    "with",
    "yield",
    "pass",
    "raise",
    "except",
    "elif",
    "del",
    "global",
    "nonlocal",
    "readonly",
    "private",
    "public",
    "protected",
    "declare",
    "namespace",
    "keyof",
    "module",
    "theorem",
    "lemma",
    "structure",
    "inductive",
    "namespace",
    "end",
    "open",
    "then",
    "do",
    "val",
    "pure",
    "action",
    "temporal",
    "all",
    "any",
    "nondet",
    "contract",
    "require",
    "emit",
    "mapping",
    "returns",
    "external",
    "view",
    "payable",
    "local",
    "echo",
    "fi",
    "done",
    "esac",
];

pub(crate) fn classify(path: String, src: &str, lang: Lang) -> SourceFile {
    let mut text = Vec::with_capacity(src.len());
    let mut line_starts = vec![0u32];
    for (i, line) in src.lines().enumerate() {
        if i > 0 {
            text.push(b'\n');
            line_starts.push(text.len() as u32);
        }
        for c in line.chars() {
            if c == '\t' {
                text.extend_from_slice(b"    ");
            } else {
                text.push(map_char(c));
            }
        }
    }
    let class = lex(&text, lang);
    SourceFile {
        path,
        text,
        class,
        line_starts,
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

fn lex(t: &[u8], lang: Lang) -> Vec<u8> {
    let n = t.len();
    let mut cls = vec![C_SPACE; n];
    if lang == Lang::Markdown {
        lex_markdown(t, &mut cls);
        return cls;
    }
    let mut i = 0;
    let mut yaml_block: Option<YamlBlock> = None;
    let at = |i: usize, s: &[u8]| t[i..].starts_with(s);
    while i < n {
        if let Some(block) = &mut yaml_block {
            if i == 0 || t[i - 1] == b'\n' {
                let end = i + t[i..].iter().position(|b| *b == b'\n').unwrap_or(n - i);
                if block.contains(&t[i..end]) {
                    mark_string(t, &mut cls, i, end);
                    i = (end + 1).min(n);
                    continue;
                }
                yaml_block = None;
            }
        }
        let c = t[i];
        if c == b'\n' || c == b' ' {
            i += 1;
            continue;
        }
        let line_comment = match lang {
            Lang::Hash => c == b'#',
            Lang::Yaml => c == b'#' && (i == 0 || t[i - 1].is_ascii_whitespace()),
            Lang::Lean => at(i, b"--"),
            Lang::Json | Lang::Css => false,
            _ => at(i, b"//"),
        };
        if line_comment {
            while i < n && t[i] != b'\n' {
                cls[i] = C_COMMENT;
                i += 1;
            }
            continue;
        }
        let block = match lang {
            Lang::CLike | Lang::Rust | Lang::Css | Lang::Quint => {
                at(i, b"/*").then_some(&b"*/"[..])
            }
            Lang::Lean => at(i, b"/-").then_some(&b"-/"[..]),
            _ => None,
        };
        if let Some(close) = block {
            let start = i;
            i += 2;
            let mut depth = 1;
            while i < n && depth != 0 {
                if at(i, close) {
                    depth -= 1;
                    i += 2;
                } else if lang == Lang::Rust && at(i, b"/*") {
                    depth += 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            for k in start..i {
                if t[k] != b'\n' && t[k] != b' ' {
                    cls[k] = C_COMMENT;
                }
            }
            continue;
        }
        if lang == Lang::Rust && matches!(c, b'r' | b'b' | b'c') {
            if let Some(end) = rust_raw_string_end(&t[i..]) {
                mark_string(t, &mut cls, i, i + end);
                i += end;
                continue;
            }
        }
        if lang == Lang::Yaml && matches!(c, b'|' | b'>') {
            yaml_block = yaml_block_start(t, i);
        }
        if lang == Lang::Hash && (at(i, b"\"\"\"") || at(i, b"'''")) {
            let q = &t[i..i + 3];
            let start = i;
            i += 3;
            while i < n && !at(i, q) {
                i += 1;
            }
            i = (i + 3).min(n);
            mark_string(t, &mut cls, start, i);
            continue;
        }
        let quote = c == b'"'
            || c == b'`'
            || (c == b'\'' && !matches!(lang, Lang::Rust | Lang::Lean | Lang::Quint))
            || (c == b'\''
                && lang == Lang::Rust
                && i + 2 < n
                && (t[i + 2] == b'\'' || t[i + 1] == b'\\'));
        if quote && (lang != Lang::Yaml || yaml_quote_start(t, i)) {
            let start = i;
            i += 1;
            while i < n {
                if t[i] == c {
                    if lang == Lang::Yaml && c == b'\'' && t.get(i + 1) == Some(&c) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                if t[i] == b'\\' && !(lang == Lang::Yaml && c == b'\'') {
                    i = (i + 2).min(n);
                    continue;
                }
                if i < n && t[i] == b'\n' && c != b'`' && !matches!(lang, Lang::Rust | Lang::Yaml) {
                    break;
                }
                i += 1;
            }
            if t.get(i) == Some(&c) {
                i += 1;
            }
            mark_string(t, &mut cls, start, i);
            continue;
        }
        if c.is_ascii_digit() {
            while i < n && (t[i].is_ascii_alphanumeric() || t[i] == b'_' || t[i] == b'.') {
                cls[i] = C_NUMBER;
                i += 1;
            }
            continue;
        }
        if is_ident_start(c) {
            let start = i;
            while i < n && is_ident(t[i]) {
                i += 1;
            }
            let word = std::str::from_utf8(&t[start..i]).unwrap_or("");
            let k = if lang == Lang::Json {
                if start > 0 && t[start - 1] == b'"' {
                    C_STRING
                } else {
                    C_KEYWORD
                }
            } else if KEYWORDS.contains(&word) {
                C_KEYWORD
            } else if i < n && (t[i] == b'(' || (t[i] == b'!' && lang == Lang::Rust)) {
                C_CALL
            } else if c.is_ascii_uppercase() {
                C_TYPE
            } else {
                C_IDENT
            };
            cls[start..i].fill(k);
            continue;
        }
        cls[i] = if c == b'#' && lang == Lang::Rust {
            C_HEADING
        } else {
            C_PUNCT
        };
        i += 1;
    }
    cls
}

fn rust_raw_string_end(text: &[u8]) -> Option<usize> {
    let prefix = if text.starts_with(b"r") {
        1
    } else if text.starts_with(b"br") || text.starts_with(b"cr") {
        2
    } else {
        return None;
    };
    let hashes = text[prefix..].iter().take_while(|&&c| c == b'#').count();
    let quote = prefix + hashes;
    if hashes > 255 || text.get(quote) != Some(&b'"') {
        return None;
    }
    let delimiter = &text[prefix..quote];
    let start = quote + 1;
    let close = text[start..]
        .iter()
        .enumerate()
        .find(|&(i, &c)| c == b'"' && text[start + i + 1..].starts_with(delimiter));
    Some(close.map_or(text.len(), |(i, _)| start + i + 1 + hashes))
}

struct YamlBlock {
    parent_indent: usize,
    content_indent: Option<usize>,
}

fn yaml_quote_start(text: &[u8], i: usize) -> bool {
    let previous = text[..i].iter().rev().find(|&&b| b != b' ');
    matches!(
        previous,
        None | Some(b'\n' | b':' | b'-' | b'?' | b'[' | b'{' | b',')
    )
}

impl YamlBlock {
    fn contains(&mut self, line: &[u8]) -> bool {
        let indent = line.iter().take_while(|&&b| b == b' ').count();
        if indent == line.len() {
            return true;
        }
        if indent <= self.parent_indent {
            return false;
        }
        indent >= *self.content_indent.get_or_insert(indent)
    }
}

fn yaml_block_start(text: &[u8], i: usize) -> Option<YamlBlock> {
    let start = text[..i]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |n| n + 1);
    let prefix = &text[start..i];
    let indent = prefix.iter().take_while(|&&b| b == b' ').count();
    let last = prefix.iter().rposition(|&b| b != b' ');
    let parent_indent = match last {
        None => indent,
        Some(n) if n + 1 < prefix.len() && prefix[n] == b':' => {
            let mut key_start = indent;
            while prefix[key_start..].starts_with(b"- ") {
                key_start += 2;
                key_start += prefix[key_start..]
                    .iter()
                    .take_while(|&&b| b == b' ')
                    .count();
            }
            key_start
        }
        Some(n) if n == indent && n + 1 < prefix.len() && prefix[n] == b'-' => indent,
        Some(_)
            if indent == 0
                && prefix
                    .strip_prefix(b"--- ")
                    .is_some_and(|rest| rest.iter().all(|&b| b == b' ')) =>
        {
            0
        }
        _ => return None,
    };
    let mut tail = text[i + 1..].iter().copied().take_while(|&b| b != b'\n');
    let mut content_indent = None;
    let mut chomping = false;
    while let Some(b) = tail.next() {
        match b {
            b'1'..=b'9' if content_indent.is_none() => {
                content_indent = Some(parent_indent + usize::from(b - b'0'));
            }
            b'+' | b'-' if !chomping => chomping = true,
            b' ' => {
                if !matches!(tail.find(|&b| b != b' '), None | Some(b'#')) {
                    return None;
                }
                break;
            }
            _ => return None,
        }
    }
    Some(YamlBlock {
        parent_indent,
        content_indent,
    })
}

fn mark_string(t: &[u8], cls: &mut [u8], s: usize, e: usize) {
    for k in s..e {
        if t[k] != b'\n' && t[k] != b' ' {
            cls[k] = C_STRING;
        }
    }
}

fn lex_markdown(t: &[u8], cls: &mut [u8]) {
    let mut fence = None;
    let mut s = 0;
    while s <= t.len() {
        let e = t[s..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|p| s + p)
            .unwrap_or(t.len());
        let line = &t[s..e];
        let trimmed = line
            .iter()
            .position(|b| *b != b' ')
            .map(|p| &line[p..])
            .unwrap_or(&[]);
        let marker = if line.len() - trimmed.len() <= 3 {
            trimmed
                .first()
                .copied()
                .filter(|b| matches!(b, b'`' | b'~'))
                .and_then(|b| {
                    let n = trimmed.iter().take_while(|&&c| c == b).count();
                    (n >= 3).then_some((b, n, &trimmed[n..]))
                })
        } else {
            None
        };
        let in_fence = fence.is_some();
        if let Some((b, n, tail)) = marker {
            match fence {
                Some((open, size)) if b == open && n >= size && tail.iter().all(|&c| c == b' ') => {
                    fence = None
                }
                None if b == b'~' || !tail.contains(&b'`') => fence = Some((b, n)),
                _ => {}
            }
        }
        let k = if in_fence || fence.is_some() {
            C_STRING
        } else if trimmed.starts_with(b"#") {
            C_HEADING
        } else if trimmed.starts_with(b">") {
            C_COMMENT
        } else if trimmed.starts_with(b"|") {
            C_PUNCT
        } else {
            C_IDENT
        };
        let mut in_code = false;
        for (j, b) in line.iter().enumerate() {
            if *b == b' ' {
                continue;
            }
            if k == C_IDENT && *b == b'`' {
                in_code = !in_code;
                cls[s + j] = C_PUNCT;
                continue;
            }
            cls[s + j] = if in_code {
                C_TYPE
            } else if k == C_IDENT && (*b == b'*' || *b == b'[' || *b == b']' || *b == b'-') {
                C_PUNCT
            } else {
                k
            };
        }
        s = e + 1;
    }
}
