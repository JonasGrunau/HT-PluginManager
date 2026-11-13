//! Where the GitHub token lives: the macOS login keychain or the Windows
//! Credential Manager, so a sign-in survives restarts.

const SERVICE: &str = "de.grunau.ht-plugin-manager";
const ACCOUNT: &str = "github";

fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT).ok()
}

/// `HTPM_TOKEN` (for development and snapshots) wins over the stored login.
pub fn load_token() -> Option<String> {
    if let Ok(t) = std::env::var("HTPM_TOKEN")
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_string());
    }
    entry()?.get_password().ok().filter(|t| !t.is_empty())
}

pub fn save_token(token: &str) -> Result<(), String> {
    entry()
        .ok_or_else(|| "No credential store on this computer.".to_string())?
        .set_password(token)
        .map_err(|e| format!("Can't store the login: {e}"))
}

pub fn forget_token() {
    if let Some(e) = entry() {
        let _ = e.delete_credential();
    }
}

/// The token of an existing `gh` CLI login, if there is one.
pub fn gh_cli_token() -> Option<String> {
    let out = crate::platform::command("gh").args(["auth", "token"]).env("PATH", crate::config::tool_path()).output().ok()?;
    let token = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !token.is_empty()).then_some(token)
}
