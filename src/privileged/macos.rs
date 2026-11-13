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

pub const HELPER: &str = "/Library/PrivilegedHelperTools/de.grunau.ht-plugin-manager.helper";
const SUDOERS: &str = "/etc/sudoers.d/ht-plugin-manager";

/// Team of the "Developer ID Installer: Jonas Grunau" certificate the
/// release pipelines sign with.
const TEAM_ID: &str = "JQ4STVWTQ3";

const EXIT_REJECTED: i32 = 3;

fn helper_script() -> String {
    format!(
        r#"#!/bin/bash
# HT Plugin Manager install helper (written by the app, runs as root via {SUDOERS}).
#   pkg <file.pkg>      install a pkg headless, only if Gatekeeper accepts it as a
#                       notarized Developer ID package from team {TEAM_ID}
#   bundle <dir>        copy a locally built .vst3 / .component into /Library/Audio/Plug-Ins
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
work=$(mktemp -d /private/var/tmp/ht-plugin-manager.XXXXXX)
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

/// `printf` of `content` into `dest`, for a root shell: the bytes come from
/// this process's memory, never from a file the user could swap meanwhile.
fn write_file_command(content: &str, dest: &str) -> String {
    format!("printf '%s' {} > {dest}", sh_quote(content))
}

/// The one root command `install_helper` runs: both files written into a
/// root-only temp dir, the sudoers rule checked with `visudo -c`, then both
/// installed.
fn helper_install_command(user: &str) -> String {
    [
        "d=$(mktemp -d /private/var/tmp/ht-plugin-manager-setup.XXXXXX)".to_string(),
        r#"trap 'rm -rf "$d"' EXIT"#.to_string(),
        write_file_command(&helper_script(), r#""$d/helper""#),
        write_file_command(&format!("{user} ALL=(root) NOPASSWD: {HELPER}\n"), r#""$d/sudoers""#),
        r#"/usr/sbin/visudo -cf "$d/sudoers""#.to_string(),
        "install -d -o root -g wheel -m 755 /Library/PrivilegedHelperTools".to_string(),
        format!(r#"install -o root -g wheel -m 755 "$d/helper" {HELPER}"#),
        format!(r#"install -o root -g wheel -m 440 "$d/sudoers" {SUDOERS}"#),
    ]
    .join(" && ")
}

/// One password prompt: writes the helper and the sudoers rule (checked with
/// `visudo -c` before it goes live).
pub fn install_helper() -> Result<(), String> {
    let user = std::env::var("USER").map_err(|_| "Can't tell who is logged in.".to_string())?;
    if !user.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Err(format!("Unusual user name {user:?}; set up the helper by hand."));
    }
    run_as_admin(
        &helper_install_command(&user),
        "HT Plugin Manager wants to install plugin updates without asking every time. This is the only time it asks.",
    )?;
    match helper_state() {
        HelperState::Ready => Ok(()),
        _ => Err("The helper is in place, but sudo still wants a password for it.".into()),
    }
}

fn sudo_helper(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("/usr/bin/sudo").arg("-n").arg(HELPER).args(args).output().map_err(|e| format!("Can't run the install helper: {e}"))
}

/// sudo itself said no (the rule went away since `helper_state` looked):
/// the helper never ran, so the password prompt can still do the job.
fn sudo_refused(out: &std::process::Output) -> bool {
    !out.status.success() && String::from_utf8_lossy(&out.stderr).trim_start().starts_with("sudo:")
}

/// What the helper checks, asked as the user: is this pkg notarized and
/// signed by our Developer ID team?
fn signed_by_us(pkg: &Path) -> bool {
    let Ok(out) = Command::new("/usr/sbin/spctl").args(["--assess", "--type", "install", "-vv"]).arg(pkg).output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    out.status.success() && text.lines().any(|l| l.starts_with("origin=Developer ID Installer: ") && l.ends_with(&format!("({TEAM_ID})")))
}

/// Installs a release pkg headless. Silent through the helper when the pkg is
/// signed by our team; otherwise one password prompt, which says so when
/// the signature is not ours.
pub fn install_release(pkg: &Path, product: &str) -> Result<(), String> {
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = pkg.to_string_lossy();
    if helper_state() == HelperState::Ready {
        let out = sudo_helper(&["pkg", &path])?;
        if out.status.success() {
            return Ok(());
        }
        if out.status.code() != Some(EXIT_REJECTED) && !sudo_refused(&out) {
            return Err(failure("installer", &out));
        }
        // Not signed by our team (e.g. an unsigned release), or sudo said no: ask instead.
    }
    let prompt = if signed_by_us(pkg) {
        format!("HT Plugin Manager wants to install {product}.")
    } else {
        format!("HT Plugin Manager wants to install {product}. Careful: this package is not signed by Jonas Grunau's Developer ID.")
    };
    run_as_admin(&format!("/usr/sbin/installer -pkg {} -target /", sh_quote(&path)), &prompt)
}

/// Copies locally built bundles into /Library/Audio/Plug-Ins.
pub fn install_bundles(bundles: &[PathBuf], product: &str) -> Result<(), String> {
    super::check_bundles(bundles)?;
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if helper_state() == HelperState::Ready {
        let mut refused = false;
        for b in bundles {
            let out = sudo_helper(&["bundle", &b.to_string_lossy()])?;
            if sudo_refused(&out) {
                refused = true;
                break;
            }
            if !out.status.success() {
                return Err(failure("copy", &out));
            }
        }
        if !refused {
            return Ok(());
        }
    }
    let mut steps = Vec::new();
    for b in bundles {
        let name = b.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dest = crate::platform::system_dir(&name).unwrap_or_default().join(&name);
        let (src, dst) = (sh_quote(&b.to_string_lossy()), sh_quote(&dest.to_string_lossy()));
        steps.push(format!("rm -rf {dst} && ditto --norsrc {src} {dst} && chown -R root:wheel {dst} && chmod -R u-s,g-s,go-w {dst}"));
    }
    run_as_admin(&steps.join(" && "), &format!("HT Plugin Manager wants to install your build of {product}."))
}

/// Removes the plug-in's bundles from /Library/Audio/Plug-Ins. Always through
/// the password prompt: the helper can only install, and an uninstall is rare
/// enough to ask. The user's own plug-in folder is left alone.
pub fn uninstall_bundles(bundles: &[String], product: &str) -> Result<(), String> {
    let copies = super::installed_copies(bundles);
    super::check_bundles(&copies)?;
    if copies.is_empty() {
        return Ok(());
    }
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let steps: Vec<String> = copies.iter().map(|c| format!("rm -rf -- {}", sh_quote(&c.to_string_lossy()))).collect();
    run_as_admin(&steps.join(" && "), &format!("HT Plugin Manager wants to uninstall {product}."))
}

/// Makes hosts see a new Audio Unit without a reboot.
pub fn refresh_audio_units() {
    let _ = Command::new("/usr/bin/killall").args(["-9", "AudioComponentRegistrar"]).output();
}

/// The tool's last word: stderr, else stdout (`installer` prints its errors there).
fn failure(what: &str, out: &std::process::Output) -> String {
    let last = |bytes: &[u8]| String::from_utf8_lossy(bytes).lines().rev().map(str::trim).find(|l| !l.is_empty()).map(str::to_string);
    match last(&out.stderr).or_else(|| last(&out.stdout)) {
        Some(line) => format!("{what} failed: {line}"),
        None => format!("{what} failed ({})", out.status),
    }
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
        let dir = std::env::temp_dir().join(format!("ht-plugin-manager-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("helper");
        std::fs::write(&path, helper_script()).unwrap();
        let ok = Command::new("/bin/bash").arg("-n").arg(&path).status().unwrap().success();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ok);
        assert!(helper_script().contains("(JQ4STVWTQ3)$"));
    }

    /// The root shell gets the helper byte for byte from the printf, quotes,
    /// `%` and all, so `helper_state` sees the script it expects.
    #[test]
    fn printf_writes_the_helper_exactly() {
        let out = Command::new("/bin/sh").args(["-c", &write_file_command(&helper_script(), "/dev/stdout")]).output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8(out.stdout).unwrap(), helper_script());
        let tricky = "it's 100%s \\n done\n";
        let out = Command::new("/bin/sh").args(["-c", &write_file_command(tricky, "/dev/stdout")]).output().unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), tricky);
    }

    #[test]
    fn helper_install_reads_no_user_files() {
        let cmd = helper_install_command("jonas");
        assert!(!cmd.contains("Application Support"));
        assert!(cmd.contains("visudo -cf"));
        let ok = Command::new("/bin/sh").args(["-n", "-c", &cmd]).status().unwrap().success();
        assert!(ok, "{cmd}");
    }

    #[test]
    fn failures_fall_back_to_stdout() {
        use std::os::unix::process::ExitStatusExt;
        let out = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: b"installer: Error - the package is damaged.\n".to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(failure("installer", &out), "installer failed: installer: Error - the package is damaged.");
    }
}
