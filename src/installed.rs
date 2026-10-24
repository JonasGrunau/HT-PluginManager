//! What is installed: the plugin bundles in the system and user plugin
//! folders, and the version each one states (Info.plist on macOS, the VST3
//! moduleinfo.json everywhere).

use crate::platform;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Copy {
    pub version: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Installed {
    /// Copies in the system folders releases install into: the ones HurenTohn manages.
    pub system: Vec<Copy>,
    /// Copies in the user's own plugin folder, typically from a local
    /// `start.sh` build. Hosts see these too, so the list mentions them.
    pub user: Vec<Copy>,
}

impl Installed {
    pub fn scan(bundles: &[String]) -> Self {
        let mut found = Installed::default();
        for bundle in bundles.iter().filter(|b| valid_bundle_name(b)) {
            if let Some(c) = platform::system_dir(bundle).and_then(|d| read(&d.join(bundle))) {
                found.system.push(c);
            }
            if let Some(c) = platform::user_dir(bundle).and_then(|d| read(&d.join(bundle))) {
                found.user.push(c);
            }
        }
        found
    }

    /// The installed version, taking the oldest format if VST3 and AU differ,
    /// so a half-updated plug-in still shows its update.
    pub fn version(&self) -> Option<semver::Version> {
        self.system.iter().filter_map(|c| c.version.as_deref().and_then(parse_version)).min()
    }

    pub fn version_string(&self) -> Option<String> {
        self.version().map(|v| v.to_string())
    }

    pub fn user_version(&self) -> Option<String> {
        self.user.iter().filter_map(|c| c.version.as_deref().and_then(parse_version)).min().map(|v| v.to_string())
    }
}

/// A plain bundle file name, the same rule the macOS helper enforces:
/// `PsyBox.vst3`, `Phase Fucker.component`. No path separators, no `..`, no
/// leading dot, so joining it to a plugin folder can only name a bundle in it.
pub fn valid_bundle_name(name: &str) -> bool {
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "._ -".contains(c));
    let stem = name.strip_suffix(".vst3").or_else(|| name.strip_suffix(".component"));
    plain && stem.is_some_and(|s| !s.is_empty())
}

/// Lenient semver: `0.3` → `0.3.0`, `v1.2.3` → `1.2.3`.
pub fn parse_version(s: &str) -> Option<semver::Version> {
    let s = s.trim().trim_start_matches('v');
    semver::Version::parse(s).ok().or_else(|| {
        let parts: Vec<&str> = s.split('.').collect();
        match parts.as_slice() {
            [a] => semver::Version::parse(&format!("{a}.0.0")).ok(),
            [a, b] => semver::Version::parse(&format!("{a}.{b}.0")).ok(),
            _ => None,
        }
    })
}

pub fn bundle_version(bundle: &Path) -> Option<String> {
    let from_plist = plist::Value::from_file(bundle.join("Contents/Info.plist"))
        .ok()
        .and_then(|p| p.as_dictionary()?.get("CFBundleShortVersionString")?.as_string().map(str::to_string));
    from_plist.or_else(|| {
        let info = std::fs::read_to_string(bundle.join("Contents/Resources/moduleinfo.json")).ok()?;
        moduleinfo_version(&info)
    })
}

/// The first `"Version": "…"` of a VST3 moduleinfo.json. The file is JSON5
/// (trailing commas), so serde_json can't read it.
fn moduleinfo_version(info: &str) -> Option<String> {
    let rest = &info[info.find("\"Version\"")? + "\"Version\"".len()..];
    let rest = &rest[rest.find('"')? + 1..];
    Some(rest[..rest.find('"')?].to_string())
}

fn read(path: &Path) -> Option<Copy> {
    path.is_dir().then(|| Copy { version: bundle_version(path) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_leniently() {
        assert_eq!(parse_version("v0.3.2"), semver::Version::parse("0.3.2").ok());
        assert_eq!(parse_version("0.3"), semver::Version::parse("0.3.0").ok());
        assert_eq!(parse_version("nope"), None);
    }

    #[test]
    fn bundle_names_are_plain_file_names() {
        assert!(valid_bundle_name("PsyBox.vst3"));
        assert!(valid_bundle_name("Phase Fucker.component"));
        for bad in ["", "..", ".vst3", "../x.vst3", "a/b.vst3", r"a\b.vst3", "PsyBox", "PsyBox.dll", ".hidden.vst3", "Psy—Box.vst3"] {
            assert!(!valid_bundle_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn reads_the_version_from_moduleinfo() {
        let info = "{\n  \"Name\": \"PhaseFucker\",\n  \"Version\": \"0.3.2\",\n  \"Classes\": [ { \"Version\": \"9\", }, ],\n}";
        assert_eq!(moduleinfo_version(info).as_deref(), Some("0.3.2"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn audio_units_go_to_components() {
        assert!(platform::system_dir("PsyBox.component").unwrap().ends_with("Components"));
        assert!(platform::system_dir("PsyBox.vst3").unwrap().ends_with("VST3"));
    }
}
