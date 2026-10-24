//! Installing into the system plugin folders without installer windows.
//! macOS: `installer -pkg` through a root helper (one password prompt, ever).
//! Windows: the plugin's Inno Setup installer with /VERYSILENT (one UAC prompt per install).

use std::sync::Mutex;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum HelperState {
    /// Installs run without asking.
    Ready,
    /// Not set up (or out of date): every install asks for the password.
    Missing,
    /// No silent-install helper on this platform: every install asks for admin rights.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Unsupported,
}

/// Installers refuse to run twice at once; neither do we.
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// Where each of `bundles` sits in the system plugin folders, for the ones
/// that are there: what an uninstall removes.
fn installed_copies(bundles: &[String]) -> Vec<std::path::PathBuf> {
    bundles.iter().filter_map(|b| crate::platform::system_dir(b).map(|d| d.join(b))).filter(|p| p.is_dir()).collect()
}

/// The last guard before anything is deleted or copied as admin: every
/// bundle must end in a plain bundle file name, or the destination joined
/// from it could be the plugin folder itself.
fn check_bundles(bundles: &[std::path::PathBuf]) -> Result<(), String> {
    let plain = |b: &std::path::PathBuf| b.file_name().and_then(|n| n.to_str()).is_some_and(crate::installed::valid_bundle_name);
    match bundles.iter().find(|b| !plain(b)) {
        Some(bad) => Err(format!("Refusing to install {}: not a plain .vst3 or .component bundle.", bad.display())),
        None => Ok(()),
    }
}
