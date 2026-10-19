//! Visual settings and `module.json` forms (6.1): read with a base hash, write through the daemon.

use super::{AppCore, AppResult, SettingsDoc, SettingsFile};
use serde_json::Value;
use std::path::PathBuf;

impl AppCore {
    pub fn settings_read(&self, _root: PathBuf, _file: SettingsFile) -> AppResult<SettingsDoc> {
        todo!("wave 2")
    }

    /// Validates `value` (`ModuleJson` / config parse), then writes through the daemon against
    /// `base`; formatting of untouched keys is kept.
    pub fn settings_write(
        &self,
        _root: PathBuf,
        _file: SettingsFile,
        _base: String,
        _value: Value,
    ) -> AppResult<SettingsDoc> {
        todo!("wave 2")
    }
}
