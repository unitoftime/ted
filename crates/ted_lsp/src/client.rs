//! Clients: one running server per (language, project root), and the documents it serves.
//!
//! All state lives in the editor-wide `Lsp` extension and is touched only on the UI
//! thread. Requests register a callback under their id; the reader thread delivers the
//! response to `on_response`, which runs it. Messages sent while the server is still
//! initializing are queued and flushed once it is ready.
//!
//! Documents are synced whole: the server gets a buffer's full text when its version has
//! changed, once it has been stable for one sync tick, and right before any request about
//! it (`prepare`).

use std::collections::{HashMap, HashSet};
use std::mem;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use ted_core::completion::TriggerChars;
use ted_core::text::collapse_tilde;
use ted_core::{BufferId, Editor};

use crate::protocol::{path_to_uri, Encoding};
use crate::servers::{config_json, ServerSpec};
use crate::transport::{Outgoing, ServerContext, Transport};
use crate::{diagnostics, handles};

/// Index of a client in `Lsp::clients`. Never reused: a restarted server gets a new one,
/// so late messages from the old process can't be mistaken for the new one's.
pub type ServerId = usize;

type Callback = Box<dyn FnOnce(&mut Editor, Result<Value, String>)>;

enum State {
    /// Waiting for the `initialize` response; outgoing messages wait here.
    Starting(Vec<Outgoing>),
    Ready,
    Exited,
}

/// What a server offers besides definitions, references and diagnostics.
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    pub completion: bool,
    pub hover: bool,
    pub rename: bool,
    pub formatting: bool,
    pub range_formatting: bool,
}

impl Capabilities {
    fn from_json(caps: &Value) -> Self {
        let provides = |name| !matches!(caps.get(name), None | Some(Value::Null) | Some(Value::Bool(false)));
        Self {
            completion: provides("completionProvider"),
            hover: provides("hoverProvider"),
            rename: provides("renameProvider"),
            formatting: provides("documentFormattingProvider"),
            range_formatting: provides("documentRangeFormattingProvider"),
        }
    }
}

pub struct Client {
    pub spec: &'static ServerSpec,
    pub root: PathBuf,
    pub encoding: Encoding,
    capabilities: Capabilities,
    /// Characters after which the server offers completions (`.`, `:`).
    triggers: Vec<char>,
    /// The configuration it started with (`lsp.settings.<language>`), sent again once
    /// it is initialized.
    config: Value,
    transport: Option<Transport>,
    state: State,
    next_id: i64,
    pending: HashMap<i64, Callback>,
}

impl Client {
    fn send(&mut self, message: Outgoing) {
        match (&mut self.state, &self.transport) {
            (State::Starting(queue), _) => queue.push(message),
            (State::Ready, Some(transport)) => {
                let _ = transport.tx.send(message);
            }
            _ => {}
        }
    }

    fn is_live(&self) -> bool {
        !matches!(self.state, State::Exited)
    }

    fn name(&self) -> &str {
        self.spec.command
    }
}

/// A buffer the server has open.
pub struct Document {
    pub server: ServerId,
    pub uri: String,
    /// Buffer version the server has.
    sent: u64,
    /// Buffer version seen at the last sync tick; syncing waits until it holds still.
    seen: u64,
}

#[derive(Default)]
pub struct Lsp {
    clients: Vec<Client>,
    pub docs: HashMap<BufferId, Document>,
    /// Servers that failed to start, by (language, root); not retried until `lsp-restart`.
    failed: HashSet<(&'static str, PathBuf)>,
}

/// A document whose server is ready for requests.
#[derive(Debug, Clone)]
pub struct Ready {
    pub server: ServerId,
    pub uri: String,
    /// How the server counts columns.
    pub encoding: Encoding,
}

/// Buffer `buffer`'s document, if its server is ready and `supports` the request about to
/// be made. The server's copy of the text is brought up to date first.
pub fn prepare(ed: &mut Editor, buffer: BufferId, supports: impl Fn(&Capabilities) -> bool) -> Option<Ready> {
    let lsp = ed.ext::<Lsp>()?;
    let doc = lsp.docs.get(&buffer)?;
    let client = &lsp.clients[doc.server];
    if !matches!(client.state, State::Ready) || !supports(&client.capabilities) {
        return None;
    }
    let ready = Ready { server: doc.server, uri: doc.uri.clone(), encoding: client.encoding };
    sync(ed, buffer);
    Some(ready)
}

/// The column encoding of the server serving `path`, if any.
pub fn encoding_for(ed: &Editor, path: &Path) -> Option<Encoding> {
    let lsp = ed.ext::<Lsp>()?;
    let id = ed.buffers.find_path(path)?;
    lsp.docs.get(&id).map(|doc| lsp.clients[doc.server].encoding)
}

/// Opens buffer `id` with its language's server, starting the server if needed.
pub fn attach(ed: &mut Editor, id: BufferId) {
    if !ed.settings.get(handles(ed).enabled) || ed.ext::<Lsp>().is_some_and(|l| l.docs.contains_key(&id)) {
        return;
    }
    let Some(buf) = ed.buffers.get(id) else {
        return;
    };
    let Some(spec) = ServerSpec::for_mode(&buf.mode().name) else {
        return;
    };
    let Some(path) = buf.path().filter(|p| !p.is_dir()).map(Path::to_path_buf) else {
        return;
    };
    let command = ed.settings.get(handles(ed).server(spec).command).to_string();
    if command.trim().is_empty() {
        return;
    }
    let root = spec.root_for(&path);
    let lsp = ed.ext_mut::<Lsp>();
    let existing = lsp.clients.iter().position(|c| c.is_live() && c.spec.language == spec.language && c.root == root);
    let server = match existing {
        Some(server) => server,
        None if lsp.failed.contains(&(spec.language, root.clone())) => return,
        None => match start(ed, spec, &command, &root) {
            Ok(server) => server,
            Err(e) => {
                ed.ext_mut::<Lsp>().failed.insert((spec.language, root));
                ed.set_status(format!("Could not start language server {}", e));
                return;
            }
        },
    };

    let buf = &ed.buffers[id];
    let (version, text) = (buf.version(), buf.text().clone());
    let uri = path_to_uri(&path);
    let lsp = ed.ext_mut::<Lsp>();
    let open = Outgoing::Open { uri: uri.clone(), language: spec.language_id(&path), version, text };
    lsp.clients[server].send(open);
    lsp.docs.insert(id, Document { server, uri, sent: version, seen: version });
    let keymap = handles(ed).keymap;
    ed.buffers[id].enable_keymap(keymap);
    set_triggers(ed, id);
    diagnostics::refresh_buffer(ed, id);
}

/// Hands buffer `id` its server's completion trigger characters.
fn set_triggers(ed: &mut Editor, id: BufferId) {
    let Some(lsp) = ed.ext::<Lsp>() else {
        return;
    };
    let triggers = lsp.docs.get(&id).map(|doc| lsp.clients[doc.server].triggers.clone()).unwrap_or_default();
    if let Some(buf) = ed.buffers.get_mut(id) {
        *buf.local_mut::<TriggerChars>() = TriggerChars(triggers);
    }
}

/// Closes buffer `id` on its server.
pub fn detach(ed: &mut Editor, id: BufferId) {
    let keymap = handles(ed).keymap;
    if let Some(buf) = ed.buffers.get_mut(id) {
        buf.disable_keymap(keymap);
        *buf.local_mut::<TriggerChars>() = TriggerChars::default();
    }
    let lsp = ed.ext_mut::<Lsp>();
    if let Some(doc) = lsp.docs.remove(&id) {
        let params = json!({ "textDocument": { "uri": doc.uri } });
        lsp.clients[doc.server].send(Outgoing::notification("textDocument/didClose", params));
    }
}

pub fn did_save(ed: &mut Editor, id: BufferId) {
    sync(ed, id);
    let lsp = ed.ext_mut::<Lsp>();
    if let Some(doc) = lsp.docs.get(&id) {
        let params = json!({ "textDocument": { "uri": doc.uri } });
        lsp.clients[doc.server].send(Outgoing::notification("textDocument/didSave", params));
    }
}

/// Sends buffer `id`'s text if the server's copy is out of date.
pub fn sync(ed: &mut Editor, id: BufferId) {
    let Some(buf) = ed.buffers.get(id) else {
        return;
    };
    let (version, text) = (buf.version(), buf.text().clone());
    let lsp = ed.ext_mut::<Lsp>();
    let Some(doc) = lsp.docs.get_mut(&id).filter(|doc| doc.sent != version) else {
        return;
    };
    doc.sent = version;
    doc.seen = version;
    let change = Outgoing::Change { uri: doc.uri.clone(), version, text };
    lsp.clients[doc.server].send(change);
}

/// Sync tick: sends documents whose text changed and has held still since the last tick,
/// so typing doesn't send the text on every key.
pub fn sync_idle(ed: &mut Editor) -> bool {
    let Some(lsp) = ed.ext::<Lsp>() else {
        return false;
    };
    let versions: Vec<(BufferId, u64)> =
        lsp.docs.keys().filter_map(|&id| Some((id, ed.buffers.get(id)?.version()))).collect();
    for (id, version) in versions {
        let lsp = ed.ext_mut::<Lsp>();
        let Some(doc) = lsp.docs.get_mut(&id).filter(|doc| doc.sent != version) else {
            continue;
        };
        if doc.seen == version {
            sync(ed, id);
        } else {
            doc.seen = version;
        }
    }
    false
}

/// Sends a request; `on_result` runs on the UI thread with the server's answer. A server
/// that is gone answers with an error right away.
pub fn request(
    ed: &mut Editor,
    server: ServerId,
    method: &str,
    params: Value,
    on_result: impl FnOnce(&mut Editor, Result<Value, String>) + 'static,
) {
    let lsp = ed.ext_mut::<Lsp>();
    let Some(client) = lsp.clients.get_mut(server).filter(|c| c.is_live()) else {
        on_result(ed, Err("language server is not running".to_string()));
        return;
    };
    let id = client.next_id;
    client.next_id += 1;
    client.pending.insert(id, Box::new(on_result));
    client.send(Outgoing::request(id, method, params));
}

pub fn on_response(ed: &mut Editor, server: ServerId, id: i64, result: Result<Value, String>) {
    let callback = ed.ext_mut::<Lsp>().clients.get_mut(server).and_then(|c| c.pending.remove(&id));
    if let Some(callback) = callback {
        callback(ed, result);
    }
}

/// The server process ended on its own. One that dies before it is initialized (not
/// installed, broken setup) is not started again until `lsp-restart`.
pub fn on_exit(ed: &mut Editor, server: ServerId) {
    let Some(client) = ed.ext_mut::<Lsp>().clients.get(server) else {
        return;
    };
    let msg = format!("Language server {} exited (M-x lsp-restart to start it again)", client.name());
    let never_started =
        (matches!(client.state, State::Starting(_))).then(|| (client.spec.language, client.root.clone()));
    let lsp = ed.ext_mut::<Lsp>();
    lsp.failed.extend(never_started);
    stop(ed, server);
    ed.set_status(msg);
}

pub fn show_message(ed: &mut Editor, server: ServerId, text: &str) {
    if let Some(client) = ed.ext::<Lsp>().and_then(|l| l.clients.get(server)) {
        let msg = format!("{}: {}", client.name(), text);
        ed.set_status(msg);
    }
}

/// `lsp-restart`: replaces the active buffer's server with a fresh one and reopens every
/// buffer that used it. Also retries servers that failed to start.
pub fn restart(ed: &mut Editor) {
    let active = ed.active_buffer_id();
    let server = ed.ext::<Lsp>().and_then(|l| l.docs.get(&active)).map(|doc| doc.server);
    let lsp = ed.ext_mut::<Lsp>();
    lsp.failed.clear();
    if let Some(server) = server {
        lsp.docs.retain(|_, doc| doc.server != server);
        stop(ed, server);
    }
    for id in ed.buffers.ids() {
        attach(ed, id);
    }
    if !ed.ext::<Lsp>().is_some_and(|l| l.docs.contains_key(&active)) {
        ed.set_status("No language server for this buffer");
    }
}

/// Shuts `server` down and fails its outstanding requests, so waiting commands move on.
fn stop(ed: &mut Editor, server: ServerId) {
    let Some(client) = ed.ext_mut::<Lsp>().clients.get_mut(server) else {
        return;
    };
    client.state = State::Exited;
    if let Some(transport) = client.transport.take() {
        transport.shutdown();
    }
    for (_, callback) in mem::take(&mut client.pending) {
        callback(ed, Err("language server stopped".to_string()));
    }
}

fn start(ed: &mut Editor, spec: &'static ServerSpec, command: &str, root: &Path) -> Result<ServerId, String> {
    let server = ed.ext_mut::<Lsp>().clients.len();
    let argv: Vec<&str> = command.split_whitespace().collect();
    let root_uri = path_to_uri(root);
    let config = config_json(ed.settings.get(handles(ed).server(spec).config));
    let context = ServerContext {
        section: spec.section,
        settings: config.clone(),
        root_uri: root_uri.clone(),
        root_name: root.file_name().map_or_else(String::new, |n| n.to_string_lossy().to_string()),
    };
    let transport = Transport::spawn(&argv, root, server, ed.job_context(), context)
        .map_err(|e| format!("'{}': {}", command, e))?;
    let _ = transport.tx.send(Outgoing::request(0, "initialize", initialize_params(root, &root_uri, config.clone())));

    let mut client = Client {
        spec,
        root: root.to_path_buf(),
        encoding: Encoding::Utf16,
        capabilities: Capabilities::default(),
        triggers: Vec::new(),
        config,
        transport: Some(transport),
        state: State::Starting(Vec::new()),
        next_id: 1,
        pending: HashMap::new(),
    };
    client.pending.insert(0, Box::new(move |ed, result| initialized(ed, server, result)));
    ed.ext_mut::<Lsp>().clients.push(client);
    ed.set_status(format!("Starting {} in {}", argv.first().unwrap_or(&command), collapse_tilde(root)));
    Ok(server)
}

fn initialize_params(root: &Path, root_uri: &str, settings: Value) -> Value {
    let name = root.file_name().map_or_else(String::new, |n| n.to_string_lossy().to_string());
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "ted" },
        "rootUri": root_uri,
        "rootPath": root,
        "workspaceFolders": [{ "uri": root_uri, "name": name }],
        "initializationOptions": settings,
        "capabilities": {
            "general": { "positionEncodings": ["utf-8", "utf-16"] },
            "textDocument": {
                "synchronization": { "didSave": true },
                "definition": { "linkSupport": true },
                "references": {},
                "publishDiagnostics": {},
                "completion": {
                    "completionItem": { "snippetSupport": false, "insertReplaceSupport": true, "labelDetailsSupport": true },
                    "contextSupport": true,
                },
                "hover": { "contentFormat": ["markdown", "plaintext"] },
                "rename": {},
                "formatting": {},
                "rangeFormatting": {},
            },
            "workspace": {
                "configuration": true,
                "workspaceFolders": true,
                "workspaceEdit": { "documentChanges": true },
            },
            "window": { "workDoneProgress": false },
        },
    })
}

fn initialized(ed: &mut Editor, server: ServerId, result: Result<Value, String>) {
    let Some(client) = ed.ext_mut::<Lsp>().clients.get_mut(server) else {
        return;
    };
    let result = match result {
        Ok(result) => result,
        Err(e) => {
            let msg = format!("Language server {} failed to initialize: {}", client.name(), e);
            stop(ed, server);
            ed.set_status(msg);
            return;
        }
    };
    let caps = result.get("capabilities").unwrap_or(&Value::Null);
    client.encoding = Encoding::from_name(caps.get("positionEncoding").and_then(Value::as_str));
    client.capabilities = Capabilities::from_json(caps);
    let triggers = caps.pointer("/completionProvider/triggerCharacters").and_then(Value::as_array);
    client.triggers = triggers.into_iter().flatten().filter_map(|t| t.as_str()?.chars().next()).collect();
    let State::Starting(queued) = mem::replace(&mut client.state, State::Ready) else {
        return;
    };
    client.send(Outgoing::notification("initialized", json!({})));
    if !client.config.is_null() {
        let params = json!({ "settings": { client.spec.section: client.config.clone() } });
        client.send(Outgoing::notification("workspace/didChangeConfiguration", params));
    }
    for message in queued {
        client.send(message);
    }
    let served: Vec<BufferId> =
        ed.ext_mut::<Lsp>().docs.iter().filter(|(_, doc)| doc.server == server).map(|(&id, _)| id).collect();
    for id in served {
        set_triggers(ed, id);
    }
}
