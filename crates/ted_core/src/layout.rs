//! The window layout: a binary split tree whose leaves own the views.

use crate::buffer::BufferId;
use crate::frame::Rect;
use crate::view::{View, ViewId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitType {
    /// Stacks top and bottom.
    Horizontal,
    /// Places side by side.
    Vertical,
}

/// One node of a layout in pre-order: a split is followed by its first then its second
/// subtree. How a layout is taken apart and rebuilt outside the process (sessions).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tile<V> {
    Split(SplitType, f32),
    Leaf(V),
}

#[derive(Debug, Clone)]
enum Node {
    Leaf(View),
    Split { split: SplitType, ratio: f32, first: Box<Node>, second: Box<Node> },
}

impl Node {
    fn views<'a>(&'a self, out: &mut Vec<&'a View>) {
        match self {
            Node::Leaf(v) => out.push(v),
            Node::Split { first, second, .. } => {
                first.views(out);
                second.views(out);
            }
        }
    }

    fn views_mut<'a>(&'a mut self, out: &mut Vec<&'a mut View>) {
        match self {
            Node::Leaf(v) => out.push(v),
            Node::Split { first, second, .. } => {
                first.views_mut(out);
                second.views_mut(out);
            }
        }
    }

    fn find(&self, id: ViewId) -> Option<&View> {
        match self {
            Node::Leaf(v) => (v.id() == id).then_some(v),
            Node::Split { first, second, .. } => first.find(id).or_else(|| second.find(id)),
        }
    }

    fn find_mut(&mut self, id: ViewId) -> Option<&mut View> {
        match self {
            Node::Leaf(v) => (v.id() == id).then_some(v),
            Node::Split { first, second, .. } => match first.find_mut(id) {
                Some(v) => Some(v),
                None => second.find_mut(id),
            },
        }
    }

    /// Stand-in used while restructuring the tree in place; never observable.
    fn placeholder() -> Node {
        Node::Leaf(View::detached())
    }

    fn map(&mut self, f: impl FnOnce(Node) -> Node) {
        let taken = std::mem::replace(self, Node::placeholder());
        *self = f(taken);
    }

    fn split_leaf(&mut self, target: ViewId, split: SplitType, new_view: &mut Option<View>) -> bool {
        match self {
            Node::Leaf(v) if v.id() == target => {
                let second = Box::new(Node::Leaf(new_view.take().expect("split inserts one view")));
                self.map(|leaf| Node::Split { split, ratio: 0.5, first: Box::new(leaf), second });
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.split_leaf(target, split, new_view) || second.split_leaf(target, split, new_view)
            }
        }
    }

    /// Removes the leaf `target`, promoting its sibling. The root leaf is never removed.
    fn remove_leaf(&mut self, target: ViewId) -> bool {
        let Node::Split { first, second, .. } = self else {
            return false;
        };
        let is_target = |n: &Node| matches!(n, Node::Leaf(v) if v.id() == target);
        if is_target(first) {
            let sibling = std::mem::replace(second.as_mut(), Node::placeholder());
            *self = sibling;
            return true;
        }
        if is_target(second) {
            let sibling = std::mem::replace(first.as_mut(), Node::placeholder());
            *self = sibling;
            return true;
        }
        first.remove_leaf(target) || second.remove_leaf(target)
    }

    fn tiles<'a>(&'a self, out: &mut Vec<Tile<&'a View>>) {
        match self {
            Node::Leaf(v) => out.push(Tile::Leaf(v)),
            Node::Split { split, ratio, first, second } => {
                out.push(Tile::Split(*split, *ratio));
                first.tiles(out);
                second.tiles(out);
            }
        }
    }

    /// Builds the subtree at the front of `tiles`, numbering views from `next_id`. `None`
    /// if the tiles end before the subtree does.
    fn from_tiles<V>(
        tiles: &mut impl Iterator<Item = Tile<V>>,
        view: &mut impl FnMut(ViewId, V) -> View,
        next_id: &mut u32,
    ) -> Option<Node> {
        match tiles.next()? {
            Tile::Leaf(v) => {
                let id = ViewId(*next_id);
                *next_id += 1;
                Some(Node::Leaf(view(id, v)))
            }
            Tile::Split(split, ratio) => {
                let first = Box::new(Node::from_tiles(tiles, view, next_id)?);
                let second = Box::new(Node::from_tiles(tiles, view, next_id)?);
                Some(Node::Split { split, ratio, first, second })
            }
        }
    }

    fn rects(&self, bounds: Rect, sep: f32, views: &mut Vec<(ViewId, Rect)>, seps: &mut Vec<Rect>) {
        if bounds.is_empty() {
            return;
        }
        match self {
            Node::Leaf(v) => views.push((v.id(), bounds)),
            Node::Split { split, ratio, first, second } => {
                let r = if (0.1..=0.9).contains(ratio) { *ratio } else { 0.5 };
                let (a, s, b) = match split {
                    SplitType::Horizontal => {
                        let top_h = ((bounds.h - sep).max(0.0) * r).floor();
                        let bottom_h = (bounds.h - sep - top_h).max(0.0);
                        (
                            Rect::new(bounds.x, bounds.y, bounds.w, top_h),
                            Rect::new(bounds.x, bounds.y + top_h, bounds.w, sep),
                            Rect::new(bounds.x, bounds.y + top_h + sep, bounds.w, bottom_h),
                        )
                    }
                    SplitType::Vertical => {
                        let left_w = ((bounds.w - sep).max(0.0) * r).floor();
                        let right_w = (bounds.w - sep - left_w).max(0.0);
                        (
                            Rect::new(bounds.x, bounds.y, left_w, bounds.h),
                            Rect::new(bounds.x + left_w, bounds.y, sep, bounds.h),
                            Rect::new(bounds.x + left_w + sep, bounds.y, right_w, bounds.h),
                        )
                    }
                };
                first.rects(a, sep, views, seps);
                seps.push(s);
                second.rects(b, sep, views, seps);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Layout {
    root: Node,
    active: ViewId,
    next_id: u32,
}

impl Layout {
    pub fn new(buffer: BufferId, wrap: bool) -> Self {
        let view = View::new(ViewId(0), buffer, wrap);
        Self { root: Node::Leaf(view), active: ViewId(0), next_id: 1 }
    }

    pub fn active_id(&self) -> ViewId {
        self.active
    }

    pub fn set_active(&mut self, id: ViewId) -> bool {
        let exists = self.view(id).is_some();
        if exists {
            self.active = id;
        }
        exists
    }

    pub fn views(&self) -> Vec<&View> {
        let mut out = Vec::new();
        self.root.views(&mut out);
        out
    }

    pub fn views_mut(&mut self) -> Vec<&mut View> {
        let mut out = Vec::new();
        self.root.views_mut(&mut out);
        out
    }

    /// The views showing `buffer`.
    pub fn views_showing(&mut self, buffer: BufferId) -> impl Iterator<Item = &mut View> {
        self.views_mut().into_iter().filter(move |v| v.buffer == buffer)
    }

    pub fn leaf_ids(&self) -> Vec<ViewId> {
        self.views().iter().map(|v| v.id()).collect()
    }

    pub fn view(&self, id: ViewId) -> Option<&View> {
        self.root.find(id)
    }

    pub fn view_mut(&mut self, id: ViewId) -> Option<&mut View> {
        self.root.find_mut(id)
    }

    pub fn active(&self) -> &View {
        self.view(self.active).expect("the active view is always in the layout")
    }

    pub fn active_mut(&mut self) -> &mut View {
        let id = self.active;
        self.view_mut(id).expect("the active view is always in the layout")
    }

    /// Splits the active view, which stays active; the new view shows the same buffer.
    pub fn split(&mut self, split: SplitType) -> ViewId {
        let id = ViewId(self.next_id);
        self.next_id += 1;
        let mut new_view = Some(self.active().split_from(id));
        self.root.split_leaf(self.active, split, &mut new_view);
        id
    }

    /// Closes the active view. Returns false if it is the only one.
    pub fn close_active(&mut self) -> bool {
        if !self.root.remove_leaf(self.active) {
            return false;
        }
        self.active = self.leaf_ids()[0];
        true
    }

    /// Keeps only the active view.
    pub fn maximize_active(&mut self) {
        self.root = Node::Leaf(self.active().clone());
    }

    pub fn cycle(&mut self, delta: isize) {
        let ids = self.leaf_ids();
        let pos = ids.iter().position(|&id| id == self.active).unwrap_or(0);
        self.active = ids[(pos as isize + delta).rem_euclid(ids.len() as isize) as usize];
    }

    /// Replaces this layout with `saved`, keeping view ids unique going forward.
    pub fn restore(&mut self, saved: &Layout) {
        let next_id = self.next_id.max(saved.next_id);
        *self = saved.clone();
        self.next_id = next_id;
    }

    /// The split tree in pre-order.
    pub fn tiles(&self) -> Vec<Tile<&View>> {
        let mut out = Vec::new();
        self.root.tiles(&mut out);
        out
    }

    /// Rebuilds a layout from `tiles` (as `tiles` lists them), making each leaf's view with
    /// `view` and activating the `active`th leaf. `None` if the tiles don't form a tree.
    pub fn from_tiles<V>(
        tiles: impl IntoIterator<Item = Tile<V>>,
        active: usize,
        mut view: impl FnMut(ViewId, V) -> View,
    ) -> Option<Layout> {
        let mut next_id = 0;
        let root = Node::from_tiles(&mut tiles.into_iter(), &mut view, &mut next_id)?;
        let active = ViewId((active as u32).min(next_id - 1));
        Some(Self { root, active, next_id })
    }

    pub fn view_at(&self, x: f32, y: f32) -> Option<ViewId> {
        self.views().iter().find(|v| v.bounds().contains(x, y)).map(|v| v.id())
    }

    /// Screen rects of every view and of the separators between them.
    pub fn rects(&self, bounds: Rect, sep: f32) -> (Vec<(ViewId, Rect)>, Vec<Rect>) {
        let (mut views, mut seps) = (Vec::new(), Vec::new());
        self.root.rects(bounds, sep.max(1.0), &mut views, &mut seps);
        (views, seps)
    }
}
