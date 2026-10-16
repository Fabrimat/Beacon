//! Contract: diagnostics (6.3). Every diagnostic has a stable [`Code`]; workstreams add
//! variants here (with a doc string, which `beacon explain` renders), never ad-hoc strings.
//! SARIF 2.1 is the machine format; the compact text view is the default.

use crate::path::RelPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Note,
}

/// 1-based line/column; `None` when only the file is known.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Span {
    pub path: RelPath,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// A text edit shaped like an LSP `TextEdit` (6.3 fix-its).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Fix {
    pub description: String,
    pub path: RelPath,
    /// Byte range in the file; `start == end` inserts.
    pub start: usize,
    pub end: usize,
    pub replacement: String,
    pub safe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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
    /// The same `.lang` key is defined twice across modules.
    LangDuplicateKey = "BCN0020", Error;
    /// JSON deep merge found two different scalar values for the same key.
    MergeConflict = "BCN0021", Error;
    /// A symlink, junction, or other reparse point inside the project was skipped. User code
    /// never sees files outside the project root.
    ReparsePointSkipped = "BCN0030", Note;
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
    /// `beacon.lock.json` would change but `--frozen` was given.
    LockfileFrozen = "BCN0070", Error;
    /// `beacon.lock.json` is not valid.
    LockfileInvalid = "BCN0071", Error;
    /// A `manifest.json` in a `packs.*` folder was edited by hand; it is generated output.
    ManifestDrift = "BCN0072", Warning;
    /// A file could not be written (locked by the game, permissions, path too long).
    EmitFailed = "BCN0080", Error;
    /// The emit ledger is missing or corrupt; no stale files were deleted.
    LedgerMissing = "BCN0081", Warning;
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
