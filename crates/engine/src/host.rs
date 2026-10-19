//! Contract: the two out-of-crate services the engine calls. `beacon-jshost` implements
//! [`GeneratorHost`], `beacon-bundler` implements [`Bundler`]; the engine consumes the traits
//! (no crate cycle) and the CLI wires the implementations in. Tests can use fakes.

use crate::diag::Diagnostic;
use crate::model::{ContentItem, GenCtx, Generator, ObservedInputs};
use crate::path::{RelPath, Vfs};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub struct GenResult {
    pub items: Vec<ContentItem>,
    pub inputs: ObservedInputs,
    pub diagnostics: Vec<Diagnostic>,
}

/// Runs one generator in a fresh JS context. Limits (5.4) become diagnostics, never panics.
pub trait GeneratorHost: Send + Sync {
    fn run(&self, generator: &Generator, ctx: &GenCtx, vfs: &dyn Vfs) -> GenResult;
    /// Identity of the host's observable behavior (runtime API, prelude, TS stripping). Part
    /// of the generator cache key: change it whenever the same inputs could give other output.
    fn version(&self) -> String;
}

pub struct BundleRequest<'a> {
    /// Project root: esbuild cwd and resolveDir.
    pub root: &'a Path,
    /// Sorted; the bundler synthesizes one stdin entry importing each for side effects.
    pub entries: &'a [RelPath],
    pub sourcemap: bool,
    pub minify: bool,
    /// The target's `define`, exposed to scripts as `beacon.define.<K>` (compile-time
    /// constants: `--define:beacon.define.<K>=<json>`).
    pub define: &'a BTreeMap<String, Value>,
    /// Modules the game provides at runtime (the manifest's script module dependencies):
    /// left as imports, with their deep imports (`--external:<n>`, `--external:<n>/*`).
    /// Everything else, including npm-only `@minecraft/*` packages, is bundled.
    pub externals: &'a [String],
}

pub struct BundleResult {
    /// `None` on failure; the body of `BP/scripts/main.js`.
    pub code: Option<String>,
    pub map: Option<String>,
    /// Metafile inputs under root (for watch); `<stdin>` and externals filtered out.
    pub inputs: BTreeSet<RelPath>,
    /// Some input lay outside the root (`../shared/x.ts`) and is not in `inputs`: the engine
    /// cannot see when it changes, so such a bundle is never reused.
    pub external_inputs: bool,
    /// `BundlerMissing` | `BundleError`, with spans from esbuild's errors.
    pub diagnostics: Vec<Diagnostic>,
}

/// Game-script bundler (esbuild CLI: `--bundle --format=esm --platform=neutral
/// --external:<each declared script module>`).
pub trait Bundler: Send + Sync {
    fn bundle(&self, req: &BundleRequest) -> BundleResult;
    /// e.g. `esbuild 0.25.4`, recorded in the lockfile (4.2).
    fn version(&self) -> Option<String>;
}
