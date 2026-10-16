//! `GeneratorHost` over QuickJS (ARCHITECTURE 5.2 observed inputs, 5.3 content, 5.4 sandbox).
//!
//! Flow of one `run`: pre-collect the import closure through the `Vfs` (oxc: strip TS, find
//! relative imports; JSON becomes data modules), evaluate the entry in a fresh context, read
//! back the content descriptors the runtime API collected, and convert them to `ContentItem`s.

use crate::Limits;
use beacon_engine::diag::{Code, Diagnostic};
use beacon_engine::host::{GenResult, GeneratorHost};
use beacon_engine::model::{
    Body, ContentItem, Dest, GenCtx, Generator, Merge, ObservedInputs, Pack,
};
use beacon_engine::path::{RelPath, Vfs};
use beacon_engine::suffix::route_file;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Exception, Function, Module, Runtime};
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct Host {
    limits: Limits,
}

impl Host {
    pub fn new(limits: Limits) -> Self {
        Self { limits }
    }
}

// ---------------------------------------------------------------- compile

/// `(generated line, generated col, source line)`, 0-based, sorted.
type LineMap = Vec<(u32, u32, u32)>;

pub(crate) struct Compiled {
    pub js: String,
    /// Relative imports / re-exports left after type erasure.
    pub imports: Vec<String>,
    map: LineMap,
}

/// TS -> JS with oxc (syntax only), collecting import specifiers and a line map.
pub(crate) fn compile(path: &str, src: &str) -> Result<Compiled, String> {
    use oxc::allocator::Allocator;
    use oxc::ast::ast::Statement;
    use oxc::codegen::{Codegen, CodegenOptions};
    use oxc::parser::Parser;
    use oxc::semantic::SemanticBuilder;
    use oxc::span::SourceType;
    use oxc::transformer::{TransformOptions, Transformer};

    let alloc = Allocator::default();
    let st = SourceType::from_path(path).map_err(|e| e.to_string())?;
    let parsed = Parser::new(&alloc, src, st).parse();
    if let Some(d) = parsed.diagnostics.first() {
        return Err(format!("{path}: {d}"));
    }
    let mut program = parsed.program;
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();
    let opts = TransformOptions::default();
    let t = Transformer::new(&alloc, std::path::Path::new(path), &opts)
        .build_with_scoping(scoping, &mut program);
    if let Some(d) = t.diagnostics.first() {
        return Err(format!("{path}: {d}"));
    }
    let imports = program
        .body
        .iter()
        .filter_map(|s| match s {
            Statement::ImportDeclaration(d) => Some(d.source.value.to_string()),
            Statement::ExportAllDeclaration(d) => Some(d.source.value.to_string()),
            Statement::ExportFromDeclaration(d) => Some(d.source.value.to_string()),
            _ => None,
        })
        .collect();
    let out = Codegen::new()
        .with_options(CodegenOptions {
            source_map_path: Some(path.into()),
            ..Default::default()
        })
        .build(&program);
    let map = out.map.map_or(vec![], |m| {
        let mut v: Vec<_> = m
            .get_tokens()
            .map(|t| (t.get_dst_line(), t.get_dst_col(), t.get_src_line()))
            .collect();
        v.sort_unstable();
        v
    });
    Ok(Compiled {
        js: out.code,
        imports,
        map,
    })
}

/// `*.json` import: the text goes in as a string literal, never spliced as source.
pub(crate) fn json_module(text: &str) -> String {
    let lit = serde_json::to_string(text).expect("string serializes");
    format!("export default JSON.parse({lit});")
}

// ---------------------------------------------------------------- closure

#[derive(Default)]
struct Closure {
    /// Module name (project-relative path) -> (JS, line map).
    mods: HashMap<String, (String, LineMap)>,
    /// `(importer, specifier)` -> module name.
    resolved: HashMap<(String, String), String>,
}

fn err(g: &Generator, msg: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Code::GeneratorError, msg).at(g.path.clone(), None, None)
}

/// Hash (and optionally read) a file, recording it in the observed inputs. NotFound is a
/// negative lookup; any other error marks the evaluation uncacheable.
fn observe(
    vfs: &dyn Vfs,
    inputs: &mut ObservedInputs,
    p: &RelPath,
    bytes: bool,
) -> io::Result<Vec<u8>> {
    let r = vfs
        .hash(p)
        .and_then(|h| Ok((h, if bytes { vfs.read(p)? } else { vec![] })));
    match r {
        Ok((h, b)) => {
            inputs.files.insert(p.clone(), h);
            Ok(b)
        }
        Err(e) => {
            if e.kind() == io::ErrorKind::NotFound {
                inputs.negatives.insert(p.clone());
            } else {
                inputs.uncacheable = true;
            }
            Err(e)
        }
    }
}

/// Resolves `spec` against `importer` with path arithmetic only: candidate files in order.
fn candidates(importer: &RelPath, spec: &str) -> Option<Vec<String>> {
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return None;
    }
    let mut segs: Vec<&str> = importer.parent().map_or(vec![], |_| {
        let s = importer.as_str();
        s[..s.len() - importer.file_name().len()]
            .trim_end_matches('/')
            .split('/')
            .collect()
    });
    for p in spec.split('/') {
        match p {
            "." | "" => {}
            ".." => {
                segs.pop()?;
            }
            p => segs.push(p),
        }
    }
    let base = segs.join("/");
    Some(
        if [".json", ".ts", ".js"].iter().any(|e| base.ends_with(e)) {
            vec![base]
        } else {
            vec![
                format!("{base}.ts"),
                format!("{base}.js"),
                format!("{base}/index.ts"),
            ]
        },
    )
}

fn collect(g: &Generator, vfs: &dyn Vfs, r: &mut GenResult) -> Option<Closure> {
    let mut c = Closure::default();
    let mut seen = HashSet::from([g.path.to_string()]);
    let read = |p: &RelPath, r: &mut GenResult| -> Option<String> {
        match observe(vfs, &mut r.inputs, p, true) {
            Ok(b) => String::from_utf8(b)
                .map_err(|_| r.diagnostics.push(err(g, format!("{p}: not valid UTF-8"))))
                .ok(),
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    r.diagnostics.push(err(g, format!("cannot read {p}: {e}")));
                }
                None
            }
        }
    };
    let Some(src) = read(&g.path, r) else {
        if r.diagnostics.is_empty() {
            r.diagnostics.push(err(g, "generator file not found"));
        }
        return None;
    };
    let mut todo = vec![(g.path.clone(), src)];
    while let Some((p, src)) = todo.pop() {
        let (js, imports, map) = if p.as_str().ends_with(".json") {
            (json_module(&src), vec![], vec![])
        } else {
            match compile(p.as_str(), &src) {
                Ok(x) => (x.js, x.imports, x.map),
                Err(m) => {
                    r.diagnostics.push(err(g, m));
                    continue;
                }
            }
        };
        c.mods.insert(p.to_string(), (js, map));
        for spec in imports.into_iter().filter(|s| !s.starts_with("beacon:")) {
            let unresolved = |r: &mut GenResult| {
                let d = Diagnostic::new(Code::ImportUnresolved, format!("cannot resolve `{spec}`"));
                r.diagnostics.push(d.at(p.clone(), None, None));
            };
            let Some(cands) = candidates(&p, &spec) else {
                unresolved(r);
                continue;
            };
            let mut found = None;
            for cand in cands {
                let Ok(cp) = RelPath::parse(&cand) else {
                    continue;
                };
                if seen.contains(&cand) {
                    found = Some(cand);
                    break;
                }
                if let Some(s) = read(&cp, r) {
                    seen.insert(cand.clone());
                    todo.push((cp, s));
                    found = Some(cand);
                    break;
                }
                if r.inputs.uncacheable {
                    break; // real io error: already reported, do not probe further
                }
            }
            match found {
                Some(f) => {
                    c.resolved.insert((p.to_string(), spec), f);
                }
                None if r.inputs.uncacheable => {}
                None => unresolved(r),
            }
        }
    }
    r.diagnostics.is_empty().then_some(c)
}

struct Fs(Rc<Closure>);

impl Resolver for Fs {
    fn resolve<'js>(
        &mut self,
        _: &Ctx<'js>,
        base: &str,
        name: &str,
        _: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if name == "beacon:runtime" {
            return Ok(name.into());
        }
        self.0
            .resolved
            .get(&(base.to_owned(), name.to_owned()))
            .cloned()
            .ok_or_else(|| rquickjs::Error::new_resolving(base, name))
    }
}

impl Loader for Fs {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js>> {
        if name == "beacon:runtime" {
            return Module::declare(ctx.clone(), name, RUNTIME_MODULE);
        }
        let (js, _) = self
            .0
            .mods
            .get(name)
            .ok_or_else(|| rquickjs::Error::new_loading(name))?;
        Module::declare(ctx.clone(), name, js.as_str())
    }
}

// ---------------------------------------------------------------- JS side

const RUNTIME_MODULE: &str =
    "export const { generate, assets, item, block, entity, sound, file } = globalThis;";

/// Installs the runtime API as globals; wraps nondeterministic sources and `ctx.define`.
const PRELUDE: &str = r#"(() => {
  const g = globalThis, native = g.__assets, c0 = JSON.parse(g.__ctx);
  delete g.__assets; delete g.__ctx;
  const out = [], defs = {};
  let unc = false;
  const mark = () => { unc = true; };
  const D0 = Date;
  const D = new Proxy(D0, {
    construct: (t, a, nt) => (a.length || mark(), Reflect.construct(t, a, nt)),
    apply: (t) => (mark(), t()),
    get: (t, k) => (k === "now" ? () => (mark(), t.now()) : t[k]),
  });
  Object.defineProperty(D0.prototype, "constructor", { value: D, writable: true, configurable: true });
  g.Date = D;
  const rnd = Math.random;
  Math.random = () => (mark(), rnd());
  if (g.crypto && g.crypto.randomUUID) {
    const u = g.crypto.randomUUID.bind(g.crypto);
    g.crypto.randomUUID = () => (mark(), u());
  }
  const rec = (t, k) => {
    if (typeof k === "string" && !(k in Object.prototype)) defs[k] = t[k] ?? null;
  };
  const define = new Proxy(c0.define, {
    get: (t, k) => (rec(t, k), t[k]),
    has: (t, k) => (rec(t, k), k in t),
    ownKeys: (t) => (Reflect.ownKeys(t).forEach((k) => rec(t, k)), Reflect.ownKeys(t)),
  });
  const assets = (glob) => JSON.parse(native(String(glob)));
  const c = { assets, ns: c0.namespace, namespace: c0.namespace, module: c0.module,
              edition: c0.edition, target: c0.target, define };
  const mk = (k) => (id, def = {}) => ({ __beacon: k, id, def });
  g.assets = assets;
  for (const k of ["item", "block", "entity", "sound"]) g[k] = mk(k);
  g.file = (pack, path, body, merge) => ({ __beacon: "file", pack, path, body, merge });
  g.generate = (f) => {
    const r = typeof f === "function" ? f(c) : f;
    for (const i of [].concat(r ?? [])) out.push(i);
    return r;
  };
  g.__result = () => ({ out, unc, defs });
})()"#;

// ---------------------------------------------------------------- assets()

#[allow(clippy::too_many_arguments)]
fn walk(
    vfs: &dyn Vfs,
    dir: &RelPath,
    rel: &str,
    depth: usize,
    max: usize,
    m: &globset::GlobMatcher,
    inputs: &mut ObservedInputs,
    found: &mut Vec<String>,
) {
    let names = vfs.list(Some(dir));
    inputs.dirs.insert(
        dir.clone(),
        *blake3::hash(names.join("\n").as_bytes()).as_bytes(),
    );
    for n in names {
        let Ok(child) = dir.join(&n) else { continue };
        let r = format!("{rel}{n}");
        if !vfs.list(Some(&child)).is_empty() {
            if depth < max {
                walk(
                    vfs,
                    &child,
                    &format!("{r}/"),
                    depth + 1,
                    max,
                    m,
                    inputs,
                    found,
                );
            }
        } else if m.is_match(&r) {
            found.push(r);
        }
    }
}

/// `assets(glob)`: module-relative glob over the generator's module, sorted.
fn list_assets(
    vfs: &dyn Vfs,
    module: &RelPath,
    inputs: &mut ObservedInputs,
    glob: &str,
) -> Result<String, String> {
    let glob = glob.strip_prefix("./").unwrap_or(glob);
    let m = globset::GlobBuilder::new(glob)
        .literal_separator(true)
        .build()
        .map_err(|e| e.to_string())?
        .compile_matcher();
    let max = if glob.contains("**") {
        usize::MAX
    } else {
        glob.matches('/').count()
    };
    let mut found = vec![];
    walk(vfs, module, "", 0, max, &m, inputs, &mut found);
    found.sort();
    let list: Vec<_> = found
        .iter()
        .map(|p| {
            let file = module.join(p).map_or(String::new(), |f| f.to_string());
            let name = p.rsplit('/').next().unwrap_or(p);
            json!({ "name": name.split('.').next(), "path": p, "file": file })
        })
        .collect();
    Ok(Value::Array(list).to_string())
}

// ---------------------------------------------------------------- content -> items

struct Conv<'a> {
    g: &'a Generator,
    ctx: &'a GenCtx,
    vfs: &'a dyn Vfs,
    inputs: &'a mut ObservedInputs,
    items: Vec<ContentItem>,
}

fn obj(v: &Value, what: &str) -> Result<Map<String, Value>, String> {
    match v {
        Value::Object(m) => Ok(m.clone()),
        Value::Null => Ok(Map::new()),
        _ => Err(format!("{what} must be an object")),
    }
}

/// `{format_version, <key>: {...def, description: {identifier, ...}, components?}}`.
fn wrap(id: &str, def: &Value, key: &str, fv: &str, components: bool) -> Result<Value, String> {
    let mut d = obj(def, "definition")?;
    let fv = d.remove("format_version").unwrap_or_else(|| fv.into());
    let mut desc = Map::from_iter([("identifier".to_string(), Value::from(id))]);
    if let Some(x) = d.remove("description") {
        desc.extend(obj(&x, "description")?);
    }
    d.insert("description".into(), desc.into());
    if components {
        d.entry("components").or_insert_with(|| json!({}));
    }
    Ok(json!({ "format_version": fv, key: d }))
}

fn name_of(id: &str) -> Result<&str, String> {
    match id.split_once(':') {
        Some((ns, n)) if !ns.is_empty() && !n.is_empty() => Ok(n),
        _ => Err(format!("invalid id {id:?}: expected `namespace:name`")),
    }
}

fn strip_ext(p: &str) -> &str {
    p.rsplit_once('.').map_or(p, |(s, _)| s)
}

impl Conv<'_> {
    fn push(&mut self, pack: Pack, path: &str, body: Body, merge: Merge) -> Result<(), String> {
        let path = RelPath::parse(path).map_err(|e| e.to_string())?;
        self.items.push(ContentItem {
            dest: Dest {
                output: self.g.output.clone(),
                pack,
                path,
            },
            body,
            merge,
            origin: self.g.path.clone(),
            module: self.g.module.clone(),
        });
        Ok(())
    }

    fn json(&mut self, pack: Pack, path: String, v: Value) -> Result<(), String> {
        self.push(pack, &path, Body::Json(v), Merge::Error)
    }

    fn route(&self, rel: &str) -> Result<(Pack, String, Merge), String> {
        let rel = rel.strip_prefix("./").unwrap_or(rel);
        route_file(&self.ctx.module, &self.ctx.namespace, rel)
            .ok_or_else(|| format!("no convention route for {rel:?}"))
    }

    /// Module asset (`./x`): emitted as Passthrough; returns (emitted path, source bytes).
    fn asset(&mut self, rel: &str, bytes: bool) -> Result<(String, Vec<u8>), String> {
        let (pack, path, merge) = self.route(rel)?;
        let src = self
            .g
            .module
            .join(rel.strip_prefix("./").unwrap_or(rel))
            .map_err(|e| e.to_string())?;
        let b = observe(self.vfs, self.inputs, &src, bytes)
            .map_err(|e| format!("cannot read {src}: {e}"))?;
        self.push(pack, &path, Body::Passthrough(src), merge)?;
        Ok((path, b))
    }

    fn geometry(&mut self, v: &Value) -> Result<String, String> {
        let s = v.as_str().ok_or("client.geometry must be a string")?;
        if !s.starts_with("./") {
            return Ok(s.into());
        }
        let (_, bytes) = self.asset(s, true)?;
        let j: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{s}: {e}"))?;
        j["minecraft:geometry"][0]["description"]["identifier"]
            .as_str()
            .map(String::from)
            .or_else(|| {
                j.as_object()?
                    .keys()
                    .find(|k| k.starts_with("geometry."))
                    .cloned()
            })
            .ok_or_else(|| format!("{s}: no geometry identifier found"))
    }

    fn texture(&mut self, v: &Value) -> Result<String, String> {
        let s = v.as_str().ok_or("client.texture must be a string")?;
        if !s.starts_with("./") {
            return Ok(s.into());
        }
        Ok(strip_ext(&self.asset(s, false)?.0).into())
    }

    fn entity(&mut self, id: &str, def: &Value) -> Result<(), String> {
        let name = name_of(id)?;
        let d = obj(def, "entity definition")?;
        if let Some(s) = d.get("server") {
            let v = wrap(id, s, "minecraft:entity", "1.21.0", true)?;
            self.json(Pack::Behavior, format!("entities/{name}.json"), v)?;
        }
        if let Some(c) = d.get("client") {
            let mut c = obj(c, "client")?;
            let fv = c
                .remove("format_version")
                .unwrap_or_else(|| "1.10.0".into());
            let mut desc = Map::from_iter([("identifier".to_string(), Value::from(id))]);
            if let Some(x) = c.remove("description") {
                desc.extend(obj(&x, "description")?);
            }
            if let Some(g) = c.remove("geometry") {
                desc.insert("geometry".into(), json!({ "default": self.geometry(&g)? }));
            }
            if let Some(t) = c.remove("texture") {
                desc.insert("textures".into(), json!({ "default": self.texture(&t)? }));
            }
            desc.extend(c); // materials, render_controllers, animations, ...
            let v =
                json!({ "format_version": fv, "minecraft:client_entity": { "description": desc } });
            self.json(Pack::Resource, format!("entity/{name}.json"), v)?;
        }
        if let Some(s) = d.get("spawnRule") {
            let v = wrap(id, s, "minecraft:spawn_rules", "1.8.0", false)?;
            self.json(Pack::Behavior, format!("spawn_rules/{name}.json"), v)?;
        }
        for (loc, text) in obj(d.get("lang").unwrap_or(&Value::Null), "lang")? {
            let text = text.as_str().ok_or("lang values must be strings")?;
            let body = Body::Text(format!("entity.{id}.name={text}"));
            self.push(
                Pack::Resource,
                &format!("texts/{loc}.lang"),
                body,
                Merge::LangLines,
            )?;
        }
        Ok(())
    }

    fn sound(&mut self, id: &str, def: &Value) -> Result<(), String> {
        if id.is_empty() {
            return Err("sound id must not be empty".into());
        }
        let d = obj(def, "sound definition")?;
        let file = d
            .get("file")
            .and_then(Value::as_str)
            .ok_or("sound requires `file`")?;
        let (_, path, _) = self.route(file)?;
        let category = d
            .get("category")
            .cloned()
            .unwrap_or_else(|| "neutral".into());
        let v = json!({ "format_version": "1.20.20", "sound_definitions": {
            id: { "category": category, "sounds": [strip_ext(&path)] } } });
        self.push(
            Pack::Resource,
            "sounds/sound_definitions.json",
            Body::Json(v),
            Merge::JsonDeep,
        )
    }

    fn convert(&mut self, v: &Value) -> Result<(), String> {
        let kind = v["__beacon"]
            .as_str()
            .ok_or("generator returned something that is not content")?;
        if kind == "file" {
            let pack: Pack = serde_json::from_value(v["pack"].clone())
                .map_err(|_| "file(): pack must be \"BP\" or \"RP\"")?;
            let path = v["path"].as_str().ok_or("file(): path must be a string")?;
            let body = match (v["body"].get("json"), v["body"].get("text")) {
                (Some(j), None) => Body::Json(j.clone()),
                (None, Some(Value::String(t))) => Body::Text(t.clone()),
                _ => return Err("file(): body must be { json } or { text }".into()),
            };
            let merge = match &v["merge"] {
                Value::Null => Merge::Error,
                m => {
                    serde_json::from_value(m.clone()).map_err(|e| format!("file(): merge: {e}"))?
                }
            };
            return self.push(pack, path, body, merge);
        }
        let id = v["id"].as_str().ok_or("id must be a string")?;
        let def = &v["def"];
        match kind {
            "item" | "block" => {
                let (dir, key) = if kind == "item" {
                    ("items", "minecraft:item")
                } else {
                    ("blocks", "minecraft:block")
                };
                let j = wrap(id, def, key, "1.21.0", true)?;
                self.json(Pack::Behavior, format!("{dir}/{}.json", name_of(id)?), j)
            }
            "entity" => self.entity(id, def),
            "sound" => self.sound(id, def),
            k => Err(format!("unknown content kind {k:?}")),
        }
    }
}

// ---------------------------------------------------------------- runtime + run

struct Rt {
    rt: Runtime,
    deadline: Arc<Mutex<Instant>>,
    limits: Limits,
}

thread_local! {
    static RT: RefCell<Option<Rt>> = const { RefCell::new(None) };
}

impl Rt {
    fn new(limits: Limits) -> Result<Self, String> {
        let rt = Runtime::new().map_err(|e| e.to_string())?;
        rt.set_memory_limit(limits.memory);
        rt.set_max_stack_size(limits.stack);
        let deadline = Arc::new(Mutex::new(Instant::now()));
        let d = deadline.clone();
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > *d.lock().unwrap())));
        Ok(Self {
            rt,
            deadline,
            limits,
        })
    }
}

/// What `__assets` may reach, only while one `run` is on the stack.
struct Scope {
    vfs: Cell<Option<*const dyn Vfs>>,
    inputs: RefCell<ObservedInputs>,
}

struct Guard(Rc<Scope>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.vfs.set(None);
    }
}

struct Raw {
    out: Vec<Value>,
    uncacheable: bool,
    defines: Vec<(String, Value)>,
}

/// Line of the first stack frame inside a user module, mapped back to the TS source.
fn locate(c: &Closure, stack: &str) -> Option<(RelPath, u32)> {
    stack.lines().find_map(|l| {
        let l = l.trim().strip_prefix("at ")?;
        let l = l
            .rsplit_once('(')
            .map_or(l, |(_, r)| r)
            .trim_end_matches(')');
        let mut it = l.rsplitn(3, ':');
        let (a, b) = (it.next()?, it.next()?);
        let (name, line, col) = match it.next() {
            Some(n) => (n, b.parse::<u32>().ok()?, a.parse::<u32>().ok()?),
            None => (b, a.parse::<u32>().ok()?, u32::MAX),
        };
        let (_, map) = c.mods.get(name)?;
        let line = line.saturating_sub(1);
        let src = map
            .iter()
            .rfind(|t| (t.0, t.1) <= (line, col.saturating_sub(1)))
            .map_or(line, |t| t.2);
        Some((RelPath::parse(name).ok()?, src + 1))
    })
}

fn classify(g: &Generator, limits: Limits, c: &Closure, e: CaughtError) -> Diagnostic {
    let (msg, stack) = match &e {
        CaughtError::Exception(x) => (
            x.message().unwrap_or_default(),
            x.stack().unwrap_or_default(),
        ),
        other => (other.to_string(), String::new()),
    };
    let low = msg.to_lowercase();
    let (code, text) = if low.contains("interrupted") {
        (
            Code::GeneratorTimeout,
            format!(
                "generator exceeded its {} ms budget",
                limits.budget.as_millis()
            ),
        )
    } else if low.contains("out of memory") || low.contains("allocat") {
        (
            Code::GeneratorOutOfMemory,
            format!(
                "generator exceeded its {} MiB memory limit",
                limits.memory >> 20
            ),
        )
    } else {
        (Code::GeneratorError, msg)
    };
    match locate(c, &stack) {
        Some((p, line)) => Diagnostic::new(code, text).at(p, Some(line), None),
        None => Diagnostic::new(code, text).at(g.path.clone(), None, None),
    }
}

fn eval(
    limits: Limits,
    g: &Generator,
    ctx: &GenCtx,
    vfs: &dyn Vfs,
    closure: Closure,
    inputs: &mut ObservedInputs,
) -> Result<Raw, Diagnostic> {
    RT.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot
            .as_ref()
            .is_none_or(|r| r.limits != limits || r.rt.is_job_pending())
        {
            *slot = Some(Rt::new(limits).map_err(|m| err(g, m))?);
        }
        let rt = slot.as_ref().expect("just set");
        let entry = closure.mods[g.path.as_str()].0.clone();
        let closure = Rc::new(closure);
        rt.rt.set_loader(Fs(closure.clone()), Fs(closure.clone()));
        *rt.deadline.lock().unwrap() = Instant::now() + limits.budget;
        let context = Context::full(&rt.rt).map_err(|e| err(g, e.to_string()))?;
        let scope = Rc::new(Scope {
            // SAFETY: only the lifetime is erased. The pointer is dereferenced solely while
            // `_guard` is alive; its Drop clears it on every exit path, so jobs or functions
            // that outlive this call see `None` and throw instead of touching a dead Vfs.
            vfs: Cell::new(Some(unsafe {
                std::mem::transmute::<*const (dyn Vfs + '_), *const (dyn Vfs + 'static)>(vfs)
            })),
            inputs: RefCell::new(std::mem::take(inputs)),
        });
        let _guard = Guard(scope.clone());
        let deadline = *rt.deadline.lock().unwrap();
        let r = context.with(|c| {
            let sc = scope.clone();
            let module = g.module.clone();
            let run = || -> rquickjs::Result<String> {
                let globals = c.globals();
                globals.set(
                    "__assets",
                    Function::new(c.clone(), move |c: Ctx<'_>, glob: String| {
                        let Some(vfs) = sc.vfs.get() else {
                            return Err(Exception::throw_message(
                                &c,
                                "assets() called after its generator finished",
                            ));
                        };
                        // SAFETY: `Some` only while the owning `run` is on the stack.
                        let vfs = unsafe { &*vfs };
                        list_assets(vfs, &module, &mut sc.inputs.borrow_mut(), &glob)
                            .map_err(|m| Exception::throw_message(&c, &m))
                    }),
                )?;
                globals.set("__ctx", serde_json::to_string(ctx).unwrap_or_default())?;
                c.eval::<(), _>(PRELUDE)?;
                let (_, p) = Module::declare(c.clone(), g.path.as_str(), entry)?.eval()?;
                p.finish::<()>()?;
                // Drain microtasks under the deadline; whatever is left is a timeout.
                // (job exceptions are swallowed by rquickjs; an unhandled rejection is not fatal)
                while c.execute_pending_job() {
                    if Instant::now() >= deadline {
                        return Err(Exception::throw_message(&c, "interrupted"));
                    }
                }
                c.eval("JSON.stringify(__result())")
            };
            run()
                .catch(&c)
                .map_err(|e| classify(g, limits, &closure, e))
        });
        drop(context);
        rt.rt.run_gc(); // an OOM'd context's garbage must not starve the next run
        if rt.rt.is_job_pending() {
            *slot = None; // leftover jobs must never run in another generator's context
        }
        drop(_guard);
        *inputs = scope.inputs.take();
        let v: Value = serde_json::from_str(&r?).map_err(|e| err(g, e.to_string()))?;
        Ok(Raw {
            out: v["out"].as_array().cloned().unwrap_or_default(),
            uncacheable: v["unc"].as_bool().unwrap_or(false),
            defines: v["defs"].as_object().map_or(vec![], |m| {
                m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            }),
        })
    })
}

impl GeneratorHost for Host {
    fn run(&self, g: &Generator, ctx: &GenCtx, vfs: &dyn Vfs) -> GenResult {
        let mut r = GenResult {
            items: vec![],
            inputs: ObservedInputs::default(),
            diagnostics: vec![],
        };
        let Some(closure) = collect(g, vfs, &mut r) else {
            return r;
        };
        let raw = match eval(self.limits, g, ctx, vfs, closure, &mut r.inputs) {
            Ok(x) => x,
            Err(d) => {
                r.diagnostics.push(d);
                return r;
            }
        };
        r.inputs.uncacheable |= raw.uncacheable;
        r.inputs.defines.extend(raw.defines);
        let mut conv = Conv {
            g,
            ctx,
            vfs,
            inputs: &mut r.inputs,
            items: vec![],
        };
        for v in &raw.out {
            if let Err(m) = conv.convert(v) {
                r.diagnostics.push(err(g, m));
            }
        }
        r.items = conv.items;
        r
    }
}
