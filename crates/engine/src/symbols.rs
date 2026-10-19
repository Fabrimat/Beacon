//! Workstream E1: the symbol table and reference index (5.7). Powers `beacon rename`,
//! `beacon symbols --json`, the LSP (definition, references, rename, completion, hover), MCP
//! `symbols_*` and the `SymbolUnresolved` / `SymbolDuplicate` / `SymbolCrossOutput` checks.
//!
//! Model:
//! - Every **text** file of the modules under `beacon.sources` and of static outputs is
//!   scanned on its own ([`scan`]) into [`Occurrence`]s with **byte ranges** (`usize`, the
//!   [`Fix`] unit: one range type end to end; line/column and UTF-16 positions are derived
//!   from the file text, or from the per-file line table for [`SymbolEntry`] spans). Scanning
//!   is lexical and per [`FileClass`] (`serde_json` keeps no positions): JSON is read as
//!   JSONC (comments skipped, offsets kept), and ids also hide in lang keys, mcfunction text,
//!   Molang strings and script string literals (4.3). [`scan`] is a pure function of
//!   (path, routed destination, text), so the LSP gets the same result for a dirty buffer.
//! - **Definitions** come from known places. Bedrock JSON self-describes by its top-level key:
//!   `minecraft:entity` / `minecraft:item` / `minecraft:block` `description.identifier`;
//!   the keys of `animations`, `animation_controllers`, `render_controllers`,
//!   `sound_definitions` and `texture_data`. `.lang` keys are lang-key definitions. A function
//!   is defined by its routed destination `BP/functions/<id>.mcfunction` (no text range).
//!   Only routed files (a destination) define anything.
//! - `minecraft:client_entity` and `minecraft:spawn_rules` `description.identifier` are
//!   **references** to the BP entity, never definitions (else every entity with a client file
//!   would be a `SymbolDuplicate`).
//! - **References**: *typed* where the field or command says the kind (the two identifiers
//!   above, `summon <id>`, `type=<id>`, `give <target> <id>`, `clear <target> <id>`,
//!   `setblock <x> <y> <z> <id>`, `fill ... <id>`, `function <path>`); only typed references
//!   to an entity, item, block or function are `SymbolUnresolved` when nothing defines them.
//!   A lang key embedding an id (`entity.demo:car.name`, prefix table [`LANG_KEYS`]) is a
//!   definition of the key **and** a reference to the id (kind known, not typed: never
//!   unresolved), its range covering only `demo:car`. Every other namespaced string (component
//!   groups, events, Molang `'demo:x'`, script literals) is an untyped reference resolved by
//!   name: renamed with the id, never reported. `minecraft:` names are not recorded.
//! - Template heads `` `${ns}:car` `` / `` `${namespace}:car` `` in scripts: [`scan`] records
//!   the local name (`car`, [`Occurrence::local`]); the table resolves it with the file's
//!   namespace (its output's `namespace`, else the project's). Computed ids
//!   (`` `${ns}:${x}` ``) are not seen.
//! - Generator output (content files, `*.gen.ts`) defines ids too: its location is the
//!   generator file, with a range only where the id appears there as exactly one string
//!   literal (`item("demo:key", ...)`), else none (computed: `rename` is `RenameBlocked`).
//! - Incremental: per-file results are keyed by the file's index hash; [`SymbolTable::update`]
//!   rescans only files whose hash (or routing) changed. Resolution happens at query time.
//! - Restricted mode (5.5) builds the table without generator output (literals in generator
//!   sources are still scanned); `SymbolUnresolved` is then folded into one note when the
//!   project has generators (they may define the ids).

use crate::config::ProjectConfig;
use crate::diag::{Code, Diagnostic, Fix, Severity, Span};
use crate::model::{Body, ContentItem, OutputId, Pack};
use crate::path::{Hash, RelPath, Vfs};
use crate::route::Routed;
use crate::suffix::is_generator;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Kinds of identifiers. Serialized in `symbols --json` (versioned output, 6.2): new kinds
/// may be added, existing names never change.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Entity,
    Item,
    Block,
    LangKey,
    /// mcfunction path without extension (`demo/loop`).
    Function,
    /// `animation.<...>`.
    Animation,
    /// `controller.animation.<...>`.
    AnimationController,
    /// `controller.render.<...>`.
    RenderController,
    /// Texture short name of `item_texture.json` / `terrain_texture.json`.
    Texture,
    /// Sound event of `sound_definitions.json`.
    Sound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Definition,
    Reference,
}

/// Half-open byte range `[start, end)` of the symbol name in the file text (the
/// [`Fix`] unit).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

/// One appearance of a name in one file. The file (path, output, editability) is held once
/// per file by the table, see [`Loc`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Occurrence {
    pub name: String,
    /// Always set for definitions; for references when the context says.
    pub kind: Option<SymbolKind>,
    pub role: Role,
    /// `None`: no literal to point at (a function file, an id computed by a generator).
    pub range: Option<ByteRange>,
    /// The context names the kind; unresolved, it is `SymbolUnresolved`.
    pub typed: bool,
    /// The range covers only the name after an implied namespace (`` `${ns}:car` ``):
    /// [`scan`] gives the local name, the table the full id.
    pub local: bool,
}

/// How a file is scanned (by extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    /// `.json` (JSONC tolerated): keys, string values, Molang inside strings.
    Json,
    /// `.lang`: `key=value` lines.
    Lang,
    /// `.mcfunction`: command tokens.
    McFunction,
    /// `.ts` / `.js` / `.mjs` (not `.d.ts`): string literals only (no TypeScript semantics).
    Script,
    /// Not scanned (binaries, unknown text).
    Other,
}

/// A symbol and where it is defined (`beacon symbols --json`, LSP workspace symbols,
/// completion items).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SymbolEntry {
    pub kind: SymbolKind,
    pub name: String,
    /// 1-based line/column (`Span`, column in bytes), in definition order (path, then offset).
    pub definitions: Vec<Span>,
    pub references: usize,
}

/// `beacon symbols --json` (versioned, 6.2; consumed by ctxtruth-bedrock `bedrock/ids`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SymbolsJson {
    /// Bumped only on incompatible changes.
    pub schema: u32,
    pub symbols: Vec<SymbolEntry>,
}

pub const SYMBOLS_JSON_SCHEMA: u32 = 1;

/// Lang keys that embed an id: (prefix, suffix, kind of the id). First match wins, so
/// `item.<id>.name` comes before `item.<id>`. Same keys as name_ninja in `registry.rs`. An id
/// containing `.` is ambiguous between the two `item.` forms (documented ceiling).
pub const LANG_KEYS: &[(&str, &str, SymbolKind)] = &[
    ("item.spawn_egg.entity.", ".name", SymbolKind::Entity),
    ("entity.", ".name", SymbolKind::Entity),
    ("tile.", ".name", SymbolKind::Block),
    ("item.", ".name", SymbolKind::Item),
    ("item.", "", SymbolKind::Item),
];

/// One scanned file.
#[derive(Debug)]
struct FileSyms {
    hash: Hash,
    /// Routed destination, `BP/...` or `RP/...`; `None` for unrouted files (scripts).
    dest: Option<RelPath>,
    output: Option<OutputId>,
    /// Namespace that template heads resolve with.
    ns: String,
    /// `false` in static outputs.
    editable: bool,
    /// Byte offset of every line start.
    lines: Vec<u32>,
    /// Sorted by range (`None` first).
    occs: Vec<Occurrence>,
}

impl FileSyms {
    fn span(&self, path: &RelPath, r: Option<ByteRange>) -> Span {
        let (line, column) = match r {
            Some(r) => {
                let l = self.lines.partition_point(|&s| s as usize <= r.start);
                let col = r.start - self.lines[l - 1] as usize + 1;
                (Some(l as u32), Some(col as u32))
            }
            None => (None, None),
        };
        Span {
            path: path.clone(),
            line,
            column,
        }
    }
}

/// A definition with its file.
type Def<'a> = (&'a RelPath, &'a FileSyms, &'a Occurrence);

/// An occurrence and its file.
#[derive(Debug, Clone, Copy)]
pub struct Loc<'a> {
    pub path: &'a RelPath,
    pub occ: &'a Occurrence,
    pub output: Option<&'a str>,
    /// Rename may edit it: not in a static output, and it has a range.
    pub editable: bool,
}

/// The project's symbols. Fields are private; owned by the symbols workstream.
#[derive(Debug, Default)]
pub struct SymbolTable {
    files: BTreeMap<RelPath, FileSyms>,
    /// Definitions from generator output, by generator file (ranges point into that file).
    generated: BTreeMap<RelPath, Vec<Occurrence>>,
}

// ---------------------------------------------------------------- lexical helpers

/// `ns:name` with `ns` in `[a-z0-9_]+` and `name` in `[a-z0-9_.]+`.
fn id_shape(s: &str) -> bool {
    let Some((ns, name)) = s.split_once(':') else {
        return false;
    };
    !ns.is_empty()
        && !name.is_empty()
        && ns
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'))
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.'))
}

/// A namespaced id worth recording (vanilla ids are neither renamed nor resolved).
fn is_id(s: &str) -> bool {
    id_shape(s) && !s.starts_with("minecraft:")
}

/// (kind, start, end) of the id inside a lang key.
fn lang_id(key: &str) -> Option<(SymbolKind, usize, usize)> {
    LANG_KEYS.iter().find_map(|&(pre, post, k)| {
        let id = key.strip_prefix(pre)?.strip_suffix(post)?;
        is_id(id).then_some((k, pre.len(), pre.len() + id.len()))
    })
}

fn occ(name: &str, kind: Option<SymbolKind>, role: Role, s: usize, e: usize) -> Occurrence {
    Occurrence {
        name: name.to_owned(),
        kind,
        role,
        range: Some(ByteRange { start: s, end: e }),
        typed: false,
        local: false,
    }
}

fn reference(name: &str, s: usize, e: usize) -> Occurrence {
    occ(name, None, Role::Reference, s, e)
}

fn typed(name: &str, kind: SymbolKind, s: usize, e: usize) -> Occurrence {
    Occurrence {
        typed: true,
        ..occ(name, Some(kind), Role::Reference, s, e)
    }
}

/// Namespaced ids inside free text (`[a-z0-9_.:]` runs bounded by anything else; a run with
/// uppercase or `-` is not an id). Absolute ranges.
fn tokens(text: &str, base: usize) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let run = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-');
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !run(b[i]) {
            i += 1;
            continue;
        }
        let s0 = i;
        while i < b.len() && run(b[i]) {
            i += 1;
        }
        let (mut s, mut e) = (s0, i);
        while s < e && b[s] == b'.' {
            s += 1;
        }
        while e > s && b[e - 1] == b'.' {
            e -= 1;
        }
        if is_id(&text[s..e]) {
            out.push((base + s, base + e));
        }
    }
    out
}

/// A string (JSON key or value, script literal) at `[s, e)` with no special meaning: an id,
/// a lang key, an animation/controller name, or free text with ids in it.
fn string_refs(text: &str, s: usize, e: usize, out: &mut Vec<Occurrence>) {
    let raw = &text[s..e];
    if !raw.contains('\\') {
        if is_id(raw) {
            return out.push(reference(raw, s, e));
        }
        if let Some((k, a, b)) = lang_id(raw) {
            return out.push(occ(&raw[a..b], Some(k), Role::Reference, s + a, s + b));
        }
        for (pre, k) in [
            ("controller.animation.", SymbolKind::AnimationController),
            ("controller.render.", SymbolKind::RenderController),
            ("animation.", SymbolKind::Animation),
        ] {
            if raw.starts_with(pre) && !raw.contains(char::is_whitespace) {
                return out.push(occ(raw, Some(k), Role::Reference, s, e));
            }
        }
    }
    for (a, b) in tokens(raw, s) {
        out.push(reference(&text[a..b], a, b));
    }
}

/// Scan class of a project-relative path.
pub fn classify(path: &RelPath) -> FileClass {
    let n = path.file_name();
    if n.ends_with(".json") {
        FileClass::Json
    } else if n.ends_with(".lang") {
        FileClass::Lang
    } else if n.ends_with(".mcfunction") {
        FileClass::McFunction
    } else if (n.ends_with(".ts") && !n.ends_with(".d.ts"))
        || n.ends_with(".js")
        || n.ends_with(".mjs")
    {
        FileClass::Script
    } else {
        FileClass::Other
    }
}

/// Lexical scan of one file. `dest` is its routed destination (`BP/functions/a.mcfunction`;
/// `None` = not routed, which defines nothing). Never fails: unparsable text yields what
/// could be read. Sorted by range, `None` first; template heads carry their local name.
pub fn scan(path: &RelPath, dest: Option<&RelPath>, text: &str) -> Vec<Occurrence> {
    let mut out = match classify(path) {
        FileClass::Json => scan_json(text, dest.is_some()),
        FileClass::Lang => scan_lang(text, dest.is_some()),
        FileClass::McFunction => scan_mcfunction(text, dest),
        FileClass::Script => scan_script(text),
        FileClass::Other => Vec::new(),
    };
    out.sort_by_key(|o| o.range.map(|r| (r.start, r.end)));
    out
}

/// Skips `//` and `/* */` comments at `i`; returns the new position (unchanged if none).
fn skip_comment(b: &[u8], i: usize) -> usize {
    match (b.get(i), b.get(i + 1)) {
        (Some(b'/'), Some(b'/')) => b[i..]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(b.len(), |p| i + p),
        (Some(b'/'), Some(b'*')) => b[i + 2..]
            .windows(2)
            .position(|w| w == b"*/")
            .map_or(b.len(), |p| i + 2 + p + 2),
        _ => i,
    }
}

/// End (exclusive, at the closing quote) of a string body starting at `i`.
fn string_end(b: &[u8], mut i: usize, quote: u8, stop_at_newline: bool) -> usize {
    while i < b.len() && b[i] != quote && !(stop_at_newline && b[i] == b'\n') {
        i += if b[i] == b'\\' { 2 } else { 1 };
    }
    i.min(b.len())
}

fn scan_json(text: &str, routed: bool) -> Vec<Occurrence> {
    struct Frame {
        obj: bool,
        key: Option<(usize, usize)>,
        want_key: bool,
    }
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut st: Vec<Frame> = Vec::new();
    let key_of = |f: &Frame| f.key.map(|(s, e)| &text[s..e]);
    let mut i = 0;
    while i < b.len() {
        let j = skip_comment(b, i);
        if j != i {
            i = j;
            continue;
        }
        match b[i] {
            b'{' | b'[' => st.push(Frame {
                obj: b[i] == b'{',
                key: None,
                want_key: b[i] == b'{',
            }),
            b'}' | b']' => {
                st.pop();
            }
            b':' => {
                if let Some(f) = st.last_mut() {
                    f.want_key = false;
                }
            }
            b',' => {
                if let Some(f) = st.last_mut() {
                    f.want_key = f.obj;
                }
            }
            b'"' => {
                let (s, e) = (i + 1, string_end(b, i + 1, b'"', true));
                i = e;
                let raw = &text[s..e];
                let is_key = st.last().is_some_and(|f| f.obj && f.want_key);
                // keys of the enclosing objects ("[]" for arrays), outermost first
                let n = st.len() - usize::from(is_key);
                let path: Vec<&str> = st[..n]
                    .iter()
                    .map(|f| if f.obj { key_of(f).unwrap_or("") } else { "[]" })
                    .collect();
                if is_key {
                    if let Some(f) = st.last_mut() {
                        f.key = Some((s, e));
                    }
                    let def = match path.as_slice() {
                        ["animations"] => Some(SymbolKind::Animation),
                        ["animation_controllers"] => Some(SymbolKind::AnimationController),
                        ["render_controllers"] => Some(SymbolKind::RenderController),
                        ["sound_definitions"] => Some(SymbolKind::Sound),
                        ["texture_data"] => Some(SymbolKind::Texture),
                        _ => None,
                    };
                    match def {
                        Some(k) if routed && !raw.is_empty() && !raw.contains('\\') => {
                            out.push(occ(raw, Some(k), Role::Definition, s, e))
                        }
                        _ => string_refs(text, s, e, &mut out),
                    }
                } else {
                    let ident = match path.as_slice() {
                        [top, "description", "identifier"] => match *top {
                            "minecraft:entity" => Some((SymbolKind::Entity, routed)),
                            "minecraft:item" => Some((SymbolKind::Item, routed)),
                            "minecraft:block" => Some((SymbolKind::Block, routed)),
                            "minecraft:client_entity" | "minecraft:spawn_rules" => {
                                Some((SymbolKind::Entity, false))
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    match ident {
                        Some((k, true)) if is_id(raw) => {
                            out.push(occ(raw, Some(k), Role::Definition, s, e))
                        }
                        Some((k, false)) if is_id(raw) => out.push(typed(raw, k, s, e)),
                        _ => string_refs(text, s, e, &mut out),
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn scan_lang(text: &str, routed: bool) -> Vec<Occurrence> {
    let mut out = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let bom = if start == 0 && line.starts_with('\u{feff}') {
            3
        } else {
            0
        };
        let l = &line[bom..];
        let Some((key, _)) = l.split_once('=') else {
            continue;
        };
        let key = key.trim_end();
        if key.is_empty() || key.starts_with("##") || key.starts_with(char::is_whitespace) {
            continue;
        }
        let s = start + bom;
        if routed {
            out.push(occ(
                key,
                Some(SymbolKind::LangKey),
                Role::Definition,
                s,
                s + key.len(),
            ));
        }
        if let Some((k, a, b)) = lang_id(key) {
            out.push(occ(&key[a..b], Some(k), Role::Reference, s + a, s + b));
        }
    }
    out
}

/// The kind a command gives the id starting a word, from the words before it on the line.
fn command_kind(before: &str) -> Option<SymbolKind> {
    if before.ends_with("type=") || before.ends_with("type=!") {
        return Some(SymbolKind::Entity);
    }
    if !before.ends_with(char::is_whitespace) {
        return None;
    }
    let w: Vec<&str> = before.split_whitespace().collect();
    let back = |n: usize| w.len().checked_sub(n).map(|i| w[i]);
    match (back(1), back(2), back(4), back(7)) {
        (Some("summon"), ..) => Some(SymbolKind::Entity),
        (_, Some("give" | "clear"), ..) => Some(SymbolKind::Item),
        (_, _, Some("setblock"), _) | (.., Some("fill")) => Some(SymbolKind::Block),
        _ => None,
    }
}

fn scan_mcfunction(text: &str, dest: Option<&RelPath>) -> Vec<Occurrence> {
    let mut out = Vec::new();
    if let Some(id) = dest.and_then(|d| {
        d.as_str()
            .strip_prefix("BP/functions/")?
            .strip_suffix(".mcfunction")
    }) {
        out.push(Occurrence {
            range: None,
            ..occ(id, Some(SymbolKind::Function), Role::Definition, 0, 0)
        });
    }
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        if line.trim_start().starts_with('#') {
            continue;
        }
        // `function <path>`
        let mut words = line.split_whitespace().peekable();
        while let Some(w) = words.next() {
            if w == "function"
                && let Some(p) = words.peek()
            {
                let s = start + p.as_ptr() as usize - line.as_ptr() as usize;
                out.push(typed(p, SymbolKind::Function, s, s + p.len()));
            }
        }
        for (s, e) in tokens(line, start) {
            let id = &text[s..e];
            match command_kind(&line[..s - start]) {
                Some(k) => out.push(typed(id, k, s, e)),
                None => out.push(reference(id, s, e)),
            }
        }
    }
    out
}

/// Literal head of a template literal whose body starts at `i`: `${ns}:name` up to the
/// closing backtick. Returns the name range.
fn template_head(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let rest = &b[i..];
    let n = [&b"${ns}:"[..], b"${namespace}:"]
        .into_iter()
        .find(|p| rest.starts_with(p))?
        .len();
    let s = i + n;
    let mut e = s;
    while e < b.len() && matches!(b[e], b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.') {
        e += 1;
    }
    (e > s && b.get(e) == Some(&b'`')).then_some((s, e))
}

/// Skips a `${ ... }` expression starting after `${`; returns the position after `}`.
fn skip_expr(b: &[u8], mut i: usize) -> usize {
    let mut depth = 1;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            q @ (b'"' | b'\'') => i = string_end(b, i + 1, q, true),
            b'`' => i = skip_template(b, i + 1, &mut Vec::new(), "") - 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// Scans a template body starting at `i` (after the backtick); returns the position after
/// the closing backtick. Literal segments are free text.
fn skip_template(b: &[u8], mut i: usize, out: &mut Vec<Occurrence>, text: &str) -> usize {
    let mut seg = i;
    let flush = |out: &mut Vec<Occurrence>, s: usize, e: usize| {
        if !text.is_empty() {
            for (a, z) in tokens(&text[s..e], s) {
                out.push(reference(&text[a..z], a, z));
            }
        }
    };
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'`' => {
                flush(out, seg, i);
                return i + 1;
            }
            b'$' if b.get(i + 1) == Some(&b'{') => {
                flush(out, seg, i);
                i = skip_expr(b, i + 2);
                seg = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    flush(out, seg.min(b.len()), b.len());
    b.len() + 1
}

fn scan_script(text: &str) -> Vec<Occurrence> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let j = skip_comment(b, i);
        if j != i {
            i = j;
            continue;
        }
        match b[i] {
            q @ (b'"' | b'\'') => {
                let (s, e) = (i + 1, string_end(b, i + 1, q, true));
                string_refs(text, s, e, &mut out);
                i = e + 1;
            }
            b'`' => {
                if let Some((s, e)) = template_head(b, i + 1) {
                    out.push(Occurrence {
                        local: true,
                        ..reference(&text[s..e], s, e)
                    });
                    i = e + 1;
                } else {
                    i = skip_template(b, i + 1, &mut out, text);
                }
            }
            _ => i += 1,
        }
    }
    out
}

fn line_starts(text: &str) -> Vec<u32> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i as u32 + 1))
        .collect()
}

/// Full ids for template heads.
fn resolve(mut occs: Vec<Occurrence>, ns: &str) -> Vec<Occurrence> {
    for o in occs.iter_mut().filter(|o| o.local) {
        o.name = format!("{ns}:{}", o.name);
    }
    occs
}

/// Every file below `dir`, sorted.
fn walk(vfs: &dyn Vfs, dir: &RelPath, out: &mut Vec<RelPath>) {
    for name in vfs.list(Some(dir)) {
        let Ok(p) = dir.join(&name) else { continue };
        if vfs.list(Some(&p)).is_empty() {
            out.push(p);
        } else {
            walk(vfs, &p, out);
        }
    }
}

/// (kind, id) definitions in one generated JSON body.
fn generated_defs(v: &Value) -> Vec<(SymbolKind, String)> {
    let mut out = Vec::new();
    for (key, k) in [
        ("minecraft:entity", SymbolKind::Entity),
        ("minecraft:item", SymbolKind::Item),
        ("minecraft:block", SymbolKind::Block),
    ] {
        if let Some(id) = v[key]["description"]["identifier"].as_str() {
            out.push((k, id.to_owned()));
        }
    }
    for (key, k) in [
        ("sound_definitions", SymbolKind::Sound),
        ("texture_data", SymbolKind::Texture),
    ] {
        for id in v[key].as_object().into_iter().flat_map(|m| m.keys()) {
            out.push((k, id.clone()));
        }
    }
    out.retain(|(_, id)| !id.starts_with("minecraft:"));
    out
}

const CHECKED: [SymbolKind; 4] = [
    SymbolKind::Entity,
    SymbolKind::Item,
    SymbolKind::Block,
    SymbolKind::Function,
];

impl SymbolTable {
    /// Full build from routing (which files belong to which output, static outputs) and the
    /// generator output of the build (`items`; only items whose origin is a generator count).
    /// Reads text files through `vfs` only. `restricted`: generators did not run.
    pub fn build(
        cfg: &ProjectConfig,
        routed: &Routed,
        items: &[ContentItem],
        vfs: &dyn Vfs,
        restricted: bool,
    ) -> (SymbolTable, Vec<Diagnostic>) {
        let mut t = SymbolTable::default();
        t.update(cfg, routed, items, vfs);
        let d = t.diagnostics(cfg, restricted);
        (t, d)
    }

    /// Brings the table in line with the index: files whose hash or routing changed are
    /// rescanned, removed ones drop out, generator definitions are replaced by those of
    /// `items`. Unchanged files cost a hash compare (no read).
    pub fn update(
        &mut self,
        cfg: &ProjectConfig,
        routed: &Routed,
        items: &[ContentItem],
        vfs: &dyn Vfs,
    ) {
        let is_static = |o: &str| {
            cfg.beacon
                .outputs
                .iter()
                .any(|x| x.id == o && x.static_packs.is_some())
        };
        let ns_of = |o: Option<&str>| {
            cfg.beacon
                .outputs
                .iter()
                .find(|x| Some(x.id.as_str()) == o)
                .and_then(|x| x.namespace.clone())
                .unwrap_or_else(|| cfg.namespace.clone())
        };
        // path -> (dest, output); routed files first
        let mut want: BTreeMap<RelPath, (Option<RelPath>, Option<OutputId>)> = BTreeMap::new();
        let mut module_out: HashMap<&RelPath, &OutputId> = HashMap::new();
        for i in &routed.items {
            module_out.entry(&i.module).or_insert(&i.dest.output);
            let Body::Passthrough(src) = &i.body else {
                continue;
            };
            if classify(src) == FileClass::Other {
                continue;
            }
            let pack = if i.dest.pack == Pack::Behavior {
                "BP"
            } else {
                "RP"
            };
            let dest = RelPath::parse(&format!("{pack}/{}", i.dest.path)).ok();
            want.entry(src.clone())
                .or_insert((dest, Some(i.dest.output.clone())));
        }
        for g in &routed.generators {
            module_out.entry(&g.module).or_insert(&g.output);
        }
        for (o, entries) in &routed.script_entries {
            for e in entries {
                if let Some(m) = routed.modules.keys().find(|m| {
                    e.as_str()
                        .strip_prefix(m.as_str())
                        .is_some_and(|r| r.starts_with('/'))
                }) {
                    module_out.entry(m).or_insert(o);
                }
            }
        }
        // unrouted text files of modules (scripts, generators, data modules)
        for m in routed.modules.keys() {
            let mut files = Vec::new();
            walk(vfs, m, &mut files);
            for f in files {
                if classify(&f) != FileClass::Other && !want.contains_key(&f) {
                    want.insert(f, (None, module_out.get(m).map(|o| (*o).clone())));
                }
            }
        }

        let mut old = std::mem::take(&mut self.files);
        for (path, (dest, output)) in want {
            let Ok(hash) = vfs.hash(&path) else { continue };
            let ns = ns_of(output.as_deref());
            let editable = !output.as_deref().is_some_and(is_static);
            if let Some(f) = old.remove(&path).filter(|f| {
                f.hash == hash
                    && f.dest == dest
                    && f.output == output
                    && f.ns == ns
                    && f.editable == editable
            }) {
                self.files.insert(path, f);
                continue;
            }
            // unreadable or not UTF-8: no symbols (never lossy: offsets must match the disk)
            let text = vfs
                .read(&path)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default();
            let occs = resolve(scan(&path, dest.as_ref(), &text), &ns);
            self.files.insert(
                path,
                FileSyms {
                    hash,
                    dest,
                    output,
                    ns,
                    editable,
                    lines: line_starts(&text),
                    occs,
                },
            );
        }

        self.generated.clear();
        let mut seen = BTreeSet::new();
        for i in items.iter().filter(|i| is_generator(i.origin.file_name())) {
            let Body::Json(v) = &i.body else { continue };
            for (k, id) in generated_defs(v) {
                if !seen.insert((i.origin.clone(), k, id.clone())) {
                    continue;
                }
                // the one literal of this id in the generator source, if exactly one
                let lit: Vec<&Occurrence> = self
                    .files
                    .get(&i.origin)
                    .into_iter()
                    .flat_map(|f| &f.occs)
                    .filter(|o| o.name == id && o.kind.is_none())
                    .collect();
                let (range, local) = match lit.as_slice() {
                    [o] => (o.range, o.local),
                    _ => (None, false),
                };
                self.generated
                    .entry(i.origin.clone())
                    .or_default()
                    .push(Occurrence {
                        name: id,
                        kind: Some(k),
                        role: Role::Definition,
                        range,
                        typed: false,
                        local,
                    });
            }
        }
    }

    /// `SymbolDuplicate`, `SymbolCrossOutput` and `SymbolUnresolved` (folded into one note
    /// when `restricted` and the project has generators).
    pub fn diagnostics(&self, cfg: &ProjectConfig, restricted: bool) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        // (kind, name) -> definitions (path, file, occurrence)
        let mut defs: BTreeMap<(SymbolKind, &str), Vec<Def>> = BTreeMap::new();
        for (p, f, o) in self.all_defs() {
            if let Some(k) = o.kind {
                defs.entry((k, &o.name)).or_default().push((p, f, o));
            }
        }
        // the same entity/item/block in two behavior packs (outputs or static packs)
        for ((k, name), list) in &defs {
            if !matches!(k, SymbolKind::Entity | SymbolKind::Item | SymbolKind::Block) {
                continue;
            }
            let bp: Vec<_> = list
                .iter()
                .filter(|(_, f, _)| {
                    f.dest
                        .as_ref()
                        .is_none_or(|d| d.as_str().starts_with("BP/"))
                })
                .collect();
            let outputs: BTreeSet<_> = bp.iter().map(|(_, f, _)| &f.output).collect();
            if outputs.len() < 2 {
                continue;
            }
            let (p, f, o) = bp[bp.len() - 1];
            let mut d = Diagnostic::new(
                Code::SymbolDuplicate,
                format!(
                    "{} {name} is defined in more than one behavior pack ({})",
                    kind_name(*k),
                    outputs
                        .iter()
                        .map(|o| o.as_deref().unwrap_or("?"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            d.span = Some(f.span(p, o.range));
            d.related = bp[..bp.len() - 1]
                .iter()
                .map(|(p, f, o)| f.span(p, o.range))
                .collect();
            out.push(d);
        }
        let has_gen = self.files.keys().any(|p| is_generator(p.file_name()));
        let mut suppressed = 0;
        for (p, f) in &self.files {
            for o in f
                .occs
                .iter()
                .filter(|o| o.typed && o.role == Role::Reference)
            {
                let Some(k) = o.kind.filter(|k| CHECKED.contains(k)) else {
                    continue;
                };
                let span = f.span(p, o.range);
                let at = |d: Diagnostic| Diagnostic {
                    span: Some(span.clone()),
                    ..d
                };
                match defs.get(&(k, o.name.as_str())) {
                    None if restricted && has_gen => suppressed += 1,
                    None => out.push(at(Diagnostic::new(
                        Code::SymbolUnresolved,
                        format!("{} {} is not defined by any output", kind_name(k), o.name),
                    ))),
                    Some(list) => {
                        let Some(me) = f.output.as_deref() else {
                            continue;
                        };
                        let deps = cfg
                            .beacon
                            .outputs
                            .iter()
                            .find(|x| x.id == me)
                            .map(|x| x.depends_on.as_slice())
                            .unwrap_or_default();
                        let ok = list.iter().any(|(_, g, _)| {
                            g.output
                                .as_deref()
                                .is_none_or(|o| o == me || deps.iter().any(|d| d == o))
                        });
                        if !ok {
                            let owners: BTreeSet<_> = list
                                .iter()
                                .filter_map(|(_, g, _)| g.output.as_deref())
                                .collect();
                            out.push(at(Diagnostic::new(
                                Code::SymbolCrossOutput,
                                format!(
                                    "output {me:?} uses {} {} defined only in {} (not in its dependsOn)",
                                    kind_name(k),
                                    o.name,
                                    owners.into_iter().collect::<Vec<_>>().join(", ")
                                ),
                            )));
                        }
                    }
                }
            }
        }
        if suppressed > 0 {
            let mut d = Diagnostic::new(
                Code::SymbolUnresolved,
                format!(
                    "{suppressed} reference(s) not resolved and not reported: restricted mode, generators did not run and may define them"
                ),
            );
            d.severity = Severity::Note;
            out.push(d);
        }
        out
    }

    /// Re-scans one file from unsaved editor text (LSP overlay); does not touch disk state.
    /// The next [`SymbolTable::update`] rescans it from disk. Files the table does not know
    /// are ignored.
    pub fn overlay(&mut self, path: &RelPath, text: &str) {
        if let Some(f) = self.files.get_mut(path) {
            f.occs = resolve(scan(path, f.dest.as_ref(), text), &f.ns);
            f.lines = line_starts(text);
            f.hash = [0; 32];
        }
    }

    fn loc<'a>(&'a self, p: &'a RelPath, o: &'a Occurrence) -> Loc<'a> {
        let f = self.files.get(p);
        Loc {
            path: p,
            occ: o,
            output: f.and_then(|f| f.output.as_deref()),
            editable: f.is_some_and(|f| f.editable) && o.range.is_some(),
        }
    }

    /// Every definition with its file (generated ones after scanned ones, both by path).
    fn all_defs(&self) -> impl Iterator<Item = Def<'_>> {
        let scanned = self
            .files
            .iter()
            .flat_map(|(p, f)| f.occs.iter().map(move |o| (p, f, o)));
        let generated = self.generated.iter().filter_map(|(p, v)| {
            let f = self.files.get(p)?;
            Some(v.iter().map(move |o| (p, f, o)))
        });
        scanned
            .chain(generated.flatten())
            .filter(|(_, _, o)| o.role == Role::Definition)
    }

    pub fn definitions(&self, kind: SymbolKind, name: &str) -> Vec<Loc<'_>> {
        self.all_defs()
            .filter(|(_, _, o)| o.kind == Some(kind) && o.name == name)
            .map(|(p, _, o)| self.loc(p, o))
            .collect()
    }

    /// References resolved to (`kind`, `name`): those of that kind, untyped ones by name.
    pub fn references(&self, kind: SymbolKind, name: &str) -> Vec<Loc<'_>> {
        self.files
            .iter()
            .flat_map(|(p, f)| f.occs.iter().map(move |o| (p, o)))
            .filter(|(_, o)| {
                o.role == Role::Reference && o.name == name && o.kind.is_none_or(|k| k == kind)
            })
            .map(|(p, o)| self.loc(p, o))
            .collect()
    }

    /// The innermost occurrence whose range contains `offset` (end inclusive: a cursor right
    /// after a name is on it).
    pub fn at(&self, path: &RelPath, offset: usize) -> Option<Loc<'_>> {
        let (path, f) = self.files.get_key_value(path)?;
        let generated = self.generated.get(path).into_iter().flatten();
        f.occs
            .iter()
            .chain(generated)
            .filter_map(|o| o.range.map(|r| (r, o)))
            .filter(|(r, _)| r.start <= offset && offset <= r.end)
            .min_by_key(|(r, _)| r.end - r.start)
            .map(|(_, o)| self.loc(path, o))
    }

    /// Search over defined names: exact, then prefix, then substring matches (then by name and
    /// kind), capped at `limit`. An empty `query` lists everything; empty `kinds` = all kinds.
    pub fn find(&self, query: &str, kinds: &[SymbolKind], limit: usize) -> Vec<SymbolEntry> {
        let mut entries: BTreeMap<(SymbolKind, &str), Vec<Span>> = BTreeMap::new();
        for (p, f, o) in self.all_defs() {
            let Some(k) = o.kind.filter(|k| kinds.is_empty() || kinds.contains(k)) else {
                continue;
            };
            entries
                .entry((k, &o.name))
                .or_default()
                .push(f.span(p, o.range));
        }
        let (mut typed, mut untyped) = (HashMap::new(), HashMap::new());
        for o in self.files.values().flat_map(|f| &f.occs) {
            if o.role == Role::Reference {
                match o.kind {
                    Some(k) => *typed.entry((k, o.name.as_str())).or_insert(0) += 1,
                    None => *untyped.entry(o.name.as_str()).or_insert(0) += 1,
                }
            }
        }
        let mut out: Vec<(u8, SymbolEntry)> = entries
            .into_iter()
            .filter_map(|((k, name), mut definitions)| {
                let rank = if query.is_empty() || name == query {
                    0
                } else if name.starts_with(query) {
                    1
                } else if name.contains(query) {
                    2
                } else {
                    return None;
                };
                definitions
                    .sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
                let references = typed.get(&(k, name)).copied().unwrap_or(0)
                    + untyped.get(name).copied().unwrap_or(0);
                Some((
                    rank,
                    SymbolEntry {
                        kind: k,
                        name: name.to_owned(),
                        definitions,
                        references,
                    },
                ))
            })
            .collect();
        out.sort_by(|(ra, a), (rb, b)| (ra, &a.name, a.kind).cmp(&(rb, &b.name, b.kind)));
        out.into_iter().take(limit).map(|(_, e)| e).collect()
    }
}

fn kind_name(k: SymbolKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.replace('_', " ")))
        .unwrap_or_default()
}

/// Edits renaming `from` to `to` everywhere: every definition and every reference resolved
/// to it (including the id inside lang keys and template heads). `kind: None` renames every
/// kind `from` is defined as (a block and its item); `Some` only that kind, and then an
/// untyped reference is ambiguous (`RenameBlocked`) when `from` is also defined as another
/// kind. All-or-nothing: `RenameBlocked` if `from` is a `minecraft:` id or not defined, `to`
/// is not a valid id or already defined (any kind), or an occurrence is not editable (a
/// static output, an id computed by a generator, a template head moving to another
/// namespace). Edits are sorted by (path, start) and never overlap. Used by `beacon rename`,
/// LSP `textDocument/rename` and MCP `rename_symbol`.
pub fn rename(
    table: &SymbolTable,
    kind: Option<SymbolKind>,
    from: &str,
    to: &str,
) -> Result<Vec<Fix>, Diagnostic> {
    let blocked = |msg: String| Err(Diagnostic::new(Code::RenameBlocked, msg));
    let at = |p: &RelPath, r: Option<ByteRange>| {
        let s = table.files.get(p).map(|f| f.span(p, r)).unwrap_or(Span {
            path: p.clone(),
            line: None,
            column: None,
        });
        match (s.line, s.column) {
            (Some(l), Some(c)) => format!("{p}:{l}:{c}"),
            _ => p.to_string(),
        }
    };
    if from.starts_with("minecraft:") || to.starts_with("minecraft:") {
        return blocked(format!(
            "`minecraft:` ids are never renamed ({from} -> {to})"
        ));
    }
    if !id_shape(to) {
        return blocked(format!(
            "invalid id {to:?}: want `namespace:name` ([a-z0-9_], `.` allowed in the name)"
        ));
    }
    let defined: Vec<_> = table
        .all_defs()
        .filter(|(_, _, o)| o.name == from)
        .collect();
    let kinds: BTreeSet<SymbolKind> = defined.iter().filter_map(|(_, _, o)| o.kind).collect();
    if kinds.is_empty() {
        return blocked(format!("{from} is not defined"));
    }
    let chosen: BTreeSet<SymbolKind> = match kind {
        Some(k) if kinds.contains(&k) => [k].into(),
        Some(k) => {
            return blocked(format!("{from} is not defined as {}", kind_name(k)));
        }
        None => kinds.clone(),
    };
    if let Some((p, _, o)) = table.all_defs().find(|(_, _, o)| o.name == to) {
        return blocked(format!(
            "{to} is already defined ({} at {})",
            o.kind.map(kind_name).unwrap_or_default(),
            at(p, o.range)
        ));
    }
    let ambiguous = chosen.len() < kinds.len();
    let to_ns = to.split_once(':').map_or("", |(n, _)| n);
    let mut fixes = Vec::new();
    let mut edit = |p: &RelPath, f: Option<&FileSyms>, o: &Occurrence| -> Result<(), String> {
        let Some(f) = f.filter(|f| f.editable) else {
            return Err(format!(
                "{from} is used in a static output (never edited): {}",
                at(p, o.range)
            ));
        };
        let Some(r) = o.range else {
            return Err(match o.role {
                Role::Definition if is_generator(p.as_str()) => format!(
                    "{from} is computed by generator {p} (not one literal there); rename it in the generator"
                ),
                _ => format!("{from} has no text to edit in {p}"),
            });
        };
        let replacement = if o.local {
            if to_ns != f.ns {
                return Err(format!(
                    "{} uses `${{ns}}:` ({}), which cannot become namespace {to_ns:?}",
                    at(p, o.range),
                    f.ns
                ));
            }
            to[to_ns.len() + 1..].to_owned()
        } else {
            to.to_owned()
        };
        fixes.push(Fix {
            description: format!("rename {from} to {to}"),
            path: p.clone(),
            start: r.start,
            end: r.end,
            replacement,
            safe: true,
        });
        Ok(())
    };
    let scanned = table
        .files
        .iter()
        .flat_map(|(p, f)| f.occs.iter().map(move |o| (p, o)));
    let generated = table
        .generated
        .iter()
        .flat_map(|(p, v)| v.iter().map(move |o| (p, o)));
    for (p, o) in scanned.chain(generated) {
        if o.name != from {
            continue;
        }
        let hit = match (o.role, o.kind) {
            (_, Some(k)) => chosen.contains(&k),
            (Role::Reference, None) if ambiguous => {
                return blocked(format!(
                    "ambiguous: {from} is defined as {} and the untyped reference at {} could be any of them; rename without --kind",
                    kinds
                        .iter()
                        .map(|k| kind_name(*k))
                        .collect::<Vec<_>>()
                        .join(" and "),
                    at(p, o.range)
                ));
            }
            (_, None) => true,
        };
        if hit && let Err(m) = edit(p, table.files.get(p), o) {
            return blocked(m);
        }
    }
    fixes.sort_by(|a, b| (&a.path, a.start, a.end).cmp(&(&b.path, b.start, b.end)));
    fixes.dedup_by(|b, a| a.path == b.path && a.start == b.start && a.end == b.end);
    if let Some(w) = fixes
        .windows(2)
        .find(|w| w[0].path == w[1].path && w[0].end > w[1].start)
    {
        return blocked(format!(
            "overlapping occurrences of {from} in {}",
            w[0].path
        ));
    }
    Ok(fixes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_serialize_snake_case() {
        let v =
            serde_json::to_value([SymbolKind::AnimationController, SymbolKind::LangKey]).unwrap();
        assert_eq!(v, serde_json::json!(["animation_controller", "lang_key"]));
    }

    #[test]
    fn tokens_are_bounded_ids() {
        let t = "summon demo:car; demo:car_key 'demo:x.' Demo:no a-b:c entity.demo:car.name 12:30";
        let got: Vec<&str> = tokens(t, 0).into_iter().map(|(s, e)| &t[s..e]).collect();
        assert_eq!(got, ["demo:car", "demo:car_key", "demo:x", "12:30"]);
        assert_eq!(lang_id("item.demo:a.name"), Some((SymbolKind::Item, 5, 11)));
        assert_eq!(lang_id("item.demo:a"), Some((SymbolKind::Item, 5, 11)));
        assert_eq!(lang_id("item.minecraft:a"), None);
    }
}
