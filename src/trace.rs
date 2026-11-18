//! Quint counterexamples drawn on the spec that produced them.
//!
//! A Quint run that violates an invariant emits an ITF trace: an ordered list
//! of states, each binding every state variable. The spec itself is already a
//! tile on the map, so a trace needs no new geometry — it is an overlay whose
//! marks move as the trace is stepped. The lines that light up are where each
//! variable is declared and where it is assigned, so stepping a counterexample
//! walks the reader through the part of the spec that produced it.

use crate::overlay::{FileOverlay, FileState, Overlay, M_NONE, M_PAST, M_STEP};
use crate::scan::{SourceFile, C_COMMENT, C_STRING};
pub use serde_json::Value as Json;

pub fn parse_json(text: &str) -> Option<Json> {
    serde_json::from_str(text).ok()
}

/// Renders an ITF value the way Quint prints it.
pub fn render(v: &Json) -> String {
    match v {
        Json::Null => "null".into(),
        Json::Bool(b) => b.to_string(),
        Json::Number(n) => n.to_string(),
        Json::String(s) => s.clone(),
        Json::Array(a) => {
            let items: Vec<String> = a.iter().map(render).collect();
            format!("[{}]", items.join(", "))
        }
        Json::Object(fields) => {
            // ITF tags: #bigint, #set, #map, #tup are wrappers, not records.
            if let Some(Json::String(n)) = v.get("#bigint") {
                return n.clone();
            }
            if let Some(Json::Array(items)) = v.get("#set") {
                let items: Vec<String> = items.iter().map(render).collect();
                return format!("Set({})", items.join(", "));
            }
            if let Some(Json::Array(items)) = v.get("#tup") {
                let items: Vec<String> = items.iter().map(render).collect();
                return format!("({})", items.join(", "));
            }
            if let Some(Json::Array(pairs)) = v.get("#map") {
                let items: Vec<String> = pairs
                    .iter()
                    .map(|p| match p.as_array().map(Vec::as_slice) {
                        Some([k, val]) => format!("{} -> {}", render(k), render(val)),
                        _ => render(p),
                    })
                    .collect();
                return format!("Map({})", items.join(", "));
            }
            let items: Vec<String> = fields
                .iter()
                .filter(|(k, _)| !k.starts_with('#'))
                .map(|(k, val)| format!("{k}: {}", render(val)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

// Sort only unordered ITF wrappers, including those nested in ordered values.
fn same_value(left: &Json, right: &Json) -> bool {
    match (left, right) {
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_value(x, y))
        }
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.get(key)
                        .is_some_and(|other| match (key.as_str(), value, other) {
                            ("#set" | "#map", Json::Array(x), Json::Array(y)) if x != y => {
                                x.len() == y.len() && unordered_key(x) == unordered_key(y)
                            }
                            _ => same_value(value, other),
                        })
                })
        }
        _ => left == right,
    }
}

fn unordered_key(items: &[Json]) -> Vec<String> {
    let mut keys: Vec<_> = items.iter().map(canonical_value).collect();
    keys.sort_unstable();
    keys
}

fn canonical_value(value: &Json) -> String {
    match value {
        Json::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_value)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Json::Object(fields) => {
            let mut pairs: Vec<_> = fields.iter().collect();
            pairs.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
            let mut entries = Vec::with_capacity(pairs.len());
            for (key, item) in pairs {
                let encoded_key = serde_json::to_string(key).expect("JSON string");
                let encoded_value = match (key.as_str(), item) {
                    ("#set" | "#map", Json::Array(items)) => {
                        format!("[{}]", unordered_key(items).join(","))
                    }
                    _ => canonical_value(item),
                };
                entries.push(format!("{encoded_key}:{encoded_value}"));
            }
            format!("{{{}}}", entries.join(","))
        }
        _ => value.to_string(),
    }
}

// ---------------------------------------------------------------- trace

pub struct Trace {
    /// Spec path as recorded in the trace, relative to the repository root.
    pub source: String,
    pub status: String,
    pub vars: Vec<String>,
    /// Rendered value per variable, in `vars` order, for each state.
    pub states: Vec<Vec<String>>,
    changes: Vec<Vec<usize>>,
    /// Index into `Scene::sources` once bound, and the lines each var owns.
    pub file: Option<usize>,
    pub var_lines: Vec<Vec<usize>>,
    pub step: usize,
    pub playing: bool,
    /// Seconds spent on the current step.
    pub elapsed: f64,
    /// Seconds each step is held during playback.
    pub step_secs: f64,
    /// Wrap to the first state after the last. A recording plays once instead.
    pub looping: bool,
}

/// Seconds a step is held during playback, unless `--trace-rate` says otherwise.
pub const STEP_SECS: f64 = 1.6;

/// How long a trace that plays once stays on its last state before it is done.
pub const END_HOLD_SECS: f64 = 1.5;

impl Trace {
    pub fn parse(text: &str) -> Option<Trace> {
        let j = parse_json(text)?;
        let vars: Vec<String> = j
            .get("vars")?
            .as_array()?
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<_>>()?;
        let raw_states = j.get("states")?.as_array()?;
        if raw_states.is_empty() {
            return None;
        }
        let mut states = Vec::with_capacity(raw_states.len());
        let mut changes = Vec::with_capacity(raw_states.len());
        let mut previous_values = vec![None; vars.len()];
        for state in raw_states {
            let row = state.as_object()?;
            let mut current = Vec::with_capacity(vars.len());
            let mut changed = Vec::new();
            for (i, (name, previous)) in vars.iter().zip(&mut previous_values).enumerate() {
                let value = row.get(name);
                let is_same = match (*previous, value) {
                    (Some(a), Some(b)) => same_value(a, b),
                    (None, None) => !states.is_empty(),
                    _ => false,
                };
                if !is_same {
                    changed.push(i);
                }
                current.push(value.map(render).unwrap_or_default());
                *previous = value;
            }
            states.push(current);
            changes.push(changed);
        }
        let meta = j.get("#meta");
        Some(Trace {
            source: meta
                .and_then(|m| m.get("source"))
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
            status: meta
                .and_then(|m| m.get("status"))
                .and_then(Json::as_str)
                .unwrap_or("unknown")
                .to_string(),
            var_lines: vec![Vec::new(); vars.len()],
            vars,
            states,
            changes,
            file: None,
            step: 0,
            playing: true,
            elapsed: 0.0,
            step_secs: STEP_SECS,
            looping: true,
        })
    }

    /// Locates the spec among the scanned sources and the lines each variable
    /// owns: its `var` declaration and every line that assigns it (`name'`).
    pub fn bind(&mut self, sources: &[SourceFile]) -> bool {
        self.file = None;
        for lines in &mut self.var_lines {
            lines.clear();
        }
        let normalized = self.source.replace('\\', "/");
        let unqualified = normalized.trim_start_matches("./");
        let index = sources
            .iter()
            .position(|s| s.path == self.source)
            .or_else(|| sources.iter().position(|s| s.path == unqualified))
            .or_else(|| {
                sources
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| {
                        unqualified
                            .strip_suffix(&s.path)
                            .is_some_and(|prefix| prefix.ends_with('/'))
                    })
                    .max_by_key(|(_, s)| s.path.len())
                    .map(|(i, _)| i)
            });
        let Some(i) = index else {
            return false;
        };
        let src = &sources[i];
        let mut code = Vec::new();
        for line in 0..src.line_count() {
            let (text, classes) = src.line(line);
            code.clear();
            code.extend(text.iter().zip(classes).map(|(&c, &class)| {
                if matches!(class, C_COMMENT | C_STRING) {
                    b' '
                } else {
                    c
                }
            }));
            for (v, name) in self.vars.iter().enumerate() {
                if owns(&code, name.as_bytes()) {
                    self.var_lines[v].push(line);
                }
            }
        }
        self.file = Some(i);
        true
    }

    /// Variables whose value differs from the previous state.
    pub fn changed(&self, step: usize) -> Vec<usize> {
        self.changes
            .get(step)
            .cloned()
            .unwrap_or_else(|| (0..self.vars.len()).collect())
    }

    /// Rewrites the spec's marks for the current step. Lines belonging to a
    /// variable written now are bright; those written earlier stay dim.
    pub fn apply(&self, ov: &mut Overlay, lines: usize) {
        let Some(file) = self.file else { return };
        let mut marks = ov.files[file].take().map_or_else(Vec::new, |f| f.marks);
        marks.clear();
        marks.resize(lines, M_NONE);
        // The first state already binds every variable. Later past states
        // can only mark the same lines, even when seeking out of order.
        if self.step > 0 {
            for &line in self.var_lines.iter().flatten() {
                if let Some(mark) = marks.get_mut(line) {
                    *mark = M_PAST;
                }
            }
        }
        let changed = self.changes.get(self.step).unwrap_or(&self.changes[0]);
        for &v in changed {
            for &l in &self.var_lines[v] {
                if let Some(m) = marks.get_mut(l) {
                    *m = M_STEP;
                }
            }
        }
        ov.files[file] = Some(FileOverlay {
            state: FileState::Modified,
            rename: None,
            added: 0,
            removed: 0,
            marks,
        });
    }

    /// Advances playback; returns true when the step changed. A fast rate can
    /// take several steps in one frame, so the remainder carries over.
    pub fn tick(&mut self, dt: f64) -> bool {
        let n = self.states.len();
        if !self.playing || n == 0 {
            return false;
        }
        self.elapsed += dt;
        if n == 1 {
            return false;
        }
        let due = (self.elapsed / self.step_secs) as usize;
        // Played once, the trace parks on its last state, and `elapsed` goes
        // on to time the hold.
        let steps = if self.looping {
            due
        } else {
            due.min(n - 1 - self.step)
        };
        if steps == 0 {
            return false;
        }
        self.elapsed -= steps as f64 * self.step_secs;
        self.step = (self.step + steps % n) % n;
        true
    }

    /// A trace that plays once is done after holding its last state.
    pub fn finished(&self) -> bool {
        !self.looping && self.step + 1 >= self.states.len() && self.elapsed >= END_HOLD_SECS
    }

    pub fn seek(&mut self, delta: isize) {
        let n = self.states.len() as isize;
        if n == 0 {
            return;
        }
        self.step = (self.step as isize + delta).rem_euclid(n) as usize;
        self.elapsed = 0.0;
    }

    /// One HUD line per variable for the current state.
    pub fn state_lines(&self) -> Vec<(String, bool)> {
        let changed = self.changed(self.step);
        self.vars
            .iter()
            .enumerate()
            .map(|(v, name)| {
                let value = self.states[self.step].get(v).cloned().unwrap_or_default();
                (format!("{name} = {value}"), changed.contains(&v))
            })
            .collect()
    }
}

/// True when `line` is somewhere a change to `name` can come from: its `var`
/// declaration, or an assignment whose right-hand side is not the variable
/// itself. Quint actions must assign every variable, so the identity writes
/// (`best' = best`) that pad every action carry no information and would
/// otherwise light the whole spec on every step.
fn owns(line: &[u8], name: &[u8]) -> bool {
    if name.is_empty() {
        return false;
    }
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while let Some(p) = line[i..]
        .windows(name.len())
        .position(|w| w == name)
        .map(|p| p + i)
    {
        i = p + name.len();
        if p > 0 && ident(line[p - 1]) {
            continue;
        }
        match line.get(i) {
            // `name' = rhs`
            Some(b'\'') => {
                let mut j = i + 1;
                while line.get(j).is_some_and(|c| c.is_ascii_whitespace()) {
                    j += 1;
                }
                if line.get(j) != Some(&b'=') {
                    continue;
                }
                j += 1;
                while line.get(j).is_some_and(|c| c.is_ascii_whitespace()) {
                    j += 1;
                }
                let start = j;
                while line.get(j).is_some_and(|c| ident(*c)) {
                    j += 1;
                }
                if &line[start..j] != name {
                    return true;
                }
                // `name' = name` only counts if something follows it.
                let mut k = j;
                while line.get(k).is_some_and(|c| c.is_ascii_whitespace()) {
                    k += 1;
                }
                if !matches!(line.get(k), None | Some(b',') | Some(b'}') | Some(b')')) {
                    return true;
                }
            }
            // `var name: T`
            Some(c) if !ident(*c) => {
                let head = std::str::from_utf8(&line[..p]).unwrap_or("").trim_start();
                if head == "var" || head.starts_with("var ") {
                    return true;
                }
            }
            None => {}
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/channel-payout-liveness.inv_escape_naive_safe.itf.json"
    );

    #[test]
    fn parses_itf_values() {
        assert_eq!(render(&parse_json(r##"{"#bigint":"42"}"##).unwrap()), "42");
        assert_eq!(render(&parse_json("true").unwrap()), "true");
        assert_eq!(
            render(&parse_json(r##"{"#set":[{"#bigint":"1"},{"#bigint":"2"}]}"##).unwrap()),
            "Set(1, 2)"
        );
        assert_eq!(
            render(&parse_json(r##"{"#map":[[{"#bigint":"1"},true]]}"##).unwrap()),
            "Map(1 -> true)"
        );
        assert_eq!(
            render(&parse_json(r##"{"a":1,"#meta":{}}"##).unwrap()),
            "{a: 1}"
        );
    }

    #[test]
    fn strict_json_preserves_supplementary_unicode_and_rejects_malformed_input() {
        let trace =
            Trace::parse(r#"{"vars":["x"],"states":[{"x":"\uD83D\uDE00"},{"x":"\uD83D\uDE01"}]}"#)
                .unwrap();
        assert_eq!(trace.states, [["😀"], ["😁"]]);
        assert_eq!(trace.changed(1), [0]);
        for invalid in [
            r#"{"vars":["x"],"states":[{"x":1}]} trailing"#,
            r#"{"vars":["x"],"states":[{"x":"\q"}]}"#,
            r#"{"vars":["x"],"states":[{"x":"\uD83D"}]}"#,
            r#"{"vars":["x"],"states":[{"x":01}]}"#,
            "{\"vars\":[\"x\"],\"states\":[{\"x\":\"line\nbreak\"}]}",
            r#"{"vars":["x"],"states":[]}"#,
        ] {
            assert!(Trace::parse(invalid).is_none(), "{invalid:?}");
        }
    }

    #[test]
    fn unordered_itf_values_are_equal_even_nested_but_sequences_are_ordered() {
        let trace = Trace::parse(r##"{
            "vars":["s","m","seq","typed"],
            "states":[
                {"s":{"#set":[1,2]},"m":{"#map":[[1,{"#set":[3,4]}],[2,true]]},"seq":[1,2],"typed":1},
                {"s":{"#set":[2,1]},"m":{"#map":[[2,true],[1,{"#set":[4,3]}]]},"seq":[2,1],"typed":"1"}
            ]
        }"##).unwrap();
        assert_eq!(trace.changed(1), [2, 3]);
        assert_ne!(trace.states[0][0], trace.states[1][0]);
    }

    #[test]
    fn binding_prefers_component_paths_and_only_marks_quint_code() {
        use crate::scan::{classify, Lang};
        let text = "var best: int\nbest' = best + 1\n// best' = amount\nlet note = \"best' = amount\"\n/*\nbest' = amount\n*/\nbest' = \"literal\" // real write\n";
        let sources = ["bar.qnt", "foobar.qnt", "src/foobar.qnt"]
            .map(|path| classify(path.into(), text, Lang::Quint));
        let mut trace = Trace::parse(
            r##"{"#meta":{"source":"foobar.qnt"},"vars":["best"],"states":[{"best":0}]}"##,
        )
        .unwrap();
        assert!(trace.bind(&sources));
        assert_eq!(trace.file, Some(1));
        assert_eq!(trace.var_lines[0], [0, 1, 7]);
        trace.source = r"C:\checkout\src\foobar.qnt".into();
        assert!(trace.bind(&sources));
        assert_eq!(trace.file, Some(2));
        assert_eq!(trace.var_lines[0], [0, 1, 7]);
        trace.source = "anotherfoobar.qnt".into();
        assert!(!trace.bind(&sources));
        assert_eq!(trace.file, None);
        assert!(trace.var_lines[0].is_empty());
    }

    #[test]
    fn one_state_records_its_final_hold() {
        let mut trace = Trace::parse(r#"{"vars":["x"],"states":[{"x":0}]}"#).unwrap();
        trace.looping = false;
        assert!(!trace.finished());
        assert!(!trace.tick(END_HOLD_SECS / 2.0));
        assert!(!trace.finished());
        assert!(!trace.tick(END_HOLD_SECS / 2.0));
        assert!(trace.finished());
    }

    #[test]
    fn reads_a_real_counterexample() {
        let text = std::fs::read_to_string(REAL).expect("checked-in ITF trace");
        let t = Trace::parse(&text).expect("parses");
        assert_eq!(t.status, "violation");
        assert_eq!(t.source, "formal-specs/channel/channel-payout-liveness.qnt");
        assert_eq!(t.states.len(), 6);
        assert!(t.vars.contains(&"paidNaiveEscape".to_string()));
        // The first state binds every variable, so all of them read as changed.
        assert_eq!(t.changed(0).len(), t.vars.len());
        // Somewhere in the trace a single step must flip the unsafe payout.
        let flips = (1..t.states.len())
            .filter(|&s| !t.changed(s).is_empty())
            .count();
        assert!(flips > 0, "a counterexample must change state");
    }

    #[test]
    fn a_variable_owns_its_declaration_and_its_real_writes() {
        assert!(owns(b"  var best: int", b"best"));
        assert!(owns(b"    best' = amount,", b"best"));
        assert!(owns(b"best' = 1", b"best"));
        assert!(owns(b"    best' = best + 1, disputed' = disputed", b"best"));
        // Reads are not assignments, and neither are longer names.
        assert!(!owns(b"    if (best > 0) {", b"best"));
        assert!(!owns(b"    var bestEffort: int", b"best"));
        assert!(!owns(b"    notbest' = 1", b"best"));
        // Quint pads every action with identity writes; they carry nothing.
        assert!(!owns(b"    best' = best, finalized' = finalized,", b"best"));
        assert!(!owns(b"    best' = best }", b"best"));
    }

    #[test]
    fn a_trace_naming_an_unknown_spec_stays_unbound() {
        // Callers key off the return value; `file` must stay None so nothing
        // downstream can unwrap its way into a panic.
        let text = std::fs::read_to_string(REAL).unwrap();
        let mut t = Trace::parse(&text).unwrap();
        assert!(!t.bind(&[]), "no sources can match");
        assert_eq!(t.file, None);
    }

    #[test]
    fn a_fast_rate_takes_several_steps_per_frame() {
        let text = std::fs::read_to_string(REAL).unwrap();
        let mut t = Trace::parse(&text).unwrap();
        // Twice as many steps as frames (64 and 32 a second, which are exact
        // in binary): two steps a frame.
        t.step_secs = 1.0 / 64.0;
        assert!(t.tick(1.0 / 32.0));
        assert_eq!(t.step, 2);
        // The default rate holds each step, and wraps after the last.
        t.step_secs = STEP_SECS;
        t.step = t.states.len() - 1;
        t.elapsed = 0.0;
        assert!(!t.tick(1.0));
        assert!(t.tick(1.0));
        assert_eq!(t.step, 0);
    }

    #[test]
    fn a_trace_played_once_parks_on_its_last_state_then_finishes() {
        let text = std::fs::read_to_string(REAL).unwrap();
        let mut t = Trace::parse(&text).unwrap();
        let last = t.states.len() - 1;
        t.looping = false;
        t.step_secs = 0.1;
        // One long frame runs past the end: it stops on the last state.
        assert!(t.tick(10.0));
        assert_eq!(t.step, last);
        assert!(t.finished(), "held well past END_HOLD_SECS");

        t.step = last - 1;
        t.elapsed = 0.0;
        assert!(t.tick(0.1));
        assert_eq!(t.step, last);
        assert!(!t.finished(), "just arrived; the hold has not run");
        assert!(!t.tick(END_HOLD_SECS));
        assert_eq!(t.step, last);
        assert!(t.finished());
    }

    #[test]
    fn marks_walk_the_spec_as_the_trace_steps() {
        let text = std::fs::read_to_string(REAL).unwrap();
        let mut t = Trace::parse(&text).unwrap();
        t.file = Some(0);
        t.var_lines = vec![vec![1], vec![2], vec![3], vec![4], vec![5]];
        let mut ov = Overlay::empty(1);

        t.step = 0;
        t.apply(&mut ov, 8);
        assert_eq!(ov.mark(0, 1), M_STEP, "state 0 binds every variable");

        // Later steps keep earlier writes visible, but dimmed.
        t.step = t.states.len() - 1;
        t.apply(&mut ov, 8);
        let now = t.changed(t.step);
        for v in 0..t.vars.len() {
            let want = if now.contains(&v) { M_STEP } else { M_PAST };
            assert_eq!(ov.mark(0, t.var_lines[v][0]), want);
        }

        t.seek(1);
        t.apply(&mut ov, 8);
        assert!((1..=5).all(|line| ov.mark(0, line) == M_STEP));
        assert_eq!(ov.mark(0, 6), M_NONE);
    }
}
