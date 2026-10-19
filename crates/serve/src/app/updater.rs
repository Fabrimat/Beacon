//! Updater commands (`crate::update` decides). The alpha ships the fail-closed policy only:
//! no feed, no key, no `tauri-plugin-updater` registration (phase5-plan decision 7).

use super::{AppCore, AppResult};

impl AppCore {
    /// Never downloads: fetches the feed and evaluates it (`crate::update::evaluate`).
    pub fn update_check(&self) -> AppResult<crate::update::Decision> {
        todo!("wave 3")
    }

    /// Only from the user's click on an `Available` decision for `version`.
    pub fn update_install(&self, _version: String) -> AppResult<()> {
        todo!("wave 3")
    }

    /// Reinstalls the previous version's kept installer after re-verifying it.
    pub fn update_rollback(&self) -> AppResult<()> {
        todo!("wave 3")
    }
}
