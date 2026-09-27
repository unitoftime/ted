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
//! Settings (`init.rhai`): `lsp.enabled`, `lsp.diagnostics`, and per language
//! `lsp.server.<language>` (the server's command line, empty to disable it) and
//! `lsp.settings.<language>` (a map sent as the server's configuration). `M-x lsp-restart`
//! restarts the active buffer's server, picking up changed settings.

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

use std::rc::Rc;
use std::time::Duration;

use ted_core::{chain, Editor, KeymapId, Map, Plugin, Setting};

use crate::servers::{ServerSpec, SERVERS};

/// How often edited documents are considered for syncing; text that held still for one
/// tick is sent.
const SYNC_TICK: Duration = Duration::from_millis(200);

pub struct LspPlugin;

/// The settings and keymap the plugin creates (editor-wide, see `handles`).
pub(crate) struct Handles {
    pub enabled: Setting<bool>,
    pub diagnostics: Setting<bool>,
    /// Each language's server settings, in `SERVERS` order.
    servers: Vec<ServerSettings>,
    /// Keys of buffers attached to a server (a minor keymap).
    pub keymap: KeymapId,
}

pub(crate) struct ServerSettings {
    /// The command line starting the server.
    pub command: Setting<String>,
    /// The server's configuration: initialization options and `workspace/configuration`.
    pub config: Setting<Map>,
}

impl Handles {
    pub fn server(&self, spec: &ServerSpec) -> &ServerSettings {
        let index = SERVERS.iter().position(|s| s.language == spec.language).expect("specs come from SERVERS");
        &self.servers[index]
    }
}

pub(crate) fn handles(ed: &Editor) -> &Handles {
    ed.ext::<Handles>().expect("installed by LspPlugin::init")
}

impl Plugin for LspPlugin {
    fn name(&self) -> &str {
        "lsp"
    }

    fn init(&mut self, ed: &mut Editor) {
        let s = &mut ed.settings;
        let handles = Handles {
            enabled: s.define("lsp.enabled", true, "Start language servers for files that have one"),
            diagnostics: s.define("lsp.diagnostics", true, "Show language server errors and warnings in buffers"),
            servers: SERVERS
                .iter()
                .map(|spec| ServerSettings {
                    command: s.define(
                        &format!("lsp.server.{}", spec.language),
                        spec.command,
                        &format!("Command line of the {} language server (empty disables it)", spec.language),
                    ),
                    config: s.define(
                        &format!("lsp.settings.{}", spec.language),
                        Map::new(),
                        &format!(
                            "Configuration sent to the {} language server, under '{}'",
                            spec.language, spec.section
                        ),
                    ),
                })
                .collect(),
            keymap: ed.keymaps.ensure("lsp"),
        };
        ed.watch_setting(handles.diagnostics, diagnostics::refresh_all);
        ed.set_ext(handles);

        diagnostics::register(ed);
        hover::register(ed);
        rename::register(ed);
        ed.commands.register("lsp-restart", "Restart the language server of this buffer", |ed, _| {
            client::restart(ed);
        });
        ed.hooks.buffer_opened.push(Rc::new(client::attach));
        ed.hooks.buffer_saved.push(Rc::new(client::did_save));
        ed.hooks.buffer_killed.push(Rc::new(client::detach));
        ed.hooks.post_command.push(Rc::new(diagnostics::echo));
        ed.add_timer(SYNC_TICK, client::sync_idle);
        chain::register(ed, 100, xref::LspBackend);
        chain::register(ed, 100, completion::LspBackend);
        chain::register(ed, 100, format::LspBackend);
        ed.bind_all("lsp", &[("C-c !", "lsp-diagnostics"), ("C-c .", "lsp-hover")]);
    }
}
