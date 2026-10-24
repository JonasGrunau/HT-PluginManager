//! What differs between macOS and Windows: folders, labels in the UI, the
//! release asset to install, and opening links.

use std::path::PathBuf;
use std::process::Command;

#[cfg(not(any(target_os = "macos", windows)))]
compile_error!("HurenTohn runs on macOS and Windows.");

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_else(|| ".".into()))
}

/// `.component` bundles are Audio Units: macOS only.
pub fn is_au(bundle: &str) -> bool {
    bundle.ends_with(".component")
}

/// Starts `cmd` and waits for it on a short-lived thread, so a finished
/// `open` doesn't linger as a zombie process until HurenTohn quits.
fn spawn_detached(mut cmd: Command) {
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// A Command that never flashes a console window on Windows.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub const SYSTEM_DIR_LABEL: &str = "/Library/Audio/Plug-Ins";
    pub const USER_DIR_LABEL: &str = "~/Library/Audio/Plug-Ins";
    pub const USER_DIR_SHORT: &str = "~/LIBRARY";
    /// The release asset every plugin release carries for this platform.
    pub const INSTALLER_SUFFIX: &str = "-macOS.pkg";
    pub const INSTALL_HOW: &str = "installs it into /Library/Audio/Plug-Ins, no installer windows";
    pub const INSTALL_DETAIL: &str = "installer -pkg, no windows";
    pub const UNINSTALL_HOW: &str = "macOS asks for your password first";

    pub fn support_dir() -> PathBuf {
        home().join("Library/Application Support/HurenTohn")
    }
    pub fn download_dir() -> PathBuf {
        home().join("Library/Caches/HurenTohn/downloads")
    }
    pub fn log_dir() -> PathBuf {
        home().join("Library/Logs/HurenTohn")
    }

    /// Apps started from Finder only get /usr/bin:/bin:/usr/sbin:/sbin,
    /// which misses Homebrew's cmake, ninja and gh.
    pub fn tool_path() -> String {
        let inherited = std::env::var("PATH").unwrap_or_default();
        format!("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:{inherited}")
    }

    pub fn system_dir(bundle: &str) -> Option<PathBuf> {
        Some(PathBuf::from(if is_au(bundle) { "/Library/Audio/Plug-Ins/Components" } else { "/Library/Audio/Plug-Ins/VST3" }))
    }
    pub fn user_dir(bundle: &str) -> Option<PathBuf> {
        Some(home().join("Library/Audio/Plug-Ins").join(if is_au(bundle) { "Components" } else { "VST3" }))
    }

    pub fn open_url(url: &str) {
        let mut cmd = Command::new("/usr/bin/open");
        cmd.arg(url);
        spawn_detached(cmd);
    }
}

#[cfg(windows)]
mod imp {
    use super::*;

    pub const SYSTEM_DIR_LABEL: &str = r"C:\Program Files\Common Files\VST3";
    pub const USER_DIR_LABEL: &str = r"%LOCALAPPDATA%\Programs\Common\VST3";
    pub const USER_DIR_SHORT: &str = "YOUR USER FOLDER";
    pub const INSTALLER_SUFFIX: &str = "-Windows-Setup.exe";
    pub const INSTALL_HOW: &str = "runs its installer silently (Windows asks once for admin rights)";
    pub const INSTALL_DETAIL: &str = "Setup.exe /VERYSILENT";
    pub const UNINSTALL_HOW: &str = "Windows asks for admin rights first";

    fn env_dir(var: &str) -> PathBuf {
        std::env::var_os(var).map(PathBuf::from).unwrap_or_else(home)
    }
    pub fn support_dir() -> PathBuf {
        env_dir("APPDATA").join("HurenTohn")
    }
    pub fn download_dir() -> PathBuf {
        env_dir("LOCALAPPDATA").join(r"HurenTohn\downloads")
    }
    pub fn log_dir() -> PathBuf {
        env_dir("LOCALAPPDATA").join(r"HurenTohn\logs")
    }
    pub fn tool_path() -> String {
        std::env::var("PATH").unwrap_or_default()
    }

    fn common_files() -> PathBuf {
        std::env::var_os("CommonProgramW6432")
            .or_else(|| std::env::var_os("CommonProgramFiles"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Common Files"))
    }
    pub fn system_dir(bundle: &str) -> Option<PathBuf> {
        (!is_au(bundle)).then(|| common_files().join("VST3"))
    }
    pub fn user_dir(bundle: &str) -> Option<PathBuf> {
        (!is_au(bundle)).then(|| env_dir("LOCALAPPDATA").join(r"Programs\Common\VST3"))
    }

    /// Explorer hands URLs to the default browser and files to their app.
    pub fn open_url(url: &str) {
        let mut cmd = Command::new("explorer.exe");
        cmd.arg(url);
        spawn_detached(cmd);
    }
}

pub use imp::*;

/// Centres the system traffic lights vertically on the app's own title bar:
/// `bar_height` window points, the last `rule` of them its bottom hairline.
/// Their horizontal place stays macOS's own.
/// macOS lays the buttons out again on every resize, so this runs each frame;
/// it only touches a frame that is off.
#[cfg(target_os = "macos")]
pub fn place_traffic_lights(frame: &eframe::Frame, bar_height: f64, rule: f64) {
    use objc2_app_kit::{NSView, NSWindowButton};
    use objc2_foundation::NSPoint;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = frame.window_handle() else { return };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return };
    // Safety: winit's live content view; eframe calls us on the main thread.
    let view: &NSView = unsafe { appkit.ns_view.cast().as_ref() };
    let Some(window) = view.window() else { return };
    let kinds = [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton];
    let Some(close) = window.standardWindowButton(kinds[0]) else { return };

    // The buttons sit in the title bar container; make it as tall as our bar.
    // Safety: plain view-hierarchy getters on the main thread.
    let container = unsafe { close.superview().and_then(|v| v.superview()) };
    let Some(container) = container else { return };
    let mut c = container.frame();
    let top = window.frame().size.height - bar_height;
    if (c.size.height - bar_height).abs() > 0.5 || (c.origin.y - top).abs() > 0.5 {
        c.size.height = bar_height;
        c.origin.y = top;
        container.setFrame(c);
    }
    for kind in kinds {
        let Some(button) = window.standardWindowButton(kind) else { continue };
        let f = button.frame();
        // AppKit counts y from the bottom, where the rule is. Half points are
        // whole pixels on Retina.
        let y = ((rule + (bar_height - rule - f.size.height) / 2.0) * 2.0).round() / 2.0;
        if (f.origin.y - y).abs() > 0.25 {
            button.setFrameOrigin(NSPoint::new(f.origin.x, y));
        }
    }
}
