//! Visual settings and `module.json` forms (6.1): read with a base hash, write through the daemon.
//! Forms are `module.json` only (phase 5 decision 8); `config.json` forms come after the alpha.

use super::client::call_tool;
use super::{AppCore, AppError, AppResult, SettingsDoc, SettingsFile, sanitize, sanitize_value};
use crate::Grant;
use beacon_engine::config::MODULE_FILE;
use beacon_engine::diag::Code;
use beacon_engine::path::RelPath;
use beacon_engine::settings;
use serde_json::{Value, json};
use std::path::PathBuf;

/// `module.json` of `file`; `config.json` has no form yet.
fn module_path(file: &SettingsFile) -> AppResult<RelPath> {
    match file {
        SettingsFile::Module { module } => RelPath::parse(&format!("{module}/{MODULE_FILE}"))
            .map_err(|e| AppError::new(Code::ModuleJsonInvalid, &e.0)),
        SettingsFile::Config => Err(AppError::new(
            Code::ConfigField,
            "config.json has no settings form yet",
        )),
    }
}

fn doc(file: SettingsFile, value: &Value, base: String) -> SettingsDoc {
    SettingsDoc {
        file,
        // project text reaches the webview sanitized
        value: sanitize_value(value),
        schema: settings::schema(),
        base,
    }
}

impl AppCore {
    /// The project's module folders (project-relative, sanitized), one `module.json` form each.
    pub fn settings_list(&self, root: PathBuf) -> AppResult<Vec<String>> {
        let root = crate::discovery::canonical_root(&root);
        Ok(settings::module_files(&root)?
            .iter()
            .filter_map(|p| p.parent())
            .map(|p| sanitize(p.as_str()))
            .collect())
    }

    pub fn settings_read(&self, root: PathBuf, file: SettingsFile) -> AppResult<SettingsDoc> {
        let root = crate::discovery::canonical_root(&root);
        let d = settings::read(&root, &module_path(&file)?)?;
        Ok(doc(file, &d.value, d.base))
    }

    /// Validates `value` (`ModuleJson`) and writes it through the daemon's `write_settings`
    /// (grant `edit`) against `base`; key order and indentation of the file are kept.
    /// `BCN0164` when the file changed since `base`, `BCN0163` when an editor has it open dirty.
    pub fn settings_write(
        &self,
        root: PathBuf,
        file: SettingsFile,
        base: String,
        value: Value,
    ) -> AppResult<SettingsDoc> {
        let root = crate::discovery::canonical_root(&root);
        let rel = module_path(&file)?;
        let out = call_tool(
            &self.beacon_exe,
            &root,
            Some(Grant::Edit),
            "write_settings",
            json!({ "path": rel.as_str(), "base": base, "value": value, "dryRun": false }),
        )?
        .json()?;
        let base = out["base"].as_str().unwrap_or_default().to_owned();
        Ok(doc(file, &out["value"], base))
    }
}
