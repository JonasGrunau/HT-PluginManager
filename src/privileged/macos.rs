//! Installing into /Library without the Installer app.
//!
//! First start asks for the admin password once and puts a small root-owned
//! helper into /Library/PrivilegedHelperTools plus a sudoers rule that allows
//! only that helper. From then on `sudo -n` runs it without a password:
//! - `pkg <file>` runs `installer -pkg` headless, but only for packages that
//!   Gatekeeper accepts as notarized and signed by our Developer ID team.
//! - `bundle <dir>` copies a locally built `.vst3`/`.component` into place.
//!
//! Without the helper (declined, or an unsigned pkg) each install asks for the
//! password through the standard macOS prompt instead. Still no installer windows.

use super::{HelperState, INSTALL_LOCK};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const HELPER: &str = "/Library/PrivilegedHelperTools/de.grunau.hurentohn.helper";
const SUDOERS: &str = "/etc/sudoers.d/hurentohn";

/// Team of the "Developer ID Installer: Jonas Grunau" certificate the
/// release pipelines sign with.
const TEAM_ID: &str = "JQ4STVWTQ3";

const EXIT_REJECTED: i32 = 3;

fn helper_script() -> String {
    format!(
        r#"#!/bin/bash
# HurenTohn install helper (written by HurenTohn, runs as root via {SUDOERS}).
#   pkg <file.pkg>      install a pkg headless, only if Gatekeeper accepts it as a
#                       notarized Developer ID package from team {TEAM_ID}
#   bundle <dir>        copy a locally built .vst3 / .component into /Library/Audio/Plug-Ins
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
work=$(mktemp -d /private/var/tmp/hurentohn.XXXXXX)
trap 'rm -rf "$work"' EXIT

case "${{1:-}}" in
pkg)
    src=${{2:?}}
    [[ $src == *.pkg && -f $src ]] || {{ echo "not a pkg: $src" >&2; exit 2; }}
    # Check and install the same root-owned copy.
    cp -X "$src" "$work/plugin.pkg"
    verdict=$(spctl --assess --type install -vv "$work/plugin.pkg" 2>&1) || {{ echo "$verdict" >&2; exit {EXIT_REJECTED}; }}
    grep -q "^origin=Developer ID Installer: .*({TEAM_ID})$" <<<"$verdict" || {{ echo "$verdict" >&2; exit {EXIT_REJECTED}; }}
    installer -pkg "$work/plugin.pkg" -target /
    ;;
bundle)
    src=${{2:?}}; src=${{src%/}}
    name=$(basename "$src")
    case $name in
        *.vst3) dest=/Library/Audio/Plug-Ins/VST3 ;;
        *.component) dest=/Library/Audio/Plug-Ins/Components ;;
        *) echo "not a plug-in bundle: $src" >&2; exit 2 ;;
    esac
    [[ $name =~ ^[A-Za-z0-9][A-Za-z0-9._\ -]*$ && -d $src && ! -L $src ]] || {{ echo "bad bundle: $src" >&2; exit 2; }}
    ditto --norsrc "$src" "$work/$name"
    chown -R root:wheel "$work/$name"
    chmod -R u-s,g-s,go-w "$work/$name"
    mkdir -p "$dest"
    rm -rf "${{dest:?}}/$name"
    mv "$work/$name" "$dest/$name"
    ;;
*)
    echo "usage: $0 pkg <file.pkg> | bundle <dir>" >&2; exit 2 ;;
esac
"#
    )
}

pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Ready only if the helper on disk is the one this build would write and
/// sudo lets us run it without a password.
pub fn helper_state() -> HelperState {
    let current = std::fs::read_to_string(HELPER).map(|s| s == helper_script()).unwrap_or(false);
    if !current {
        return HelperState::Missing;
    }
    let allowed = Command::new("/usr/bin/sudo").args(["-n", "-l", HELPER]).output().map(|o| o.status.success()).unwrap_or(false);
    if allowed { HelperState::Ready } else { HelperState::Missing }
}

/// Runs `command` as root through the macOS admin prompt.
/// The command goes in as an argument, so no AppleScript quoting is involved.
pub fn run_as_admin(command: &str, prompt: &str) -> Result<(), String> {
    let out = Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "do shell script (item 1 of argv) with prompt (item 2 of argv) with administrator privileges",
            "-e",
            "end run",
            command,
            prompt,
        ])
        .output()
        .map_err(|e| format!("Can't ask for the admin password: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("-128") {
        Err("Cancelled at the password prompt.".into())
    } else {
        Err(err.trim().trim_start_matches("execution error: ").to_string())
    }
}

/// One password prompt: writes the helper and the sudoers rule (checked with
/// `visudo -c` before it goes live).
pub fn install_helper() -> Result<(), String> {
    let user = std::env::var("USER").map_err(|_| "Can't tell who is logged in.".to_string())?;
    if !user.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Err(format!("Unusual user name {user:?}; set up the helper by hand."));
    }
    let staging = crate::config::support_dir().join("helper-staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let helper = staging.join("helper");
    let sudoers = staging.join("sudoers");
    std::fs::write(&helper, helper_script()).map_err(|e| e.to_string())?;
    std::fs::write(&sudoers, format!("{user} ALL=(root) NOPASSWD: {HELPER}\n")).map_err(|e| e.to_string())?;

    let (h, s) = (helper.to_string_lossy(), sudoers.to_string_lossy());
    let command = [
        "install -d -o root -g wheel -m 755 /Library/PrivilegedHelperTools".to_string(),
        format!("install -o root -g wheel -m 755 {} {HELPER}", sh_quote(&h)),
        format!("/usr/sbin/visudo -cf {}", sh_quote(&s)),
        format!("install -o root -g wheel -m 440 {} {SUDOERS}", sh_quote(&s)),
    ]
    .join(" && ");
    let result = run_as_admin(
        &command,
        "HurenTohn Plugin Manager wants to install plugin updates without asking every time. This is the only time it asks.",
    );
    let _ = std::fs::remove_dir_all(&staging);
    result?;
    match helper_state() {
        HelperState::Ready => Ok(()),
        _ => Err("The helper is in place, but sudo still wants a password for it.".into()),
    }
}

fn sudo_helper(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("/usr/bin/sudo").arg("-n").arg(HELPER).args(args).output().map_err(|e| format!("Can't run the install helper: {e}"))
}

/// Installs a release pkg headless. Silent through the helper when the pkg is
/// signed by our team; otherwise one password prompt.
pub fn install_release(pkg: &Path, product: &str) -> Result<(), String> {
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = pkg.to_string_lossy();
    if helper_state() == HelperState::Ready {
        let out = sudo_helper(&["pkg", &path])?;
        if out.status.success() {
            return Ok(());
        }
        if out.status.code() != Some(EXIT_REJECTED) {
            return Err(failure("installer", &out));
        }
        // Not signed by our team (e.g. an unsigned release): ask instead.
    }
    run_as_admin(
        &format!("/usr/sbin/installer -pkg {} -target /", sh_quote(&path)),
        &format!("HurenTohn Plugin Manager wants to install {product}."),
    )
}

/// Copies locally built bundles into /Library/Audio/Plug-Ins.
pub fn install_bundles(bundles: &[PathBuf], product: &str) -> Result<(), String> {
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if helper_state() == HelperState::Ready {
        for b in bundles {
            let out = sudo_helper(&["bundle", &b.to_string_lossy()])?;
            if !out.status.success() {
                return Err(failure("copy", &out));
            }
        }
        return Ok(());
    }
    let mut steps = Vec::new();
    for b in bundles {
        let name = b.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dest = crate::platform::system_dir(&name).unwrap_or_default().join(&name);
        let (src, dst) = (sh_quote(&b.to_string_lossy()), sh_quote(&dest.to_string_lossy()));
        steps.push(format!("rm -rf {dst} && ditto --norsrc {src} {dst} && chown -R root:wheel {dst} && chmod -R u-s,g-s,go-w {dst}"));
    }
    run_as_admin(&steps.join(" && "), &format!("HurenTohn Plugin Manager wants to install your build of {product}."))
}

/// Makes hosts see a new Audio Unit without a reboot.
pub fn refresh_audio_units() {
    let _ = Command::new("/usr/bin/killall").args(["-9", "AudioComponentRegistrar"]).output();
}

fn failure(what: &str, out: &std::process::Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr);
    let line = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.is_empty() { format!("{what} failed ({})", out.status) } else { format!("{what} failed: {line}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_single_quotes() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn helper_script_is_valid_bash() {
        let dir = std::env::temp_dir().join(format!("hurentohn-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("helper");
        std::fs::write(&path, helper_script()).unwrap();
        let ok = Command::new("/bin/bash").arg("-n").arg(&path).status().unwrap().success();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ok);
        assert!(helper_script().contains("(JQ4STVWTQ3)$"));
    }
}
