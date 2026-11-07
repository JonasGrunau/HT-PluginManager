//! Windows: the plugin release's Inno Setup installer runs with /VERYSILENT,
//! and builds of main are copied in (and uninstalls removed) by this app
//! itself, started a second time as admin (`admin_task`). Both go through
//! ShellExecuteEx's "runas", one UAC prompt each; Windows has no sudoers to
//! make that silent. No PowerShell and no script files: a hidden elevated
//! `powershell -ExecutionPolicy Bypass -File` is what antivirus behaviour
//! rules look for.

use super::{HelperState, INSTALL_LOCK};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The first argument that makes the app the admin half of an install or an
/// uninstall of bundles; the bundle paths follow.
const ADMIN_INSTALL: &str = "--admin-install-bundles";
const ADMIN_UNINSTALL: &str = "--admin-uninstall-bundles";
/// The admin half's exit codes: a refused bundle, any other failure, or
/// `OS_ERROR` plus the Windows error code (the "customer" bit of an error code,
/// so no system code reads as one of ours).
const EXIT_REFUSED: u32 = 2;
const EXIT_FAILED: u32 = 1;
const OS_ERROR: u32 = 0x2000_0000;

pub fn helper_state() -> HelperState {
    HelperState::Unsupported
}

pub fn install_helper() -> Result<(), String> {
    Err("Windows asks for admin rights once per install.".into())
}

/// Runs `<Product>-<v>-Windows-Setup.exe` without its wizard.
pub fn install_release(setup: &Path, product: &str) -> Result<(), String> {
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    match run_elevated(setup, &["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"]) {
        Ok(0) => Ok(()),
        Ok(code) => Err(format!("Installing {product} failed: exit code {code}")),
        Err(e) => Err(format!("Installing {product} failed: {e}")),
    }
}

/// Copies locally built VST3 bundles into Common Files\VST3.
pub fn install_bundles(bundles: &[PathBuf], product: &str) -> Result<(), String> {
    super::check_bundles(bundles)?;
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    run_admin_task(ADMIN_INSTALL, bundles).map_err(|e| format!("Installing your build of {product} failed: {e}"))
}

/// Removes the plug-in from Common Files\VST3. A release install keeps its
/// Inno Setup uninstaller inside the bundle (`{app}` is the bundle), so that
/// runs first and takes the Apps & Features entry with it; whatever is left,
/// such as a build of main, is deleted.
pub fn uninstall_bundles(bundles: &[String], product: &str) -> Result<(), String> {
    let copies = super::installed_copies(bundles);
    super::check_bundles(&copies)?;
    if copies.is_empty() {
        return Ok(());
    }
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    run_admin_task(ADMIN_UNINSTALL, &copies).map_err(|e| format!("Uninstalling {product} failed: {e}"))
}

/// Audio Units don't exist on Windows.
pub fn refresh_audio_units() {}

/// The admin half of `install_bundles` and `uninstall_bundles`: the app started
/// again through UAC with `ADMIN_INSTALL` or `ADMIN_UNINSTALL` and bundle
/// paths. Returns its exit code, or `None` for a normal start. It trusts its
/// arguments no more than it must: every bundle name is checked again, and
/// whatever folder a path names, it only ever writes to the system plug-in
/// folder (installs read the bundle from the path; uninstalls take only its name).
pub fn admin_task() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    let task = args.next()?;
    let bundles: Vec<PathBuf> = args.map(PathBuf::from).collect();
    let result = if task == ADMIN_INSTALL {
        admin_install(&bundles)
    } else if task == ADMIN_UNINSTALL {
        admin_uninstall(&bundles)
    } else {
        return None;
    };
    Some(result.err().unwrap_or(0) as i32)
}

fn admin_install(bundles: &[PathBuf]) -> Result<(), u32> {
    super::check_bundles(bundles).map_err(|_| EXIT_REFUSED)?;
    for b in bundles {
        let Some(name) = b.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(dir) = crate::platform::system_dir(name) else { continue };
        let dest = dir.join(name);
        std::fs::create_dir_all(&dir).map_err(os_code)?;
        if dest.symlink_metadata().is_ok() {
            std::fs::remove_dir_all(&dest).map_err(os_code)?;
        }
        copy_tree(b, &dest)?;
    }
    Ok(())
}

fn admin_uninstall(bundles: &[PathBuf]) -> Result<(), u32> {
    super::check_bundles(bundles).map_err(|_| EXIT_REFUSED)?;
    for b in bundles {
        let Some(name) = b.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(dir) = crate::platform::system_dir(name) else { continue };
        let bundle = dir.join(name);
        let uninstaller = bundle.join("unins000.exe");
        if uninstaller.is_file() {
            // Inno's uninstaller hands over to a copy of itself in %TEMP% and
            // returns early: wait up to 30 s for the bundle to go.
            let _ = crate::platform::command(&uninstaller).args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]).status();
            for _ in 0..60 {
                if bundle.symlink_metadata().is_err() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        if bundle.symlink_metadata().is_ok() {
            std::fs::remove_dir_all(&bundle).map_err(os_code)?;
        }
    }
    Ok(())
}

/// Copies the bundle `from` to `to`. Links are refused: the bundle sits in a
/// user's build folder, and the admin half must not follow a link out of it
/// to files only an admin may read.
fn copy_tree(from: &Path, to: &Path) -> Result<(), u32> {
    let meta = from.symlink_metadata().map_err(os_code)?;
    if meta.file_type().is_symlink() {
        return Err(EXIT_REFUSED);
    }
    if meta.is_dir() {
        std::fs::create_dir(to).map_err(os_code)?;
        for entry in std::fs::read_dir(from).map_err(os_code)? {
            let entry = entry.map_err(os_code)?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(from, to).map_err(os_code)?;
    }
    Ok(())
}

/// An I/O error as the admin half's exit code.
fn os_code(e: std::io::Error) -> u32 {
    e.raw_os_error().map_or(EXIT_FAILED, |c| OS_ERROR | c as u32)
}

/// Starts this app again as admin with `task` and `bundles`, and turns the
/// admin half's exit code into a message.
fn run_admin_task(task: &str, bundles: &[PathBuf]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("can't find the app's own exe: {e}"))?;
    let mut args = vec![task];
    for b in bundles {
        args.push(b.to_str().ok_or_else(|| format!("{} is not a Unicode path.", b.display()))?);
    }
    match run_elevated(&exe, &args)? {
        0 => Ok(()),
        EXIT_REFUSED => Err("refused: not a plain .vst3 bundle, or it contains links.".into()),
        c if c & OS_ERROR != 0 => Err(std::io::Error::from_raw_os_error((c & !OS_ERROR) as i32).to_string()),
        c => Err(format!("exit code {c}")),
    }
}

/// Starts `program` as admin (the UAC prompt) through ShellExecuteEx's
/// "runas" verb, waits for it and returns its exit code.
fn run_elevated(program: &Path, args: &[&str]) -> Result<u32, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError};
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |s: &OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let verb = wide(OsStr::new("runas"));
    let file = wide(program.as_os_str());
    let params = wide(OsStr::new(&args.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ")));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // Hand back the process to wait on, and report errors here instead of in a dialog.
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: params.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to outlive the call; the
    // process handle is closed exactly once, after the wait.
    unsafe {
        // ShellExecuteEx can hand the call to shell extensions, which need COM.
        let com = CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32);
        let started = ShellExecuteExW(&mut info) != 0;
        let error = GetLastError();
        if com >= 0 {
            CoUninitialize();
        }
        if !started {
            return Err(if error == ERROR_CANCELLED {
                "cancelled at the Windows admin prompt.".into()
            } else {
                std::io::Error::from_raw_os_error(error as i32).to_string()
            });
        }
        if info.hProcess.is_null() {
            return Err("Windows started nothing to wait for.".into());
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 0u32;
        let read = GetExitCodeProcess(info.hProcess, &mut code) != 0;
        CloseHandle(info.hProcess);
        if read { Ok(code) } else { Err("can't read its exit code.".into()) }
    }
}

/// One argument quoted for a Windows command line, so the started program's
/// argument parser (the C runtime's, or Rust's) reads it back unchanged.
fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        // Backslashes only escape in front of a quote: doubled there, plus one for the quote.
        let n = if c == '"' { backslashes * 2 + 1 } else { backslashes };
        out.extend(std::iter::repeat_n('\\', n));
        out.push(c);
        backslashes = 0;
    }
    // The closing quote follows, so trailing backslashes double too.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quoting() {
        assert_eq!(quote_arg("/VERYSILENT"), "/VERYSILENT");
        assert_eq!(quote_arg(r"C:\Users\Jo\build\A.vst3"), r"C:\Users\Jo\build\A.vst3");
        assert_eq!(quote_arg(r"C:\Program Files\Common Files\VST3\A.vst3"), r#""C:\Program Files\Common Files\VST3\A.vst3""#);
        assert_eq!(quote_arg(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote_arg(r#"say "hi"\"#), r#""say \"hi\"\\""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_arg(""), r#""""#);
    }

    #[test]
    fn admin_exit_codes_round_trip() {
        let e = std::io::Error::from_raw_os_error(32);
        let code = os_code(e);
        assert_ne!(code & OS_ERROR, 0);
        assert_eq!(code & !OS_ERROR, 32);
        assert_eq!(os_code(std::io::Error::other("no code")), EXIT_FAILED);
    }
}
