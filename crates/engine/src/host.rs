//! Contract: the two out-of-crate services the engine calls. `beacon-jshost` implements
//! [`GeneratorHost`], `beacon-bundler` implements [`Bundler`]; the engine consumes the traits
//! (no crate cycle) and the CLI wires the implementations in. Tests can use fakes.

use crate::diag::Diagnostic;
use crate::model::{ContentItem, GenCtx, Generator, ObservedInputs};
use crate::path::{RelPath, Vfs};
use std::collections::BTreeSet;
use std::path::Path;

pub struct GenResult {
    pub items: Vec<ContentItem>,
    pub inputs: ObservedInputs,
    pub diagnostics: Vec<Diagnostic>,
}

/// Runs one generator in a fresh JS context. Limits (5.4) become diagnostics, never panics.
pub trait GeneratorHost: Send + Sync {
    fn run(&self, generator: &Generator, ctx: &GenCtx, vfs: &dyn Vfs) -> GenResult;
}

pub struct BundleRequest<'a> {
    /// Project root: esbuild cwd and resolveDir.
    pub root: &'a Path,
    /// Sorted; the bundler synthesizes one stdin entry importing each for side effects.
    pub entries: &'a [RelPath],
    pub sourcemap: bool,
    pub minify: bool,
}

pub struct BundleResult {
    /// `None` on failure; the body of `BP/scripts/main.js`.
    pub code: Option<String>,
    pub map: Option<String>,
    /// Metafile inputs under root (for watch); `<stdin>` and externals filtered out.
    pub inputs: BTreeSet<RelPath>,
    /// `BundlerMissing` | `BundleError`, with spans from esbuild's errors.
    pub diagnostics: Vec<Diagnostic>,
}

/// Game-script bundler (esbuild CLI: `--bundle --format=esm --platform=neutral
/// --external:@minecraft/*`).
pub trait Bundler: Send + Sync {
    fn bundle(&self, req: &BundleRequest) -> BundleResult;
    /// e.g. `esbuild 0.25.4`, recorded in the lockfile (4.2).
    fn version(&self) -> Option<String>;
}
