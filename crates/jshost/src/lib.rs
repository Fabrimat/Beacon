//! Seed of the Beacon JS host: QuickJS behind one trait, TS stripped by oxc.
//! One runtime per host (thread), one fresh context per evaluation.

use rquickjs::loader::{ImportAttributes, Loader, Resolver};

use rquickjs::{CatchResultExt, Context, Ctx, Function, Module, Runtime};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod host;
pub mod legacy;
pub mod mem;

pub use host::Host;

pub use serde_json::Value;

/// Virtual project files: path (`gen/a.ts`, `data/x.json`) -> source text.
pub type Files = HashMap<String, String>;

pub struct Output {
    /// Content items (`{kind, id, ...}`), in emission order.
    pub items: Vec<Value>,
    /// Script touched `Date.now` / `Math.random`: result must not be cached.
    pub uncacheable: bool,
}

/// The engine seam (ARCHITECTURE 5.4 / 11.2): swappable for V8 if perf demands.
pub trait Evaluator {
    /// Evaluate `entry` (a key of `files`) in a fresh context. `assets` is the
    /// project-relative file list that `assets(glob)` filters.
    fn eval(&self, entry: &str, files: &Files, assets: &[String]) -> Result<Output, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub memory: usize,
    pub stack: usize,
    pub budget: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory: 64 << 20,
            stack: 512 << 10,
            budget: Duration::from_secs(2),
        }
    }
}

pub struct QuickJs {
    rt: Runtime,
    deadline: Arc<Mutex<Instant>>,
    budget: Duration,
}

impl QuickJs {
    pub fn new(limits: Limits) -> Result<Self, String> {
        let rt = Runtime::new().map_err(|e| e.to_string())?;
        rt.set_memory_limit(limits.memory);
        rt.set_max_stack_size(limits.stack);
        let deadline = Arc::new(Mutex::new(Instant::now()));
        let d = deadline.clone();
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > *d.lock().unwrap())));
        Ok(Self {
            rt,
            deadline,
            budget: limits.budget,
        })
    }

    /// Create a bare context (exposed so the spike can time/measure it).
    pub fn context(&self) -> Result<Context, String> {
        Context::full(&self.rt).map_err(|e| e.to_string())
    }
}

/// Strip TypeScript syntax (no type checking) with oxc.
pub fn strip_ts(path: &str, src: &str) -> Result<String, String> {
    host::compile(path, src).map(|c| c.js)
}

const RUNTIME_MODULE: &str = r#"
const out = globalThis.__out;
const push = (kind) => (id, def = {}) => { const i = { kind, id, ...def }; out.push(i); return i; };
export const sound = push("sound");
export const entity = push("entity");
export const item = push("item");
export const assets = (glob) => globalThis.__assets(glob);
export const generate = (f) => { const r = typeof f === "function" ? f() : f; for (const i of [].concat(r ?? [])) out.push(i); };
"#;

// Flag nondeterministic sources; `new Date()` is not covered (ponytail: wrap if generators need it).
const PRELUDE: &str = r#"
globalThis.__out = [];
globalThis.__uncacheable = false;
const now = Date.now, rnd = Math.random;
Date.now = () => (globalThis.__uncacheable = true, now());
Math.random = () => (globalThis.__uncacheable = true, rnd());
"#;

struct Fs(Rc<Files>);

fn normalize(base: &str, name: &str) -> String {
    let mut parts: Vec<&str> = base
        .rsplit_once('/')
        .map_or(vec![], |(d, _)| d.split('/').collect());
    for p in name.split('/') {
        match p {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

impl Resolver for Fs {
    fn resolve<'js>(
        &mut self,
        _: &Ctx<'js>,
        base: &str,
        name: &str,
        _: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if name.starts_with("beacon:") {
            return Ok(name.into());
        }
        if !name.starts_with('.') {
            return Err(rquickjs::Error::new_resolving(base, name));
        }
        let p = normalize(base, name);
        [
            p.clone(),
            format!("{p}.ts"),
            format!("{p}.js"),
            format!("{p}/index.ts"),
        ]
        .into_iter()
        .find(|c| self.0.contains_key(c))
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
        let src = self
            .0
            .get(name)
            .ok_or_else(|| rquickjs::Error::new_loading(name))?;
        let js = if name.ends_with(".json") {
            host::json_module(src)
        } else if name.ends_with(".ts") {
            strip_ts(name, src).map_err(|m| rquickjs::Error::new_loading_message(name, m))?
        } else {
            src.clone()
        };
        Module::declare(ctx.clone(), name, js)
    }
}

impl Evaluator for QuickJs {
    fn eval(&self, entry: &str, files: &Files, assets: &[String]) -> Result<Output, String> {
        // ponytail: copies files per eval; share an Rc<Files> when it shows in profiles.
        let fs = Rc::new(files.clone());
        self.rt.set_loader(Fs(fs.clone()), Fs(fs));
        *self.deadline.lock().unwrap() = Instant::now() + self.budget;
        let ctx = self.context()?;
        ctx.with(|ctx| {
            let run = || -> rquickjs::Result<(String, bool)> {
                let a = assets.to_vec();
                ctx.globals().set(
                    "__assets",
                    Function::new(ctx.clone(), move |glob: String| -> Vec<String> {
                        match globset::Glob::new(&glob) {
                            Ok(g) => {
                                let m = g.compile_matcher();
                                a.iter().filter(|p| m.is_match(p)).cloned().collect()
                            }
                            Err(_) => vec![],
                        }
                    }),
                )?;
                ctx.eval::<(), _>(PRELUDE)?;
                let p = Module::evaluate(
                    ctx.clone(),
                    "beacon:entry",
                    format!("import \"./{entry}\";"),
                )?;
                p.finish::<()>()?;
                let json: String = ctx.eval("JSON.stringify(globalThis.__out)")?;
                Ok((json, ctx.eval("globalThis.__uncacheable")?))
            };
            let (json, uncacheable) = run().catch(&ctx).map_err(|e| e.to_string())?;
            let items = serde_json::from_str(&json).map_err(|e| e.to_string())?;
            Ok(Output { items, uncacheable })
        })
    }
}
