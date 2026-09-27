//! Transient-style menus: a popup of labeled single-key actions, grouped under headings.
//!
//! ```ignore
//! let menu = Menu::new("commit", "Commit")
//!     .group("Create")
//!     .entry('c', "Commit", |ed| start_commit(ed))
//!     .entry('a', "Amend", |ed| amend(ed));
//! ed.push_modal(menu);
//! ```

use std::ops::Range;

use crate::editor::Editor;
use crate::face::FaceId;
use crate::frame::{Frame, Rect};
use crate::keymap::KeymapId;
use crate::ui::Modal;
use crate::view::RenderCtx;

type Action = Box<dyn FnOnce(&mut Editor)>;

struct Entry {
    /// The key as shown, e.g. `c` or `TAB`.
    key_label: String,
    /// Selectable entries have a key and an action; notes only document a binding.
    key: Option<char>,
    label: String,
    action: Option<Action>,
}

struct Group {
    heading: String,
    entries: Vec<Entry>,
}

pub struct Menu {
    id: String,
    title: String,
    groups: Vec<Group>,
    keymap: KeymapId,
}

impl Menu {
    pub fn new(id: &str, title: impl Into<String>) -> Self {
        Self { id: id.to_string(), title: title.into(), groups: Vec::new(), keymap: KeymapId::MENU }
    }

    /// Starts a new group; following entries are listed under `heading`.
    pub fn group(mut self, heading: impl Into<String>) -> Self {
        self.groups.push(Group { heading: heading.into(), entries: Vec::new() });
        self
    }

    pub fn entry(self, key: char, label: impl Into<String>, action: impl FnOnce(&mut Editor) + 'static) -> Self {
        self.push(Entry {
            key_label: key.to_string(),
            key: Some(key),
            label: label.into(),
            action: Some(Box::new(action)),
        })
    }

    /// A display-only line documenting a binding that isn't a single character (`TAB`).
    pub fn note(self, key_label: impl Into<String>, label: impl Into<String>) -> Self {
        self.push(Entry { key_label: key_label.into(), key: None, label: label.into(), action: None })
    }

    fn push(mut self, entry: Entry) -> Self {
        if self.groups.is_empty() {
            self = self.group("");
        }
        self.groups.last_mut().expect("a group exists").entries.push(entry);
        self
    }

    pub fn has_key(&self, key: char) -> bool {
        self.groups.iter().flat_map(|g| &g.entries).any(|e| e.key == Some(key))
    }

    /// Consumes the menu and runs the action bound to `key`, if any.
    pub fn choose(self, ed: &mut Editor, key: char) {
        let entry = self.groups.into_iter().flat_map(|g| g.entries).find(|e| e.key == Some(key));
        if let Some(action) = entry.and_then(|e| e.action) {
            action(ed);
        }
    }
}

impl Modal for Menu {
    fn id(&self) -> &str {
        &self.id
    }

    fn keymap(&self) -> KeymapId {
        self.keymap
    }

    fn label(&self) -> &str {
        &self.title
    }

    /// Groups are laid out as columns in a panel along the bottom of the windows, wrapping
    /// onto further bands of columns when they don't fit side by side.
    fn render_overlay(&mut self, _ed: &Editor, frame: &mut Frame, cx: &RenderCtx) {
        let (faces, m) = (cx.faces, cx.metrics);
        let columns: Vec<(usize, f32)> = self
            .groups
            .iter()
            .map(|group| {
                let key_cols = group.entries.iter().map(|e| e.key_label.chars().count()).max().unwrap_or(1) + 1;
                let chars = group
                    .entries
                    .iter()
                    .map(|e| e.label.chars().count() + key_cols)
                    .chain([group.heading.chars().count()])
                    .max()
                    .unwrap_or(0);
                (key_cols, (chars + 3) as f32 * m.char_w)
            })
            .collect();
        let mut bands: Vec<Range<usize>> = Vec::new();
        let mut band_w = 0.0;
        for (i, &(_, width)) in columns.iter().enumerate() {
            match bands.last_mut() {
                Some(band) if band_w + width <= frame.width - 16.0 => band.end = i + 1,
                _ => {
                    bands.push(i..i + 1);
                    band_w = 0.0;
                }
            }
            band_w += width;
        }
        // Each band: a heading row plus its tallest group.
        let band_rows =
            |band: &Range<usize>| self.groups[band.clone()].iter().map(|g| g.entries.len()).max().unwrap_or(0) + 1;
        let rows = 1 + bands.iter().map(band_rows).sum::<usize>();
        let panel_h = rows as f32 * m.line_h + 8.0;
        let minibuffer_h = m.line_h + 4.0;
        let panel = Rect::new(0.0, (frame.height - minibuffer_h - panel_h).max(0.0), frame.width, panel_h);
        frame.fill_rect(panel, faces.bg(FaceId::POPUP));
        frame.fill_rect(Rect::new(panel.x, panel.y, panel.w, 2.0), faces.bg(FaceId::POPUP_BORDER));

        let text_y = |row: usize| panel.y + 4.0 + row as f32 * m.line_h;
        frame.draw_text_clipped(panel.x + 8.0, text_y(0), self.title.as_str(), faces.text(FaceId::POPUP_HEADER), panel);

        let mut top = 1;
        for band in &bands {
            let mut x = panel.x + 8.0;
            for (group, &(key_cols, width)) in self.groups[band.clone()].iter().zip(&columns[band.clone()]) {
                let clip = Rect::new(x, panel.y, width, panel.h);
                frame.draw_text_clipped(x, text_y(top), group.heading.as_str(), faces.text(FaceId::POPUP_PROMPT), clip);
                for (i, entry) in group.entries.iter().enumerate() {
                    let y = text_y(top + i + 1);
                    let key_face = if entry.key.is_some() { FaceId::POPUP_HIGHLIGHT } else { FaceId::SHADOW };
                    frame.draw_text_clipped(x, y, entry.key_label.as_str(), faces.text(key_face), clip);
                    let label_x = x + key_cols as f32 * m.char_w;
                    frame.draw_text_clipped(label_x, y, entry.label.as_str(), faces.text(FaceId::POPUP), clip);
                }
                x += width;
            }
            top += band_rows(band);
        }
    }
}
