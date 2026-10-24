//! Where HurenTohn keeps its files, the user-editable config and the record of
//! what it installed.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Client ID of the GitHub OAuth app that signs in through the device flow.
/// Client IDs are public; the app needs no secret for the device flow.
/// `config.json` can override it.
pub const GITHUB_CLIENT_ID: &str = match option_env!("HURENTOHN_GITHUB_CLIENT_ID") {
    Some(id) => id,
    None => "",
};

pub const DEFAULT_WHITELIST: [&str; 5] =
    ["JonasGrunau/PsyBox", "JonasGrunau/PhaseFucker", "JonasGrunau/BuildApp", "JonasGrunau/VocAnal", "JonasGrunau/DisTitten"];

/// Local JUCE checkouts tried for builds of main, so a build doesn't clone JUCE.
const JUCE_CANDIDATES: [&str; 1] = ["Projects/open_music_analyzer/plugin/third_party/JUCE"];

/// Config, install records and source checkouts (`~/Library/Application Support/HurenTohn`,
/// `%APPDATA%\HurenTohn`); release installers and build logs live next to the caches.
pub use crate::platform::{download_dir, home, log_dir, support_dir, tool_path};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    /// OAuth app client ID for the device-flow sign-in. Empty: use the built-in one.
    pub github_client_id: String,
    /// `owner/repo` of every plug-in the manager may show. Repos the signed-in
    /// account can't see are left out without a word.
    pub whitelist: Vec<String>,
    /// JUCE checkout passed to builds of main as FETCHCONTENT_SOURCE_DIR_JUCE.
    /// Empty: try the known local checkout, else let CMake download JUCE.
    pub juce_dir: String,
    /// Set once the first start has offered the silent-install helper, so
    /// the password prompt comes once, not on every start.
    pub helper_offered: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            github_client_id: String::new(),
            whitelist: DEFAULT_WHITELIST.iter().map(|s| s.to_string()).collect(),
            juce_dir: String::new(),
            helper_offered: false,
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        support_dir().join("config.json")
    }

    /// Reads `config.json`, writing the defaults on first start so there is a
    /// file to edit. A broken file falls back to the defaults and says why.
    pub fn load() -> (Self, Option<String>) {
        let path = Self::path();
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(cfg) => (cfg, None),
                Err(e) => (Self::default(), Some(format!("{} is not valid JSON ({e}); using defaults.", path.display()))),
            },
            Err(_) => {
                let cfg = Self::default();
                cfg.save();
                (cfg, None)
            }
        }
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(support_dir());
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::path(), json);
        }
    }

    pub fn client_id(&self) -> &str {
        if self.github_client_id.trim().is_empty() { GITHUB_CLIENT_ID } else { self.github_client_id.trim() }
    }

    pub fn juce(&self) -> Option<PathBuf> {
        if !self.juce_dir.trim().is_empty() {
            return Some(PathBuf::from(self.juce_dir.trim()));
        }
        JUCE_CANDIDATES.iter().map(|rel| home().join(rel)).find(|p| p.join("CMakeLists.txt").is_file())
    }
}

/// How the installed copy got there. Only builds of main need this: a release
/// is recognised by its version, a build of main carries the version number of
/// whatever release it follows.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "source", rename_all = "lowercase")]
pub enum InstallRecord {
    Release { tag: String, bundle_version: String },
    Main { sha: String, committed_at: String, bundle_version: String },
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Installs(pub HashMap<String, InstallRecord>);

impl Installs {
    fn path() -> PathBuf {
        support_dir().join("installs.json")
    }

    pub fn load() -> Self {
        std::fs::read(Self::path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(support_dir());
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::path(), json);
        }
    }
}
