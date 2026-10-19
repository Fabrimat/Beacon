//! The watch loop's event side (9.3), shared by `beacon watch` and the daemon's watcher:
//! file events coalesced into one [`Batch`] per build.
//!
//! - Events are coalesced until 6 ms of quiet (editor atomic saves are bursts), no long
//!   debounce. Files whose size changed between their last event and the end of the burst are
//!   re-stat'ed (20 ms apart, at most 10 times) so half-written files are not read.
//! - A watcher error or `Rescan` flag (Windows `ReadDirectoryChangesW` buffer overflow) makes
//!   the batch a rescan (`Session::rescan`).
//! - Ignored: `.git`, the caller's output dirs (`.beacon`, `dist`, `packs.*`, `--out`), and
//!   the lockfile the build writes.
//! - If the native watcher cannot start (some network and synced folders), falls back to
//!   `notify::PollWatcher` every 500 ms and says so on stderr.

use beacon_engine::build::{Session, canon};
use beacon_engine::config::{CONFIG_FILE, LOCK_FILE};
use notify::event::ModifyKind;
use notify::{Config, EventKind, PollWatcher, RecursiveMode, Watcher};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

const QUIET: Duration = Duration::from_millis(6);
const POLL: Duration = Duration::from_millis(500);
const STABLE_WAIT: Duration = Duration::from_millis(20);
const STABLE_TRIES: usize = 10;

enum Msg {
    Fs(notify::Result<notify::Event>),
    Quit,
}

/// Changes seen since the last build: path -> size at the latest event.
#[derive(Debug, Default)]
pub struct Batch {
    pub paths: BTreeMap<PathBuf, Option<u64>>,
    pub rescan: bool,
}

impl Batch {
    /// Hands the batch to a session: the next build re-indexes only these paths, or all.
    pub fn apply(&self, s: &mut Session) {
        if self.rescan {
            s.rescan();
        } else {
            s.changed(self.paths.keys().cloned());
        }
    }

    /// `config.json` changed: pack folders (and so the ignore list) may have moved.
    pub fn touches_config(&self, root: &Path) -> bool {
        self.paths.contains_key(&root.join(CONFIG_FILE))
    }
}

/// A running recursive watch of a project root.
pub struct Watch {
    root: PathBuf,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    _w: Box<dyn Watcher + Send>,
}

fn start(root: &Path, tx: &Sender<Msg>) -> notify::Result<Box<dyn Watcher + Send>> {
    let t = tx.clone();
    let native = notify::recommended_watcher(move |e| {
        let _ = t.send(Msg::Fs(e));
    })
    .and_then(|mut w| w.watch(root, RecursiveMode::Recursive).map(|_| w));
    match native {
        Ok(w) => Ok(Box::new(w)),
        Err(e) => {
            eprintln!("note: native file watching failed ({e}); polling every 500 ms instead");
            let t = tx.clone();
            let mut w = PollWatcher::new(
                move |e| {
                    let _ = t.send(Msg::Fs(e));
                },
                Config::default().with_poll_interval(POLL),
            )?;
            w.watch(root, RecursiveMode::Recursive)?;
            Ok(Box::new(w))
        }
    }
}

/// `p` is `d` or below it (ASCII case-insensitive on Windows).
fn under(p: &Path, d: &Path) -> bool {
    let mut pc = p.components();
    d.components().all(|dc| {
        pc.next().is_some_and(|c| {
            c == dc || cfg!(windows) && c.as_os_str().eq_ignore_ascii_case(dc.as_os_str())
        })
    })
}

fn size(p: &Path) -> Option<u64> {
    std::fs::symlink_metadata(p).ok().map(|m| m.len())
}

impl Watch {
    /// Watches `root` (the session's canonical root) recursively.
    pub fn start(root: &Path) -> notify::Result<Watch> {
        let (tx, rx) = mpsc::channel();
        let w = start(root, &tx)?;
        Ok(Watch {
            root: root.to_path_buf(),
            tx,
            rx,
            _w: w,
        })
    }

    /// Makes [`Watch::next`] return `None` (between batches, never inside one): Ctrl-C.
    pub fn quitter(&self) -> impl Fn() + Send + 'static {
        let tx = self.tx.clone();
        move || {
            let _ = tx.send(Msg::Quit);
        }
    }

    /// Blocks for the next batch of relevant changes; `None` on quit or when the watcher is
    /// gone. `ignored`: absolute dirs whose changes do not count (`Session::output_dirs`).
    pub fn next(&self, ignored: &[PathBuf]) -> Option<Batch> {
        let root = &self.root;
        let git = root.join(".git");
        // the build itself (re)writes the lockfile (+ its temp file)
        let own_lock = |p: &Path| {
            p.parent() == Some(root.as_path())
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(LOCK_FILE))
        };
        let mut b = Batch::default();
        let mut next = self.rx.recv().ok();
        while let Some(m) = next {
            let ev = match m {
                Msg::Quit => return None,
                Msg::Fs(ev) => ev,
            };
            match ev {
                // e.g. an event buffer overflow
                Err(_) => b.rescan = true,
                Ok(ev) if matches!(ev.kind, EventKind::Access(_)) => {}
                Ok(ev) => {
                    b.rescan |= ev.need_rescan();
                    // a directory's own timestamp changes (reported lazily on NTFS) say nothing:
                    // its entries have their own events
                    let touch = matches!(ev.kind, EventKind::Modify(k) if !matches!(k, ModifyKind::Name(_)));
                    for p in ev.paths {
                        // same spelling as the session root (symlinked / 8.3 / case variants)
                        let p = if under(&p, root) { p } else { canon(&p) };
                        if touch && std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
                            continue;
                        }
                        // the parent of an ignored dir (`packs/`) only changes with its outputs
                        let skip = |i: &PathBuf| under(&p, i) || (&p != root && under(i, &p));
                        if !ignored.iter().chain([&git]).any(skip) && !own_lock(&p) {
                            b.paths.insert(p.clone(), size(&p)); // latest sight: an in-place write is truncate then fill
                        }
                    }
                }
            }
            // keep collecting until QUIET of silence, but only once something relevant came
            next = if b.rescan || !b.paths.is_empty() {
                match self.rx.recv_timeout(QUIET) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return None,
                }
            } else {
                self.rx.recv().ok()
            };
        }
        if b.paths.is_empty() && !b.rescan {
            return None; // watcher gone
        }
        for _ in 0..STABLE_TRIES {
            let mut moved = false;
            for (p, old) in &mut b.paths {
                let now = size(p);
                moved |= now != *old;
                *old = now;
            }
            if !moved {
                break;
            }
            std::thread::sleep(STABLE_WAIT);
        }
        Some(b)
    }
}
