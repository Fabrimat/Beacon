//! `beacon-bundler`: implements the engine's [`Bundler`] by spawning the esbuild CLI.
//! ponytail: one process per call; the upgrade path is the esbuild service protocol, only if
//! the TS watch budget (p95 < 300 ms) fails.

use beacon_engine::diag::{Code, Diagnostic};
use beacon_engine::host::{BundleRequest, BundleResult, Bundler};
use beacon_engine::path::RelPath;
pub use beacon_fetch::Confirm;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

mod download;
mod pins;

pub struct Esbuild {
    exe: PathBuf,
    version: Option<String>,
    /// `BundlerUnpinned` notes, delivered with the first bundle.
    notes: Mutex<Vec<Diagnostic>>,
}

enum Source {
    Env,
    NodeModules,
    Tools,
    Path,
}

impl Esbuild {
    /// `BEACON_ESBUILD`, then the native `node_modules/@esbuild/<platform>` binary (noted if its
    /// version is not the pin), then the pinned binary in the tools dir, then a download of the
    /// pin (only per `confirm`), then PATH (noted). Never a `.cmd`/`.ps1` shim. It executes the
    /// binary (`--version`): call it for trusted sessions only.
    pub fn find(root: &Path, confirm: Confirm) -> Result<Esbuild, Diagnostic> {
        Self::find_with(
            root,
            std::env::var_os("BEACON_ESBUILD"),
            beacon_fetch::default_dir(),
            confirm,
        )
    }

    fn find_with(
        root: &Path,
        env: Option<OsString>,
        tools: Option<PathBuf>,
        confirm: Confirm,
    ) -> Result<Esbuild, Diagnostic> {
        let missing = || Diagnostic::new(Code::BundlerMissing, Code::BundlerMissing.doc().trim());
        let (exe, src) = match env {
            Some(p) => (
                Some(PathBuf::from(p))
                    .filter(|p| p.is_file())
                    .ok_or_else(missing)?,
                Source::Env,
            ),
            None => Self::search(root, tools.as_deref(), confirm)?,
        };
        let raw = Command::new(&exe)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let unpinned = |what: &str| {
            Diagnostic::new(
                Code::BundlerUnpinned,
                format!(
                    "using esbuild {} from {what}, not the pinned {}",
                    raw.as_deref().unwrap_or("?"),
                    pins::VERSION
                ),
            )
        };
        let notes = match src {
            Source::NodeModules if raw.as_deref() != Some(pins::VERSION) => {
                vec![unpinned("node_modules")]
            }
            Source::Path => vec![unpinned("PATH")],
            _ => vec![],
        };
        Ok(Esbuild {
            exe,
            version: raw.map(|v| format!("esbuild {v}")),
            notes: Mutex::new(notes),
        })
    }

    fn search(
        root: &Path,
        tools: Option<&Path>,
        confirm: Confirm,
    ) -> Result<(PathBuf, Source), Diagnostic> {
        let platform = pins::platform();
        let nm = root.join("node_modules");
        let dir = nm.join("@esbuild").join(&platform);
        let native = if cfg!(windows) {
            dir.join("esbuild.exe")
        } else {
            dir.join("bin").join("esbuild")
        };
        if native.is_file() {
            return Ok((native, Source::NodeModules));
        }
        let shim = nm.join("esbuild/bin/esbuild");
        if is_native(&shim) {
            return Ok((shim, Source::NodeModules));
        }
        let Some(tools) = tools else {
            return Self::on_path().ok_or_else(|| {
                Diagnostic::new(Code::BundlerMissing, Code::BundlerMissing.doc().trim())
            });
        };
        if let Some(pinned) = download::verified(tools, &platform) {
            return Ok((pinned, Source::Tools));
        }
        match download::install(tools, &platform, confirm) {
            Ok(p) => Ok((p, Source::Tools)),
            // a bad hash is never papered over; any other failure falls back to PATH
            Err(e) if e.code == Code::DownloadHashMismatch => Err(e),
            Err(e) => Self::on_path().ok_or(e),
        }
    }

    fn on_path() -> Option<(PathBuf, Source)> {
        let name = if cfg!(windows) {
            "esbuild.exe"
        } else {
            "esbuild"
        };
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
            .map(|p| (p, Source::Path))
    }
}

/// An [`Esbuild`] located on first use (`find` can download and prompt, and is only needed
/// when the project has scripts). A failed lookup is reported as the bundle's diagnostic.
pub struct LazyEsbuild {
    root: PathBuf,
    confirm: Confirm,
    found: OnceLock<Result<Esbuild, Diagnostic>>,
}

impl LazyEsbuild {
    pub fn new(root: &Path, confirm: Confirm) -> Self {
        LazyEsbuild {
            root: root.to_path_buf(),
            confirm,
            found: OnceLock::new(),
        }
    }
}

impl Bundler for LazyEsbuild {
    fn bundle(&self, req: &BundleRequest) -> BundleResult {
        match self
            .found
            .get_or_init(|| Esbuild::find(&self.root, self.confirm))
        {
            Ok(e) => e.bundle(req),
            Err(d) => BundleResult {
                code: None,
                map: None,
                inputs: BTreeSet::new(),
                external_inputs: true,
                diagnostics: vec![d.clone()],
            },
        }
    }

    /// Known after the first bundle; never triggers a lookup (or a download).
    fn version(&self) -> Option<String> {
        self.found.get()?.as_ref().ok()?.version()
    }
}

/// The npm `esbuild/bin/esbuild` is a node script (`#!`) unless the postinstall swapped in the binary.
fn is_native(p: &Path) -> bool {
    let mut head = [0u8; 2];
    std::fs::File::open(p)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && head != *b"#!"
}

impl Bundler for Esbuild {
    fn bundle(&self, req: &BundleRequest) -> BundleResult {
        let mut r = self.run(req);
        let notes = std::mem::take(&mut *self.notes.lock().expect("notes"));
        r.diagnostics.splice(0..0, notes);
        r
    }

    fn version(&self) -> Option<String> {
        self.version.clone()
    }
}

impl Esbuild {
    fn run(&self, req: &BundleRequest) -> BundleResult {
        let mut res = BundleResult {
            code: None,
            map: None,
            inputs: BTreeSet::new(),
            // until the metafile proves otherwise: unknown inputs are never reused
            external_inputs: true,
            diagnostics: vec![],
        };
        let fail = |res: &mut BundleResult, msg: String| {
            res.diagnostics
                .push(Diagnostic::new(Code::BundleError, msg))
        };
        let defines = match define_args(req.define) {
            Ok(d) => d,
            Err(e) => {
                fail(&mut res, e);
                return res;
            }
        };
        let tmp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(e) => {
                fail(&mut res, format!("cannot create temp dir: {e}"));
                return res;
            }
        };
        let out = tmp.path().join("main.js");
        let meta = tmp.path().join("meta.json");
        let map = tmp.path().join("main.js.map");
        let mut cmd = Command::new(&self.exe);
        cmd.current_dir(req.root)
            .args([
                "--bundle",
                "--format=esm",
                "--platform=neutral",
                "--main-fields=module,main",
            ])
            .args([
                "--target=es2022",
                "--external:@minecraft/*",
                "--sourcefile=beacon-entry.js",
            ])
            .args(["--log-level=warning", "--color=false"])
            .args(defines)
            .arg(format!("--metafile={}", meta.display()))
            .arg(format!("--outfile={}", out.display()));
        if req.sourcemap {
            cmd.arg("--sourcemap=external");
        }
        if req.minify {
            cmd.arg("--minify");
        }
        let stdin: String = req
            .entries
            .iter()
            .map(|e| format!("import {};\n", serde_json::Value::from(format!("./{e}"))))
            .collect();
        let output = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                c.stdin.take().expect("piped").write_all(stdin.as_bytes())?; // dropped = closed
                c.wait_with_output()
            });
        let output = match output {
            Ok(o) => o,
            Err(e) => {
                fail(&mut res, format!("cannot run esbuild: {e}"));
                return res;
            }
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        res.diagnostics = parse_errors(&stderr);
        if !output.status.success() {
            if res.diagnostics.is_empty() {
                fail(&mut res, stderr.trim().to_string());
            }
            return res;
        }
        res.code = std::fs::read_to_string(&out).ok();
        if res.code.is_none() {
            fail(&mut res, "esbuild produced no output".into());
        }
        if req.sourcemap {
            res.map = std::fs::read_to_string(&map).ok();
        }
        if let Some(v) = std::fs::read_to_string(&meta)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            res.external_inputs = false;
            for k in v["inputs"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, _)| k)
            {
                if matches!(k.as_str(), "<stdin>" | "beacon-entry.js") {
                    continue;
                }
                // `../x` (outside the root) or an absolute / unparsable path: not trackable
                match RelPath::parse(k).ok().filter(|_| !k.starts_with("../")) {
                    Some(p) => {
                        res.inputs.insert(p);
                    }
                    None => res.external_inputs = true,
                }
            }
        }
        res
    }
}

/// The target's `define` as compile-time constants: `--define:beacon.define.<K>=<json>`.
/// Keys must be plain JS identifiers (they become part of the esbuild flag).
fn define_args(define: &BTreeMap<String, Value>) -> Result<Vec<String>, String> {
    define
        .iter()
        .map(|(k, v)| {
            let ident = k
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
                && k.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
            if ident {
                Ok(format!("--define:beacon.define.{k}={v}"))
            } else {
                Err(format!("define key {k:?} is not a JavaScript identifier"))
            }
        })
        .collect()
}

/// Parses `✘ [ERROR] msg` / `X [ERROR] msg` blocks plus the `    path:line:col:` line after.
/// The caller falls back to the whole stderr when nothing parses.
fn parse_errors(stderr: &str) -> Vec<Diagnostic> {
    let mut out = vec![];
    let mut lines = stderr.lines().peekable();
    while let Some(l) = lines.next() {
        let Some(msg) = l
            .strip_prefix("✘ [ERROR] ")
            .or_else(|| l.strip_prefix("X [ERROR] "))
        else {
            continue;
        };
        let mut d = Diagnostic::new(Code::BundleError, msg.trim());
        // the location is the first indented `...:` line before the next block
        while let Some(n) = lines.peek() {
            if n.starts_with('✘') || n.starts_with("X [") || n.starts_with('▲') {
                break;
            }
            let n = lines.next().expect("peeked");
            if let Some(loc) = n.strip_prefix("    ").and_then(|s| s.strip_suffix(':')) {
                let mut p = loc.rsplitn(3, ':');
                if let (Some(c), Some(ln), Some(file)) = (p.next(), p.next(), p.next())
                    && let (Ok(c), Ok(ln)) = (c.parse::<u32>(), ln.parse::<u32>())
                    && let Ok(rel) = RelPath::parse(&file.replace('\\', "/"))
                {
                    // esbuild columns are 0-based
                    d = d.at(rel, Some(ln), Some(c + 1));
                }
                break;
            }
        }
        out.push(d);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_env_is_bundler_missing() {
        let e = Esbuild::find_with(
            Path::new("."),
            Some("definitely/not/here".into()),
            None,
            Confirm::No,
        )
        .err()
        .expect("err");
        assert_eq!(e.code, Code::BundlerMissing);
    }

    #[test]
    fn define_flags() {
        let d: BTreeMap<String, Value> =
            serde_json::from_str(r#"{"BETA_MODE":true,"name":"a \"q\"","n":1.5,"o":{"x":[1]}}"#)
                .unwrap();
        assert_eq!(
            define_args(&d).unwrap(),
            [
                "--define:beacon.define.BETA_MODE=true",
                r#"--define:beacon.define.n=1.5"#,
                r#"--define:beacon.define.name="a \"q\"""#,
                r#"--define:beacon.define.o={"x":[1]}"#,
            ]
        );
        for bad in ["a.b", "1x", "a=b", "", "a-b"] {
            let d = BTreeMap::from([(bad.to_string(), Value::Bool(true))]);
            assert!(define_args(&d).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_real_samples() {
        let s = "✘ [ERROR] Expected \";\" but found \"x\"\n\n    src/a.js:1:6:\n      1 │ let x x\n        ╵       ^\n\n";
        let d = parse_errors(s);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "Expected \";\" but found \"x\"");
        let sp = d[0].span.as_ref().expect("span");
        assert_eq!(
            (sp.path.as_str(), sp.line, sp.column),
            ("src/a.js", Some(1), Some(7))
        );
        let s = "X [ERROR] Could not resolve \"./nope\"\n\n    beacon-entry.js:1:7:\n      1 │ import \"./nope\";\n        ╵        ~~~~~~~\n\n";
        assert_eq!(parse_errors(s).len(), 1);
        assert!(parse_errors("boom").is_empty());
    }
}
