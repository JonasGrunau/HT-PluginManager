//! Windows: the plugin release's Inno Setup installer runs with /VERYSILENT,
//! and builds of main are copied by an elevated PowerShell script. Both go
//! through one UAC prompt each; Windows has no sudoers to make that silent.

use super::{HelperState, INSTALL_LOCK};
use std::path::{Path, PathBuf};

pub fn helper_state() -> HelperState {
    HelperState::Unsupported
}

pub fn install_helper() -> Result<(), String> {
    Err("Windows asks for admin rights once per install.".into())
}

/// Runs `<Product>-<v>-Windows-Setup.exe` without its wizard.
pub fn install_release(setup: &Path, product: &str) -> Result<(), String> {
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    run_elevated(&setup.to_string_lossy(), &["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"])
        .map_err(|e| format!("Installing {product} failed: {e}"))
}

/// Copies locally built VST3 bundles into Common Files\VST3.
pub fn install_bundles(bundles: &[PathBuf], product: &str) -> Result<(), String> {
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut script = String::from("$ErrorActionPreference = 'Stop'\r\n");
    for b in bundles {
        let name = b.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(dir) = crate::platform::system_dir(&name) else { continue };
        let dest = dir.join(&name);
        script.push_str(&format!(
            "New-Item -ItemType Directory -Force -Path {} | Out-Null\r\nif (Test-Path -LiteralPath {d}) {{ Remove-Item -LiteralPath {d} -Recurse -Force }}\r\nCopy-Item -LiteralPath {} -Destination {d} -Recurse -Force\r\n",
            ps_quote(&dir.to_string_lossy()),
            ps_quote(&b.to_string_lossy()),
            d = ps_quote(&dest.to_string_lossy()),
        ));
    }
    let dir = crate::config::support_dir().join("install");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("install-build.ps1");
    std::fs::write(&path, script).map_err(|e| e.to_string())?;
    run_elevated("powershell.exe", &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", &path.to_string_lossy()])
        .map_err(|e| format!("Installing your build of {product} failed: {e}"))
}

/// Audio Units don't exist on Windows.
pub fn refresh_audio_units() {}

/// Starts `program` elevated (the UAC prompt), waits, and passes its exit code on.
fn run_elevated(program: &str, args: &[&str]) -> Result<(), String> {
    // Start-Process joins -ArgumentList with spaces and doesn't quote, so the
    // arguments go in as one string with Windows quoting applied.
    let joined = args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.to_string() }).collect::<Vec<_>>().join(" ");
    let command = format!(
        "$p = Start-Process -FilePath {} -ArgumentList {} -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
        ps_quote(program),
        ps_quote(&joined)
    );
    let out = crate::platform::command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &command])
        .output()
        .map_err(|e| format!("can't start PowerShell: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("canceled by the user") || err.contains("cancelled by the user") {
        return Err("cancelled at the Windows admin prompt.".into());
    }
    let line = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    Err(if line.is_empty() { format!("exit code {}", out.status.code().unwrap_or(-1)) } else { line })
}

/// PowerShell single quotes: only `'` needs escaping, as `''`.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_quoting() {
        assert_eq!(ps_quote(r"C:\Users\Jo's\x.exe"), r"'C:\Users\Jo''s\x.exe'");
    }
}
