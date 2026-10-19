//! Contract (phase 4): git-sourced plugins and `beacon plugin update` (5.10).
//!
//! The only runnable plugin kind today is `legacyFilter` (14.3). Its `cwd` may now come from
//! git instead of the project: `{"legacyFilter": {..., "id": "<id>", "source": {"git":
//! "<https url>", "ref": "<tag, branch or full sha>"}}}`.
//!
//! - **Pins live in the lockfile** (`Lockfile::plugins`): full commit SHA + blake3 content
//!   hash of the fetched tree (sorted paths + per-file hashes, `.git` excluded). Builds use
//!   the pin only; they never resolve a ref, so a moving branch cannot change a build.
//!   A source without a pin is `PluginUnpinned`; a checkout not matching its hash is
//!   `PluginHashMismatch` and does not run.
//! - **`beacon plugin update [<id>...]`** resolves `ref` to a full SHA with the system `git`
//!   (`ls-remote`, then `init` + `fetch --depth 1 <sha>` + `checkout` in a scratch dir with
//!   hooks, templates and symlinks off, never a shell), moves the tree to
//!   `<data dir>/beacon/plugins/<id>/<sha>/` (no `.git`, files read-only), shows the diff
//!   (old/new SHA, files added / removed / changed) and writes the lock only after
//!   confirmation (`--yes`, or a TTY prompt). Branch names are accepted for `ref` only here,
//!   and are always recorded as the SHA they resolved to. Allowed in restricted mode like the
//!   esbuild download: it runs `git`, never project code. Builds never fetch: a pin whose
//!   checkout is missing is `PluginUnpinned` (`update` restores it when `ref` still resolves
//!   to the pinned commit).
//! - **Trust re-prompt**: the trust key (5.5) becomes `config.json` plugins **plus** the
//!   lock's plugin pins ([`trust_key`], [`project_trust_key`]). An update therefore
//!   invalidates the stored decision and the next run asks again. With no pins the key is
//!   exactly today's (the plugin array), so existing trust decisions stay valid.

use crate::config::{CONFIG_FILE, LOCK_FILE};
use crate::diag::{Code, Diagnostic};
use crate::lock::Lockfile;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One pinned plugin in `beacon.lock.json` `plugins.<id>`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PluginLock {
    /// Git URL as written in `config.json` (`https://`; `file://` for tests and local mirrors).
    pub git: String,
    /// Full 40-hex (SHA-1) or 64-hex (SHA-256) commit id.
    pub commit: String,
    /// blake3 of the fetched tree, hex.
    pub hash: String,
}

/// What an update would change for one plugin (shown before writing; `--json` output).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdate {
    pub id: String,
    pub from: Option<PluginLock>,
    pub to: PluginLock,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

/// The value hashed into the trust key: the `config.json` plugin array, plus
/// `{"pins": lock.plugins}` appended only when there are pins.
pub fn trust_key(config_plugins: &[Value], lock: Option<&Lockfile>) -> Vec<Value> {
    let mut v = config_plugins.to_vec();
    if let Some(l) = lock.filter(|l| !l.plugins.is_empty()) {
        v.push(serde_json::json!({ "pins": l.plugins }));
    }
    v
}

fn config_plugins(root: &Path) -> Vec<Value> {
    let cfg: Value = std::fs::read(root.join(CONFIG_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    cfg["beacon"]["plugins"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn read_lock(root: &Path) -> Option<Lockfile> {
    serde_json::from_slice(&std::fs::read(root.join(LOCK_FILE)).ok()?).ok()
}

/// The trust key of a project on disk (`config.json` plugins + lockfile pins; a missing or
/// broken file just means "none"). What `trust::{is_trusted, trust}` take as `plugins`.
pub fn project_trust_key(root: &Path) -> Vec<Value> {
    trust_key(&config_plugins(root), read_lock(root).as_ref())
}

/// `<data dir>/beacon/plugins` (`BEACON_HOME` replaces `<data dir>/beacon`).
fn plugins_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("BEACON_HOME") {
        Some(h) => PathBuf::from(h),
        None => dirs::data_local_dir()?.join("beacon"),
    };
    Some(base.join("plugins"))
}

fn plain(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// `<data dir>/beacon/plugins/<id>/<commit>`; `None` when `id` / `commit` are not plain names.
pub fn checkout_dir(id: &str, commit: &str) -> Option<PathBuf> {
    if plain(id) && is_sha(commit) && !commit.bytes().any(|b| b.is_ascii_uppercase()) {
        Some(plugins_dir()?.join(id).join(commit))
    } else {
        None
    }
}

/// (combined hash, path -> file hash) of a checkout: sorted `/` paths with per-file blake3,
/// `.git` excluded. File modes are not hashed; links are refused.
pub fn tree_hash(dir: &Path) -> std::io::Result<(String, BTreeMap<String, String>)> {
    let mut files = BTreeMap::new();
    let mut todo = vec![(dir.to_owned(), String::new())];
    while let Some((d, rel)) = todo.pop() {
        for e in std::fs::read_dir(&d)? {
            let e = e?;
            let name = e
                .file_name()
                .into_string()
                .map_err(|n| std::io::Error::other(format!("{n:?}: not UTF-8")))?;
            if rel.is_empty() && name == ".git" {
                continue;
            }
            let rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let m = std::fs::symlink_metadata(e.path())?;
            if crate::legacy_filter::is_link(&m) {
                return Err(std::io::Error::other(format!(
                    "{rel}: link in a plugin tree"
                )));
            } else if m.is_dir() {
                todo.push((e.path(), rel));
            } else if m.is_file() {
                let mut h = blake3::Hasher::new();
                h.update_reader(std::fs::File::open(e.path())?)?;
                files.insert(rel, h.finalize().to_hex().to_string());
            }
        }
    }
    let mut all = blake3::Hasher::new();
    for (p, h) in &files {
        all.update(p.as_bytes());
        all.update(b"\0");
        all.update(h.as_bytes());
        all.update(b"\n");
    }
    Ok((all.finalize().to_hex().to_string(), files))
}

/// A build's view of a pinned plugin: the verified checkout dir, else `PluginUnpinned` /
/// `PluginHashMismatch`. Never touches the network.
pub(crate) fn verified_checkout(
    id: &str,
    git: &str,
    lock: Option<&Lockfile>,
) -> Result<PathBuf, (Code, String)> {
    let unpinned = |m: String| (Code::PluginUnpinned, m);
    let pin = lock
        .and_then(|l| l.plugins.get(id))
        .filter(|p| p.git == git)
        .ok_or_else(|| {
            unpinned(format!(
                "plugin `{id}` ({git}) has no pin in {LOCK_FILE}; run `beacon plugin update {id}`"
            ))
        })?;
    let dir = checkout_dir(id, &pin.commit)
        .filter(|d| d.is_dir())
        .ok_or_else(|| {
            unpinned(format!(
                "plugin `{id}` is pinned to {} but has no local checkout; run `beacon plugin update {id}`",
                pin.commit
            ))
        })?;
    match tree_hash(&dir) {
        Ok((h, _)) if h == pin.hash => Ok(dir),
        Ok((h, _)) => Err((
            Code::PluginHashMismatch,
            format!(
                "plugin `{id}`: checkout {} hashes to {h}, the pin says {}; it was not run",
                dir.display(),
                pin.hash
            ),
        )),
        Err(e) => Err((
            Code::PluginHashMismatch,
            format!(
                "plugin `{id}`: cannot hash {}: {e}; it was not run",
                dir.display()
            ),
        )),
    }
}

fn fail(m: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Code::PluginUpdateFailed, m.into())
}

const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// Runs `git` (argument array, no shell) with everything that could execute code switched
/// off: hooks (an empty hooks dir), fsmonitor, LFS smudge, prompts; callers pass
/// `--template=` to `init`.
fn git(hooks: &Path, cwd: Option<&Path>, args: &[&str]) -> Result<String, Diagnostic> {
    let mut c = Command::new("git");
    c.arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.eol=lf",
            "-c",
            "core.symlinks=false",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "protocol.ext.allow=never",
            "-c",
        ])
        .arg(format!("core.attributesFile={NULL_DEVICE}"))
        .args(args)
        .stdin(Stdio::null());
    // the user's git environment is untrusted input: drop every GIT_* variable we do not set
    // (GIT_DIR, GIT_CONFIG_*, GIT_EXEC_PATH, GIT_ASKPASS, ...); HOME, PATH etc. stay
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().to_ascii_uppercase().starts_with("GIT_") {
            c.env_remove(&k);
        }
    }
    c.env_remove("SSH_ASKPASS")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
        .env("GIT_ATTR_NOSYSTEM", "1");
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let o = c.output().map_err(|e| {
        fail(if e.kind() == std::io::ErrorKind::NotFound {
            "`git` was not found on PATH; install git to use `beacon plugin update`".to_owned()
        } else {
            format!("cannot run git: {e}")
        })
    })?;
    if !o.status.success() {
        return Err(fail(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&o.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn is_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `ref` -> full commit id: a full SHA as is, else `ls-remote` (tag peeled, then branch).
fn resolve(hooks: &Path, url: &str, r: &str) -> Result<String, Diagnostic> {
    if is_sha(r) {
        return Ok(r.to_ascii_lowercase());
    }
    let out = git(hooks, None, &["ls-remote", "--", url, r])?;
    let refs: BTreeMap<&str, &str> = out
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(sha, name)| (name, sha))
        .collect();
    [
        format!("refs/tags/{r}^{{}}"),
        format!("refs/tags/{r}"),
        format!("refs/heads/{r}"),
        r.to_owned(),
    ]
    .iter()
    .find_map(|n| refs.get(n.as_str()))
    .map(|s| (*s).to_owned())
    .ok_or_else(|| fail(format!("ref `{r}` not found in {url}")))
}

/// Removes the scratch dir when dropped.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        clear_readonly(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Sets (or clears) the read-only flag on every file under `dir`.
fn set_readonly(dir: &Path, ro: bool) {
    let mut todo = vec![dir.to_owned()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                todo.push(e.path());
            } else {
                let mut perm = m.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(ro);
                let _ = std::fs::set_permissions(e.path(), perm);
            }
        }
    }
}

fn clear_readonly(dir: &Path) {
    set_readonly(dir, false);
}

/// Fetches `commit` fresh and moves the tree to its checkout dir (replacing a stale or
/// tampered one). Returns the tree hash and file hashes.
fn fetch_checkout(
    id: &str,
    url: &str,
    commit: &str,
) -> Result<(String, BTreeMap<String, String>), Diagnostic> {
    let io = |e: std::io::Error| fail(format!("plugin `{id}`: {e}"));
    let dest = checkout_dir(id, commit).ok_or_else(|| {
        fail(format!(
            "plugin id `{id}` must be a plain name (letters, digits, `.-_`)"
        ))
    })?;
    let root = plugins_dir().ok_or_else(|| fail("no user data directory"))?;
    std::fs::create_dir_all(&root).map_err(io)?;
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(|e| io(std::io::Error::other(e)))?;
    let top = Scratch(root.join(format!(".tmp-{:x}", u64::from_le_bytes(rnd))));
    let (hooks, work) = (top.0.join("nohooks"), top.0.join("w"));
    std::fs::create_dir_all(&hooks).map_err(io)?;
    std::fs::create_dir_all(&work).map_err(io)?;
    git(&hooks, Some(&work), &["init", "-q", "--template="])?;
    git(
        &hooks,
        Some(&work),
        &[
            "fetch",
            "-q",
            "--depth",
            "1",
            "--no-tags",
            "--",
            url,
            commit,
        ],
    )?;
    git(
        &hooks,
        Some(&work),
        &["checkout", "-q", "--detach", "FETCH_HEAD"],
    )?;
    let head = git(&hooks, Some(&work), &["rev-parse", "HEAD"])?;
    if head.trim() != commit {
        return Err(fail(format!(
            "fetched {} but expected {commit}",
            head.trim()
        )));
    }
    clear_readonly(&work.join(".git")); // pack files are read-only on Windows
    std::fs::remove_dir_all(work.join(".git")).map_err(io)?;
    let r = tree_hash(&work).map_err(io)?;
    if dest.exists() {
        clear_readonly(&dest);
        std::fs::remove_dir_all(&dest).map_err(io)?;
    }
    std::fs::create_dir_all(dest.parent().expect("has parent")).map_err(io)?;
    std::fs::rename(&work, &dest).map_err(io)?;
    set_readonly(&dest, true);
    Ok(r)
}

/// (id, git, ref) of every git-sourced `legacyFilter` in `config.json`.
pub(crate) fn sources(root: &Path) -> Vec<(String, String, String)> {
    config_plugins(root)
        .iter()
        .filter_map(|p| {
            let d = p.get("legacyFilter")?;
            let s = d.get("source")?;
            let id = d.get("id")?.as_str()?;
            Some((
                id.to_owned(),
                s.get("git")?.as_str()?.to_owned(),
                s.get("ref")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// Fetches and diffs; writes no lockfile (the new checkout is placed, unreferenced until
/// [`apply_update`]). `ids` empty = every git-sourced plugin. `to` overrides `ref`.
pub fn plan_update(
    root: &Path,
    ids: &[String],
    to: Option<&str>,
) -> Result<Vec<PluginUpdate>, Diagnostic> {
    let all = sources(root);
    if let Some(i) = ids.iter().find(|i| !all.iter().any(|(id, ..)| id == *i)) {
        return Err(fail(format!(
            "no git-sourced plugin with id `{i}` in {CONFIG_FILE}"
        )));
    }
    let lock = read_lock(root);
    let mut out = Vec::new();
    for (id, url, r) in all
        .into_iter()
        .filter(|(id, ..)| ids.is_empty() || ids.contains(id))
    {
        if url.starts_with('-') || !(url.starts_with("https://") || url.starts_with("file://")) {
            return Err(fail(format!(
                "plugin `{id}`: source.git must be an https:// URL (got {url:?})"
            )));
        }
        let r = to.unwrap_or(&r);
        if r.is_empty() || r.starts_with('-') {
            return Err(fail(format!("plugin `{id}`: bad ref {r:?}")));
        }
        // `ls-remote` needs a hooks dir too (no repo yet): an empty temp dir
        let mut rnd = [0u8; 8];
        getrandom::fill(&mut rnd).map_err(|e| fail(e.to_string()))?;
        let probe =
            Scratch(std::env::temp_dir().join(format!("beacon-plg-{:x}", u64::from_le_bytes(rnd))));
        std::fs::create_dir_all(&probe.0).map_err(|e| fail(e.to_string()))?;
        let commit = resolve(&probe.0, &url, r)?;
        let from = lock.as_ref().and_then(|l| l.plugins.get(&id)).cloned();
        let old = from
            .as_ref()
            .and_then(|f| checkout_dir(&id, &f.commit))
            .and_then(|d| tree_hash(&d).ok())
            .map(|t| t.1)
            .unwrap_or_default();
        let (hash, new) = fetch_checkout(&id, &url, &commit)?;
        out.push(PluginUpdate {
            id,
            from,
            to: PluginLock {
                git: url,
                commit,
                hash,
            },
            added: new
                .keys()
                .filter(|k| !old.contains_key(*k))
                .cloned()
                .collect(),
            removed: old
                .keys()
                .filter(|k| !new.contains_key(*k))
                .cloned()
                .collect(),
            changed: new
                .iter()
                .filter(|(k, h)| old.get(*k).is_some_and(|o| o != *h))
                .map(|(k, _)| k.clone())
                .collect(),
        });
    }
    Ok(out)
}

/// Writes the new pins to the lockfile (atomically).
pub fn apply_update(root: &Path, updates: &[PluginUpdate]) -> Result<(), Diagnostic> {
    let bad = |e: &dyn std::fmt::Display| {
        Diagnostic::new(Code::LockfileInvalid, format!("{LOCK_FILE}: {e}"))
    };
    let mut lock = match std::fs::read(root.join(LOCK_FILE)) {
        Ok(b) => serde_json::from_slice::<Lockfile>(&b).map_err(|e| bad(&e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Lockfile::default(),
        Err(e) => return Err(bad(&e)),
    };
    for u in updates {
        lock.plugins.insert(u.id.clone(), u.to.clone());
    }
    lock.save(root)
        .map_err(|e| fail(format!("cannot write {LOCK_FILE}: {e}")))
}
