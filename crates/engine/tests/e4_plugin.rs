//! Phase 4 `beacon plugin update`: git-sourced plugin pins, the trust key, and git safety.
//! Uses the system `git` against a local bare repo (`file://`); skips without git.

mod b1_common;
use b1_common::*;
use beacon_engine::diag::Code;
use beacon_engine::plugin::{
    PluginUpdate, apply_update, checkout_dir, plan_update, project_trust_key,
};
use beacon_engine::trust;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, OnceLock};

const PY: &str = if cfg!(windows) { "python" } else { "python3" };

struct Env {
    _home: tempfile::TempDir,
    /// Written by the hostile `post-checkout` template hook.
    marker: PathBuf,
    /// Written by the hostile global-config `filter.evil.smudge` driver.
    smudge_marker: PathBuf,
}

/// One `BEACON_HOME` for the whole binary (set before any test touches the environment), and
/// a hostile `GIT_TEMPLATE_DIR` whose `post-checkout` hook creates `marker`.
fn env() -> &'static Env {
    static E: OnceLock<Env> = OnceLock::new();
    E.get_or_init(|| {
        let home = tempfile::tempdir().unwrap();
        let tpl = home.path().join("tpl");
        let marker = home.path().join("hook-ran");
        write(
            &tpl,
            "hooks/post-checkout",
            &format!(
                "#!/bin/sh\ntouch \"{}\"\n",
                marker.to_string_lossy().replace('\\', "/")
            ),
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let h = tpl.join("hooks/post-checkout");
            std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let smudge_marker = home.path().join("smudge-ran");
        let gc = home.path().join("hostile.gitconfig");
        std::fs::write(
            &gc,
            format!(
                "[filter \"evil\"]
	smudge = touch \"{}\" && cat
",
                smudge_marker.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
        // SAFETY: runs once, first thing in every test, before any test spawns a process.
        unsafe {
            std::env::set_var("BEACON_HOME", home.path().join("beacon"));
            std::env::set_var("GIT_TEMPLATE_DIR", &tpl);
            std::env::set_var("GIT_CONFIG_GLOBAL", &gc);
        }
        Env {
            _home: home,
            marker,
            smudge_marker,
        }
    })
}

fn have_git() -> bool {
    let ok = Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("SKIP: git is not installed");
    }
    ok
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn url(p: &Path) -> String {
    let p = p.to_string_lossy().replace('\\', "/");
    format!("file://{}{p}", if p.starts_with('/') { "" } else { "/" })
}

/// A work repo with one commit on `main`, mirrored to a bare repo; returns (dir, work, bare url).
fn upstream() -> (tempfile::TempDir, PathBuf, String) {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    write(
        &work,
        "filter.py",
        "import pathlib\nimport helper\npathlib.Path('BP/functions/pinned.mcfunction').write_text(helper.TEXT)\n",
    );
    write(&work, "helper.py", "TEXT = 'say pinned'\n");
    write(&work, ".gitattributes", "* text=auto\n");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "one"]);
    let bare = d.path().join("bare.git");
    git(d.path(), &["clone", "-q", "--bare", "work", "bare.git"]);
    git(&work, &["remote", "add", "origin", &url(&bare)]);
    (d, work, url(&bare))
}

fn config(id: &str, git: &str) -> String {
    json!({"type":"minecraftBedrock","name":"p","targetVersion":"1.21.100","namespace":"demo",
        "beacon":{"plugins":[{"legacyFilter":{"id":id,"run":[PY,"filter.py"],
        "source":{"git":git,"ref":"main"}}}]}})
    .to_string()
}

fn proj(id: &str, git: &str) -> tempfile::TempDir {
    project(&[
        ("config.json", &config(id, git)),
        ("src/m/functions/f.mcfunction", "say hi\n"),
    ])
}

fn build(p: &Path) -> beacon_engine::build::BuildOutcome {
    session(opts(p, None), p, &Arc::new(AtomicUsize::new(0))).build()
}

fn update(p: &Path, id: &str) -> Vec<PluginUpdate> {
    let u = plan_update(p, &[id.to_owned()], None).unwrap();
    apply_update(p, &u).unwrap();
    u
}

#[test]
fn unpinned_plugin_is_an_error() {
    env();
    if !have_git() {
        return;
    }
    let (_u, _w, bare) = upstream();
    let p = proj("unpinned", &bare);
    let r = build(p.path());
    assert!(has(&r, Code::PluginUnpinned), "{:?}", r.diagnostics);
    assert!(
        !p.path()
            .join("out/main/BP/functions/pinned.mcfunction")
            .exists()
    );
}

#[test]
fn tampered_checkout_is_a_hash_mismatch() {
    env();
    if !have_git() {
        return;
    }
    let (_u, _w, bare) = upstream();
    let p = proj("tampered", &bare);
    let u = update(p.path(), "tampered");
    assert_eq!(u[0].to.commit.len(), 40);
    let dir = checkout_dir("tampered", &u[0].to.commit).unwrap();
    let f = dir.join("filter.py");
    assert!(
        f.metadata().unwrap().permissions().readonly(),
        "checkout is read-only"
    );
    // `text=auto` must not make the tree OS-dependent (core.eol=lf)
    for n in [".gitattributes", "filter.py", "helper.py"] {
        let b = std::fs::read(dir.join(n)).unwrap();
        assert!(!b.contains(&b'\r'), "{n} has CR");
    }
    // pinned and intact: the filter runs (needs python, like d4_legacy_filter), and running
    // it twice never dirties the checkout (no __pycache__, so no hash mismatch)
    if Command::new(PY).arg("--version").output().is_ok() {
        for _ in 0..2 {
            let r = build(p.path());
            assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
            assert!(
                p.path()
                    .join("out/main/BP/functions/pinned.mcfunction")
                    .is_file()
            );
            assert!(!dir.join("__pycache__").exists());
        }
    }
    std::fs::remove_dir_all(p.path().join("out")).ok();
    let mut perm = f.metadata().unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&f, perm).unwrap();
    std::fs::write(&f, "open('evil', 'w')\n").unwrap();
    let r = build(p.path());
    assert!(has(&r, Code::PluginHashMismatch), "{:?}", r.diagnostics);
    assert!(
        !p.path()
            .join("out/main/BP/functions/pinned.mcfunction")
            .exists()
    );
    assert!(!dir.join("evil").exists(), "nothing ran");
}

#[test]
fn update_pins_and_changes_the_trust_key() {
    env();
    if !have_git() {
        return;
    }
    let (_u, work, bare) = upstream();
    let p = proj("pinned", &bare);
    let root = p.path();
    let before = project_trust_key(root);
    trust::trust(root, &before).unwrap();
    assert!(trust::is_trusted(root, &project_trust_key(root)));

    let u = update(root, "pinned");
    assert!(u[0].from.is_none());
    assert_eq!(u[0].added, [".gitattributes", "filter.py", "helper.py"]);
    let lock: Value =
        serde_json::from_slice(&std::fs::read(root.join("beacon.lock.json")).unwrap()).unwrap();
    assert_eq!(lock["plugins"]["pinned"]["commit"], u[0].to.commit.as_str());
    assert_eq!(lock["plugins"]["pinned"]["git"], bare.as_str());
    let after = project_trust_key(root);
    assert_ne!(before, after);
    assert!(
        !trust::is_trusted(root, &after),
        "an update asks for trust again"
    );

    // upstream moves: the diff names the changed and added files; the pin follows
    write(&work, "filter.py", "# v2\n");
    write(&work, "extra.txt", "x");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "two"]);
    git(&work, &["push", "-q", "origin", "main"]);
    let u2 = update(root, "pinned");
    assert_eq!(u2[0].from.as_ref(), Some(&u[0].to));
    assert_ne!(u2[0].to.commit, u[0].to.commit);
    assert_eq!(u2[0].changed, ["filter.py"]);
    assert_eq!(u2[0].added, ["extra.txt"]);
    assert!(u2[0].removed.is_empty());
    assert_ne!(project_trust_key(root), after);
}

#[test]
fn project_without_pins_keeps_its_trust() {
    env();
    let plugins = [json!({"legacyFilter": {"run": [PY, "f.py"], "cwd": "tools"}})];
    let cfg = json!({"type":"minecraftBedrock","name":"p","targetVersion":"1.21.100",
        "namespace":"demo","beacon":{"plugins":plugins}})
    .to_string();
    let p = project(&[("config.json", &cfg), ("tools/f.py", "")]);
    let root = p.path();
    // trust stored the way every earlier version did: hash of the raw plugin array
    trust::trust(root, &plugins).unwrap();
    assert_eq!(project_trust_key(root), plugins);
    assert!(trust::is_trusted(root, &project_trust_key(root)));
    // a lockfile with no pins changes nothing
    build(root);
    assert!(root.join("beacon.lock.json").is_file());
    assert_eq!(project_trust_key(root), plugins);
    assert!(trust::is_trusted(root, &project_trust_key(root)));
}

#[test]
fn hooks_in_the_cloned_repo_never_run() {
    let e = env();
    if !have_git() {
        return;
    }
    let (u, _w, bare) = upstream();
    // control: a plain clone with the hostile template does run its post-checkout hook
    git(u.path(), &["clone", "-q", &bare, "control"]);
    if !e.marker.exists() {
        eprintln!("SKIP: this git does not run the template hook, nothing to prove");
        return;
    }
    std::fs::remove_file(&e.marker).unwrap();
    let p = proj("hooks", &bare);
    update(p.path(), "hooks");
    assert!(
        !e.marker.exists(),
        "post-checkout hook ran during plugin update"
    );
}

#[test]
fn hostile_git_environment_is_ignored() {
    let e = env();
    if !have_git() {
        return;
    }
    let (u, work, bare) = upstream();
    write(
        &work,
        ".gitattributes",
        "* filter=evil
",
    );
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "attrs"]);
    git(&work, &["push", "-q", "origin", "main"]);
    // control: with the user's global config a plain clone runs the smudge driver
    git(u.path(), &["clone", "-q", &bare, "control"]);
    if !e.smudge_marker.exists() {
        eprintln!("SKIP: this git did not run the smudge driver, nothing to prove");
        return;
    }
    std::fs::remove_file(&e.smudge_marker).unwrap();
    let p = proj("envhostile", &bare);
    let u = update(p.path(), "envhostile");
    assert!(u[0].added.contains(&".gitattributes".to_owned()));
    assert!(!e.smudge_marker.exists(), "a user-config filter driver ran");
}
