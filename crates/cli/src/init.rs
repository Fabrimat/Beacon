//! `beacon init` (9.0): flags, `--from init.json`, or prompts on a TTY, all feeding the
//! engine's one question spec (`beacon_engine::init`).

use super::{Opts, print_diags};
use beacon_engine::diag::{Code, Diagnostic, exit};
use beacon_engine::init::{self, Answers};
use beacon_engine::sarif;
use serde_json::json;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(clap::Args)]
pub struct InitArgs {
    /// Folder to create the project in (must be empty or missing).
    dir: Option<PathBuf>,
    /// Print the question spec as JSON and exit.
    #[arg(long)]
    describe: bool,
    /// Answers as JSON (`beacon init --describe` lists the fields); flags override it.
    #[arg(long, value_name = "init.json")]
    from: Option<PathBuf>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    namespace: Option<String>,
    /// Repeatable.
    #[arg(long = "author", value_name = "name")]
    authors: Vec<String>,
    #[arg(long, value_name = "x.y.z")]
    target_version: Option<String>,
    /// `bedrock` or `education`; repeatable.
    #[arg(long = "edition", value_name = "e")]
    editions: Vec<String>,
    /// `dev`, `mcpack` or `mcaddon`; repeatable.
    #[arg(long = "artifact", value_name = "a")]
    artifacts: Vec<String>,
    #[arg(long)]
    template: Option<String>,
    /// Run `git init`.
    #[arg(long)]
    git: bool,
    /// Do not write AGENTS.md / CLAUDE.md.
    #[arg(long)]
    no_agent_files: bool,
}

fn usage(o: &Opts, diags: &[Diagnostic]) -> i32 {
    if o.json {
        println!("{}", sarif::to_sarif(diags));
    } else {
        print_diags(diags);
    }
    if diags.iter().any(|d| d.code == Code::EmitFailed) {
        exit::DIAGNOSTICS
    } else {
        exit::USAGE
    }
}

fn err(o: &Opts, msg: String) -> i32 {
    usage(o, &[Diagnostic::new(Code::ConfigField, msg)])
}

fn ask(label: &str, default: &str) -> String {
    eprint!("{label} [{default}]: ");
    let mut s = String::new();
    let _ = io::stdin().read_line(&mut s);
    let s = s.trim();
    if s.is_empty() {
        default.into()
    } else {
        s.into()
    }
}

fn ask_list(label: &str, default: &[&str]) -> Vec<String> {
    ask(label, &default.join(","))
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// Asks for every field that no flag or `--from` file answered.
fn prompt(a: &mut Answers, folder_name: &str) {
    let name = a
        .name
        .get_or_insert_with(|| ask("Project name", folder_name))
        .clone();
    a.namespace
        .get_or_insert_with(|| ask("Namespace", &init::derive_namespace(&name)));
    a.authors
        .get_or_insert_with(|| ask_list("Authors (comma separated)", &[]));
    a.target_version
        .get_or_insert_with(|| ask("Target version", init::DEFAULT_TARGET_VERSION));
    if a.editions.is_none() {
        let v = ask_list("Editions (bedrock, education)", &["bedrock"]);
        a.editions = init::parse_list(&v).ok().flatten();
    }
    if a.artifacts.is_none() {
        let v = ask_list("Artifacts (dev, mcpack, mcaddon)", &["dev", "mcaddon"]);
        a.artifacts = init::parse_list(&v).ok().flatten();
    }
    let yn = |l: &str, d: &str| ask(l, d).to_lowercase().starts_with('y');
    a.git_init
        .get_or_insert_with(|| yn("Run git init? (y/n)", "n"));
    a.agent_files
        .get_or_insert_with(|| yn("Write AGENTS.md and CLAUDE.md? (y/n)", "y"));
}

fn folder_name(dir: &Path) -> String {
    std::path::absolute(dir)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "project".into())
}

pub fn run(o: &Opts, a: InitArgs) -> i32 {
    if a.describe {
        println!("{}", init::questions());
        return exit::OK;
    }
    let mut ans = match &a.from {
        Some(p) => match std::fs::read(p)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<Answers>(&b).map_err(|e| e.to_string()))
        {
            Ok(v) => v,
            Err(e) => return err(o, format!("{}: {e}", p.display())),
        },
        None => Answers::default(),
    };
    let (editions, artifacts) = match (
        init::parse_list(&a.editions),
        init::parse_list(&a.artifacts),
    ) {
        (Ok(e), Ok(r)) => (e, r),
        (Err(e), _) | (_, Err(e)) => return err(o, e),
    };
    ans.name = a.name.or(ans.name);
    ans.namespace = a.namespace.or(ans.namespace);
    ans.authors = (!a.authors.is_empty()).then_some(a.authors).or(ans.authors);
    ans.target_version = a.target_version.or(ans.target_version);
    ans.editions = editions.or(ans.editions);
    ans.artifacts = artifacts.or(ans.artifacts);
    ans.template = a.template.or(ans.template);
    ans.git_init = a.git.then_some(true).or(ans.git_init);
    ans.agent_files = a.no_agent_files.then_some(false).or(ans.agent_files);
    let dir = a
        .dir
        .or_else(|| ans.folder.as_ref().map(PathBuf::from))
        .unwrap_or_else(|| o.project.clone());
    // Prompts only on a real TTY, and never when answers come from `--yes` or a file.
    if io::stdin().is_terminal() && !o.yes && a.from.is_none() {
        prompt(&mut ans, &folder_name(&dir));
    }
    let plan = match init::resolve(ans, &folder_name(&dir)) {
        Ok(p) => p,
        Err(d) => return usage(o, &d),
    };
    let created = match init::scaffold(&dir, &plan) {
        Ok(c) => c,
        Err(d) => return usage(o, &d),
    };
    if !created.trusted {
        eprintln!("warning: could not record trust; run `beacon trust` before building scripts");
    }
    let mut git = false;
    if plan.git_init {
        git = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .is_ok_and(|s| s.success());
        if !git {
            eprintln!("warning: `git init` failed");
        }
    }
    if o.json {
        println!(
            "{}",
            json!({ "dir": dir.to_string_lossy(), "files": created.files, "trusted": created.trusted, "git": git })
        );
    } else {
        println!("created {} files in {}", created.files.len(), dir.display());
    }
    exit::OK
}
