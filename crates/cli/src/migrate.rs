//! `beacon migrate` (5.6): detect -> evaluate (ModularMC in the JS host, system_template
//! from `--evaluated`) -> plan -> apply -> optional `--oracle` build + compare. Also the
//! shared phase 3 command shapes (`add`, `hook`).

use super::{Opts, print_diags};
use beacon_bundler::{Confirm, LazyEsbuild};
use beacon_engine::build::{BuildOptions, Session, canon};
use beacon_engine::diag::{Code, Diagnostic, Severity, exit};
use beacon_engine::migrate::{
    self, Change, LegacyEval, LegacyKind, LegacyProject, MigrateOptions, MigrationPlan,
    plan as mplan, system_template, write,
};
use beacon_engine::model::Body;
use beacon_engine::sarif;
use beacon_jshost::legacy::{BasePaths, evaluate_modular_mc, migrate_limits};
use beacon_jshost::{Host, Limits};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct MigrateArgs {
    /// The Regolith project to convert (left untouched).
    #[arg(value_name = "regolith-dir")]
    pub dir: PathBuf,
    /// The new Beacon project directory (must be outside the Regolith project).
    #[arg(long, value_name = "dir")]
    pub out: PathBuf,
    /// Previously built legacy packs (`BP/`, `RP/`) to compare the migrated build with.
    #[arg(long, value_name = "dir")]
    pub oracle: Option<PathBuf>,
    /// A real legacy run's output (Regolith tmp layout), used instead of evaluating the maps.
    #[arg(long, value_name = "dir")]
    pub evaluated: Option<PathBuf>,
    /// Environment the maps see as `process.env` (repeatable).
    #[arg(long = "env", value_name = "K=V", value_parser = parse_env)]
    pub env: Vec<(String, String)>,
    /// Project namespace (default: derived from the legacy project).
    #[arg(long, value_name = "ns")]
    pub namespace: Option<String>,
    /// Show the plan and report; write nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Allow a non-empty `--out`. With `--oracle`, the migrated project is then built trusted
    /// over the whole `--out` tree, files that were already there included.
    #[arg(long)]
    pub force: bool,
    /// Allow evaluating the legacy map files (they are code) for this run.
    #[arg(long)]
    pub trust: bool,
    /// The Regolith profile `--oracle` was built with: its migrated target is built (packs
    /// only) for the comparison. Default: the `dev` target, else the packs build.
    #[arg(long, value_name = "profile", requires = "oracle")]
    pub oracle_profile: Option<String>,
}

fn parse_env(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.into(), v.into())),
        _ => Err(format!("expected K=V, got `{s}`")),
    }
}

/// What `beacon add` creates.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum AddKind {
    Entity,
    Item,
    Block,
    Module,
}

/// Claude Code hook events (8.2).
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum HookEvent {
    PreToolUse,
    Stop,
}

impl MigrateArgs {
    pub fn options(&self) -> MigrateOptions {
        MigrateOptions {
            out: self.out.clone(),
            oracle: self.oracle.clone(),
            evaluated: self.evaluated.clone(),
            env: self.env.iter().cloned().collect(),
            dry_run: self.dry_run,
            force: self.force,
            namespace: self.namespace.clone(),
        }
    }
}

fn fail(o: &Opts, diags: &[Diagnostic], code: i32) -> i32 {
    if o.json {
        println!(
            "{}",
            serde_json::to_string(&sarif::to_sarif(diags)).unwrap()
        );
    } else {
        print_diags(diags);
    }
    code
}

fn evaluate(p: &LegacyProject, opts: &MigrateOptions) -> Result<LegacyEval, Vec<Diagnostic>> {
    let unsupported = |m: &str| vec![Diagnostic::new(Code::MigrateUnsupported, m)];
    match (p.kinds.as_slice(), &opts.evaluated) {
        ([LegacyKind::SystemTemplate], Some(e)) => system_template::ingest(p, e),
        ([LegacyKind::SystemTemplate], None) => Err(unsupported(
            "system_template maps are not evaluated by Beacon: pass --evaluated <dir>, the output of a real Regolith run (tmp layout + system_template_log.json; see scripts/fixtures-local)",
        )),
        ([LegacyKind::ModularMc], _) => {
            let (idx, _) = mplan::legacy_index(p, opts);
            let data = mplan::data_rel(p).ok_or_else(|| unsupported("bad regolith.dataPath"))?;
            let (bp, rp) = mplan::base_paths(p);
            let mut ev = evaluate_modular_mc(
                &idx,
                &data,
                &BasePaths { bp, rp },
                &opts.env,
                migrate_limits(),
            );
            if opts.evaluated.is_some() {
                let mut d = Diagnostic::new(
                    Code::MigrateUnsupported,
                    "--evaluated is read for system_template only; the ModularMC maps were evaluated",
                );
                d.severity = Severity::Warning;
                ev.diagnostics.push(d);
            }
            Ok(ev)
        }
        _ => Err(unsupported(
            "modular_mc and system_template in one project: migrate is not supported for both at once yet",
        )),
    }
}

/// `--oracle`: builds the migrated project (packs projection, trusted) into
/// `<out>/.beacon/migrate-oracle` and compares it with the legacy output.
fn oracle(
    o: &Opts,
    project: &LegacyProject,
    profile: Option<&str>,
    out: &std::path::Path,
    oracle: &std::path::Path,
    plan: &mut MigrationPlan,
) -> bool {
    let dir = out.join(".beacon").join("migrate-oracle");
    if let Some(l) = write::link_under(out, &dir) {
        plan.changes.push(Change::Note(Diagnostic::new(
            Code::EmitFailed,
            format!("--oracle: refusing to build through link {}", l.display()),
        )));
        return false;
    }
    let _ = std::fs::remove_dir_all(&dir);
    let confirm = if o.download {
        Confirm::Yes
    } else {
        Confirm::Tty
    };
    // The target of the compared profile. Regolith's packaging profiles are the default
    // profile's packs plus world steps; their Beacon targets are archives and may carry a
    // `targetVersion` of another profile (`convert_to_bedrock` in `bedrock_packaging`, merged
    // into `world`), which the plain packs build would pick up. The default profile's `dev`
    // target builds those packs at the project's `targetVersion`.
    // With `--oracle-profile`, the target the migrated config maps that profile to.
    let config = plan
        .changes
        .iter()
        .find_map(|c| match c {
            Change::Config(v) => Some(v.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let target = match profile {
        Some(p) => match mplan::oracle_target(project, p, &config) {
            Ok(t) => Some(t),
            Err(m) => {
                plan.changes
                    .push(Change::Note(Diagnostic::new(Code::MigrateOracleDiff, m)));
                return false;
            }
        },
        None => (config["beacon"]["targets"]["dev"]["artifact"] == "dev").then(|| "dev".into()),
    };
    let artifact = target
        .as_ref()
        .and_then(|t| config["beacon"]["targets"][t]["artifact"].as_str())
        .unwrap_or("")
        .to_string();
    let dev = artifact == "dev";
    let opts = BuildOptions {
        target: target.clone(),
        out: Some(dir.clone()),
        trusted: true,
        ..Default::default()
    };
    let r = Session::new(
        out,
        opts,
        Box::new(Host::new(Limits::default())),
        Some(Box::new(LazyEsbuild::new(out, confirm))),
    )
    .build();
    let mut ok = !r.diagnostics.iter().any(|d| d.severity == Severity::Error);
    plan.changes
        .extend(r.diagnostics.into_iter().map(Change::Note));
    // dev layout: `development_{behavior,resource}_packs/<name>-<worktree> BP|RP` -> BP/, RP/
    let built = if dev {
        let cmp = dir.join("compare");
        for (from, to) in [
            ("development_behavior_packs", "BP"),
            ("development_resource_packs", "RP"),
        ] {
            let found = std::fs::read_dir(dir.join(from))
                .into_iter()
                .flatten()
                .flatten()
                .find(|e| e.file_type().is_ok_and(|t| t.is_dir()));
            if let Some(e) = found {
                let _ = std::fs::create_dir_all(&cmp);
                let _ = std::fs::rename(e.path(), cmp.join(to));
            }
        }
        cmp
    } else if target.is_some() {
        // an archive (`.mcworld`, `.mctemplate`, `.mcaddon`, `.mcpack`): only its BP/RP
        let cmp = dir.join("compare");
        let archives: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| matches!(x, "mcworld" | "mctemplate" | "mcaddon" | "mcpack"))
            })
            .collect();
        let mut taken = std::collections::BTreeSet::new();
        for a in &archives {
            match write::extract_packs(a, &cmp, &mut taken) {
                Ok(d) => {
                    ok &= d.is_empty();
                    plan.changes.extend(d.into_iter().map(Change::Note));
                }
                Err(e) => {
                    plan.changes.push(Change::Note(Diagnostic::new(
                        Code::EmitFailed,
                        format!("--oracle: cannot read {}: {e}", a.display()),
                    )));
                    ok = false;
                }
            }
        }
        cmp
    } else {
        dir.join(mplan::OUTPUT)
    };
    if let Some(t) = &target {
        plan.changes.push(Change::Note(Diagnostic::new(
            Code::MigrateToPort,
            format!("--oracle compared with target `{t}` ({artifact}), packs only"),
        )));
    }
    let rep = write::oracle_compare(oracle, &built, plan);
    for d in &rep.diffs {
        let m = serde_json::to_string(d).unwrap_or_default();
        plan.changes
            .push(Change::Note(Diagnostic::new(Code::MigrateOracleDiff, m)));
        ok = false;
    }
    plan.report.oracle = Some(rep);
    // the comparison build is scratch: the migrated project keeps no trace of it
    let _ = std::fs::remove_dir_all(&dir);
    let notes: Vec<Diagnostic> = plan
        .changes
        .iter()
        .filter_map(|c| match c {
            Change::Note(d) => Some(d.clone()),
            _ => None,
        })
        .collect();
    let md = mplan::render_report(&plan.report, &notes);
    if let Err(e) = write::put_under(out, &out.join("MIGRATION.md"), md.as_bytes()) {
        plan.changes.push(Change::Note(Diagnostic::new(
            Code::EmitFailed,
            format!("MIGRATION.md: {e}"),
        )));
        ok = false;
    }
    for c in &mut plan.changes {
        if let Change::Write { to, body } = c
            && to.as_str() == "MIGRATION.md"
        {
            *body = Body::Text(md.clone());
        }
    }
    ok
}

pub fn run(o: &Opts, a: &MigrateArgs) -> i32 {
    // the maps are code (5.5): only for a trusted session
    if !(a.trust || beacon_engine::trust::is_trusted(&a.dir, &[])) {
        let d = Diagnostic::new(
            Code::Untrusted,
            "migrate evaluates the legacy map files (code); pass --trust or run `beacon trust` in the legacy project",
        );
        return fail(o, &[d], exit::UNTRUSTED);
    }
    let opts = a.options();
    let project = match migrate::detect(&a.dir) {
        Ok(p) => p,
        Err(d) => return fail(o, &d, exit::DIAGNOSTICS),
    };
    if let Err(d) = write::check_out(&project.root, &opts.out, opts.force) {
        return fail(o, &d, exit::DIAGNOSTICS);
    }
    // the comparison build is wiped before it runs: never let it be the oracle
    if let Some(or) = &opts.oracle {
        let scratch = canon(&opts.out.join(".beacon").join("migrate-oracle"));
        if canon(or).starts_with(&scratch) {
            let d = Diagnostic::new(
                Code::MigrateOutInvalid,
                format!(
                    "--oracle {} is inside the comparison build dir {}",
                    or.display(),
                    scratch.display()
                ),
            );
            return fail(o, &[d], exit::DIAGNOSTICS);
        }
    }
    let mut plan =
        match evaluate(&project, &opts).and_then(|ev| migrate::plan(&project, &ev, &opts)) {
            Ok(p) => p,
            Err(d) => return fail(o, &d, exit::DIAGNOSTICS),
        };
    let mut code = exit::OK;
    if !opts.dry_run {
        if let Err(d) = migrate::apply(&project, &plan, &opts) {
            return fail(o, &d, exit::DIAGNOSTICS);
        }
        if let Some(or) = &opts.oracle
            && !oracle(
                o,
                &project,
                a.oracle_profile.as_deref(),
                &opts.out,
                or,
                &mut plan,
            )
        {
            code = exit::DIAGNOSTICS;
        }
    }
    if o.json {
        println!("{}", serde_json::to_string(&plan).unwrap());
    } else {
        let notes: Vec<Diagnostic> = plan
            .changes
            .iter()
            .filter_map(|c| match c {
                Change::Note(d) => Some(d.clone()),
                _ => None,
            })
            .collect();
        print_diags(&notes);
        let r = &plan.report;
        let verb = if opts.dry_run { "would write" } else { "wrote" };
        println!(
            "{verb} {} files in {} modules to {}; {} to port, {} flattened, {} dropped (see MIGRATION.md)",
            r.files,
            r.modules.len(),
            opts.out.display(),
            r.to_port.len(),
            r.flattened.len(),
            r.dropped.len()
        );
        if let Some(rep) = &r.oracle {
            println!(
                "oracle: {} compared, {} differences",
                rep.compared,
                rep.diffs.len()
            );
        }
    }
    code
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_pairs() {
        assert_eq!(
            super::parse_env("A=b=c").unwrap(),
            ("A".into(), "b=c".into())
        );
        assert!(super::parse_env("=x").is_err());
        assert!(super::parse_env("nope").is_err());
    }
}
