//! ponytail: PHASE-0 STUB. A copy-only placeholder so the perf harness has a real binary
//! to measure. Phase 1 replaces it with the real build pipeline; do not extend it.
//!
//! `beacon build --project <dir> --out <dir>` copies `<project>/src` to `<out>`.
//! `beacon watch ...` builds, prints `ready`, then copies changed files.

use notify::{EventKind, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::{env, fs, io, process, sync::mpsc};

fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(&p, files)?;
        } else {
            files.push(p);
        }
    }
    Ok(())
}

/// Copy `src` to `dst` unless dst has the same size and is not older.
fn copy_file(src: &Path, dst: &Path) -> io::Result<()> {
    let (s, d) = (fs::metadata(src)?, fs::metadata(dst));
    if let Ok(d) = d
        && d.len() == s.len()
        && d.modified()? >= s.modified()?
    {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(src, dst)?;
    Ok(())
}

fn build(src: &Path, out: &Path) -> io::Result<()> {
    let mut files = Vec::new();
    walk(src, &mut files)?;
    for f in files {
        copy_file(&f, &out.join(f.strip_prefix(src).unwrap()))?;
    }
    Ok(())
}

fn watch(src: &Path, out: &Path) -> notify::Result<()> {
    let src = &fs::canonicalize(src)?;
    build(src, out)?;
    let (tx, rx) = mpsc::channel();
    let mut w = notify::recommended_watcher(tx)?;
    w.watch(src, RecursiveMode::Recursive)?;
    println!("ready");
    for ev in rx {
        let ev = ev?;
        if matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_)) {
            for p in ev.paths.iter().filter(|p| p.is_file()) {
                // Retry once: the writer may still hold the file.
                let dst = out.join(p.strip_prefix(src).unwrap());
                if copy_file(p, &dst).is_err() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    if let Err(e) = copy_file(p, &dst) {
                        eprintln!("copy {}: {e}", p.display());
                    }
                }
            }
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
    };
    let cmd = args.first().map(String::as_str);
    if matches!(cmd, Some("--version" | "-V")) {
        println!("beacon {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let (Some(project), Some(out)) = (flag("--project"), flag("--out")) else {
        eprintln!("usage: beacon <build|watch> --project <dir> --out <dir> | --version");
        process::exit(2);
    };
    let src = project.join("src");
    let r = match cmd {
        Some("build") => build(&src, &out).map_err(Into::into),
        Some("watch") => watch(&src, &out),
        _ => {
            eprintln!("unknown command");
            process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
