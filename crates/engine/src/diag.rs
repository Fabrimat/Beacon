//! Contract: diagnostics (6.3). Every diagnostic has a stable [`Code`]; workstreams add
//! variants here (with a doc string, which `beacon explain` renders), never ad-hoc strings.
//! SARIF 2.1 is the machine format; the compact text view is the default.

use crate::path::RelPath;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Note,
}

/// 1-based line/column; `None` when only the file is known.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub path: RelPath,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// A text edit shaped like an LSP `TextEdit` (6.3 fix-its).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Fix {
    pub description: String,
    pub path: RelPath,
    /// Byte range in the file; `start == end` inserts.
    pub start: usize,
    pub end: usize,
    pub replacement: String,
    pub safe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    pub message: String,
    pub span: Option<Span>,
    /// Other locations involved (e.g. the other file of a collision).
    pub related: Vec<Span>,
    pub fixes: Vec<Fix>,
}

impl Diagnostic {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: code.default_severity(),
            code,
            message: message.into(),
            span: None,
            related: Vec::new(),
            fixes: Vec::new(),
        }
    }

    pub fn at(mut self, path: RelPath, line: Option<u32>, column: Option<u32>) -> Self {
        self.span = Some(Span { path, line, column });
        self
    }

    pub fn related(mut self, path: RelPath) -> Self {
        self.related.push(Span {
            path,
            line: None,
            column: None,
        });
        self
    }
}

macro_rules! codes {
    ($( $(#[doc = $doc:literal])+ $name:ident = $id:literal, $sev:ident; )+) => {
        /// Stable diagnostic codes. The id string (`BCN0001`) never changes meaning.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Code { $( $(#[doc = $doc])+ $name, )+ }

        impl Code {
            pub const ALL: &[Code] = &[$(Code::$name),+];
            pub fn id(self) -> &'static str { match self { $(Code::$name => $id),+ } }
            pub fn default_severity(self) -> Severity { match self { $(Code::$name => Severity::$sev),+ } }
            /// Long explanation for `beacon explain`.
            pub fn doc(self) -> &'static str { match self { $(Code::$name => concat!($($doc, "\n"),+)),+ } }
            pub fn from_id(id: &str) -> Option<Code> {
                Self::ALL.iter().copied().find(|c| c.id().eq_ignore_ascii_case(id))
            }
        }
    };
}

impl serde::Serialize for Code {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.id())
    }
}

impl<'de> serde::Deserialize<'de> for Code {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let id = String::deserialize(d)?;
        Code::from_id(&id).ok_or_else(|| serde::de::Error::custom(format!("unknown code {id}")))
    }
}

codes! {
    /// `config.json` is missing, unreadable, or not valid JSON.
    ConfigInvalid = "BCN0001", Error;
    /// A `config.json` field has the wrong type or an invalid value (e.g. namespace `minecraft`).
    ConfigField = "BCN0002", Error;
    /// `module.json` is not valid JSON or does not match its schema.
    ModuleJsonInvalid = "BCN0003", Error;
    /// A file in a module matches no convention rule and no `module.json` path.
    /// Fix: rename it with a known suffix, or map it in `module.json` `paths`.
    UnknownSuffix = "BCN0010", Warning;
    /// Two items route to the same destination and neither declares a merge policy.
    /// Fix: rename one, or set `onConflict` in `module.json`.
    RouteCollision = "BCN0011", Error;
    /// Two destinations differ only by letter case; they collide on Windows and macOS.
    CaseFoldCollision = "BCN0012", Error;
    /// A path is not portable: reserved device name, trailing dot/space, `:` or a character
    /// Windows rejects.
    NonPortablePath = "BCN0013", Error;
    /// A module is not covered by any output's `include` globs.
    ModuleWithoutOutput = "BCN0014", Error;
    /// A target's `world` folder (or a `worlds` glob match) does not exist or is not a world
    /// (no `level.dat`).
    WorldNotFound = "BCN0015", Error;
    /// A static output's `bp`/`rp` folder is missing, outside the project, or has no valid
    /// `manifest.json`.
    StaticPackInvalid = "BCN0016", Error;
    /// Two packs (generated or static) share a header UUID; the game loads only one of them.
    DuplicatePackUuid = "BCN0017", Error;
    /// The same `.lang` key is defined twice across modules.
    LangDuplicateKey = "BCN0020", Error;
    /// JSON deep merge found two different scalar values for the same key.
    MergeConflict = "BCN0021", Error;
    /// A file taking part in a merge could not be read, or is not valid JSON.
    MergeInputInvalid = "BCN0022", Error;
    /// A `module.json` `hooks` entry names a function file that does not exist in the module.
    /// Fix: use the module-relative path without extension (`functions/loop`).
    HookUnknownFunction = "BCN0024", Error;
    /// A symlink, junction, or other reparse point inside the project was skipped. User code
    /// never sees files outside the project root.
    ReparsePointSkipped = "BCN0030", Note;
    /// A world's `level.dat` could not be read or edited (bad header or NBT, or a `levelDat`
    /// override with an unknown path or the wrong type).
    LevelDatInvalid = "BCN0031", Error;
    /// `embedPacks: false`: the world references packs that the artifact will not contain.
    EmbedPacksMissing = "BCN0032", Warning;
    /// The `marketplace` target's `art` folder lacks expected store or marketing art.
    ArtMissing = "BCN0033", Warning;
    /// A generator threw, failed to compile, or returned something that is not content.
    GeneratorError = "BCN0040", Error;
    /// A generator exceeded its time budget.
    GeneratorTimeout = "BCN0041", Error;
    /// A generator exceeded its memory limit.
    GeneratorOutOfMemory = "BCN0042", Error;
    /// A generator import could not be resolved (only relative paths, JSON and `beacon:*`
    /// modules are allowed).
    ImportUnresolved = "BCN0043", Error;
    /// The project has code to run (generators, plugins) and is not trusted. Run with
    /// `--trust` or trust it from the app/editor. Restricted mode still validates.
    Untrusted = "BCN0050", Error;
    /// esbuild was not found. Set `BEACON_ESBUILD`, or install it (`npm i -D esbuild`).
    BundlerMissing = "BCN0060", Error;
    /// The game-script bundle failed.
    BundleError = "BCN0061", Error;
    /// esbuild was found outside the pinned download (PATH or `node_modules`); its version is
    /// not the one Beacon pins, so bundles may differ across machines.
    BundlerUnpinned = "BCN0062", Note;
    /// A pinned tool download failed (network, HTTP status, or disk).
    DownloadFailed = "BCN0063", Error;
    /// A downloaded file does not match its pinned blake3 hash; it was discarded.
    DownloadHashMismatch = "BCN0064", Error;
    /// A download is needed but was not confirmed. Pass `--yes`, or run on a TTY to be asked.
    DownloadNeedsConfirm = "BCN0065", Error;
    /// `beacon.lock.json` would change but `--frozen` was given.
    LockfileFrozen = "BCN0070", Error;
    /// `beacon.lock.json` is not valid.
    LockfileInvalid = "BCN0071", Error;
    /// A `manifest.json` in a `packs.*` folder was edited by hand; it is generated output.
    ManifestDrift = "BCN0072", Warning;
    /// A pack's content changed since its last release but its version did not
    /// (`versionPolicy: "manual"`). Fix: `beacon pack --set-version <x.y.z>`.
    PackVersionStale = "BCN0073", Error;
    /// A new pack version is not greater than the last released one; versions are monotonic.
    PackVersionNotMonotonic = "BCN0074", Error;
    /// A pack changed since its last release; the next `beacon pack` will bump its version.
    PackReleaseBehind = "BCN0075", Warning;
    /// A file could not be written (locked by the game, permissions, path too long).
    EmitFailed = "BCN0080", Error;
    /// The emit ledger is missing or corrupt; no stale files were deleted.
    LedgerMissing = "BCN0081", Warning;
    /// An output folder (or a folder between it and its base) is a symlink or junction, so
    /// writes would land wherever it points. Untrusted builds refuse to write through it
    /// (error); trusted builds write and warn (e.g. a pack folder junctioned into com.mojang).
    OutputIsLink = "BCN0082", Warning;
    /// Generated agent docs (`AGENTS.md`) are out of date with the project. Fix: regenerate
    /// them (`beacon fix`).
    AgentDocsStale = "BCN0090", Error;
    /// Internal error; please report it.
    Internal = "BCN0999", Error;
}

/// Stable process exit codes (6.2).
pub mod exit {
    pub const OK: i32 = 0;
    /// Diagnostics with severity error.
    pub const DIAGNOSTICS: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const UNTRUSTED: i32 = 3;
    pub const INTERNAL: i32 = 4;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_unique_and_documented() {
        let mut ids: Vec<_> = Code::ALL.iter().map(|c| c.id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), Code::ALL.len());
        assert!(Code::ALL.iter().all(|c| !c.doc().trim().is_empty()));
        assert_eq!(Code::from_id("bcn0011"), Some(Code::RouteCollision));
    }
}
