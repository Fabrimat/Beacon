//! ModularMC evaluation for `beacon migrate` (5.6), phase 3 A1: runs the project's `_map.ts`
//! files and `auto-map.ts` once in the JS host (TS strip, relative and `with {type: "json"}`
//! imports, a `process.env` shim) and records the concrete output. Never used at build time.
//!
//! The evaluation itself is `js/modular_mc.js`, a port of ModularMC's map-ts / json-template /
//! text-template / json-merge / auto-map-resolver (MIT, see NOTICE). Rust only provides the
//! import closure, the read-only VFS functions (`__legacy.*`), limits and result conversion.

use crate::Limits;
use crate::host::{Closure, Fs, collect};
use beacon_engine::diag::{Code, Diagnostic};
use beacon_engine::host::GenResult;
use beacon_engine::migrate::{Contributor, EvalOutput, LegacyEval};
use beacon_engine::model::{Body, Generator};
use beacon_engine::path::{RelPath, Vfs};
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::{
    CatchResultExt, CaughtError, Context, Ctx, Exception, Function, Module, Object, Runtime,
};
use serde::Deserialize;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

const DRIVER: &str = include_str!("../js/modular_mc.js");
/// Never visible to the maps, whatever the `Vfs` indexed.
const EXCLUDED: [&str; 3] = ["node_modules", ".regolith", ".git"];

/// Limits for migrate-only evaluation: one context, 300 s (one-shot and trusted; large
/// projects take ~20 s on a release build, longer on debug), 256 MiB.
pub fn migrate_limits() -> Limits {
    Limits {
        memory: 256 << 20,
        budget: Duration::from_secs(300),
        ..Limits::default()
    }
}

/// The project's base pack dirs (Regolith `packs.behaviorPack` / `packs.resourcePack`,
/// project-relative). Regolith copies them, and the data dir, into its tmp dir before ModularMC
/// runs, so map entries conflict with their files: they seed the virtual output tree as
/// `BP/..`, `RP/..` and `data/..`. They must be indexed by `vfs` (not excluded).
#[derive(Debug, Clone, Default)]
pub struct BasePaths {
    pub bp: Option<RelPath>,
    pub rp: Option<RelPath>,
}

/// Evaluates every `<data_dir>/modular_mc/**/_map.ts` (sorted by path, like ModularMC) and
/// applies its `MAP` entries in a virtual output tree. `vfs` indexes the legacy project root;
/// `data_dir` is the project-relative `dataPath`. `env` is what `process.env` returns; every
/// read is recorded in [`LegacyEval::env_read`]. Failures are diagnostics in the result.
/// Only files some entry wrote to are outputs; seeded base files stay out.
pub fn evaluate_modular_mc(
    vfs: &dyn Vfs,
    data_dir: &RelPath,
    base: &BasePaths,
    env: &BTreeMap<String, String>,
    limits: Limits,
) -> LegacyEval {
    let mut ev = LegacyEval::default();
    let unsupported = |ev: &mut LegacyEval, m: String| {
        ev.diagnostics
            .push(Diagnostic::new(Code::MigrateUnsupported, m));
    };
    let Ok(root) = data_dir.join("modular_mc") else {
        unsupported(&mut ev, format!("bad data dir {data_dir}"));
        return ev;
    };
    let mut maps = vec![];
    find_maps(vfs, &root, &mut maps);
    maps.sort(); // ModularMC sorts the full posix paths
    let auto = root.join("auto-map.ts").ok().filter(|p| vfs.exists(p));

    // One closure for everything: collect per entry file, merge.
    let mut closure = Closure::default();
    for p in maps.iter().chain(auto.iter()) {
        let g = Generator {
            path: p.clone(),
            module: root.clone(),
            output: String::new(),
            namespace: String::new(),
        };
        let mut r = GenResult {
            items: vec![],
            inputs: Default::default(),
            diagnostics: vec![],
        };
        match collect(&g, vfs, &mut r) {
            Some(c) => {
                closure.mods.extend(c.mods);
                closure.resolved.extend(c.resolved);
            }
            None => {
                for d in r.diagnostics {
                    unsupported(&mut ev, format!("{p}: {}", d.message));
                }
            }
        }
    }
    if !ev.diagnostics.is_empty() {
        return ev;
    }

    let deadline = Instant::now() + limits.budget;
    let Ok(rt) = Runtime::new() else {
        unsupported(&mut ev, "cannot start the JS runtime".into());
        return ev;
    };
    rt.set_memory_limit(limits.memory);
    rt.set_max_stack_size(limits.stack);
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let closure = Rc::new(closure);
    rt.set_loader(Wrap(closure.clone()), Wrap(closure.clone()));
    let Ok(context) = Context::full(&rt) else {
        unsupported(&mut ev, "cannot create the JS context".into());
        return ev;
    };
    let slot: Slot = Rc::new(Cell::new(Some(
        // SAFETY: only the lifetime is erased. The pointer is dereferenced solely while the
        // guard below is alive; its Drop clears it on every exit path, so anything that
        // outlives this call sees `None` and throws.
        unsafe { std::mem::transmute::<*const (dyn Vfs + '_), *const (dyn Vfs + 'static)>(vfs) },
    )));
    struct Guard(Slot);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.set(None);
        }
    }
    let _guard = Guard(slot.clone());

    let mut seed = vec![];
    for (to, from) in [
        ("BP", &base.bp),
        ("RP", &base.rp),
        ("data", &Some(data_dir.clone())),
    ] {
        if let Some(f) = from {
            seed_files(vfs, f, to, &mut seed);
        }
    }
    let seed_json = serde_json::to_string(&seed).unwrap_or_else(|_| "[]".into());
    let env_json = serde_json::to_string(env).unwrap_or_else(|_| "{}".into());
    let (dump, fail) = context.with(|c| {
        let mut fail: Option<Diagnostic> = None;
        let run = || -> Result<String, (Option<String>, CaughtError<'_>)> {
            install(&c, slot.clone(), &env_json, &seed_json)
                .catch(&c)
                .map_err(|e| (None, e))?;
            let mmc: Object = c.globals().get("__mmc").catch(&c).map_err(|e| (None, e))?;
            let load = |p: &RelPath, f: &str| {
                (|| {
                    let js = closure.mods[p.as_str()].0.as_str();
                    let d = Module::declare(c.clone(), p.as_str(), js)?;
                    d.meta()?.set("main", false)?;
                    let (m, pr) = d.eval()?;
                    pr.finish::<()>()?;
                    let f: Function = mmc.get(f)?;
                    f.call::<_, ()>((p.as_str(), m.namespace()?))
                })()
                .catch(&c)
                .map_err(|e| (Some(p.to_string()), e))
            };
            if let Some(a) = &auto
                && let Err((m, e)) = load(a, "autoMap")
            {
                let d = classify(m.as_deref(), &e, limits, deadline);
                if d.message.contains("Deno") || d.message.contains("budget") {
                    return Err((m, e)); // not just a missing AUTO_MAP
                }
                // upstream: warn and continue with an empty resolver
                let _ = c.eval::<(), _>(format!(
                    "console.warn({})",
                    serde_json::Value::from(format!("Failed to load AUTO_MAP: {}", d.message))
                ));
            }
            for p in &maps {
                load(p, "add")?;
            }
            let run: Function = mmc.get("run").catch(&c).map_err(|e| (None, e))?;
            run.call::<_, String>(()).catch(&c).map_err(|e| (None, e))
        };
        match run() {
            Ok(s) => (Some(s), fail),
            Err((m, e)) => {
                fail = Some(classify(m.as_deref(), &e, limits, deadline));
                let timed_out = fail.as_ref().is_some_and(|d| d.message.contains("budget"));
                let dump = (!timed_out)
                    .then(|| {
                        let f: Function = c
                            .globals()
                            .get::<_, Object>("__mmc")
                            .ok()?
                            .get("dump")
                            .ok()?;
                        f.call::<_, String>((rquickjs::Undefined,)).ok()
                    })
                    .flatten();
                (dump, fail)
            }
        }
    });
    drop(context);
    drop(_guard);
    ev.diagnostics.extend(fail);
    if let Some(d) = dump {
        absorb(&mut ev, &d);
    }
    ev
}

/// Every file under `from` as `(to/<rel>, from/<rel>)`.
fn seed_files(vfs: &dyn Vfs, from: &RelPath, to: &str, out: &mut Vec<(String, String)>) {
    for n in vfs.list(Some(from)) {
        let Ok(p) = from.join(&n) else { continue };
        if EXCLUDED.contains(&n.as_str()) {
            continue;
        }
        if vfs.list(Some(&p)).is_empty() {
            out.push((format!("{to}/{n}"), p.to_string()));
        } else {
            seed_files(vfs, &p, &format!("{to}/{n}"), out);
        }
    }
}

/// Posix paths of every `_map.ts` under `dir`.
fn find_maps(vfs: &dyn Vfs, dir: &RelPath, out: &mut Vec<RelPath>) {
    for n in vfs.list(Some(dir)) {
        let Ok(p) = dir.join(&n) else { continue };
        if EXCLUDED.contains(&n.as_str()) {
            continue;
        }
        if n == "_map.ts" {
            out.push(p);
        } else {
            find_maps(vfs, &p, out);
        }
    }
}

// ---------------------------------------------------------------- JS side

struct Wrap(Rc<Closure>);

impl Resolver for Wrap {
    fn resolve<'js>(
        &mut self,
        c: &Ctx<'js>,
        base: &str,
        name: &str,
        a: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        Fs(self.0.clone()).resolve(c, base, name, a)
    }
}

impl Loader for Wrap {
    fn load<'js>(
        &mut self,
        c: &Ctx<'js>,
        name: &str,
        a: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js>> {
        let m = Fs(self.0.clone()).load(c, name, a)?;
        m.meta()?.set("main", false)?; // Deno: only the entry module has `import.meta.main`
        Ok(m)
    }
}

/// A path the maps may touch: inside the root (`RelPath`), outside the excluded dirs.
fn guard(p: &str) -> Result<Option<RelPath>, String> {
    let p = p.trim_matches('/');
    if p.is_empty() || p == "." {
        return Ok(None);
    }
    if p.split('/').any(|s| EXCLUDED.contains(&s)) {
        return Err(format!("{p}: excluded from the project"));
    }
    RelPath::parse(p).map(Some).map_err(|e| format!("{p}: {e}"))
}

fn statements(src: &str) -> usize {
    use oxc::allocator::Allocator;
    use oxc::parser::{ParseOptions, Parser};
    use oxc::span::SourceType;
    let a = Allocator::default();
    let o = ParseOptions {
        allow_return_outside_function: true,
        ..Default::default()
    };
    Parser::new(&a, src, SourceType::ts())
        .with_options(o)
        .parse()
        .program
        .body
        .len()
}

/// The host functions, then the env and the driver (which takes `__legacy` private).
fn install<'js>(c: &Ctx<'js>, slot: Slot, env_json: &str, seed: &str) -> rquickjs::Result<()> {
    host_fns(c, slot)?;
    c.globals().set("__env", env_json)?;
    c.globals().set("__seed", seed)?;
    c.eval::<(), _>(DRIVER)
}

type Slot = Rc<Cell<Option<*const dyn Vfs>>>;

/// `__legacy.{readText,listDir,exists,countStatements}`: read-only, VFS-backed.
fn host_fns<'js>(c: &Ctx<'js>, slot: Slot) -> rquickjs::Result<()> {
    let h = Object::new(c.clone())?;
    // SAFETY (all four): `Some` only while `evaluate_modular_mc` is on the stack.
    let vfs = move |c: &Ctx<'_>| -> rquickjs::Result<&'static dyn Vfs> {
        slot.get()
            .map(|p| unsafe { &*p })
            .ok_or_else(|| Exception::throw_message(c, "VFS used after evaluation finished"))
    };
    let fail = |c: &Ctx<'_>, m: String| Exception::throw_message(c, &m);
    let v = vfs.clone();
    h.set(
        "readText",
        Function::new(c.clone(), move |c: Ctx<'_>, p: String| {
            let p = guard(&p).map_err(|m| fail(&c, m))?;
            let p = p.ok_or_else(|| fail(&c, "readText: not a file".into()))?;
            let b = v(&c)?.read(&p).map_err(|e| fail(&c, format!("{p}: {e}")))?;
            Ok::<_, rquickjs::Error>(String::from_utf8(b).ok())
        }),
    )?;
    let v = vfs.clone();
    h.set(
        "exists",
        Function::new(c.clone(), move |c: Ctx<'_>, p: String| {
            let p = guard(&p).map_err(|m| fail(&c, m))?;
            Ok::<_, rquickjs::Error>(p.is_none_or(|p| v(&c).is_ok_and(|v| v.exists(&p))))
        }),
    )?;
    let v = vfs;
    h.set(
        "listDir",
        Function::new(c.clone(), move |c: Ctx<'_>, p: String| {
            let dir = guard(&p).map_err(|m| fail(&c, m))?;
            let v = v(&c)?;
            let names = v
                .list(dir.as_ref())
                .into_iter()
                .filter(|n| !EXCLUDED.contains(&n.as_str()))
                .map(|n| {
                    let child = match &dir {
                        Some(d) => d.join(&n),
                        None => RelPath::parse(&n),
                    };
                    let is_dir = child.is_ok_and(|ch| !v.list(Some(&ch)).is_empty());
                    if is_dir { format!("{n}/") } else { n }
                })
                .collect::<Vec<_>>();
            Ok::<_, rquickjs::Error>(names)
        }),
    )?;
    h.set(
        "countStatements",
        Function::new(c.clone(), |s: String| statements(&s)),
    )?;
    c.globals().set("__legacy", h)
}

// ---------------------------------------------------------------- results

/// A caught JS error as a diagnostic: `Deno` references, timeouts and the rest all end up as
/// `MigrateUnsupported` naming the map.
fn classify(
    map: Option<&str>,
    e: &CaughtError<'_>,
    limits: Limits,
    deadline: Instant,
) -> Diagnostic {
    let msg = match e {
        CaughtError::Exception(x) => x.message().unwrap_or_default(),
        other => other.to_string(),
    };
    let low = msg.to_lowercase();
    let text = if low.contains("interrupted") || Instant::now() > deadline {
        format!(
            "evaluation exceeded its {} s budget",
            limits.budget.as_secs_f32()
        )
    } else if low.contains("out of memory") {
        format!(
            "evaluation exceeded its {} MiB memory limit",
            limits.memory >> 20
        )
    } else if msg.contains("Deno") {
        format!("uses Deno APIs, which Beacon does not evaluate: {msg}")
    } else {
        msg
    };
    let text = match map {
        Some(m) => format!("{m}: {text}"),
        None => text,
    };
    Diagnostic::new(Code::MigrateUnsupported, text)
}

#[derive(Deserialize)]
struct Failure {
    map: String,
    message: String,
}

#[derive(Deserialize)]
struct Out {
    path: String,
    kind: String,
    text: Option<String>,
    src: Option<RelPath>,
    contributors: Vec<Contributor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Dump {
    error: Option<Failure>,
    outputs: Vec<Out>,
    scripts: Vec<RelPath>,
    env_read: BTreeMap<String, Option<String>>,
    logs: Vec<String>,
}

fn absorb(ev: &mut LegacyEval, json: &str) {
    let d: Dump = match serde_json::from_str(json) {
        Ok(d) => d,
        Err(e) => {
            let m = format!("unreadable evaluation result: {e}");
            ev.diagnostics
                .push(Diagnostic::new(Code::MigrateUnsupported, m));
            return;
        }
    };
    if let Some(f) = d.error {
        let t = if f.message.contains("Deno") {
            format!(
                "uses Deno APIs, which Beacon does not evaluate: {}",
                f.message
            )
        } else {
            f.message
        };
        let m = format!("{}: {t}", f.map);
        ev.diagnostics
            .push(Diagnostic::new(Code::MigrateUnsupported, m));
    }
    for o in d.outputs {
        let body = match (o.kind.as_str(), o.text, o.src) {
            ("pass", _, Some(s)) => Ok(Body::Passthrough(s)),
            ("json", Some(t), _) => serde_json::from_str(&t)
                .map(Body::Json)
                .map_err(|e| e.to_string()),
            ("text", Some(t), _) => Ok(Body::Text(t)),
            (k, ..) => Err(format!("bad body kind {k}")),
        };
        match body {
            Ok(body) => {
                let path = o.path;
                let eo = EvalOutput {
                    path: path.clone(),
                    body,
                    contributors: o.contributors,
                };
                ev.outputs.insert(path, eo);
            }
            Err(e) => {
                let m = format!("{}: {e}", o.path);
                ev.diagnostics
                    .push(Diagnostic::new(Code::MigrateUnsupported, m));
            }
        }
    }
    ev.scripts = d.scripts;
    ev.env_read = d.env_read;
    ev.logs = d.logs;
}

#[cfg(test)]
mod tests {
    use super::*;
    use beacon_engine::vfs::Index;

    #[test]
    fn host_functions_stay_inside_the_root() {
        let d = tempfile::tempdir().unwrap();
        for p in [
            "ok/a.txt",
            ".regolith/s.txt",
            "node_modules/p/s.txt",
            ".git/s",
            "ok/.regolith/s",
        ] {
            let f = d.path().join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, "x").unwrap();
        }
        // `.regolith` is deliberately indexed: the functions must refuse it themselves
        let (idx, _) = Index::build(d.path(), &[], None);
        let vfs: &dyn Vfs = &idx;
        // SAFETY: `slot` is cleared below, before `idx` is dropped.
        let slot: Slot = Rc::new(Cell::new(Some(unsafe {
            std::mem::transmute::<*const (dyn Vfs + '_), *const (dyn Vfs + 'static)>(vfs)
        })));
        let rt = Runtime::new().unwrap();
        let cx = Context::full(&rt).unwrap();
        cx.with(|c| {
            host_fns(&c, slot.clone()).unwrap();
            let t = |js: &str| c.eval::<String, _>(js).unwrap();
            assert_eq!(t("__legacy.readText('ok/a.txt')"), "x");
            assert_eq!(t("JSON.stringify(__legacy.listDir(''))"), r#"["ok/"]"#);
            assert_eq!(t("JSON.stringify(__legacy.listDir('ok'))"), r#"["a.txt"]"#);
            for bad in [
                "readText('../x')",
                "readText('ok/../../x')",
                "readText('/etc/passwd')",
                "readText('C:/Windows/win.ini')",
                "readText('.regolith/s.txt')",
                "readText('ok/.regolith/s')",
                "readText('node_modules/p/s.txt')",
                "readText('.git/s')",
                "listDir('.regolith')",
                "listDir('../')",
                "exists('node_modules')",
                "exists('../x')",
            ] {
                let r = c.eval::<String, _>(format!(
                    "try {{ String(__legacy.{bad}) }} catch (e) {{ 'threw' }}"
                ));
                assert_eq!(r.unwrap(), "threw", "{bad}");
            }
        });
        slot.set(None);
    }
}
