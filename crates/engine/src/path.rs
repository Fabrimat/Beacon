//! Contract: project-relative paths and the VFS read interface. This is the sandbox boundary
//! (ARCHITECTURE 5.4): user code (generators, plugins, the module loader) reaches files only
//! through [`Vfs`], and a [`Vfs`] only serves paths that are in the project index. Anything not
//! indexed (outside the root, behind a symlink/junction, excluded dirs) does not exist to it.

use std::fmt;
use unicode_normalization::{UnicodeNormalization, is_nfc};

/// A normalized project-relative path: `/`-separated, NFC, no empty, `.` or `..` segments,
/// no drive letters, UNC, ADS (`:`), backslashes, reserved device names, or trailing dot/space.
/// Byte order of the inner string is the canonical sort order (5.2).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadPath(pub String);

const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

impl RelPath {
    /// Parses a path that must already be relative and `/`-separated. Non-NFC input is
    /// normalized (macOS hands out NFD names); everything else invalid is rejected.
    pub fn parse(s: &str) -> Result<Self, BadPath> {
        let bad = |why: &str| Err(BadPath(format!("{s:?}: {why}")));
        if s.is_empty() {
            return bad("empty");
        }
        let s: String = if is_nfc(s) {
            s.to_owned()
        } else {
            s.nfc().collect()
        };
        if s.contains('\\') {
            return bad("backslash");
        }
        if s.contains(':') {
            return bad("':' (drive letter or alternate data stream)");
        }
        if s.starts_with('/') {
            return bad("absolute");
        }
        for seg in s.split('/') {
            if seg.is_empty() || seg == "." || seg == ".." {
                return bad("empty, '.' or '..' segment");
            }
            if seg.ends_with('.') || seg.ends_with(' ') {
                return bad("trailing dot or space");
            }
            if seg.chars().any(|c| c.is_control() || "<>\"|?*".contains(c)) {
                return bad("character not allowed on Windows");
            }
            let stem = seg.split('.').next().unwrap_or(seg).to_ascii_lowercase();
            if RESERVED.contains(&stem.as_str()) {
                return bad("reserved device name");
            }
        }
        Ok(RelPath(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Joins a relative child (`a/b`), re-validating the result.
    pub fn join(&self, child: &str) -> Result<Self, BadPath> {
        RelPath::parse(&format!("{}/{}", self.0, child))
    }

    pub fn parent(&self) -> Option<RelPath> {
        self.0.rsplit_once('/').map(|(p, _)| RelPath(p.to_owned()))
    }

    pub fn file_name(&self) -> &str {
        self.0.rsplit_once('/').map_or(&self.0, |(_, n)| n)
    }

    /// Case-folded key for collision checks (two outputs differing only in case collide on
    /// Windows/macOS, so it is an error on every OS).
    pub fn fold_key(&self) -> String {
        self.0.to_lowercase()
    }
}

impl TryFrom<String> for RelPath {
    type Error = BadPath;
    fn try_from(s: String) -> Result<Self, BadPath> {
        RelPath::parse(&s)
    }
}

impl From<RelPath> for String {
    fn from(p: RelPath) -> String {
        p.0
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl fmt::Display for BadPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid path {}", self.0)
    }
}

/// blake3 of file bytes (or of a sorted name list for directories).
pub type Hash = [u8; 32];

/// Read-only view of the indexed project. Implementations must:
/// - serve only indexed paths (never touch the filesystem for an unindexed path);
/// - on read, open without following reparse points and re-check (size, mtime, file id)
///   against the index; a mismatch means the file changed (re-index, return the new state),
///   a reparse point is `NotFound`.
///
/// Errors: `ErrorKind::NotFound` means "not in the index" (a negative lookup for the cache);
/// any other error (e.g. a sharing violation while an editor writes) is a real failure the
/// caller reports, and must never be cached as a negative.
pub trait Vfs {
    fn exists(&self, p: &RelPath) -> bool;
    /// Full bytes of an indexed file.
    fn read(&self, p: &RelPath) -> std::io::Result<Vec<u8>>;
    /// Content hash of an indexed file (from the index, no read).
    fn hash(&self, p: &RelPath) -> std::io::Result<Hash>;
    /// Sorted names of the direct children of an indexed directory (files and dirs);
    /// empty if absent. `None` = root.
    fn list(&self, dir: Option<&RelPath>) -> Vec<String>;
    /// Streams an indexed file into `out` without loading it (binaries, 5.2).
    fn stream(&self, p: &RelPath, out: &mut dyn std::io::Write) -> std::io::Result<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rejects_escapes() {
        for s in [
            "",
            "..",
            "a/../b",
            "./a",
            "/etc",
            "C:/x",
            "C:x",
            "\\\\srv\\share",
            "a\\b",
            "a//b",
            "file.txt:stream",
            "con",
            "src/aux.json",
            "LPT1.txt",
            "a/b.",
            "a/b ",
            "a/b?",
        ] {
            assert!(RelPath::parse(s).is_err(), "accepted {s:?}");
        }
        assert_eq!(
            RelPath::parse("src/items/sword.item.json")
                .unwrap()
                .as_str(),
            "src/items/sword.item.json"
        );
        assert_eq!(
            RelPath::parse("console.json").unwrap().file_name(),
            "console.json"
        );
    }

    #[test]
    fn normalizes_nfd() {
        let nfd = "src/cafe\u{301}.png";
        assert_eq!(RelPath::parse(nfd).unwrap().as_str(), "src/caf\u{e9}.png");
    }

    proptest! {
        /// Whatever parses stays inside the root after the OS resolves it.
        #[test]
        fn parsed_paths_stay_inside_root(s in "\\PC{0,40}") {
            if let Ok(p) = RelPath::parse(&s) {
                let root = std::path::Path::new("/r00t");
                let joined = root.join(p.as_str());
                let mut depth = 0i32;
                for c in joined.components() {
                    match c {
                        std::path::Component::Normal(_) => depth += 1,
                        std::path::Component::ParentDir => depth -= 1,
                        std::path::Component::CurDir => {}
                        _ => {}
                    }
                }
                prop_assert!(joined.starts_with(root));
                prop_assert!(depth >= 1);
                prop_assert!(!p.as_str().split('/').any(|seg| seg == ".." || seg == "."));
            }
        }
    }
}
