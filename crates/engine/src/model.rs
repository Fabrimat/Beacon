//! Contract: the content model (5.3: "one content model, one merge primitive").

use crate::path::{Hash, RelPath};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Output id from `beacon.outputs[].id`; the implicit single output is `"main"`.
pub type OutputId = String;

/// A pack version (`[major, minor, patch]`), as written in manifests.
pub type Version = [u32; 3];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Pack {
    #[serde(rename = "BP")]
    Behavior,
    #[serde(rename = "RP")]
    Resource,
}

/// Where an item lands: one pack of one output, at a pack-relative path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Dest {
    pub output: OutputId,
    pub pack: Pack,
    pub path: RelPath,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Body {
    /// Raw convention file: bytes copied as-is (JSONC, comments and key order preserved),
    /// never re-serialized. Binaries are streamed, never loaded (5.2).
    Passthrough(RelPath),
    /// Generated or merged JSON; written with the canonical writer.
    Json(Value),
    Text(String),
    /// Generated binary (e.g. an edited `level.dat`); hashed and written like `Text`.
    Bytes(Vec<u8>),
}

/// What happens when several items share a `Dest` (the one merge primitive).
///
/// Group rule for one `Dest`:
/// 1. `Skip` items yield to any non-`Skip` item (defaults); if all are `Skip`, the first by
///    origin path wins.
/// 2. The remaining items must share one policy. `Error` with 2+ items, or mixed policies,
///    is `Code::RouteCollision`.
/// 3. `JsonDeep`: objects recurse, arrays concatenate (in origin order), equal scalars are
///    fine, differing scalars are `Code::MergeConflict`.
/// 4. `LangLines`: lines concatenate in origin order; the same key twice is
///    `Code::LangDuplicateKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Merge {
    #[default]
    Error,
    Skip,
    JsonDeep,
    LangLines,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentItem {
    pub dest: Dest,
    pub body: Body,
    pub merge: Merge,
    /// Source that produced it (the file itself, or the generator).
    pub origin: RelPath,
    /// Module folder, e.g. `src/vehicles` (folder = module).
    pub module: RelPath,
}

/// A generator file to evaluate (`*.gen.ts`). Produced by routing (A1), run by the
/// `GeneratorHost` (A3).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Generator {
    pub path: RelPath,
    pub module: RelPath,
    pub output: OutputId,
    pub namespace: String,
}

/// Everything a generator evaluation actually read (5.2 "cache keyed by observed inputs").
/// Recorded by the JS host, hashed by the VFS, used as the cache key and for watch
/// invalidation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedInputs {
    /// Files read (including imported modules and JSON data modules).
    pub files: BTreeMap<RelPath, Hash>,
    /// Directory listings (globs, `assets()`), keyed by dir; value = hash of sorted names.
    pub dirs: BTreeMap<RelPath, Hash>,
    /// Paths probed and not found (failed import resolution, missing optional files).
    pub negatives: BTreeSet<RelPath>,
    /// `ctx.define.*` values read (4.4).
    pub defines: BTreeMap<String, Value>,
    /// Allowlisted env vars read, with values.
    pub env: BTreeMap<String, String>,
    /// `Date.now`, `new Date()`, `Math.random`, `crypto.randomUUID` used, or an I/O error
    /// other than not-found happened during the evaluation: never cached.
    pub uncacheable: bool,
}

/// Build context a generator sees as `ctx` (5.3).
#[derive(Debug, Clone, Serialize)]
pub struct GenCtx {
    /// Module name (last path segment, e.g. `vehicles`), not the path.
    pub module: String,
    pub namespace: String,
    pub edition: Edition,
    pub target: String,
    pub define: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edition {
    Bedrock,
    Education,
}
