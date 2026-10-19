//! Updater commands (`crate::update` decides). The alpha ships the fail-closed policy only:
//! no feed, no key, no `tauri-plugin-updater` registration (phase5-plan decision 7). Nothing
//! here touches the network.

use super::{AppCore, AppError, AppResult};
use crate::update::{self, Decision};
use beacon_engine::diag::Code;

fn disabled() -> AppError {
    AppError::new(
        Code::UpdateDisabled,
        "updates are off in this build (unsigned alpha, no update feed)",
    )
}

impl AppCore {
    /// This build's policy decision. Never fetches: with no key or feed compiled in it is
    /// `Disabled`; a build that has both has no fetch code yet, so it is also refused.
    pub fn update_check(&self) -> AppResult<Decision> {
        match update::BUILD.feed_url(update::Channel::Stable) {
            None => Ok(Decision::Disabled),
            Some(_) => Err(disabled()),
        }
    }

    pub fn update_install(&self, _version: String) -> AppResult<()> {
        Err(disabled())
    }

    pub fn update_rollback(&self) -> AppResult<()> {
        Err(disabled())
    }
}
