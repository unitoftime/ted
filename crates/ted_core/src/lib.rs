//! ted_core: the editor engine, independent of any frontend.
//!
//! Architecture in brief:
//! - `Buffer`s (text, mode, undo history) live in `Buffers`, addressed by `BufferId`.
//! - `View`s (cursor, scroll) are the leaves of the window `Layout` and refer to buffers.
//! - Every operation is a named command in `Commands`; `Keymaps` map key sequences to them.
//! - Keys resolve against layers: the top modal's keymap, else the buffer's minor keymaps
//!   over its mode's keymap over the global keymap.
//! - Modals (prompts, pickers, custom UIs) sit on a stack and take callbacks.
//! - Everything drawn is styled through named `Face`s; themes override faces by name.
//! - Settings are named, typed and documented, like commands and faces.
//! - Buffers carry decorations (styled ranges that track edits) and typed local state.
//! - Slow work runs as background jobs that send closures back to the UI thread.
//! - Definitions, completion and formatting ask chains of backends (`chain`): language
//!   servers first, built-in fallbacks after.
//! - Plugins extend all of the above through `Editor`; users rebind via `init.rhai`.

pub mod brackets;
pub mod buffer;
pub mod chain;
pub mod command;
pub mod commands;
pub mod completion;
pub mod config;
pub mod doc;
pub mod editor;
pub mod ext;
pub mod face;
pub mod format;
pub mod frame;
pub mod fuzzy;
pub mod grep;
pub mod indent;
pub mod jobs;
pub mod key;
pub mod keymap;
pub mod kill_ring;
pub mod layout;
pub mod locations;
pub mod mode;
pub mod modes;
pub mod plugin;
pub mod project;
pub mod recentf;
pub mod rows;
pub mod session;
pub mod settings;
pub mod syntax;
pub mod text;
pub mod theme;
pub mod ui;
pub mod view;
pub mod xref;

pub use buffer::{Buffer, BufferId, Buffers, Decoration, Edit, Margin, StyledText};
pub use command::{Arg, CommandId, Commands};
pub use config::ConfigOp;
pub use doc::Doc;
pub use editor::{Editor, StartupOptions};
pub use face::{Face, FaceId, Faces};
pub use frame::{Color, Frame, Metrics, Rect, Style};
pub use jobs::{JobContext, JobHandle};
pub use key::{Key, KeyCode, KeyEvent, Modifiers};
pub use keymap::{Binding, Keymap, KeymapId, Keymaps};
pub use kill_ring::KillRing;
pub use layout::{Layout, SplitType, Tile};
pub use locations::{Location, LocationList, Severity};
pub use mode::{IndentStyle, Indentation, Mode, ModeOverrides, ModeRegistry};
pub use plugin::{Hooks, Plugin, SaveToken};
pub use project::Project;
pub use recentf::RecentFiles;
pub use settings::{Map, Setting, Settings, Value};
pub use syntax::{Grammar, SyntaxKind, SyntaxToken};
pub use text::{collapse_tilde, expand_tilde};
pub use theme::Theme;
pub use ui::{Choice, LineInput, Menu, Modal, Picker, PickerItem, Prompt};
pub use view::{Cursor, View, ViewId};
