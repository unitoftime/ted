//! Small text primitives shared by buffers and single-line inputs.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Whether `c` can be part of an identifier (a symbol to look up or complete).
pub fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Chars to move backward over: trailing non-word chars, then the word itself.
/// `chars_rev` yields characters walking backward from the cursor.
pub fn word_len_backward(chars_rev: impl Iterator<Item = char>) -> usize {
    let mut chars = chars_rev.peekable();
    let mut n = 0;
    while chars.next_if(|c| !c.is_alphanumeric()).is_some() {
        n += 1;
    }
    while chars.next_if(|c| c.is_alphanumeric()).is_some() {
        n += 1;
    }
    n
}

/// Chars to move forward over: leading non-word chars, then the word itself.
pub fn word_len_forward(chars: impl Iterator<Item = char>) -> usize {
    word_len_backward(chars)
}

pub fn expand_tilde<P: AsRef<Path>>(path: P) -> PathBuf {
    let p = path.as_ref();
    if let Ok(stripped) = p.strip_prefix("~") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(stripped);
        }
    }
    p.to_path_buf()
}

/// Displays `path` with the home directory shortened to `~`, preserving a trailing slash.
pub fn collapse_tilde<P: AsRef<Path>>(path: P) -> String {
    let p = path.as_ref();
    let path_str = p.to_string_lossy();
    let ends_with_slash = path_str.ends_with('/');

    if let Some(home_os) = std::env::var_os("HOME") {
        let home = Path::new(&home_os);
        let canon_home = home.canonicalize().ok();
        for base in std::iter::once(home).chain(canon_home.as_deref()) {
            if let Ok(stripped) = p.strip_prefix(base) {
                let stripped = stripped.to_string_lossy();
                let mut res = if stripped.is_empty() { "~".to_string() } else { format!("~/{}", stripped) };
                if ends_with_slash && !res.ends_with('/') {
                    res.push('/');
                }
                return res;
            }
        }
    }
    path_str.to_string()
}

/// Expands `~`, resolves against the working directory and canonicalizes when possible.
pub fn absolutize<P: AsRef<Path>>(path: P) -> PathBuf {
    let expanded = expand_tilde(path);
    let full = if expanded.is_absolute() {
        expanded
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(expanded),
            Err(_) => expanded,
        }
    };
    full.canonicalize().unwrap_or(full)
}

/// The directory a buffer visiting `path` works in: `path` itself if it is a directory,
/// else its parent; the working directory for buffers without a file.
pub fn directory_of(path: Option<&Path>) -> PathBuf {
    match path.map(absolutize) {
        Some(path) if path.is_dir() => path,
        Some(path) => path.parent().map(Path::to_path_buf).unwrap_or(path),
        None => absolutize("."),
    }
}

/// Truncates `text` to at most `max` chars, ending with an ellipsis when cut.
pub fn truncate_with_ellipsis(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Breaks `text` into lines of at most `width` chars, between words where it can.
pub fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut len = 0;
    for word in text.split(' ') {
        let mut word_len = word.chars().count();
        if len > 0 && len + 1 + word_len > width {
            lines.push(std::mem::take(&mut line));
            len = 0;
        }
        if len > 0 {
            line.push(' ');
            len += 1;
        }
        let mut rest = word;
        while len + word_len > width {
            let split = rest.char_indices().nth(width - len).map_or(rest.len(), |(i, _)| i);
            line.push_str(&rest[..split]);
            lines.push(std::mem::take(&mut line));
            rest = &rest[split..];
            word_len -= width - len;
            len = 0;
        }
        line.push_str(rest);
        len += word_len;
    }
    lines.push(line);
    lines
}

/// Labels for `paths` as short as they can be while telling them apart: each is its file
/// name, grown by directories from the right while another path shares it (`view/mod.rs`,
/// `ui/mod.rs`). Repeats of a path get the same label; a path that needs all of itself is
/// shown whole, `~`-relative.
pub fn short_paths(paths: &[&Path]) -> Vec<String> {
    let mut unique = paths.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let parts: Vec<Vec<&OsStr>> = unique.iter().map(|p| p.iter().collect()).collect();
    let suffix = |i: usize, depth: usize| &parts[i][parts[i].len().saturating_sub(depth)..];
    let mut depth = vec![1; unique.len()];
    loop {
        let mut counts: HashMap<&[&OsStr], usize> = HashMap::new();
        for (i, &d) in depth.iter().enumerate() {
            *counts.entry(suffix(i, d)).or_default() += 1;
        }
        let shared: Vec<usize> =
            (0..unique.len()).filter(|&i| counts[suffix(i, depth[i])] > 1 && depth[i] < parts[i].len()).collect();
        if shared.is_empty() {
            break;
        }
        for i in shared {
            depth[i] += 1;
        }
    }
    let labels: Vec<String> = (0..unique.len())
        .map(|i| match depth[i] < parts[i].len() {
            true => suffix(i, depth[i]).iter().collect::<PathBuf>().display().to_string(),
            false => collapse_tilde(unique[i]),
        })
        .collect();
    paths.iter().map(|p| labels[unique.binary_search(p).expect("`unique` holds every path")].clone()).collect()
}

/// `line` without its surrounding whitespace, and `highlights` (char ranges of `line`)
/// moved to match, dropping what fell in the whitespace.
pub fn trim_highlighted(line: &str, highlights: &[Range<usize>]) -> (String, Vec<Range<usize>>) {
    let start = line.chars().take_while(|c| c.is_whitespace()).count();
    let text = line.trim().to_string();
    let len = text.chars().count();
    let moved = highlights
        .iter()
        .map(|r| r.start.saturating_sub(start).min(len)..r.end.saturating_sub(start).min(len))
        .filter(|r| !r.is_empty())
        .collect();
    (text, moved)
}

/// `text` cut at the char ranges `highlights` (in order, not overlapping) into consecutive
/// pieces, each with whether it is highlighted.
pub fn highlight_runs<'a>(text: &'a str, highlights: &[Range<usize>]) -> Vec<(&'a str, bool)> {
    let bounds: Vec<usize> = text.char_indices().map(|(b, _)| b).chain([text.len()]).collect();
    let len = bounds.len() - 1;
    let mut runs = Vec::with_capacity(highlights.len() * 2 + 1);
    let mut at = 0;
    for range in highlights {
        let (start, end) = (range.start.clamp(at, len), range.end.min(len));
        if start < end {
            runs.extend([(at..start, false), (start..end, true)]);
            at = end;
        }
    }
    runs.push((at..len, false));
    runs.into_iter().filter(|(r, _)| !r.is_empty()).map(|(r, hl)| (&text[bounds[r.start]..bounds[r.end]], hl)).collect()
}

/// Orders names as people read them: ignoring ASCII case, with runs of digits compared as
/// numbers (`file2` before `file10`). Names equal that way fall back to their bytes.
pub fn natural_cmp(a: &[u8], b: &[u8]) -> Ordering {
    fn digits(s: &[u8]) -> usize {
        s.iter().take_while(|c| c.is_ascii_digit()).count()
    }
    fn number(s: &[u8]) -> &[u8] {
        &s[s.iter().take_while(|&&c| c == b'0').count()..]
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let order = if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (m, n) = (digits(&a[i..]), digits(&b[j..]));
            let (x, y) = (number(&a[i..i + m]), number(&b[j..j + n]));
            i += m;
            j += n;
            x.len().cmp(&y.len()).then_with(|| x.cmp(y))
        } else {
            let order = a[i].to_ascii_lowercase().cmp(&b[j].to_ascii_lowercase());
            i += 1;
            j += 1;
            order
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    (a.len() - i).cmp(&(b.len() - j)).then_with(|| a.cmp(b))
}

/// `size` as `ls -h` shows it: bytes below 1K, then one decimal below 10 of a unit (`4.0K`)
/// and whole units above (`12K`), rounded up.
pub fn human_size(size: u64) -> String {
    const UNITS: [char; 6] = ['K', 'M', 'G', 'T', 'P', 'E'];
    if size < 1024 {
        return size.to_string();
    }
    let mut value = size as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let tenths = (value * 10.0).ceil() as u64;
    if tenths < 100 {
        return format!("{}.{}{}", tenths / 10, tenths % 10, UNITS[unit]);
    }
    match value.ceil() as u64 {
        1024 if unit + 1 < UNITS.len() => format!("1.0{}", UNITS[unit + 1]),
        whole => format!("{}{}", whole, UNITS[unit]),
    }
}
