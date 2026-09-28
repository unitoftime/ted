//! The edit journal: a buffer's recent changes, each an `Edit` of the text as it was just
//! before it. Something mirroring the text (a language server) catches up from an older
//! version by replaying them instead of copying the whole text.
//!
//! Replacing the whole text (undo, reloading) is recorded as the one region that differs,
//! so it replays as cheaply as typing.

use std::collections::VecDeque;

use ropey::iter::Chunks;
use ropey::Rope;

use super::Edit;

/// Edits kept at most, and the text they insert at most; older ones are forgotten, and a
/// version from before them has to be caught up some other way.
const MAX_EDITS: usize = 1024;
const MAX_BYTES: usize = 4 << 20;

#[derive(Default)]
pub(crate) struct Journal {
    /// The version `edits` start from.
    base: u64,
    edits: VecDeque<Edit>,
    /// Text the edits insert, in bytes.
    bytes: usize,
}

impl Journal {
    /// Records `edit`, which turned version `version` into the next.
    pub fn record(&mut self, version: u64, edit: Edit) {
        if self.base + self.edits.len() as u64 != version {
            self.clear(version);
        }
        self.bytes += edit.text.len();
        self.edits.push_back(edit);
        while self.edits.len() > MAX_EDITS || self.bytes > MAX_BYTES {
            let Some(old) = self.edits.pop_front() else { break };
            self.bytes -= old.text.len();
            self.base += 1;
        }
    }

    fn clear(&mut self, version: u64) {
        self.edits.clear();
        (self.base, self.bytes) = (version, 0);
    }

    /// The edits from `version` up to `current`, if all are still recorded.
    pub fn since(&self, version: u64, current: u64) -> Option<impl ExactSizeIterator<Item = &Edit>> {
        let skip = usize::try_from(version.checked_sub(self.base)?).ok()?;
        let complete = self.base + self.edits.len() as u64 == current;
        (complete && skip <= self.edits.len()).then(|| self.edits.range(skip..))
    }
}

/// The one edit turning `old` into `new`: whatever lies between what they share at the
/// start and at the end.
pub(crate) fn diff(old: &Rope, new: &Rope) -> Edit {
    let prefix = shared_bytes(old.chunks(), new.chunks(), false);
    let suffix = shared_bytes(reversed(old), reversed(new), true);
    let suffix = suffix.min(old.len_bytes().min(new.len_bytes()) - prefix);
    // A byte count can end inside a char: widen to whole chars, which both texts share.
    let start = old.byte_to_char(prefix);
    let char_ceil = |r: &Rope, byte: usize| {
        let c = r.byte_to_char(byte);
        c + usize::from(r.char_to_byte(c) < byte)
    };
    let old_end = char_ceil(old, old.len_bytes() - suffix);
    let new_end = char_ceil(new, new.len_bytes() - suffix);
    Edit::new(start..old_end, new.slice(start..new_end).to_string())
}

/// `rope`'s chunks, last to first.
fn reversed(rope: &Rope) -> Chunks<'_> {
    rope.chunks_at_byte(rope.len_bytes()).0.reversed()
}

/// Bytes the chunk streams `a` and `b` have in common at their start or, fed their chunks
/// last to first, at their end.
fn shared_bytes<'a>(mut a: impl Iterator<Item = &'a str>, mut b: impl Iterator<Item = &'a str>, from_end: bool) -> usize {
    let (mut x, mut y): (&[u8], &[u8]) = (&[], &[]);
    let mut shared = 0;
    loop {
        if x.is_empty() {
            let Some(chunk) = a.next() else { return shared };
            x = chunk.as_bytes();
            continue;
        }
        if y.is_empty() {
            let Some(chunk) = b.next() else { return shared };
            y = chunk.as_bytes();
            continue;
        }
        let n = x.len().min(y.len());
        let (xs, ys) = if from_end { (&x[x.len() - n..], &y[y.len() - n..]) } else { (&x[..n], &y[..n]) };
        if xs != ys {
            let same = |(p, q): &(&u8, &u8)| p == q;
            let run = if from_end {
                xs.iter().rev().zip(ys.iter().rev()).take_while(same).count()
            } else {
                xs.iter().zip(ys).take_while(same).count()
            };
            return shared + run;
        }
        shared += n;
        if from_end {
            (x, y) = (&x[..x.len() - n], &y[..y.len() - n]);
        } else {
            (x, y) = (&x[n..], &y[n..]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_finds_the_changed_region() {
        let cases = [
            ("hello world", "hello brave world", 6..6, "brave "),
            ("abc", "abc", 3..3, ""),
            ("", "new", 0..0, "new"),
            ("aaa", "aa", 2..3, ""),
            ("naïve café", "naïve cafés", 10..10, "s"),
            ("é", "è", 0..1, "è"),
        ];
        for (old, new, range, text) in cases {
            let edit = diff(&Rope::from_str(old), &Rope::from_str(new));
            assert_eq!((edit.range, edit.text.as_str()), (range, text), "{old:?} -> {new:?}");
        }
    }
}
