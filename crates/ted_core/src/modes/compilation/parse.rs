//! Turns raw build output into display text plus what was recognized in it: severity
//! keywords, file locations and their list entries.
//!
//! Runs on the compilation's job thread, so the UI thread only appends finished batches.
//! Each line is cleaned (ANSI escapes dropped, `\r` rewrites resolved) and then tried
//! against a few hand-written matchers, one pass per line without allocating:
//!
//! | Format | Example |
//! |---|---|
//! | GNU (gcc, clang, go, zig, make) | `src/main.c:10:5: error: ...`, `Makefile:3: *** ...` |
//! | rustc | `error[E0308]: ...` then `  --> src/main.rs:10:5` |
//! | Python | `  File "app.py", line 12, in main` |
//! | make directory tracking | `make[1]: Entering directory '/src/lib'` |

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::face::FaceId;
use crate::locations::{Entry, Location, Severity};

/// A face over a byte range of `Batch::text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub face: FaceId,
}

/// Complete output lines ready to append, with their styling and list entries.
#[derive(Debug, Default)]
pub struct Batch {
    pub text: String,
    pub spans: Vec<Span>,
    /// Entries with `line` relative to the first line of `text`.
    pub entries: Vec<Entry>,
    lines: usize,
}

impl Batch {
    /// A blank line then `text` in `face`: ted's own closing message, not program output.
    pub fn footer(text: &str, face: Option<FaceId>) -> Self {
        let spans = face.map(|face| Span { range: 1..1 + text.len(), face }).into_iter().collect();
        Self { text: format!("\n{}\n", text), spans, entries: Vec::new(), lines: 2 }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// What one line was recognized as; ranges are byte offsets into the line.
#[derive(Default)]
struct LineMatch {
    keyword: Option<(Range<usize>, Severity)>,
    location: Option<(Range<usize>, Severity, Location)>,
}

/// Stateful across lines: rustc's `-->` takes the severity of the message above it, and
/// make's directory changes decide what relative paths mean.
pub struct OutputParser {
    /// Base directory, then one per `Entering directory` still open.
    dirs: Vec<PathBuf>,
    severity: Severity,
    /// A trailing line that has not ended yet.
    pending: Vec<u8>,
    clean: Vec<u8>,
}

impl OutputParser {
    pub fn new(dir: PathBuf) -> Self {
        Self { dirs: vec![dir], severity: Severity::Error, pending: Vec::new(), clean: Vec::new() }
    }

    /// Consumes raw output; every completed line is added to `batch`.
    pub fn feed(&mut self, mut bytes: &[u8], batch: &mut Batch) {
        while let Some(nl) = bytes.iter().position(|&b| b == b'\n') {
            if self.pending.is_empty() {
                self.line(&bytes[..nl], batch);
            } else {
                let mut line = std::mem::take(&mut self.pending);
                line.extend_from_slice(&bytes[..nl]);
                self.line(&line, batch);
                line.clear();
                self.pending = line;
            }
            bytes = &bytes[nl + 1..];
        }
        self.pending.extend_from_slice(bytes);
        // A progress line redrawn with `\r` only needs its latest version.
        if let Some(cr) = self.pending.iter().rposition(|&b| b == b'\r').filter(|&cr| cr + 1 < self.pending.len()) {
            self.pending.drain(..=cr);
        }
    }

    /// Ends the output, flushing a final line that had no newline.
    pub fn finish(&mut self, batch: &mut Batch) {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.line(&line, batch);
        }
    }

    fn line(&mut self, raw: &[u8], batch: &mut Batch) {
        clean_line(raw, &mut self.clean);
        let start = batch.text.len();
        batch.text.push_str(&String::from_utf8_lossy(&self.clean));
        let m = self.recognize(&batch.text[start..]);
        batch.text.push('\n');

        if let Some((range, severity)) = m.keyword {
            if let Some(face) = severity.face() {
                batch.spans.push(Span { range: start + range.start..start + range.end, face });
            }
        }
        if let Some((range, severity, location)) = m.location {
            batch.spans.push(Span { range: start + range.start..start + range.end, face: FaceId::LINK });
            batch.entries.push(Entry { line: batch.lines, severity, location });
        }
        batch.lines += 1;
    }

    fn recognize(&mut self, line: &str) -> LineMatch {
        if self.track_directory(line) {
            return LineMatch::default();
        }
        let indent = line.len() - line.trim_start().len();
        let body = &line[indent..];
        if let Some(m) = self.rustc_location(body, indent) {
            return m;
        }
        if let Some(m) = self.python_location(body, indent) {
            return m;
        }
        if let Some((keyword, severity)) = message_keyword(line) {
            self.severity = severity;
            return LineMatch { keyword: Some((keyword, severity)), location: None };
        }
        self.gnu_location(line, indent).unwrap_or_default()
    }

    /// `make[1]: Entering directory '/x'` / `Leaving directory`; true if the line was one.
    fn track_directory(&mut self, line: &str) -> bool {
        if !line.starts_with("make") {
            return false;
        }
        if let Some(i) = line.find("Entering directory ") {
            let quoted = &line[i + "Entering directory ".len()..];
            let dir = quoted.trim_matches(|c| matches!(c, '\'' | '`' | '"'));
            let dir = self.resolve(dir);
            self.dirs.push(dir);
            return true;
        }
        if line.contains("Leaving directory ") {
            if self.dirs.len() > 1 {
                self.dirs.pop();
            }
            return true;
        }
        false
    }

    /// rustc's `--> path:line:col` (severity of the message above) or `::: path:line:col`
    /// (a note pointing elsewhere).
    fn rustc_location(&self, body: &str, indent: usize) -> Option<LineMatch> {
        let severity = match body.get(..4)? {
            "--> " => self.severity,
            "::: " => Severity::Info,
            _ => return None,
        };
        let spec = body[4..].trim_end();
        let (rest, col) = spec.rsplit_once(':')?;
        let (path, line) = rest.rsplit_once(':')?;
        let (line, col) = (line.parse::<usize>().ok()?, col.parse::<usize>().ok()?);
        let start = indent + 4;
        let location = self.location(path, line, Some(col));
        Some(LineMatch { keyword: None, location: Some((start..start + spec.len(), severity, location)) })
    }

    /// Python tracebacks: `File "path", line N`.
    fn python_location(&self, body: &str, indent: usize) -> Option<LineMatch> {
        let quoted = body.strip_prefix("File \"")?;
        let (path, rest) = quoted.split_once('"')?;
        let digits = rest.strip_prefix(", line ")?;
        let len = digits.bytes().take_while(u8::is_ascii_digit).count();
        let line = digits[..len].parse().ok()?;
        let end = indent + "File \"".len() + path.len() + 1 + ", line ".len() + len;
        let location = self.location(path, line, None);
        Some(LineMatch { keyword: None, location: Some((indent..end, Severity::Error, location)) })
    }

    /// `path:line[:col]: [severity:] message`, the format gcc, clang, go, zig and make use.
    /// The location is the line's first word and must end with a colon; the message's
    /// keyword decides the severity, defaulting to error.
    fn gnu_location(&self, line: &str, indent: usize) -> Option<LineMatch> {
        let word_end = line[indent..].find(char::is_whitespace).map_or(line.len(), |i| indent + i);
        let word = &line[indent..word_end];
        let spec = word.strip_suffix(':')?;
        // The path is everything before the first `:<digits>` that completes the spec,
        // which keeps drive letters and colons inside paths intact.
        let (path, line_no, col) = spec.match_indices(':').find_map(|(i, _)| {
            let path = &spec[..i];
            let mut numbers = spec[i + 1..].split(':');
            let line_no = numbers.next()?.parse::<usize>().ok()?;
            let col = match numbers.next() {
                Some(col) => Some(col.parse::<usize>().ok()?),
                None => None,
            };
            numbers.next().is_none().then_some((path, line_no, col))
        })?;
        if path.is_empty() || path.contains("://") || path.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }

        let message_start = word_end + (line.len() - word_end - line[word_end..].trim_start().len());
        let keyword = message_keyword(&line[message_start..])
            .map(|(range, severity)| (message_start + range.start..message_start + range.end, severity));
        let severity = keyword.as_ref().map_or(Severity::Error, |k| k.1);
        let location = self.location(path, line_no, col);
        Some(LineMatch { keyword, location: Some((indent..indent + spec.len(), severity, location)) })
    }

    /// A location from 1-based compiler coordinates.
    fn location(&self, path: &str, line: usize, col: Option<usize>) -> Location {
        Location { path: self.resolve(path), line: line.saturating_sub(1), col: col.unwrap_or(1).saturating_sub(1) }
    }

    fn resolve(&self, path: &str) -> PathBuf {
        let path = Path::new(path.strip_prefix("./").unwrap_or(path));
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.dirs.last().expect("the base directory is never popped").join(path)
        }
    }
}

/// A leading `error:`, `warning[W1]:`, `fatal error:`, `note:` ... (any case): the range
/// of the keyword and code, before the colon.
fn message_keyword(text: &str) -> Option<(Range<usize>, Severity)> {
    const KEYWORDS: &[(&str, Severity)] = &[
        ("error", Severity::Error),
        ("fatal error", Severity::Error),
        ("warning", Severity::Warning),
        ("note", Severity::Info),
        ("help", Severity::Info),
        ("info", Severity::Info),
    ];
    KEYWORDS.iter().find_map(|&(keyword, severity)| {
        let head = text.get(..keyword.len())?;
        if !head.eq_ignore_ascii_case(keyword) {
            return None;
        }
        let rest = &text[keyword.len()..];
        let code_len = match rest.as_bytes().first()? {
            b':' => 0,
            b'[' => rest.find("]:")? + 1,
            _ => return None,
        };
        Some((0..keyword.len() + code_len, severity))
    })
}

/// Copies `raw` (one line, without its `\n`) into `out` as it would appear on a terminal:
/// escape sequences dropped, and text before a carriage return overwritten.
fn clean_line(raw: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            0x1b => i = skip_escape(raw, i),
            b'\r' => {
                if i + 1 < raw.len() {
                    out.clear();
                }
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
}

/// The index after the escape sequence starting at `raw[i]` (CSI, OSC, or two bytes).
fn skip_escape(raw: &[u8], i: usize) -> usize {
    match raw.get(i + 1) {
        Some(b'[') => raw[i + 2..].iter().position(|b| (0x40..=0x7e).contains(b)).map_or(raw.len(), |p| i + 3 + p),
        Some(b']') => {
            let body = &raw[i + 2..];
            let end = body.iter().enumerate().find_map(|(p, &b)| match b {
                0x07 => Some(p + 1),
                0x1b if body.get(p + 1) == Some(&b'\\') => Some(p + 2),
                _ => None,
            });
            end.map_or(raw.len(), |e| i + 2 + e)
        }
        _ => (i + 2).min(raw.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(output: &str) -> Batch {
        let mut parser = OutputParser::new(PathBuf::from("/proj"));
        let mut batch = Batch::default();
        parser.feed(output.as_bytes(), &mut batch);
        parser.finish(&mut batch);
        batch
    }

    fn located(output: &str) -> Vec<(usize, Severity, String, usize, usize)> {
        parse(output)
            .entries
            .into_iter()
            .map(|e| (e.line, e.severity, e.location.path.display().to_string(), e.location.line, e.location.col))
            .collect()
    }

    fn spans(batch: &Batch) -> Vec<(&str, FaceId)> {
        batch.spans.iter().map(|s| (&batch.text[s.range.clone()], s.face)).collect()
    }

    #[test]
    fn recognizes_error_formats() {
        use Severity::*;
        type Expected = Option<(Severity, &'static str, usize, usize)>;
        let cases: &[(&str, Expected)] = &[
            ("src/a.c:10:5: error: expected ';'", Some((Error, "/proj/src/a.c", 9, 4))),
            ("src/a.c:10:5: warning: unused variable", Some((Warning, "/proj/src/a.c", 9, 4))),
            ("./main.go:3:2: undefined: x", Some((Error, "/proj/main.go", 2, 1))),
            ("    foo_test.go:12: got 1, want 2", Some((Error, "/proj/foo_test.go", 11, 0))),
            ("Makefile:4: *** missing separator.  Stop.", Some((Error, "/proj/Makefile", 3, 0))),
            ("/abs/x.zig:1:1: note: here", Some((Info, "/abs/x.zig", 0, 0))),
            ("  File \"app/main.py\", line 7, in run", Some((Error, "/proj/app/main.py", 6, 0))),
            ("   --> src/lib.rs:20:9", Some((Error, "/proj/src/lib.rs", 19, 8))),
            ("   ::: /rust/core.rs:5:1", Some((Info, "/rust/core.rs", 4, 0))),
            ("12:30:45: server started", None),
            ("http://localhost:8080: refused", None),
            ("error: could not compile `ted`", None),
            ("   Compiling ted v0.1.0 (/proj)", None),
        ];
        for (line, expected) in cases {
            let got = located(line).into_iter().next().map(|(_, s, p, l, c)| (s, p, l, c));
            let expected = expected.map(|(s, p, l, c)| (s, p.to_string(), l, c));
            assert_eq!(got, expected, "{}", line);
        }
    }

    #[test]
    fn rustc_arrow_takes_severity_of_its_message() {
        let output = "warning: unused import\n --> src/a.rs:1:5\nerror[E0308]: mismatched types\n --> src/b.rs:2:3\n";
        let batch = parse(output);
        let severities: Vec<_> = batch.entries.iter().map(|e| (e.line, e.severity)).collect();
        assert_eq!(severities, vec![(1, Severity::Warning), (3, Severity::Error)]);
        assert_eq!(
            spans(&batch),
            vec![
                ("warning", FaceId::WARNING),
                ("src/a.rs:1:5", FaceId::LINK),
                ("error[E0308]", FaceId::ERROR),
                ("src/b.rs:2:3", FaceId::LINK),
            ]
        );
    }

    #[test]
    fn make_directories_resolve_relative_paths() {
        let output = "make[1]: Entering directory '/proj/lib'\nx.c:1:1: error: a\nmake[1]: Leaving directory '/proj/lib'\ny.c:2:1: error: b\n";
        let paths: Vec<_> = located(output).into_iter().map(|(_, _, p, _, _)| p).collect();
        assert_eq!(paths, vec!["/proj/lib/x.c", "/proj/y.c"]);
    }

    #[test]
    fn cleans_terminal_output_across_chunks() {
        let mut parser = OutputParser::new(PathBuf::from("/proj"));
        let mut batch = Batch::default();
        parser.feed(b"\x1b[1m\x1b[31merror\x1b[0m: bad\r\n50%\r100", &mut batch);
        assert_eq!(batch.text, "error: bad\n");
        parser.feed(b"%\n\x1b]0;title\x07done", &mut batch);
        parser.finish(&mut batch);
        assert_eq!(batch.text, "error: bad\n100%\ndone\n");
    }
}
