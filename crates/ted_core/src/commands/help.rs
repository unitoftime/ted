//! Help: M-x, prefix help, mode help (`h` in special buffers), and the `describe-*`
//! commands. Everything shown is derived from the command and settings registries and the
//! live keymaps, so it never drifts from the actual bindings. Descriptions go to the
//! `help` buffer and end with the `init.rhai` line that changes what they describe, so
//! rebinding starts from `describe-key`.

use std::collections::HashSet;

use crate::buffer::StyledText;
use crate::command::{Arg, CommandId};
use crate::editor::Editor;
use crate::face::FaceId;
use crate::key::{format_seq, Key};
use crate::keymap::{Binding, KeymapId, Resolved};
use crate::mode::Mode;
use crate::settings::Value;
use crate::ui::{Menu, Modal, PickerItem};

const MAX_KEYS_SHOWN: usize = 3;
const HELP_BUFFER: &str = "help";
const DESCRIBE_KEY_MAP: &str = "describe-key";

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("execute-extended-command", "Run any command by name (M-x)", |ed, _| {
        let groups = bindings_under(ed, &[]);
        let ids: Vec<CommandId> = ed.commands.iter().filter(|(_, c)| !c.hidden).map(|(id, _)| id).collect();
        let items = ids
            .iter()
            .map(|&id| {
                let keys =
                    groups.iter().find(|(b, _)| b.command == id && b.arg == Arg::None).map(|(_, k)| k.as_slice());
                command_item(ed, id, &Arg::None, keys.unwrap_or_default())
            })
            .collect();
        ed.pick("execute-extended-command", "Functions (M-x)", items, move |ed, index| {
            ed.call(ids[index], &Arg::None);
        });
    });
    c.register("describe-prefix", "List the commands bound under a key prefix", |ed, arg| {
        let Some(prefix) = arg.str().and_then(|p| Key::parse_seq(p).ok()) else {
            return;
        };
        let groups = bindings_under(ed, &prefix);
        let items = groups.iter().map(|(b, keys)| command_item(ed, b.command, &b.arg, keys)).collect();
        let bindings: Vec<Binding> = groups.into_iter().map(|(b, _)| b).collect();
        let title = format!("Helper: {} commands", format_seq(&prefix));
        ed.pick("describe-prefix", title, items, move |ed, index| {
            let b = &bindings[index];
            ed.call(b.command, &b.arg);
        });
    });

    c.register("mode-help", "Show the keys of this buffer's mode, and run one", |ed, _| mode_help(ed));
    c.register("describe-key", "Show what a key sequence runs and how to rebind it", |ed, _| {
        let layers = ed.active_layers();
        let keymap = ed.keymaps.id(DESCRIBE_KEY_MAP).expect("registered with the command");
        ed.push_modal(DescribeKey { keymap, layers, keys: Vec::new() });
        ed.set_status("Describe key: ");
    });
    let input =
        c.register_hidden("describe-key-input", "Read the next key of the sequence to describe", describe_key_input);
    c.register("describe-bindings", "List every key binding active in this buffer, by keymap", |ed, _| {
        describe_bindings(ed);
    });
    c.register("describe-command", "Show a command's documentation and key bindings", |ed, _| {
        let ids: Vec<CommandId> = ed.commands.iter().filter(|(_, c)| !c.hidden).map(|(id, _)| id).collect();
        let items = ids.iter().map(|&id| command_item(ed, id, &Arg::None, &[])).collect();
        ed.pick("describe-command", "Describe command", items, move |ed, index| describe_command(ed, ids[index]));
    });
    c.register("describe-setting", "Show a setting's value, documentation and how to change it", |ed, _| {
        pick_setting(ed, "describe-setting", "Describe setting", describe_setting);
    });
    c.register("set-setting", "Change a setting for this session", |ed, _| {
        pick_setting(ed, "set-setting", "Set setting", |ed, name| {
            let Some(entry) = ed.settings.entry(&name) else { return };
            let current = match &entry.value {
                Value::Str(s) => s.clone(),
                other => other.to_string(),
            };
            let label = format!("Set {} ({}): ", name, entry.expected());
            ed.prompt("set-setting", label, current, move |ed, text| {
                let Some(like) = ed.settings.entry(&name).map(|e| e.default.clone()) else { return };
                let result = Value::parse_like(&text, &like)
                    .ok_or_else(|| format!("'{}' is not a valid {}", text, like.kind()))
                    .and_then(|value| ed.set_setting(&name, &value));
                match result {
                    Ok(()) => ed.set_status(format!("{} set to {}", name, text)),
                    Err(e) => ed.set_status(e),
                }
            });
        });
    });
    c.register("reload-init", "Reset bindings, settings and faces to defaults, then rerun init.rhai", |ed, _| {
        ed.reload_init();
    });

    let describe_key_map = ed.keymaps.ensure(DESCRIBE_KEY_MAP);
    let map = ed.keymaps.get_mut(describe_key_map);
    map.fallback = Some(input);
    map.opaque = true;
    ed.define_mode(Mode::new("Help").special());
}

/// Shows `text` in the `help` buffer, from the top.
pub fn show_help(ed: &mut Editor, text: StyledText) {
    let id = ed.special_buffer(HELP_BUFFER, "Help");
    ed.buffers[id].set_styled("help", text);
    ed.show_buffer(id);
    for view in ed.layout.views_showing(id) {
        view.reset();
    }
}

/// Reads a key sequence and resolves it against the layers that were active before.
struct DescribeKey {
    keymap: KeymapId,
    layers: Vec<KeymapId>,
    keys: Vec<Key>,
}

impl Modal for DescribeKey {
    fn id(&self) -> &str {
        "describe-key"
    }

    fn keymap(&self) -> KeymapId {
        self.keymap
    }
}

fn describe_key_input(ed: &mut Editor, arg: &Arg) {
    let Some(key) = arg.key() else { return };
    let Some(state) = ed.modal::<DescribeKey>() else { return };
    let resolved = ed.keymaps.resolve(&state.layers, &state.keys, key);
    let mut keys = state.keys.clone();
    match resolved {
        Resolved::Prefix { key } => {
            keys.push(key);
            ed.set_status(format!("Describe key: {}-", format_seq(&keys)));
            if let Some(state) = ed.modal_mut::<DescribeKey>() {
                state.keys = keys;
            }
        }
        Resolved::Command { binding, keymap, .. } => {
            keys.push(key);
            ed.take_modal::<DescribeKey>();
            describe_binding(ed, &keys, &binding, keymap);
        }
        Resolved::Unbound => {
            keys.push(key);
            ed.take_modal::<DescribeKey>();
            ed.set_status(format!("{} is undefined", format_seq(&keys)));
        }
    }
}

fn describe_binding(ed: &mut Editor, keys: &[Key], binding: &Binding, keymap: KeymapId) {
    let seq = format_seq(keys);
    let spec = binding_spec(ed, binding);
    let map_name = ed.keymaps.get(keymap).name.clone();
    let mut text = StyledText::new();
    text.push(&seq, Some(FaceId::KEYWORD));
    text.push(" runs ", None);
    text.push(&spec, Some(FaceId::FUNCTION));
    text.line(&[(" from keymap ", None), (&map_name, Some(FaceId::TYPE))]);
    text.line(&[]);
    text.line(&[("  ", None), (&ed.commands.get(binding.command).doc, None)]);
    text.line(&[]);
    heading(&mut text, "Rebind it in init.rhai");
    let name = ed.commands.get(binding.command).name.clone();
    snippet(&mut text, &bind_line(&map_name, &seq, "<command>"));
    snippet(&mut text, &format!("{}  // new keys for {}", bind_line(&map_name, "<keys>", &name), name));
    snippet(&mut text, &unbind_line(&map_name, &seq));
    ed.set_status(format!("{} runs {}", seq, spec));
    show_help(ed, text);
}

fn describe_command(ed: &mut Editor, id: CommandId) {
    let cmd = ed.commands.get(id);
    let (name, doc) = (cmd.name.clone(), cmd.doc.clone());
    let mut text = StyledText::new();
    text.line(&[(&name, Some(FaceId::FUNCTION))]);
    text.line(&[]);
    text.line(&[("  ", None), (&doc, None)]);
    text.line(&[]);

    let mut bound: Vec<(String, String, String)> = Vec::new();
    for (_, map) in ed.keymaps.iter() {
        for (seq, binding) in map.bindings() {
            if binding.command == id {
                bound.push((map.name.clone(), format_seq(&seq), binding_spec(ed, binding)));
            }
        }
    }
    bound.sort();
    heading(&mut text, "Key bindings");
    if bound.is_empty() {
        text.line(&[("  none (run it with M-x)", Some(FaceId::SHADOW))]);
    }
    for (map, keys, spec) in &bound {
        text.line(&[
            ("  ", None),
            (&format!("{:<16}", keys), Some(FaceId::KEYWORD)),
            (spec, None),
            ("  in ", None),
            (map, Some(FaceId::TYPE)),
        ]);
    }
    text.line(&[]);
    heading(&mut text, "Bind it in init.rhai");
    snippet(&mut text, &bind_line("global", "<keys>", &name));
    snippet(&mut text, &bind_line("<keymap>", "<keys>", &name));
    show_help(ed, text);
}

fn pick_setting(ed: &mut Editor, id: &str, title: &str, then: impl FnOnce(&mut Editor, String) + 'static) {
    let names: Vec<String> = ed.settings.entries().map(|e| e.name.clone()).collect();
    let items = ed.settings.entries().map(|e| PickerItem::new(&e.name, format!("{} · {}", e.value, e.doc))).collect();
    ed.pick(id, title, items, move |ed, index| then(ed, names[index].clone()));
}

fn describe_setting(ed: &mut Editor, name: String) {
    let Some(entry) = ed.settings.entry(&name) else { return };
    let mut text = StyledText::new();
    text.line(&[
        (&entry.name, Some(FaceId::FUNCTION)),
        (" = ", None),
        (&entry.value.to_string(), Some(FaceId::STRING)),
    ]);
    text.line(&[]);
    text.line(&[("  ", None), (&entry.doc, None)]);
    text.line(&[]);
    text.line(&[("  Type:    ", Some(FaceId::SHADOW)), (&entry.expected(), None)]);
    text.line(&[("  Default: ", Some(FaceId::SHADOW)), (&entry.default.to_string(), None)]);
    text.line(&[]);
    heading(&mut text, "Change it in init.rhai (or for this session with M-x set-setting)");
    snippet(&mut text, &format!("set({:?}, {});", entry.name, entry.value));
    show_help(ed, text);
}

/// `help` listing of the active buffer's layers: minor keymaps, the mode's keymap (and
/// the maps it inherits from), then global, each with the name `init.rhai` refers to it by.
fn describe_bindings(ed: &mut Editor) {
    let id = ed.active_buffer_id();
    let buf = &ed.buffers[id];
    let mut text = StyledText::new();
    text.line(&[
        ("Key bindings in ", None),
        (buf.name(), Some(FaceId::KEYWORD)),
        (&format!(" ({} mode)", buf.mode().name), None),
    ]);
    text.line(&[]);
    let mut shadowed: HashSet<Vec<Key>> = HashSet::new();
    for map_id in layer_maps(ed, &ed.buffer_layers(id)) {
        let map = ed.keymaps.get(map_id);
        let mut bindings = map.bindings();
        if bindings.is_empty() {
            continue;
        }
        bindings.sort_by_key(|(seq, _)| (seq.len(), format_seq(seq)));
        let kind = match map_id {
            KeymapId::GLOBAL => "Global keymap",
            _ if Some(map_id) == buf.mode().keymap => "Mode keymap",
            _ if buf.minor_keymaps().contains(&map_id) => "Minor keymap",
            _ => "Inherited keymap",
        };
        text.line(&[(kind, Some(FaceId::HEADING)), (" ", None), (&map.name, Some(FaceId::TYPE))]);
        for (seq, binding) in bindings {
            let face = if shadowed.insert(seq.clone()) { FaceId::KEYWORD } else { FaceId::SHADOW };
            let doc = &ed.commands.get(binding.command).doc;
            let spec = binding_spec(ed, binding);
            text.line(&[
                ("  ", None),
                (&format!("{:<16}", format_seq(&seq)), Some(face)),
                (&format!("{:<32}", spec), None),
                (doc, Some(FaceId::SHADOW)),
            ]);
        }
        text.line(&[]);
    }
    heading(
        &mut text,
        "Rebind in init.rhai: bind(keys, command) for global, bind_mode(keymap, keys, command) otherwise",
    );
    show_help(ed, text);
}

fn heading(text: &mut StyledText, title: &str) {
    text.line(&[(title, Some(FaceId::HEADING))]);
}

fn snippet(text: &mut StyledText, line: &str) {
    text.line(&[("  ", None), (line, Some(FaceId::STRING))]);
}

fn bind_line(keymap: &str, keys: &str, spec: &str) -> String {
    match keymap {
        "global" => format!("bind({:?}, {:?});", keys, spec),
        _ => format!("bind_mode({:?}, {:?}, {:?});", keymap, keys, spec),
    }
}

fn unbind_line(keymap: &str, keys: &str) -> String {
    match keymap {
        "global" => format!("unbind({:?});", keys),
        _ => format!("unbind_mode({:?}, {:?});", keymap, keys),
    }
}

/// A binding as `init.rhai` writes it: the command name and its argument, if any.
fn binding_spec(ed: &Editor, binding: &Binding) -> String {
    let name = &ed.commands.get(binding.command).name;
    match &binding.arg {
        Arg::None | Arg::Key(_) | Arg::Point(..) => name.clone(),
        Arg::Int(n) => format!("{} {}", name, n),
        Arg::Char(c) => format!("{} {}", name, c),
        Arg::Str(s) => format!("{} {}", name, s),
    }
}

/// `layers` expanded with their parent chains, top first, without repeats.
fn layer_maps(ed: &Editor, layers: &[KeymapId]) -> Vec<KeymapId> {
    let mut out = Vec::new();
    for &layer in layers {
        let mut next = Some(layer);
        while let Some(id) = next.filter(|id| !out.contains(id)) {
            out.push(id);
            next = ed.keymaps.get(id).parent;
        }
    }
    out
}

/// A binding reachable through some layers, with every key sequence that runs it
/// (shortest first) and the keymap that binds it.
struct Reachable {
    binding: Binding,
    seqs: Vec<Vec<Key>>,
    keymap: KeymapId,
}

/// The bindings of `layers` (with their parents) whose sequences pass `keep`, each once, in
/// layer order. Sequences shadowed by a higher layer are skipped.
fn reachable(ed: &Editor, layers: &[KeymapId], keep: impl Fn(&[Key]) -> bool) -> Vec<Reachable> {
    let mut seen: HashSet<Vec<Key>> = HashSet::new();
    let mut found: Vec<Reachable> = Vec::new();
    for map in layer_maps(ed, layers) {
        let mut bindings = ed.keymaps.get(map).bindings();
        bindings.sort_by_key(|(seq, _)| (seq.len(), format_seq(seq)));
        for (seq, binding) in bindings {
            if !keep(&seq) || !seen.insert(seq.clone()) {
                continue;
            }
            match found.iter_mut().find(|r| r.binding == *binding) {
                Some(r) => r.seqs.push(seq),
                None => found.push(Reachable { binding: binding.clone(), seqs: vec![seq], keymap: map }),
            }
        }
    }
    found
}

/// Bindings reachable in the active buffer under `prefix`, with their key sequences.
fn bindings_under(ed: &Editor, prefix: &[Key]) -> Vec<(Binding, Vec<String>)> {
    let mut found = reachable(ed, &ed.buffer_layers(ed.active_buffer_id()), |seq| seq.starts_with(prefix));
    found.sort_by_key(|r| r.binding.command);
    found
        .into_iter()
        .map(|r| (r.binding, r.seqs.iter().take(MAX_KEYS_SHOWN).map(|s| format_seq(s)).collect()))
        .collect()
}

/// `h` in special buffers: a menu of the keys the active buffer adds to the global ones
/// (its mode's, the maps that inherits and its minor keymaps), grouped as the mode
/// declares, the rest by keymap. Pressing an entry's key runs it.
fn mode_help(ed: &mut Editor) {
    let id = ed.active_buffer_id();
    let mode = ed.buffers[id].mode().clone();
    let layers: Vec<KeymapId> = ed.buffer_layers(id).into_iter().filter(|&l| l != KeymapId::GLOBAL).collect();
    let this = ed.commands.id("mode-help");
    let mut found = reachable(ed, &layers, |seq| !seq.iter().any(Key::is_mouse));
    found.retain(|r| Some(r.binding.command) != this);
    if found.is_empty() {
        ed.set_status(format!("{} has no keys of its own", mode.name));
        return;
    }

    let mut menu = Menu::new("mode-help", format!("{} keys", mode.name));
    let mut listed = vec![false; found.len()];
    for (heading, names) in &mode.help_groups {
        let found = &found;
        let members: Vec<usize> = names
            .iter()
            .filter_map(|name| ed.commands.id(name))
            .flat_map(|command| (0..found.len()).filter(move |&i| found[i].binding.command == command))
            .filter(|&i| !listed[i])
            .collect();
        menu = help_group(ed, menu, heading.clone(), found, &members, &mut listed);
    }
    for map in layer_maps(ed, &layers) {
        let members: Vec<usize> = (0..found.len()).filter(|&i| !listed[i] && found[i].keymap == map).collect();
        let heading = match map {
            _ if Some(map) == mode.keymap => mode.name.clone(),
            KeymapId::SPECIAL => "General".to_string(),
            _ => title_case(&ed.keymaps.get(map).name),
        };
        menu = help_group(ed, menu, heading, &found, &members, &mut listed);
    }
    ed.push_modal(menu);
}

/// Adds `members` of `found` to `menu` under `heading` (nothing when there are none): a
/// runnable entry for a command with a single-character key, else a note of its keys.
fn help_group(
    ed: &Editor,
    mut menu: Menu,
    heading: String,
    found: &[Reachable],
    members: &[usize],
    listed: &mut [bool],
) -> Menu {
    if members.is_empty() {
        return menu;
    }
    menu = menu.group(heading);
    for &i in members {
        listed[i] = true;
        let Reachable { binding, seqs, .. } = &found[i];
        let doc = ed.commands.get(binding.command).doc.clone();
        let key = seqs.iter().find_map(|seq| match seq.as_slice() {
            [key] => key.printable(),
            _ => None,
        });
        menu = match key {
            Some(ch) => {
                let binding = binding.clone();
                menu.entry(ch, doc, move |ed| ed.call(binding.command, &binding.arg))
            }
            None => menu.note(format_seq(&seqs[0]), doc),
        };
    }
    menu
}

/// A keymap's name as a heading: `git-blame` -> `Git Blame`.
fn title_case(name: &str) -> String {
    name.split('-')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn command_item(ed: &Editor, id: CommandId, arg: &Arg, keys: &[String]) -> PickerItem {
    let cmd = ed.commands.get(id);
    let title = binding_spec(ed, &Binding { command: id, arg: arg.clone() });
    let subtitle = if keys.is_empty() { cmd.doc.clone() } else { format!("{} · {}", keys.join(", "), cmd.doc) };
    PickerItem::new(title, subtitle)
}
