//! Which server runs for which mode, where its project root is, and what it is told.
//!
//! A server's command is its mode's `lsp.server` setting, so `init.rhai` can point it
//! elsewhere (`mode("python", #{ "lsp.server": "pyright-langserver --stdio" })`) or turn it
//! off with an empty string. What the server is told is the mode's `lsp.settings`, e.g.
//! `mode("go", #{ "lsp.settings": #{ staticcheck: true } })`.

use std::path::{Path, PathBuf};

use serde_json::Value as Json;
use ted_core::{Map, Project, Value};

pub struct ServerSpec {
    /// The language's identifier in the protocol.
    pub language: &'static str,
    /// The ted mode whose buffers it serves.
    pub mode: &'static str,
    /// The mode's default `lsp.server`.
    pub command: &'static str,
    /// Files marking a project root; the outermost one inside the repository wins, so a
    /// workspace shares one server.
    pub root_markers: &'static [&'static str],
    /// Where the server looks up its settings in `workspace/configuration`.
    pub section: &'static str,
    /// The files it is told about when they change on disk (see `watched_files`): whole
    /// names, or `*` and an ending. None for a server that follows the disk by itself.
    pub watched: &'static [&'static str],
}

pub const SERVERS: &[ServerSpec] = &[
    ServerSpec {
        language: "rust",
        mode: "Rust",
        command: "rust-analyzer",
        root_markers: &["Cargo.toml"],
        section: "rust-analyzer",
        watched: &[],
    },
    ServerSpec {
        language: "go",
        mode: "Go",
        command: "gopls",
        root_markers: &["go.work", "go.mod"],
        section: "gopls",
        watched: &["*.go", "go.mod", "go.sum", "go.work"],
    },
    ServerSpec {
        language: "c",
        mode: "C/C++",
        command: "clangd",
        root_markers: &["compile_commands.json", "compile_flags.txt", ".clangd"],
        section: "clangd",
        watched: &[],
    },
    ServerSpec {
        language: "python",
        mode: "Python",
        command: "pylsp",
        root_markers: &["pyproject.toml", "setup.py", "setup.cfg"],
        section: "pylsp",
        watched: &[],
    },
];

/// A server's configuration (its mode's `lsp.settings`) as JSON; none is null.
pub fn config_json(config: &Map) -> Json {
    if config.is_empty() {
        return Json::Null;
    }
    fn convert(value: &Value) -> Json {
        match value {
            Value::Bool(b) => Json::Bool(*b),
            Value::Int(i) => Json::from(*i),
            Value::Float(x) => serde_json::Number::from_f64(*x).map_or(Json::Null, Json::Number),
            Value::Str(s) => Json::String(s.clone()),
            Value::List(items) => Json::Array(items.iter().map(convert).collect()),
            Value::Map(map) => Json::Object(map.iter().map(|(k, v)| (k.clone(), convert(v))).collect()),
        }
    }
    Json::Object(config.iter().map(|(k, v)| (k.clone(), convert(v))).collect())
}

impl ServerSpec {
    pub fn for_mode(mode: &str) -> Option<&'static ServerSpec> {
        SERVERS.iter().find(|s| s.mode == mode)
    }

    /// The root of the project `file` belongs to, for this language.
    pub fn root_for(&self, file: &Path) -> PathBuf {
        let repo = Project::of(Some(file)).root;
        let dir = file.parent().unwrap_or(file);
        let marked = dir
            .ancestors()
            .take_while(|d| d.starts_with(&repo))
            .filter(|d| self.root_markers.iter().any(|m| d.join(m).exists()))
            .last();
        marked.map_or(repo, Path::to_path_buf)
    }

    /// Whether the server is told when `file` changes on disk. Never for a hidden one
    /// (an editor's lock or backup).
    pub fn watches(&self, file: &Path) -> bool {
        let Some(name) = file.file_name().and_then(|name| name.to_str()).filter(|name| !name.starts_with('.')) else {
            return false;
        };
        self.watched.iter().any(|watched| match watched.strip_prefix('*') {
            Some(ending) => name.ends_with(ending),
            None => name == *watched,
        })
    }

    /// The `languageId` of a document, which clangd uses to tell C from C++.
    pub fn language_id(&self, file: &Path) -> &'static str {
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        match self.language {
            "c" if ["cpp", "hpp", "cc", "cxx", "hh"].contains(&ext) => "cpp",
            language => language,
        }
    }
}
