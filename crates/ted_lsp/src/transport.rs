//! A server process and the two threads that talk to it.
//!
//! The writer thread owns the server's stdin, so the UI thread never blocks on a slow
//! server; it also serializes document text, so syncing a large file costs the UI thread
//! only a rope clone. The reader thread parses everything the server sends: it answers the
//! server's own requests itself, drops chatter (progress, logs), and forwards responses,
//! diagnostics and messages to the UI thread as editor closures.

use std::io::{self, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::Duration;

use ropey::Rope;
use serde_json::{json, Map, Value};
use ted_core::process::{Piped, Program};
use ted_core::{JobContext, JobHandle};

use crate::client::{self, ServerId};
use crate::diagnostics;
use crate::protocol::{self, encode};

/// A message for the server.
pub enum Outgoing {
    Message(Value),
    /// `textDocument/didOpen` with the whole text.
    Open {
        uri: String,
        language: &'static str,
        version: u64,
        text: Rope,
    },
    /// `textDocument/didChange` replacing the whole text.
    Change {
        uri: String,
        version: u64,
        text: Rope,
    },
}

impl Outgoing {
    pub fn notification(method: &str, params: Value) -> Self {
        Outgoing::Message(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    pub fn request(id: i64, method: &str, params: Value) -> Self {
        Outgoing::Message(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
    }

    fn into_json(self) -> Value {
        match self {
            Outgoing::Message(message) => message,
            Outgoing::Open { uri, language, version, text } => {
                let document =
                    json!({ "uri": uri, "languageId": language, "version": version, "text": text.to_string() });
                Outgoing::notification("textDocument/didOpen", json!({ "textDocument": document })).into_json()
            }
            Outgoing::Change { uri, version, text } => {
                let params = json!({
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{ "text": text.to_string() }],
                });
                Outgoing::notification("textDocument/didChange", params).into_json()
            }
        }
    }
}

/// What the reader thread needs to answer the server's requests on its own.
pub struct ServerContext {
    pub section: &'static str,
    pub settings: Value,
    pub root_uri: String,
    pub root_name: String,
}

pub struct Transport {
    pub tx: Sender<Outgoing>,
    child: Child,
    job: JobHandle,
}

impl Transport {
    /// Starts `program`. Everything the server sends is delivered through `ctx`, tagged
    /// with `server`.
    pub fn spawn(
        program: &Program,
        server: ServerId,
        (job, ctx): (JobHandle, JobContext),
        context: ServerContext,
    ) -> io::Result<Self> {
        let Piped { child, stdin, stdout } = program.spawn_piped()?;
        let (tx, rx) = channel();
        thread::spawn(move || write_loop(stdin, rx));
        let replies = tx.clone();
        thread::spawn(move || read_loop(stdout, replies, ctx, server, context));
        Ok(Self { tx, child, job })
    }

    /// Asks the server to exit and stops listening to it. A server that doesn't exit
    /// promptly is killed.
    pub fn shutdown(self) {
        let Self { tx, mut child, job } = self;
        job.cancel();
        let _ = tx.send(Outgoing::request(i64::MAX, "shutdown", Value::Null));
        let _ = tx.send(Outgoing::notification("exit", Value::Null));
        thread::spawn(move || {
            for _ in 0..20 {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

fn write_loop(stdin: ChildStdin, rx: Receiver<Outgoing>) {
    let mut out = BufWriter::new(stdin);
    for message in rx {
        let bytes = encode(&message.into_json());
        if out.write_all(&bytes).and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}

fn read_loop(stdout: ChildStdout, tx: Sender<Outgoing>, ctx: JobContext, server: ServerId, context: ServerContext) {
    let mut reader = BufReader::new(stdout);
    while let Ok(Some(message)) = protocol::read_message(&mut reader) {
        if !dispatch(message, &tx, &ctx, server, &context) {
            return;
        }
    }
    ctx.send(move |ed| client::on_exit(ed, server));
}

/// Routes one message from the server. Returns false once the editor stopped listening.
fn dispatch(
    message: Value,
    tx: &Sender<Outgoing>,
    ctx: &JobContext,
    server: ServerId,
    context: &ServerContext,
) -> bool {
    let Value::Object(mut message) = message else {
        return true;
    };
    let method = message.get("method").and_then(Value::as_str).map(str::to_string);
    let params = message.remove("params").unwrap_or(Value::Null);
    match (method.as_deref(), message.remove("id")) {
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else {
                return true;
            };
            let result = match message.remove("error") {
                Some(error) => {
                    Err(error.get("message").and_then(Value::as_str).unwrap_or("request failed").to_string())
                }
                None => Ok(message.remove("result").unwrap_or(Value::Null)),
            };
            ctx.send(move |ed| client::on_response(ed, server, id, result))
        }
        (Some(method), Some(id)) => {
            let mut reply = Map::new();
            reply.insert("jsonrpc".into(), "2.0".into());
            reply.insert("id".into(), id);
            match answer(context, method, &params) {
                Some(result) => reply.insert("result".into(), result),
                None => reply.insert("error".into(), json!({ "code": -32601, "message": "Method not found" })),
            };
            let _ = tx.send(Outgoing::Message(Value::Object(reply)));
            true
        }
        (Some("textDocument/publishDiagnostics"), None) => match diagnostics::parse(&params) {
            Some((path, found)) => ctx.send(move |ed| diagnostics::publish(ed, path, found)),
            None => true,
        },
        (Some("window/showMessage"), None) => {
            // Errors and warnings only; info messages are chatter.
            let important = params.get("type").and_then(Value::as_u64).is_some_and(|t| t <= 2);
            match params.get("message").and_then(Value::as_str).filter(|_| important) {
                Some(text) => {
                    let text = text.lines().next().unwrap_or_default().to_string();
                    ctx.send(move |ed| client::show_message(ed, server, &text))
                }
                None => true,
            }
        }
        _ => true,
    }
}

/// Answers a request from the server, or `None` if it isn't supported.
fn answer(context: &ServerContext, method: &str, params: &Value) -> Option<Value> {
    match method {
        "workspace/configuration" => {
            let items = params.get("items").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
            Some(Value::Array(
                items.iter().map(|item| setting(context, item.get("section").and_then(Value::as_str))).collect(),
            ))
        }
        "workspace/workspaceFolders" => Some(json!([{ "uri": context.root_uri, "name": context.root_name }])),
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create"
        | "window/showMessageRequest" => Some(Value::Null),
        _ => None,
    }
}

/// The settings under `section` (`gopls`, `gopls.analyses`, ...), or null.
fn setting(context: &ServerContext, section: Option<&str>) -> Value {
    let Some(rest) = section.and_then(|s| s.strip_prefix(context.section)) else {
        return Value::Null;
    };
    let pointer = rest.replace('.', "/");
    context.settings.pointer(&pointer).cloned().unwrap_or(Value::Null)
}
