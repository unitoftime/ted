//! Decorations: styled char ranges attached to a buffer, grouped into layers by owner
//! (e.g. `"compilation"`, `"lsp-diagnostics"`). They move with the text as it is edited,
//! and the renderer draws them without knowing who produced them.
//!
//! A layer is a list of chunks sorted by start, each storing its ranges relative to the
//! chunk's start. An edit rewrites only the chunks it reaches into and moves every later
//! chunk by adjusting one number, and a query skips whole chunks that end before it, so
//! the thousands a language server produces stay cheap to keep and draw.

use std::ops::Range;

use crate::face::FaceId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoration {
    pub range: Range<usize>,
    pub face: FaceId,
}

impl Decoration {
    pub fn new(range: Range<usize>, face: FaceId) -> Self {
        Self { range, face }
    }
}

#[derive(Default)]
pub struct Decorations {
    layers: Vec<(String, Layer)>,
}

impl Decorations {
    fn layer_mut(&mut self, owner: &str) -> &mut Layer {
        let idx = match self.layers.iter().position(|(o, _)| o == owner) {
            Some(idx) => idx,
            None => {
                self.layers.push((owner.to_string(), Layer::default()));
                self.layers.len() - 1
            }
        };
        &mut self.layers[idx].1
    }

    /// Replaces `owner`'s decorations.
    pub fn set(&mut self, owner: &str, mut items: Vec<Decoration>) {
        items.sort_by_key(|d| d.range.start);
        *self.layer_mut(owner) = Layer::from_sorted(items);
    }

    /// Adds `items` to `owner`'s decorations (cheap when they come after the existing ones).
    pub fn extend(&mut self, owner: &str, items: impl IntoIterator<Item = Decoration>) {
        let mut items: Vec<Decoration> = items.into_iter().collect();
        items.sort_by_key(|d| d.range.start);
        let Some(first) = items.first().map(|d| d.range.start) else { return };
        let layer = self.layer_mut(owner);
        if layer.last_start().is_some_and(|last| first < last) {
            let mut all: Vec<Decoration> = layer.iter().collect();
            all.append(&mut items);
            all.sort_by_key(|d| d.range.start);
            *layer = Layer::from_sorted(all);
        } else {
            layer.append_sorted(&items);
        }
    }

    pub fn add(&mut self, owner: &str, decoration: Decoration) {
        self.layer_mut(owner).insert(decoration);
    }

    pub fn clear(&mut self, owner: &str) {
        self.layers.retain(|(o, _)| o != owner);
    }

    pub fn clear_all(&mut self) {
        self.layers.clear();
    }

    /// Decorations intersecting `range`, layer by layer in registration order.
    pub fn overlapping(&self, range: Range<usize>) -> impl Iterator<Item = Decoration> + '_ {
        self.layers.iter().flat_map(move |(_, layer)| layer.overlapping(range.clone()))
    }

    /// Shifts decorations for `len` chars inserted at `at`.
    pub(crate) fn on_insert(&mut self, at: usize, len: usize) {
        for (_, layer) in &mut self.layers {
            layer.on_insert(at, len);
        }
    }

    /// Shifts decorations for the removal of `removed`, dropping any that become empty.
    pub(crate) fn on_remove(&mut self, removed: Range<usize>) {
        for (_, layer) in &mut self.layers {
            layer.on_remove(removed.clone());
        }
    }

    /// Keeps decorations inside a buffer of `len` chars (after undo swaps the text).
    pub(crate) fn clamp(&mut self, len: usize) {
        // Removing everything past the end maps every position to at most `len`.
        self.on_remove(len..usize::MAX);
    }
}

/// Decorations per chunk: small enough to rewrite on an edit, large enough that there are
/// few chunks to move.
const CHUNK: usize = 64;

/// One owner's decorations.
#[derive(Default)]
struct Layer {
    chunks: Vec<Chunk>,
}

impl Layer {
    fn from_sorted(items: Vec<Decoration>) -> Self {
        let mut layer = Self::default();
        layer.append_sorted(&items);
        layer
    }

    fn last_start(&self) -> Option<usize> {
        let chunk = self.chunks.last()?;
        Some(chunk.start + chunk.items.last()?.range.start)
    }

    fn iter(&self) -> impl Iterator<Item = Decoration> + '_ {
        self.chunks.iter().flat_map(|c| c.items.iter().map(|d| c.absolute(d)))
    }

    /// Adds sorted `items` that start no earlier than the last decoration.
    fn append_sorted(&mut self, mut items: &[Decoration]) {
        if let Some(chunk) = self.chunks.last_mut() {
            let (fits, rest) = items.split_at(CHUNK.saturating_sub(chunk.items.len()).min(items.len()));
            for d in fits {
                let range = d.range.start - chunk.start..d.range.end - chunk.start;
                chunk.max_end = chunk.max_end.max(range.end);
                chunk.items.push(Decoration::new(range, d.face));
            }
            items = rest;
        }
        self.chunks.extend(items.chunks(CHUNK).map(|items| Chunk::new(items.to_vec())));
    }

    fn insert(&mut self, d: Decoration) {
        let idx = self.chunks.partition_point(|c| c.start <= d.range.start).saturating_sub(1);
        let Some(chunk) = self.chunks.get_mut(idx) else {
            self.chunks.push(Chunk::new(vec![d]));
            return;
        };
        chunk.insert(d);
        if chunk.items.len() > CHUNK {
            let tail = chunk.split_off(CHUNK / 2);
            self.chunks.insert(idx + 1, tail);
        }
    }

    fn overlapping(&self, range: Range<usize>) -> impl Iterator<Item = Decoration> + '_ {
        let candidates = &self.chunks[..self.chunks.partition_point(|c| c.start < range.end)];
        candidates.iter().filter(move |c| c.end() > range.start).flat_map(move |c| {
            let (start, end) = (range.start.saturating_sub(c.start), range.end - c.start);
            let items = &c.items[..c.items.partition_point(|d| d.range.start < end)];
            items.iter().filter(move |d| d.range.end > start).map(|d| c.absolute(d))
        })
    }

    fn on_insert(&mut self, at: usize, len: usize) {
        let first_after = self.chunks.partition_point(|c| c.start < at);
        for chunk in &mut self.chunks[first_after..] {
            chunk.start += len;
        }
        // Earlier chunks start before `at`, so only their contents move.
        for chunk in self.chunks[..first_after].iter_mut().filter(|c| c.end() > at) {
            let at = at - chunk.start;
            for d in &mut chunk.items {
                // Text typed at a decoration's start goes before it; inside it, it grows.
                if d.range.start >= at {
                    d.range.start += len;
                }
                if d.range.end > at {
                    d.range.end += len;
                }
            }
            chunk.max_end += len;
        }
    }

    fn on_remove(&mut self, removed: Range<usize>) {
        let len = removed.len();
        let first_after = self.chunks.partition_point(|c| c.start < removed.end);
        for chunk in &mut self.chunks[first_after..] {
            chunk.start -= len;
        }
        let map = |p: usize| if p >= removed.end { p - len } else { p.min(removed.start) };
        for chunk in self.chunks[..first_after].iter_mut().filter(|c| c.end() > removed.start) {
            chunk.remap(map);
        }
        self.chunks.retain(|c| !c.items.is_empty());
    }
}

/// Up to about `CHUNK` decorations, sorted by start, with ranges relative to `start`.
struct Chunk {
    /// Where the first decoration starts.
    start: usize,
    /// The largest relative end.
    max_end: usize,
    items: Vec<Decoration>,
}

impl Chunk {
    /// A chunk of sorted, non-empty `items` in absolute positions.
    fn new(items: Vec<Decoration>) -> Self {
        let max_end = items.iter().map(|d| d.range.end).max().unwrap_or(0);
        let mut chunk = Self { start: 0, max_end, items };
        chunk.rebase(chunk.items.first().map_or(0, |d| d.range.start));
        chunk
    }

    fn end(&self) -> usize {
        self.start + self.max_end
    }

    fn absolute(&self, d: &Decoration) -> Decoration {
        Decoration::new(self.start + d.range.start..self.start + d.range.end, d.face)
    }

    /// Makes ranges relative to `base`, which is at most the first start.
    fn rebase(&mut self, base: usize) {
        let (from, to) = (self.start, base);
        for d in &mut self.items {
            d.range = d.range.start + from - to..d.range.end + from - to;
        }
        self.max_end = self.max_end + from - to;
        self.start = base;
    }

    fn insert(&mut self, d: Decoration) {
        self.rebase(self.start.min(d.range.start));
        let range = d.range.start - self.start..d.range.end - self.start;
        let at = self.items.partition_point(|x| x.range.start <= range.start);
        self.max_end = self.max_end.max(range.end);
        self.items.insert(at, Decoration::new(range, d.face));
    }

    /// Moves the decorations from `at` on into a chunk of their own.
    fn split_off(&mut self, at: usize) -> Chunk {
        let tail = self.items.split_off(at);
        let tail = tail.iter().map(|d| self.absolute(d)).collect();
        self.max_end = self.items.iter().map(|d| d.range.end).max().unwrap_or(0);
        Chunk::new(tail)
    }

    /// Moves every position through `map`, which must keep them in order, dropping
    /// decorations that become empty.
    fn remap(&mut self, map: impl Fn(usize) -> usize) {
        let base = self.start;
        for d in &mut self.items {
            d.range = map(base + d.range.start)..map(base + d.range.end);
        }
        self.items.retain(|d| !d.range.is_empty());
        *self = Chunk::new(std::mem::take(&mut self.items));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(d: &Decorations) -> Vec<Range<usize>> {
        d.overlapping(0..usize::MAX).map(|d| d.range).collect()
    }

    #[test]
    fn decorations_track_edits() {
        let mut d = Decorations::default();
        d.set("test", vec![Decoration::new(10..20, FaceId::ERROR), Decoration::new(2..4, FaceId::WARNING)]);

        d.on_insert(0, 3); // before both: shift
        assert_eq!(ranges(&d), vec![5..7, 13..23]);
        d.on_insert(15, 2); // inside: grow
        assert_eq!(ranges(&d), vec![5..7, 13..25]);
        d.on_insert(25, 1); // at the end: unaffected
        assert_eq!(ranges(&d), vec![5..7, 13..25]);

        d.on_remove(6..14); // cuts the end of one and the start of the other
        assert_eq!(ranges(&d), vec![5..6, 6..17]);
        d.on_remove(5..6); // swallows the first entirely
        assert_eq!(ranges(&d), vec![5..16]);
    }

    /// Chunk boundaries are invisible: many decorations under random edits and queries
    /// match a plain list edited the simple way.
    #[test]
    fn chunks_match_a_plain_list() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rand = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let mut d = Decorations::default();
        let mut plain: Vec<Range<usize>> = Vec::new();
        for _ in 0..500 {
            let start = rand(5000);
            let longest = if rand(10) == 0 { 2000 } else { 20 };
            let range = start..start + 1 + rand(longest);
            d.add("test", Decoration::new(range.clone(), FaceId::ERROR));
            plain.push(range);
        }
        for _ in 0..2000 {
            let at = rand(6000);
            if rand(2) == 0 {
                let len = 1 + rand(30);
                d.on_insert(at, len);
                for r in &mut plain {
                    let (s, e) = (r.start, r.end);
                    *r = if s >= at { s + len } else { s }..if e > at { e + len } else { e };
                }
            } else {
                let longest = if rand(20) == 0 { 500 } else { 10 };
                let removed = at..at + 1 + rand(longest);
                d.on_remove(removed.clone());
                let map = |p: usize| if p >= removed.end { p - removed.len() } else { p.min(removed.start) };
                plain = plain.iter().map(|r| map(r.start)..map(r.end)).filter(|r| !r.is_empty()).collect();
            }
            let query = at..at + rand(300);
            let mut got: Vec<_> = d.overlapping(query.clone()).map(|d| d.range).collect();
            let mut want: Vec<_> = plain.iter().filter(|r| r.start < query.end && r.end > query.start).cloned().collect();
            got.sort_by_key(|r| (r.start, r.end));
            want.sort_by_key(|r| (r.start, r.end));
            assert_eq!(got, want);
        }
    }
}
