//! Decorations: styled char ranges attached to a buffer, grouped into layers by owner
//! (e.g. `"compilation"`, `"lsp-diagnostics"`). They move with the text as it is edited,
//! and the renderer draws them without knowing who produced them.

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
    /// Per owner, sorted by start.
    layers: Vec<(String, Vec<Decoration>)>,
}

impl Decorations {
    fn layer_mut(&mut self, owner: &str) -> &mut Vec<Decoration> {
        let idx = match self.layers.iter().position(|(o, _)| o == owner) {
            Some(idx) => idx,
            None => {
                self.layers.push((owner.to_string(), Vec::new()));
                self.layers.len() - 1
            }
        };
        &mut self.layers[idx].1
    }

    /// Replaces `owner`'s decorations.
    pub fn set(&mut self, owner: &str, mut items: Vec<Decoration>) {
        items.sort_by_key(|d| d.range.start);
        *self.layer_mut(owner) = items;
    }

    /// Adds `items` to `owner`'s decorations (cheap when they come after the existing ones).
    pub fn extend(&mut self, owner: &str, items: impl IntoIterator<Item = Decoration>) {
        let layer = self.layer_mut(owner);
        let sorted_until = layer.len();
        layer.extend(items);
        if layer[sorted_until.saturating_sub(1)..].windows(2).any(|w| w[0].range.start > w[1].range.start) {
            layer.sort_by_key(|d| d.range.start);
        }
    }

    pub fn add(&mut self, owner: &str, decoration: Decoration) {
        let layer = self.layer_mut(owner);
        let at = layer.partition_point(|d| d.range.start <= decoration.range.start);
        layer.insert(at, decoration);
    }

    pub fn clear(&mut self, owner: &str) {
        self.layers.retain(|(o, _)| o != owner);
    }

    pub fn clear_all(&mut self) {
        self.layers.clear();
    }

    /// Decorations intersecting `range`, layer by layer in registration order.
    pub fn overlapping(&self, range: Range<usize>) -> impl Iterator<Item = &Decoration> {
        self.layers.iter().flat_map(move |(_, layer)| {
            let candidates = &layer[..layer.partition_point(|d| d.range.start < range.end)];
            candidates.iter().filter(move |d| d.range.end > range.start)
        })
    }

    /// Shifts decorations for `len` chars inserted at `at`.
    pub(crate) fn on_insert(&mut self, at: usize, len: usize) {
        let shift = |p: &mut usize| {
            if *p >= at {
                *p += len;
            }
        };
        for (_, layer) in &mut self.layers {
            for d in layer.iter_mut() {
                // Text typed at a decoration's start goes before it; inside it, it grows.
                if d.range.start >= at {
                    shift(&mut d.range.start);
                    shift(&mut d.range.end);
                } else if d.range.end > at {
                    d.range.end += len;
                }
            }
        }
    }

    /// Shifts decorations for the removal of `removed`, dropping any that become empty.
    pub(crate) fn on_remove(&mut self, removed: Range<usize>) {
        let len = removed.len();
        let map = |p: usize| {
            if p >= removed.end {
                p - len
            } else {
                p.min(removed.start)
            }
        };
        for (_, layer) in &mut self.layers {
            for d in layer.iter_mut() {
                d.range = map(d.range.start)..map(d.range.end);
            }
            layer.retain(|d| !d.range.is_empty());
        }
    }

    /// Keeps decorations inside a buffer of `len` chars (after undo swaps the text).
    pub(crate) fn clamp(&mut self, len: usize) {
        for (_, layer) in &mut self.layers {
            for d in layer.iter_mut() {
                d.range = d.range.start.min(len)..d.range.end.min(len);
            }
            layer.retain(|d| !d.range.is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(d: &Decorations) -> Vec<Range<usize>> {
        d.overlapping(0..usize::MAX).map(|d| d.range.clone()).collect()
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
}
