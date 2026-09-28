//! Language servers for ted, built only on `ted_core`'s plugin API.
//!
//! Opening a file whose mode has a server (rust-analyzer, gopls, clangd, pylsp) starts one
//! for its project, shared by every file of that language under the same root. The server
//! then:
//!
//! - answers `find-definition` (`M-.`) and `find-references` ahead of the
//!   tree-sitter fallback, as an xref backend (see `ted_core::xref`);
//! - completes at point (`C-M-i`, see `ted_core::completion`) ahead of the words of open
//!   buffers, and says which characters (`.`, `:`) open completion by itself;
//! - formats (`format-buffer`, `format_on_save`, see `ted_core::format`);
//! - shows the documentation of the symbol at point (`lsp-hover`, `C-c .`);
//! - renames a symbol across the project (`lsp-rename`);
//! - reports diagnostics, underlined in the buffer and listed by `lsp-diagnostics`
//!   (`C-c !`).
//!
//! Settings (`init.rhai`): `lsp.enabled`, `lsp.diagnostics`, `lsp.server` (the server's
//! command line, empty for none) and `lsp.settings` (a map sent as the server's
//! configuration). Servers are per mode, so the last two are set in one:
//! `mode("go", #{ "lsp.settings": #{ staticcheck: true } })`. `M-x lsp-restart` restarts
//! the active buffer's server, picking up changed settings.

mod client;
mod completion;
mod diagnostics;
mod format;
mod hover;
mod protocol;
mod rename;
mod servers;
mod transport;
mod xref;

use std::time::Duration;

use ted_core::{chain, Editor, KeymapDef, KeymapId, Map, Plugin, Setting};

use crate::servers::SERVERS;

/// How often edited documents are considered for syncing; text that held still for one
/// tick is sent.
const SYNC_TICK: Duration = Duration::from_millis(200);

pub struct LspPlugin;

/// The settings and keymap the plugin creates (editor-wide, see `handles`).
pub(crate) struct Handles {
    pub enabled: Setting<bool>,
    pub diagnostics: Setting<bool>,
    /// The command line starting a mode's server.
    pub server: Setting<String>,
    /// A mode's server configuration: initialization options and `workspace/configuration`.
    pub config: Setting<Map>,
    /// Keys of buffers attached to a server (a minor keymap).
    pub keymap: KeymapId,
}

pub(crate) fn handles(ed: &Editor) -> &Handles {
    ed.ext::<Handles>().expect("installed by LspPlugin::init")
}

impl Plugin for LspPlugin {
    fn name(&self) -> &str {
        "lsp"
    }

    fn init(&mut self, ed: &mut Editor) {
        diagnostics::register(ed);
        hover::register(ed);
        rename::register(ed);
        ed.commands.register("lsp-restart", "Restart the language server of this buffer", |ed, _| {
            client::restart(ed);
        });

        let s = &mut ed.settings;
        let handles = Handles {
            enabled: s.define("lsp.enabled", true, "Start language servers for files that have one"),
            diagnostics: s.define("lsp.diagnostics", true, "Show language server errors and warnings in buffers"),
            server: s.define::<String>("lsp.server", "", "Command line of the mode's language server (empty for none)"),
            config: s.define("lsp.settings", Map::new(), "Configuration sent to the mode's language server"),
            keymap: ed
                .define_keymap(KeymapDef::new("lsp").keys(&[("C-c !", "lsp-diagnostics"), ("C-c .", "lsp-hover")])),
        };
        for spec in SERVERS {
            ed.set_mode_setting(spec.mode, "lsp.server", &spec.command.into()).expect("servers are for built-in modes");
        }
        ed.watch_setting(handles.diagnostics, diagnostics::refresh_all);
        ed.set_ext(handles);

        ed.hooks.on_file_visited(client::file_visited);
        ed.hooks.on_buffer_saved(client::did_save);
        ed.hooks.on_buffer_killed(client::detach);
        ed.hooks.on_post_command(diagnostics::echo);
        ed.add_timer(SYNC_TICK, client::sync_idle);
        chain::register(ed, 100, xref::LspBackend);
        chain::register(ed, 100, completion::LspBackend);
        chain::register(ed, 100, format::LspBackend);
    }
}
