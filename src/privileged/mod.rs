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
