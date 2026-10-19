//! Phase-5 contract: the first-run setup wizard (9) and Minecraft detection (15, "Windows
//! realities"). `beacon setup`, `beacon doctor`, the desktop app and the VS Code extension all
//! render the same [`SetupReport`] (`beacon setup --json`, `schema: 1`); downloads happen only
//! for the steps the user accepted, each one hash-pinned in the Beacon binary (9.2).
//!
//! The wizard installs exactly three things, all on demand, nothing else ("no other installs",
//! 14): the esbuild sidecar ([`Tool::Esbuild`], `crates/bundler` pins), the API index
//! ([`Tool::ApiIndex`], `crates/api` pins) and BDS ([`Tool::Bds`], `crates/bds` pin, only for
//! `beacon test`, only after EULA acceptance). The engine owns detection and the report shape;
//! the CLI owns the pins and the downloads (the engine does not link the fetchers).

use crate::model::Edition;
use std::path::{Path, PathBuf};

/// Version of the `beacon setup --json` shape.
pub const SCHEMA: u32 = 1;

/// Which game a `com.mojang` belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Product {
    Bedrock,
    Preview,
    Education,
}

impl Product {
    pub fn edition(self) -> Edition {
        match self {
            Product::Bedrock | Product::Preview => Edition::Bedrock,
            Product::Education => Edition::Education,
        }
    }
}

/// Folder layout of the install. Bedrock moved from UWP (`Packages\...\LocalState`) to GDK
/// (`%APPDATA%\Minecraft Bedrock\Users\Shared`); a machine that upgraded keeps the stale UWP
/// folder, so GDK wins when both exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Layout {
    Gdk,
    Uwp,
    /// Education Edition's own Win32 layout.
    Classic,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinecraftInstall {
    pub product: Product,
    pub layout: Layout,
    pub com_mojang: PathBuf,
}

/// (product, layout, base: true = roaming `%APPDATA%`, false = `%LOCALAPPDATA%`, relative path),
/// in preference order per product.
const CANDIDATES: &[(Product, Layout, bool, &str)] = &[
    (
        Product::Bedrock,
        Layout::Gdk,
        true,
        "Minecraft Bedrock/Users/Shared/games/com.mojang",
    ),
    (
        Product::Bedrock,
        Layout::Uwp,
        false,
        "Packages/Microsoft.MinecraftUWP_8wekyb3d8bbwe/LocalState/games/com.mojang",
    ),
    (
        Product::Preview,
        Layout::Gdk,
        true,
        "Minecraft Bedrock Preview/Users/Shared/games/com.mojang",
    ),
    (
        Product::Preview,
        Layout::Uwp,
        false,
        "Packages/Microsoft.MinecraftWindowsBeta_8wekyb3d8bbwe/LocalState/games/com.mojang",
    ),
    (
        Product::Education,
        Layout::Classic,
        true,
        "Minecraft Education Edition/games/com.mojang",
    ),
];

/// Every install whose `com.mojang` exists under the given roaming / local app-data roots, in
/// preference order (GDK before UWP per product).
pub fn detect_in(roaming: &Path, local: &Path) -> Vec<MinecraftInstall> {
    CANDIDATES
        .iter()
        .map(|&(product, layout, is_roaming, rel)| MinecraftInstall {
            product,
            layout,
            com_mojang: if is_roaming { roaming } else { local }.join(rel),
        })
        .filter(|i| i.com_mojang.is_dir())
        .collect()
}

/// [`detect_in`] on this user's app-data folders (empty when they are unknown).
pub fn detect() -> Vec<MinecraftInstall> {
    match (dirs::data_dir(), dirs::data_local_dir()) {
        (Some(r), Some(l)) => detect_in(&r, &l),
        _ => Vec::new(),
    }
}

/// The `com.mojang` dev deploy writes to: the first stable install of the edition (Preview is
/// never chosen implicitly).
pub fn preferred(installs: &[MinecraftInstall], edition: Edition) -> Option<&MinecraftInstall> {
    installs
        .iter()
        .find(|i| i.product != Product::Preview && i.product.edition() == edition)
}

/// What the wizard can download.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum Tool {
    /// Needed by Play for any project with scripts or generator npm imports.
    Esbuild,
    /// Needed by `check` for API truth checks (a missing index is only a note).
    ApiIndex,
    /// Needed by `beacon test` only; requires EULA acceptance.
    Bds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepStatus {
    /// Present and matching its pin.
    Installed,
    /// Not present; offered.
    Missing,
    /// The user declined it in this run.
    Declined,
    /// Download or verification failed; `diagnostic` says why.
    Failed,
    /// Beacon pins no download of this tool for this platform. Not an error unless the tool
    /// was explicitly requested.
    Unsupported,
}

/// One downloadable tool as the wizard shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub tool: Tool,
    /// Pinned version (esbuild version, game version of the index, BDS version).
    pub version: String,
    /// Download size in bytes, from the pin.
    pub bytes: u64,
    pub status: StepStatus,
    /// Pre-selected in the wizard (esbuild and the API index; BDS never is).
    pub default_selected: bool,
    /// The step needs the Minecraft EULA accepted first (BDS).
    pub needs_eula: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<crate::diag::Diagnostic>,
}

/// `beacon setup --json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupReport {
    pub schema: u32,
    pub installs: Vec<MinecraftInstall>,
    pub steps: Vec<Step>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gdk_before_uwp_and_preview_never_preferred() {
        let t = tempfile::tempdir().unwrap();
        let (r, l) = (t.path().join("roaming"), t.path().join("local"));
        for (_, _, roaming, rel) in CANDIDATES {
            std::fs::create_dir_all(if *roaming { &r } else { &l }.join(rel)).unwrap();
        }
        let found = detect_in(&r, &l);
        assert_eq!(found.len(), CANDIDATES.len());
        let b = preferred(&found, Edition::Bedrock).unwrap();
        assert_eq!((b.product, b.layout), (Product::Bedrock, Layout::Gdk));
        assert_eq!(
            preferred(&found, Edition::Education).unwrap().layout,
            Layout::Classic
        );

        // Only Preview installed: no implicit dev-deploy target for Bedrock.
        let t2 = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(
            t2.path()
                .join("Minecraft Bedrock Preview/Users/Shared/games/com.mojang"),
        )
        .unwrap();
        let only_preview = detect_in(t2.path(), t2.path());
        assert_eq!(only_preview.len(), 1);
        assert!(preferred(&only_preview, Edition::Bedrock).is_none());
    }
}
