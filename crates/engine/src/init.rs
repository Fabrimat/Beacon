//! `beacon init` (9.0): the question spec as data, answer validation, and the scaffold.
//! Frontends (CLI prompts, `--from init.json`, forms) only render the spec and collect
//! [`Answers`]; everything else happens here.

use crate::config::{Artifact, CONFIG_FILE, LOCK_FILE};
use crate::diag::{Code, Diagnostic};
use crate::lock::Lockfile;
use crate::model::Edition;
use crate::{agents, route, trust};
use serde_json::{Value, json};
use std::path::Path;

pub const SPEC_VERSION: u32 = 1;
pub const DEFAULT_TARGET_VERSION: &str = "1.26.50";
const ALLOWED_ARTIFACTS: [Artifact; 3] = [Artifact::Dev, Artifact::McPack, Artifact::McAddon];

/// The built-in starter; text files get `{{name}}` / `{{namespace}}` replaced.
// ponytail: list kept by hand, a build-time walk when there is a second template.
const MINIMAL: &[(&str, &[u8])] = &[
    (
        "package.json",
        include_bytes!("../../../templates/minimal/package.json"),
    ),
    (
        "pack_icon.png",
        include_bytes!("../../../templates/minimal/pack_icon.png"),
    ),
    (
        "src/main/scripts/main.ts",
        include_bytes!("../../../templates/minimal/src/main/scripts/main.ts"),
    ),
    (
        "src/main/functions/hello.mcfunction",
        include_bytes!("../../../templates/minimal/src/main/functions/hello.mcfunction"),
    ),
    (
        "src/main/texts/en_US.lang",
        include_bytes!("../../../templates/minimal/src/main/texts/en_US.lang"),
    ),
];

const GITIGNORE: &str =
    "# build outputs (beacon.lock.json is committed)\n/packs/\n/dist/\n/.beacon/\n/node_modules/\n";

/// Raw answers; `None` = not given (default applies, or the frontend asks).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Answers {
    pub folder: Option<String>,
    pub name: Option<String>,
    pub namespace: Option<String>,
    pub authors: Option<Vec<String>>,
    pub target_version: Option<String>,
    pub editions: Option<Vec<Edition>>,
    pub artifacts: Option<Vec<Artifact>>,
    pub template: Option<String>,
    pub git_init: Option<bool>,
    pub agent_files: Option<bool>,
}

/// Validated answers with defaults filled in.
#[derive(Debug, Clone)]
pub struct Plan {
    pub name: String,
    pub namespace: String,
    pub authors: Vec<String>,
    pub target_version: String,
    pub editions: Vec<Edition>,
    pub artifacts: Vec<Artifact>,
    pub git_init: bool,
    pub agent_files: bool,
}

/// What [`scaffold`] did.
#[derive(Debug)]
pub struct Created {
    /// Project-relative, sorted.
    pub files: Vec<String>,
    /// The trust decision was recorded (5.5: built-in template = trusted).
    pub trusted: bool,
}

/// The init steps as data (9.0), versioned. `default: null` with `defaultFrom` means derived.
pub fn questions() -> Value {
    json!({
        "schema": SPEC_VERSION,
        "questions": [
            { "field": "folder", "type": "path", "default": ".", "positional": true,
              "validation": "empty or nonexistent directory", "condition": null },
            { "field": "name", "type": "string", "default": null, "defaultFrom": "folder name",
              "validation": "non-empty", "condition": null },
            { "field": "namespace", "type": "string", "default": null, "defaultFrom": "name",
              "validation": "^[a-z0-9_]+$, not `minecraft`", "condition": null },
            { "field": "authors", "type": "string[]", "default": [],
              "validation": "non-empty entries", "condition": null },
            { "field": "targetVersion", "type": "string", "default": DEFAULT_TARGET_VERSION,
              "validation": "^[0-9]+(\\.[0-9]+){2,3}$", "condition": null },
            { "field": "editions", "type": "choice[]", "choices": ["bedrock", "education"],
              "default": ["bedrock"], "validation": "at least one", "condition": null },
            { "field": "artifacts", "type": "choice[]", "choices": ["dev", "mcpack", "mcaddon"],
              "default": ["dev", "mcaddon"],
              "validation": "at least one; one target per artifact and edition", "condition": null },
            { "field": "template", "type": "choice", "choices": ["minimal"], "default": "minimal",
              "validation": "git URLs are not supported yet", "condition": null },
            { "field": "gitInit", "type": "bool", "default": false,
              "validation": null, "condition": null },
            { "field": "agentFiles", "type": "bool", "default": true,
              "validation": null, "condition": null },
        ]
    })
}

/// Parses flag/prompt strings (`bedrock`, `mcaddon`) with the config's own serde names;
/// empty input is `None` (not answered).
pub fn parse_list<T: serde::de::DeserializeOwned>(v: &[String]) -> Result<Option<Vec<T>>, String> {
    if v.is_empty() {
        return Ok(None);
    }
    v.iter()
        .map(|s| {
            serde_json::from_value(Value::String(s.clone()))
                .map_err(|_| format!("unknown value {s:?}"))
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

fn bad(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Code::ConfigField, msg)
}

/// Lowercase `[a-z0-9_]` form of a name (`My Pack!` -> `my_pack`).
pub fn derive_namespace(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    s.trim_matches('_').to_string()
}

/// Validates and fills defaults. `folder_name` is the default for `name`.
pub fn resolve(a: Answers, folder_name: &str) -> Result<Plan, Vec<Diagnostic>> {
    let mut d = Vec::new();
    match a.template.as_deref() {
        None | Some("minimal") => {}
        Some(t) => d.push(bad(format!(
            "template {t:?} is not supported yet (only `minimal`; git URL templates come later)"
        ))),
    }
    let name = a.name.unwrap_or_else(|| folder_name.to_string());
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        d.push(bad(
            "name must be non-empty text without control characters",
        ));
    }
    let namespace = a.namespace.unwrap_or_else(|| derive_namespace(&name));
    if !route::ns_ok(&namespace) {
        d.push(bad(format!(
            "namespace {namespace:?} must be lowercase [a-z0-9_] and not `minecraft`"
        )));
    }
    let target_version = a
        .target_version
        .unwrap_or_else(|| DEFAULT_TARGET_VERSION.into());
    let parts: Vec<&str> = target_version.split('.').collect();
    if !(3..=4).contains(&parts.len())
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        d.push(bad(format!(
            "targetVersion {target_version:?} must look like 1.26.50"
        )));
    }
    let authors = a.authors.unwrap_or_default();
    if authors.iter().any(|x| x.trim().is_empty()) {
        d.push(bad("authors must not contain empty names"));
    }
    let mut editions = a.editions.unwrap_or_else(|| vec![Edition::Bedrock]);
    editions.sort();
    editions.dedup();
    let mut artifacts = a
        .artifacts
        .unwrap_or_else(|| vec![Artifact::Dev, Artifact::McAddon]);
    artifacts.sort();
    artifacts.dedup();
    if editions.is_empty() || artifacts.is_empty() {
        d.push(bad("editions and artifacts need at least one entry"));
    }
    for x in artifacts.iter().filter(|x| !ALLOWED_ARTIFACTS.contains(x)) {
        d.push(bad(format!(
            "artifact `{}` needs extra folders; add it to config.json by hand (init offers dev, mcpack, mcaddon)",
            x.name()
        )));
    }
    if !d.is_empty() {
        return Err(d);
    }
    Ok(Plan {
        name,
        namespace,
        authors,
        target_version,
        editions,
        artifacts,
        git_init: a.git_init.unwrap_or(false),
        agent_files: a.agent_files.unwrap_or(true),
    })
}

fn config_json(p: &Plan) -> String {
    let mut targets = serde_json::Map::new();
    for ed in &p.editions {
        for art in &p.artifacts {
            let id = match art {
                Artifact::Dev => "dev",
                Artifact::McPack => "pack",
                _ => "addon",
            };
            let (id, ed_name) = match ed {
                Edition::Bedrock => (id.to_string(), "bedrock"),
                Edition::Education => (format!("{id}-edu"), "education"),
            };
            targets.insert(id, json!({ "edition": ed_name, "artifact": art.name() }));
        }
    }
    let v = json!({
        "type": "minecraftBedrock",
        "name": p.name,
        "authors": p.authors,
        "targetVersion": p.target_version,
        "namespace": p.namespace,
        "packs": { "behaviorPack": "./packs/BP", "resourcePack": "./packs/RP" },
        "beacon": {
            "sources": ["./src"],
            "packMeta": {
                "name": p.name,
                "description": format!("{} (made with Beacon)", p.name),
                "icon": "./pack_icon.png",
            },
            "targets": targets,
            "scriptModules": { "@minecraft/server": "2.10.0" },
        },
    });
    serde_json::to_string_pretty(&v).expect("json") + "\n"
}

/// `.vscode/launch.json`: one attach entry per dev target, carrying only what Beacon needs to
/// resolve the rest (the VS Code extension, or `beacon debug-config`). `None` without a dev target.
fn launch_json(p: &Plan) -> Option<String> {
    if !p.artifacts.contains(&Artifact::Dev) {
        return None;
    }
    let configs: Vec<Value> = p
        .editions
        .iter()
        .map(|ed| match ed {
            Edition::Bedrock => "dev".to_string(),
            Edition::Education => "dev-edu".to_string(),
        })
        .map(|t| {
            json!({ "type": "minecraft-js", "request": "attach", "name": format!("Beacon: {t}"),
                    "beaconOutput": "main", "beaconTarget": t })
        })
        .collect();
    let v = json!({ "version": "0.2.0", "configurations": configs });
    Some(serde_json::to_string_pretty(&v).expect("json") + "\n")
}

/// Writes the project into `dir` (created if needed). Refuses a non-empty directory before
/// writing anything. Records trust last, so a half-created project is never trusted.
pub fn scaffold(dir: &Path, p: &Plan) -> Result<Created, Vec<Diagnostic>> {
    let io = |what: &str, e: std::io::Error| {
        vec![Diagnostic::new(Code::EmitFailed, format!("{what}: {e}"))]
    };
    match std::fs::read_dir(dir) {
        Ok(mut it) => {
            if it.next().is_some() {
                return Err(vec![bad(format!(
                    "{} is not empty; init only creates projects in empty folders",
                    dir.display()
                ))]);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io("cannot read the folder", e)),
    }
    std::fs::create_dir_all(dir).map_err(|e| io("cannot create the folder", e))?;
    let mut files: Vec<(String, Vec<u8>)> = vec![
        (CONFIG_FILE.into(), config_json(p).into_bytes()),
        (".gitignore".into(), GITIGNORE.into()),
    ];
    files.push((
        ".vscode/extensions.json".into(),
        (serde_json::to_string_pretty(
            &json!({ "recommendations": ["mojang-studios.minecraft-debugger"] }),
        )
        .expect("json")
            + "\n")
            .into_bytes(),
    ));
    if let Some(l) = launch_json(p) {
        files.push((".vscode/launch.json".into(), l.into_bytes()));
    }
    for (rel, bytes) in MINIMAL {
        let bytes = match std::str::from_utf8(bytes) {
            Ok(t) => t
                .replace("{{name}}", &p.name)
                .replace("{{namespace}}", &p.namespace)
                .into_bytes(),
            Err(_) => bytes.to_vec(),
        };
        files.push((rel.to_string(), bytes));
    }
    let write = |rel: &str, bytes: &[u8]| -> Result<(), Vec<Diagnostic>> {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(rel, e))?;
        }
        // create_new: never overwrite a file that appeared after the emptiness check
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| io(rel, e))?;
        std::io::Write::write_all(&mut f, bytes).map_err(|e| io(rel, e))
    };
    for (rel, bytes) in &files {
        write(rel, bytes)?;
    }
    let cfg = route::load(dir)?;
    Lockfile::ensure(dir, &cfg, &["main".to_string()], &[], &[], false).map_err(|d| vec![d])?;
    let mut names: Vec<String> = files.into_iter().map(|f| f.0).collect();
    names.push(LOCK_FILE.into());
    if p.agent_files {
        write(agents::FILE, agents::initial(&cfg).as_bytes())?;
        write("CLAUDE.md", b"@AGENTS.md\n")?;
        names.extend([agents::FILE.into(), "CLAUDE.md".into()]);
    }
    let trusted = trust::trust(dir, &cfg.beacon.plugins).is_ok();
    names.sort();
    Ok(Created {
        files: names,
        trusted,
    })
}
