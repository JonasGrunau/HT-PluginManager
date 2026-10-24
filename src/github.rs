//! The GitHub REST calls HurenTohn makes, as the signed-in user.

use serde::Deserialize;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

const API: &str = "https://api.github.com";

pub type Result<T> = std::result::Result<T, String>;

#[derive(Deserialize, Clone, Debug)]
pub struct User {
    pub login: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Repo {
    pub full_name: String,
    pub name: String,
    pub default_branch: String,
    pub html_url: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Asset {
    pub id: u64,
    pub name: String,
    pub size: u64,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Release {
    pub tag_name: String,
    pub html_url: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    /// The tag without its `v`: `v0.3.2` → `0.3.2`.
    pub fn version(&self) -> &str {
        self.tag_name.trim_start_matches('v')
    }

    /// The installer every plugin release carries for this platform:
    /// `<P>-<v>-macOS.pkg` or `<P>-<v>-Windows-Setup.exe`.
    pub fn installer(&self) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name.ends_with(crate::platform::INSTALLER_SUFFIX))
    }

    pub fn date(&self) -> &str {
        self.published_at.as_deref().map(|d| &d[..d.len().min(10)]).unwrap_or("")
    }
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub sha: String,
    pub date: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(concat!("HurenTohn/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .build()
        .into()
}

fn net(e: ureq::Error) -> String {
    format!("GitHub is not reachable: {e}")
}

#[derive(Clone)]
pub struct Gh {
    agent: ureq::Agent,
    token: String,
}

impl Gh {
    pub fn new(token: String) -> Self {
        Self { agent: agent(), token }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    fn get(&self, url: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>> {
        self.agent
            .get(url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .map_err(net)
    }

    /// GETs JSON. `Ok(None)` for 404/403/451: the repo or file is not there for
    /// this account, which callers treat as "leave it out", not as an error.
    fn json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<Option<T>> {
        let mut resp = self.get(url, "application/vnd.github+json")?;
        match resp.status().as_u16() {
            200 => resp.body_mut().read_json().map(Some).map_err(|e| format!("Unexpected answer from GitHub: {e}")),
            401 => Err("Your GitHub sign-in has expired. Sign in again.".into()),
            403 | 404 | 451 => Ok(None),
            s => Err(format!("GitHub answered {s} for {url}")),
        }
    }

    /// The signed-in user. `Err` when the token is no longer valid.
    pub fn user(&self) -> Result<User> {
        self.json(&format!("{API}/user"))?.ok_or_else(|| "GitHub did not return the signed-in user.".into())
    }

    /// The repo if this account can see it, `None` if it can't.
    pub fn repo(&self, full_name: &str) -> Result<Option<Repo>> {
        self.json(&format!("{API}/repos/{full_name}"))
    }

    /// The latest full release (GitHub skips drafts and prereleases here).
    pub fn latest_release(&self, full_name: &str) -> Result<Option<Release>> {
        self.json(&format!("{API}/repos/{full_name}/releases/latest"))
    }

    pub fn branch_head(&self, full_name: &str, branch: &str) -> Result<Commit> {
        #[derive(Deserialize)]
        struct Who {
            date: String,
        }
        #[derive(Deserialize)]
        struct Inner {
            committer: Who,
        }
        #[derive(Deserialize)]
        struct C {
            sha: String,
            commit: Inner,
        }
        let c: C = self
            .json(&format!("{API}/repos/{full_name}/commits/{branch}"))?
            .ok_or_else(|| format!("{full_name} has no branch {branch}."))?;
        Ok(Commit { sha: c.sha, date: c.commit.committer.date })
    }

    /// A file's raw bytes at `git_ref`, `None` if the repo doesn't have it.
    pub fn file(&self, full_name: &str, path: &str, git_ref: &str) -> Result<Option<Vec<u8>>> {
        let url = format!("{API}/repos/{full_name}/contents/{}?ref={git_ref}", path.trim_start_matches("./"));
        self.raw(&url)
    }

    pub fn readme(&self, full_name: &str) -> Result<Option<String>> {
        Ok(self.raw(&format!("{API}/repos/{full_name}/readme"))?.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    fn raw(&self, url: &str) -> Result<Option<Vec<u8>>> {
        let mut resp = self.get(url, "application/vnd.github.raw")?;
        match resp.status().as_u16() {
            200 => resp
                .body_mut()
                .with_config()
                .limit(40 << 20)
                .read_to_vec()
                .map(Some)
                .map_err(|e| format!("Download from GitHub broke off: {e}")),
            403 | 404 => Ok(None),
            s => Err(format!("GitHub answered {s} for {url}")),
        }
    }

    /// Any image URL from a manifest. GitHub hosts get the token, so pictures
    /// in private repos load too; other hosts don't.
    pub fn url_bytes(&self, url: &str) -> Result<Option<Vec<u8>>> {
        let github =
            ["https://github.com/", "https://raw.githubusercontent.com/", "https://api.github.com/"].iter().any(|p| url.starts_with(p));
        if github {
            return self.raw(url);
        }
        let mut resp = self.agent.get(url).call().map_err(net)?;
        if resp.status().as_u16() != 200 {
            return Ok(None);
        }
        resp.body_mut().with_config().limit(40 << 20).read_to_vec().map(Some).map_err(|e| e.to_string())
    }

    /// Streams a release asset to `dest`, reporting (bytes so far, total).
    /// GitHub redirects to a signed storage URL; ureq drops the Authorization
    /// header on that redirect, which the storage host requires.
    pub fn download_asset(&self, full_name: &str, asset: &Asset, dest: &Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
        let url = format!("{API}/repos/{full_name}/releases/assets/{}", asset.id);
        let resp = self.get(&url, "application/octet-stream")?;
        if resp.status().as_u16() != 200 {
            return Err(format!("GitHub answered {} for {}", resp.status(), asset.name));
        }
        let part = dest.with_extension("part");
        let mut file = std::fs::File::create(&part).map_err(|e| format!("Can't write {}: {e}", part.display()))?;
        let mut reader = resp.into_body().into_reader();
        let mut buf = vec![0u8; 256 * 1024];
        let mut done = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| format!("Download of {} broke off: {e}", asset.name))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            done += n as u64;
            progress(done, asset.size);
        }
        file.flush().map_err(|e| e.to_string())?;
        if asset.size != 0 && done != asset.size {
            return Err(format!("Download of {} is incomplete ({done} of {} bytes).", asset.name, asset.size));
        }
        std::fs::rename(&part, dest).map_err(|e| e.to_string())
    }
}

/// Device flow, step 1: ask GitHub for a code the user types in the browser.
pub fn request_device_code(client_id: &str) -> Result<DeviceCode> {
    let mut resp = agent()
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .send_form([("client_id", client_id), ("scope", "repo")])
        .map_err(net)?;
    let body: serde_json::Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    if let Some(err) = body.get("error_description").or(body.get("error")).and_then(|v| v.as_str()) {
        return Err(format!("GitHub refused the sign-in: {err}"));
    }
    serde_json::from_value(body).map_err(|e| format!("Unexpected answer from GitHub: {e}"))
}

pub enum Poll {
    Pending,
    SlowDown,
    Token(String),
}

/// Device flow, step 2: one poll for the token.
pub fn poll_device_token(client_id: &str, device_code: &str) -> Result<Poll> {
    let mut resp = agent()
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .send_form([("client_id", client_id), ("device_code", device_code), ("grant_type", "urn:ietf:params:oauth:grant-type:device_code")])
        .map_err(net)?;
    let body: serde_json::Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    if let Some(token) = body.get("access_token").and_then(|v| v.as_str()) {
        return Ok(Poll::Token(token.to_string()));
    }
    match body.get("error").and_then(|v| v.as_str()) {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown),
        Some("expired_token") => Err("The code expired. Start the sign-in again.".into()),
        Some("access_denied") => Err("The sign-in was cancelled on GitHub.".into()),
        Some(other) => Err(format!("GitHub refused the sign-in: {other}")),
        None => Err("Unexpected answer from GitHub.".into()),
    }
}
