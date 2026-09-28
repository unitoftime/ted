//! The wire format: JSON-RPC messages framed by `Content-Length` headers, file URIs,
//! positions with columns in the encoding the server negotiated, and text edits.

use std::io::{self, BufRead};
use std::path::{Path, PathBuf};

use ropey::Rope;
use serde_json::{json, Value};
use ted_core::{Buffer, Edit};

/// How the server counts columns within a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    /// The protocol default.
    Utf16,
}

impl Encoding {
    pub fn from_name(name: Option<&str>) -> Self {
        match name {
            Some("utf-8") => Encoding::Utf8,
            _ => Encoding::Utf16,
        }
    }

    fn units(self, c: char) -> usize {
        match self {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
        }
    }

    /// Chars covered by the first `units` columns of `line`.
    pub fn to_chars(self, line: impl Iterator<Item = char>, units: usize) -> usize {
        let mut seen = 0;
        line.take_while(|&c| {
            let fits = seen < units && c != '\n' && c != '\r';
            seen += self.units(c);
            fits
        })
        .count()
    }

    /// Columns taken by the first `chars` chars of `line`.
    pub fn to_units(self, line: impl Iterator<Item = char>, chars: usize) -> usize {
        line.take(chars).map(|c| self.units(c)).sum()
    }

    /// The server's position of char `pos` of `buf`.
    pub fn position(self, buf: &Buffer, pos: usize) -> Position {
        self.position_in(buf.text(), pos)
    }

    /// The server's position of char `pos` of `text`.
    pub fn position_in(self, text: &Rope, pos: usize) -> Position {
        let line = text.char_to_line(pos);
        let col = pos - text.line_to_char(line);
        Position { line, col: self.to_units(text.line(line).chars(), col) }
    }

    /// The char of `buf` at the server's position `p`, clamped to the text.
    pub fn char_of(self, buf: &Buffer, p: Position) -> usize {
        if p.line >= buf.len_lines() {
            return buf.len_chars();
        }
        buf.line_to_char(p.line) + self.to_chars(buf.line(p.line).chars(), p.col)
    }

    /// The chars of `buf` a JSON `Range` covers.
    pub fn chars_of(self, buf: &Buffer, range: &Value) -> Option<std::ops::Range<usize>> {
        let (start, end) = range_from_json(range)?;
        Some(self.char_of(buf, start)..self.char_of(buf, end))
    }
}

/// A position as the server counts it: 0-based line and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}

impl Position {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(Self { line: v.get("line")?.as_u64()? as usize, col: v.get("character")?.as_u64()? as usize })
    }

    pub fn to_json(self) -> Value {
        serde_json::json!({ "line": self.line, "character": self.col })
    }
}

/// A `(start, end)` range from a JSON `Range`.
pub fn range_from_json(v: &Value) -> Option<(Position, Position)> {
    Some((Position::from_json(v.get("start")?)?, Position::from_json(v.get("end")?)?))
}

/// The params of a request about the position of char `pos` in document `uri`.
pub fn text_document_position(uri: &str, buf: &Buffer, pos: usize, encoding: Encoding) -> Value {
    json!({ "textDocument": { "uri": uri }, "position": encoding.position(buf, pos).to_json() })
}

/// Reads `TextEdit[]` (or null) as edits of `buf`.
pub fn edits_from_json(v: &Value, buf: &Buffer, encoding: Encoding) -> Vec<Edit> {
    let one = |e: &Value| Some(Edit::new(encoding.chars_of(buf, e.get("range")?)?, e.get("newText")?.as_str()?));
    v.as_array().map_or_else(Vec::new, |edits| edits.iter().filter_map(one).collect())
}

/// A location in a file, as the server reported it.
#[derive(Debug, Clone)]
pub struct RawLocation {
    pub path: PathBuf,
    pub pos: Position,
}

/// Reads the answer to `definition` / `references`: null, a `Location`, or an array of
/// `Location`s or `LocationLink`s.
pub fn locations_from_json(v: &Value) -> Vec<RawLocation> {
    let one = |v: &Value| -> Option<RawLocation> {
        let (uri, range) = match v.get("targetUri") {
            Some(uri) => (uri, v.get("targetSelectionRange").or_else(|| v.get("targetRange"))?),
            None => (v.get("uri")?, v.get("range")?),
        };
        Some(RawLocation { path: uri_to_path(uri.as_str()?)?, pos: range_from_json(range)?.0 })
    };
    match v {
        Value::Array(items) => items.iter().filter_map(one).collect(),
        Value::Null => Vec::new(),
        single => one(single).into_iter().collect(),
    }
}

pub fn path_to_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &b in path.to_string_lossy().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => uri.push(b as char),
            _ => uri.push_str(&format!("%{:02X}", b)),
        }
    }
    uri
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?.as_bytes();
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut i = 0;
    while i < encoded.len() {
        let hex = encoded.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match (encoded[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(b)) => {
                bytes.push(b);
                i += 3;
            }
            (b, _) => {
                bytes.push(b);
                i += 1;
            }
        }
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

/// Frames a message for the wire.
pub fn encode(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Reads one framed message; `None` at end of stream.
pub fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    let mut header = String::new();
    loop {
        header.clear();
        if reader.read_line(&mut header)? == 0 {
            return Ok(None);
        }
        let line = header.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip() {
        let path = Path::new("/home/me/my project/src/ä.rs");
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///home/me/my%20project/src/%C3%A4.rs");
        assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
    }

    #[test]
    fn columns_convert_between_encodings() {
        let line = "a😀b";
        assert_eq!(Encoding::Utf16.to_units(line.chars(), 2), 3);
        assert_eq!(Encoding::Utf8.to_units(line.chars(), 2), 5);
        assert_eq!(Encoding::Utf16.to_chars(line.chars(), 3), 2);
        assert_eq!(Encoding::Utf8.to_chars(line.chars(), 5), 2);
        assert_eq!(Encoding::Utf16.to_chars("ab\n".chars(), 10), 2);
    }
}
