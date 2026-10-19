//! Generated agent context (8.3): the `AGENTS.md` region between [`START`] and [`END`] is
//! rebuilt from the project; anything outside the markers is never touched. A stale or
//! hand-edited region is `BCN0090` with a safe fix that replaces exactly the region.

use crate::config::ProjectConfig;
use crate::diag::{Code, Diagnostic, Fix};
use crate::emit::link_below;
use crate::model::Edition;
use crate::path::RelPath;
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE: &str = "AGENTS.md";
pub const START: &str = "<!-- beacon:generated:start -->";
pub const END: &str = "<!-- beacon:generated:end -->";

/// The generated region, markers included, `\n` line endings, no trailing newline.
/// Deterministic: a pure function of the config.
pub fn region(cfg: &ProjectConfig) -> String {
    let b = &cfg.beacon;
    let outputs = if b.outputs.is_empty() {
        "`main`".to_string()
    } else {
        b.outputs
            .iter()
            .map(|o| format!("`{}`", o.id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let targets = if b.targets.is_empty() {
        "none (packs projection under `packs/`)".to_string()
    } else {
        b.targets
            .iter()
            .map(|(id, t)| {
                let ed = match t.edition {
                    Edition::Bedrock => "bedrock",
                    Edition::Education => "education",
                };
                format!("`{id}` ({ed} {})", t.artifact.name())
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let sources = b.sources.join(", ");
    format!(
        "{START}\n\
## Beacon project `{name}`\n\
\n\
- Namespace: `{ns}`. Target version: `{ver}`. Sources: `{sources}`.\n\
- Outputs: {outputs}.\n\
- Targets: {targets}.\n\
\n\
### Commands\n\
\n\
- `beacon check`: validate the project (fast, runs no JS unless trusted). A task is done only when it passes.\n\
- `beacon build --target <target>`: build one target. `beacon pack`: release archives.\n\
- `beacon fix`: apply safe fixes, including regenerating this section.\n\
- `beacon explain <code>`: explain a diagnostic code. Add `--json` to any command for machine output.\n\
\n\
### Rules\n\
\n\
- Diagnostics (stable `BCN` codes, SARIF with `--json`) come only from `beacon check` and `beacon build`.\n\
- Never hand-edit `beacon.lock.json`, generated `manifest.json` files, or the text between the beacon:generated markers.\n\
- Content: one game concept per file under `{sources}`; names carry the kind (`ruby.item.json`).\n\
- Trust: generators, scripts and plugins run only in a trusted project (`--trust` for one run, `beacon trust` to remember). Never trust a project you have not reviewed.\n\
{END}",
        name = cfg.name,
        ns = cfg.namespace,
        ver = cfg.target_version,
    )
}

/// A fresh `AGENTS.md`: a title and the region.
pub fn initial(cfg: &ProjectConfig) -> String {
    format!("# {}\n\n{}\n", cfg.name, region(cfg))
}

enum Found {
    Missing,
    Broken,
    At(usize, usize),
}

fn find(text: &str) -> Found {
    let s: Vec<usize> = text.match_indices(START).map(|m| m.0).collect();
    let e: Vec<usize> = text.match_indices(END).map(|m| m.0).collect();
    match (s.as_slice(), e.as_slice()) {
        ([], []) => Found::Missing,
        ([a], [b]) if a < b => Found::At(*a, b + END.len()),
        _ => Found::Broken,
    }
}

/// `BCN0090` when `AGENTS.md` exists, has a region and it differs from [`region`]. A missing
/// file, a file without markers (hand-written), or an unreadable file is not an error.
pub fn check(root: &Path, cfg: &ProjectConfig) -> Vec<Diagnostic> {
    let Ok(text) = std::fs::read_to_string(root.join(FILE)) else {
        return Vec::new();
    };
    let path = RelPath::parse(FILE).expect("const");
    let (a, b) = match find(&text) {
        Found::Missing => return Vec::new(),
        Found::Broken => {
            // no safe edit: we cannot tell which text is ours
            return vec![
                Diagnostic::new(
                    Code::AgentDocsStale,
                    format!("{FILE}: the beacon:generated markers are unbalanced or duplicated; fix them by hand"),
                )
                .at(path, None, None),
            ];
        }
        Found::At(a, b) => (a, b),
    };
    let want = region(cfg);
    if text[a..b].replace("\r\n", "\n") == want {
        return Vec::new();
    }
    let line = text[..a].matches('\n').count() as u32 + 1;
    let replacement = if text.contains("\r\n") {
        want.replace('\n', "\r\n")
    } else {
        want
    };
    let mut d = Diagnostic::new(
        Code::AgentDocsStale,
        format!("{FILE}: the generated section is out of date or was edited; run `beacon fix`"),
    )
    .at(path.clone(), Some(line), Some(1));
    d.fixes.push(Fix {
        description: format!("regenerate the {FILE} section"),
        path,
        start: a,
        end: b,
        replacement,
        safe: true,
    });
    vec![d]
}

/// Applies every safe fix of `diags` to its file and returns how many were applied. Each
/// file is re-read, edited in memory and replaced atomically (temp file + rename). A file
/// that is, or sits behind, a symlink or junction is refused (`Err`) and nothing is written
/// to it; offsets that no longer fit the file's text are refused too.
pub fn apply_safe_fixes(root: &Path, diags: &[Diagnostic]) -> Result<usize, String> {
    let mut by_file: BTreeMap<&str, Vec<&Fix>> = BTreeMap::new();
    for f in diags.iter().flat_map(|d| &d.fixes).filter(|f| f.safe) {
        by_file.entry(f.path.as_str()).or_default().push(f);
    }
    let mut n = 0;
    for (rel, mut fixes) in by_file {
        let path = root.join(rel);
        if let Some(l) = link_below(root, &path) {
            return Err(format!(
                "refusing to write {rel}: {} is a symlink or junction",
                l.display()
            ));
        }
        let mut text = std::fs::read_to_string(&path).map_err(|e| format!("{rel}: {e}"))?;
        fixes.sort_by_key(|f| std::cmp::Reverse(f.start));
        let mut limit = text.len();
        for f in &fixes {
            if f.start > f.end
                || f.end > limit
                || !text.is_char_boundary(f.start)
                || !text.is_char_boundary(f.end)
                || !text[f.start..].starts_with(START)
                || !text[..f.end].ends_with(END)
            {
                return Err(format!(
                    "{rel}: fix range {}..{} does not fit the file; run `beacon check` again",
                    f.start, f.end
                ));
            }
            text.replace_range(f.start..f.end, &f.replacement);
            limit = f.start;
            n += 1;
        }
        let mut rnd = [0u8; 8];
        getrandom::fill(&mut rnd).map_err(|e| e.to_string())?;
        let tmp = root.join(format!("{rel}.{:x}.tmp", u64::from_le_bytes(rnd)));
        std::fs::write(&tmp, text).map_err(|e| format!("{rel}: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{rel}: {e}")
        })?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_with_offsets_from_other_content_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let stale = format!(
            "intro

{START}
old
{END}
"
        );
        let (a, b) = (
            stale.find(START).unwrap(),
            stale.find(END).unwrap() + END.len(),
        );
        let mut diag = Diagnostic::new(Code::AgentDocsStale, "x");
        diag.fixes.push(Fix {
            description: String::new(),
            path: RelPath::parse(FILE).unwrap(),
            start: a,
            end: b,
            replacement: "new".into(),
            safe: true,
        });
        // the file changed since the offsets were computed
        let now = format!(
            "a longer intro line

{START}
old
{END}
"
        );
        std::fs::write(d.path().join(FILE), &now).unwrap();
        assert!(apply_safe_fixes(d.path(), std::slice::from_ref(&diag)).is_err());
        assert_eq!(std::fs::read_to_string(d.path().join(FILE)).unwrap(), now);
        // same content: applies
        std::fs::write(d.path().join(FILE), &stale).unwrap();
        assert_eq!(apply_safe_fixes(d.path(), &[diag]), Ok(1));
        assert_eq!(
            std::fs::read_to_string(d.path().join(FILE)).unwrap(),
            "intro

new
"
        );
    }
}
