use crate::store::Fetcher;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Filename of our config, placed next to the launcher exe.
///
/// Named `updater.json` (not `config.json`) specifically because the install
/// root can coincide with Codex's own userData dir. Codex is Electron and
/// may write a file called `config.json` there; a collision would have us
/// overwriting its state. `updater.json` is a filename no Electron app
/// conventionally uses.
pub const CONFIG_FILENAME: &str = "updater.json";

/// Legacy filename used by pre-migration installs. Kept as a constant so
/// the migration path in `mode::detect` can find and rename it.
pub const LEGACY_CONFIG_FILENAME: &str = "config.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallMode {
    Portable,
    User,
    System,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePolicy {
    Always,
    #[default]
    Daily,
    Weekly,
    Never,
}

/// Written next to `codex-launcher.exe` once installation completes.
/// Presence of this file is what makes the launcher run in proxy mode
/// instead of installer mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub install_mode: InstallMode,
    /// Version string (e.g. "40.0.0") — matches the `version` file shipped in the MSIX.
    pub current_version: String,
    #[serde(default)]
    pub update_policy: UpdatePolicy,
    #[serde(default)]
    pub last_check_unix: Option<u64>,
    /// If Some, skip the update prompt until this time. Used by the
    /// "don't ask again for 1 day / 7 days / ever" options.
    #[serde(default)]
    pub suppress_until_unix: Option<u64>,
    /// Last version we saw on the Store. May be newer than current_version
    /// if the user deferred an update.
    #[serde(default)]
    pub known_latest: Option<String>,
    /// Specific version the user chose "Skip this version" for. Suppresses
    /// prompts only as long as the Store's latest equals this; as soon as
    /// Microsoft publishes a newer version we prompt again.
    #[serde(default)]
    pub skipped_version: Option<String>,
    #[serde(default = "default_keep_versions")]
    pub keep_versions: u32,
    /// Which strategy the launcher uses to download MSIX updates. Currently
    /// not auto-flipped on fallback success — see `installer::update_inner`.
    #[serde(default)]
    pub fetcher: Fetcher,
    /// Maintain `versions/current` as a directory junction pointing at the
    /// newest installed version. Off by default is rare — the junction
    /// gives tooling/AV/shortcuts a stable path. Users can disable it at
    /// install time if their filesystem / AV doesn't play nicely with
    /// reparse points.
    #[serde(default = "default_true")]
    pub use_current_junction: bool,
    /// Whether the Add/Remove Programs registry entry exists. Update path
    /// uses this to know whether to refresh DisplayVersion / DisplayIcon.
    /// Off by default for Portable installs.
    #[serde(default = "default_true")]
    pub register_uninstall: bool,
}

fn default_keep_versions() -> u32 {
    2
}
fn default_true() -> bool {
    true
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(path, raw)?;
        Ok(())
    }
}
