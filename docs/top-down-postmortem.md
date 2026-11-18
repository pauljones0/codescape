# Codescape top-down reliability postmortem

The reported failures fell into five areas. Some produced plausible but incorrect highlights. Others left capture running indefinitely or returned a minimap larger than the requested limit. The reproductions compare the working build from the start of this pass, which already had the top-down performance improvements, with the repaired build.

## Diff highlights and file identity

The starting build silently lost marks on files with tabs or newlines, when opened below the Git root, and in a normal checkout with `diff.mnemonicPrefix` enabled. Git and the scanner disagreed about how to name the same file. The scanner used NUL-delimited paths relative to the scan directory, while the overlay took names from patch headers without decoding Git quoting or normalizing the patch root. The parser also labeled a whole hunk as a replacement when it saw both a deletion and an addition. Pull request patches carry context lines, so an isolated addition turned amber when an unrelated deletion appeared elsewhere in its hunk. The original tests used straightforward names and zero-context hunks, which agreed with those assumptions. [Issues #3](https://github.com/flop-labs/codescape/issues/3), [#10](https://github.com/flop-labs/codescape/issues/10), [#20](https://github.com/flop-labs/codescape/issues/20), [Git diff documentation](https://git-scm.com/docs/git-diff)

The local Git invocation now requests explicit prefixes, line indicators, and relative paths. Pull request patches are scoped to the selected directory before matching. The parser decodes quoted names and classifies contiguous edits separately, flushing an edit as soon as context resumes. Replacements and deletion anchors occupy at most one mark per map line. Rename metadata follows both names into the HUD. When the checkout still contains the old path, the old tile shows the rename and the head-side marks are omitted, since they describe a different file revision. Rename treatment was also reviewed against Repo Atlas. [Repo Atlas #22](https://github.com/rajathpi/repo-atlas/pull/22)

Regression cases cover actual Git paths with tabs, newlines, and spaces, mnemonic prefixes at the checkout root and in a subdirectory, an isolated addition in a pull request hunk, and a rename against both checkout names.

## Minimap dimensions

On `drost-community`, `--atlas 64` returned a 64 × 192 image and `--atlas 0` panicked. Packing tried a fixed ladder of texel scales and left the last failed placement in place when it exhausted the ladder. Only the packing width was constrained throughout the attempt, and texture padding could also exceed an odd pixel cap. The normal-size benchmark exercised successful packing, so it never hit that exit path. [Issue #21](https://github.com/flop-labs/codescape/issues/21)

The packer now needs an explicit successful fit before allocation. It extends the texel ladder down to the smallest supported tiles, bounds padding on both axes, and limits mip levels to what the resulting texture supports. It reports impossible limits to the caller. The real checkout and its `src` subtree fit into both 64 × 64 and 63 × 63. Invalid and impossible caps return an error, and the OpenGL build produced a screenshot with the 63-pixel cap.

## Capture lifetime

The starting build kept writing frames until killed when `--loop` was combined with recording, when the trace failed to load, and when a trace had one state. Capture expected its tour or trace to signal completion. A looping tour never did, a failed trace left no playback to report completion, and a single-state trace returned before advancing the final hold timer. A user input could also interrupt the scripted camera without resolving the recording. Successful multi-state captures exercised only the paths where completion was reachable. [Issues #4](https://github.com/flop-labs/codescape/issues/4), [#19](https://github.com/flop-labs/codescape/issues/19)

Recordings now run one scripted pass and reach their final hold, including one-state traces. Capture validates the frame rate, trace, and source binding before emitting frames and reports directory or frame-write failures. In the OpenGL fixture, the same tour produced 50 frames with or without `--loop`. The single-state trace produced six frames, and the three-state fixture produced ten at its specified rate. Missing, empty, and unbound traces and an invalid rate exited before creating the output directory.

A separate screenshot check hung at `--at inf --loop`. The tour subtracted one shot duration at a time, so an infinite or sufficiently large timestamp never reached a shot. The command now rejects non-finite and negative timestamps. Looping seeks skip whole cycles before advancing through the remaining shots.

## Keyboard navigation

On the native Wayland build, pressing `Z` during the 1.2-second Home animation left the map at the same zoom. Waiting for the animation to finish made the same key work. Each animation tick replaced the camera position with its planned value and erased the previous tick's keyboard movement. Pan and zoom keys now stop the camera transition when they take control, matching pointer input.

## Source discovery and highlighting

Git enumeration supplies the map's source paths. A failed scan previously looked like a successful empty repository, and the long-line guard used UTF-8 bytes where it needed characters. A substring-based exclusion looked for `/vendor/`, so a root-level `vendor/` directory slipped through. The current top-down pipeline reports enumeration failures, counts characters, and applies dependency exclusions to path components at any depth. Tests check the Unicode threshold and distinguish vendor directories from ordinary names containing `vendor`. The existing top-down tour already derives its stops from the opened repository's largest directories and longest file. [Issues #1](https://github.com/flop-labs/codescape/issues/1), [#2](https://github.com/flop-labs/codescape/issues/2), [#13](https://github.com/flop-labs/codescape/issues/13)

Several highlighting errors shared a cause. The lexer recognized punctuation without the state needed to determine whether it was an opener, a closer, or text. A nested Rust comment ended at the inner closer. A raw string ended at an ordinary quote, letting an embedded `//` start a comment. CSS reused C-style line comments and colored everything after the slashes in an unquoted URL. Markdown toggled a boolean at the first three backticks, while YAML treated hash characters in fragments and block-scalar content as comment openers. Tests now check both the contents of these constructs and the ordinary source that follows them. The highlighter tracks Rust comment depth and raw delimiters, Markdown fence marker and length, and YAML block-scalar indentation. CSS keeps block comments and treats `//` as ordinary source. [Issues #9](https://github.com/flop-labs/codescape/issues/9), [#11](https://github.com/flop-labs/codescape/issues/11), [#14](https://github.com/flop-labs/codescape/issues/14), [#15](https://github.com/flop-labs/codescape/issues/15), [#16](https://github.com/flop-labs/codescape/issues/16), [Rust token reference](https://doc.rust-lang.org/reference/tokens.html#raw-string-literals), [CommonMark fences](https://spec.commonmark.org/0.31.2/#fenced-code-blocks), [YAML specification](https://yaml.org/spec/1.2.2/)

## ITF parsing and trace binding

The old JSON reader accepted invalid escapes and trailing data, and collapsed distinct supplementary Unicode characters into the same placeholder. The trace then compared display strings, which made a reordered set appear to change. Binding used a string suffix to find source files and searched for assignments in raw lines. As a result, `bar.qnt` could shadow `src/foobar.qnt`, and examples in comments or strings could light up as writes. An attempt to exclude strings also exposed a lexer bug: Quint's assignment apostrophe opened a string and masked later code. ASCII-only, scalar values and unique, simple paths hid these failures. [Issues #5](https://github.com/flop-labs/codescape/issues/5), [#6](https://github.com/flop-labs/codescape/issues/6), [#7](https://github.com/flop-labs/codescape/issues/7), [#8](https://github.com/flop-labs/codescape/issues/8), [#12](https://github.com/flop-labs/codescape/issues/12)

The trace uses `serde_json` for strict decoding, preserves displayed field order, and precomputes change highlights by comparing structured values. ITF sets and maps ignore entry order even when nested. Sequences remain ordered. The binder looks for exact paths first, then the longest suffix at a path-component boundary. It checks declaration and assignment tokens against the lexer's code spans, including Quint's apostrophe syntax. Regression coverage includes a real Quint counterexample, Unicode pairs, distinct number and string values, reordered and changed nested values, and code interleaved with comments and strings. [Serde JSON](https://github.com/serde-rs/json), [ITF specification](https://apalache-mc.org/docs/adr/015adr-trace.html)

## Performance validation

The release build passed 36 tests. OpenGL smoke tests exercised screenshots, directory stops, recordings, and the 3 × 3 minimum-size atlas. The pull request smoke test used a local `gh` fixture to exercise the command and render paths without depending on network or account state.

Alternating release-build runs collected the same number of glyphs in each case. The single-checkout atlas still took a median of 5 ms to build. The dense-frame timings below measure CPU glyph collection and exclude GPU drawing and desktop presentation.

| Workload | Visible glyphs | Starting build | Repaired build |
| --- | ---: | ---: | ---: |
| `drost-community` at 1920 × 1080 | 63,731 | 0.138 ms | 0.140 ms |
| `drost-community` at 3840 × 2160 | 154,596 | 0.394 ms | 0.408 ms |
| 100 copies at 3840 × 2160 | 215,118 | 0.517 ms | 0.532 ms |

The 16,000-state synthetic trace produced identical change and highlight output. Its full command median rose from 14.6 to 17.2 ms, including reporting. An isolated probe measured JSON decoding at 1.87 ms before and 2.99 ms after. Parsing and preparation together went from 3.04 to 4.93 ms. This cost occurs when opening a trace. Playback looks up the precomputed changes for the current step.
