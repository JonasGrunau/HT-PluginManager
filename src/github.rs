//! The GitHub REST calls HurenTohn makes, as the signed-in user.

use serde::Deserialize;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const API: &str = "https://api.github.com";

pub type Result<T> = std::result::Result<T, Error>;

/// What went wrong talking to GitHub. Callers that only show the message
/// turn it into a `String` with `?` or `to_string()`.
#[derive(Debug, Clone)]
pub enum Error {
    /// No answer: no network, DNS, a timeout.
    Offline(String),
    /// 401: GitHub no longer accepts the token.
    Expired,
    /// 403 / 429 from the rate limiter. Not a reason to drop the login.
    RateLimited(String),
    /// Anything else GitHub answered.
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Offline(m) | Error::RateLimited(m) | Error::Other(m) => f.write_str(m),
            Error::Expired => f.write_str("Your GitHub sign-in has expired. Sign in again."),
        }
    }
}

impl From<Error> for String {
    fn from(e: Error) -> Self {
        e.to_string()
    }
}

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

/// Every call gives up on a silent server: headers within 30 s, a body
/// within 60 s. Release downloads get longer (`download_asset`).
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(concat!("HurenTohn/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .build()
        .into()
}

fn net(e: ureq::Error) -> Error {
    Error::Offline(format!("GitHub is not reachable: {e}"))
}

/// What an answer other than 200 means. `None` for 403/404/451 without a
/// rate limit: the repo or file isn't there for this account, which callers
/// treat as "leave it out", not as an error.
fn refusal(status: u16, headers: &ureq::http::HeaderMap, url: &str) -> Option<Error> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let limited = header("x-ratelimit-remaining") == Some("0") || header("retry-after").is_some();
    match status {
        401 => Some(Error::Expired),
        429 => Some(rate_limited(header("retry-after"), header("x-ratelimit-reset"))),
        403 if limited => Some(rate_limited(header("retry-after"), header("x-ratelimit-reset"))),
        403 | 404 | 451 => None,
        s => Some(Error::Other(format!("GitHub answered {s} for {url}"))),
    }
}

/// "Try again in N min", from `retry-after` (seconds) or `x-ratelimit-reset` (epoch seconds).
fn rate_limited(retry_after: Option<&str>, reset: Option<&str>) -> Error {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let wait = retry_after
        .and_then(|s| s.trim().parse::<u64>().ok())
        .or_else(|| reset.and_then(|s| s.trim().parse::<u64>().ok()).map(|at| at.saturating_sub(now)))
        .unwrap_or(60);
    Error::RateLimited(format!("GitHub's rate limit is reached; try again in {} min.", wait.div_ceil(60).max(1)))
}

/// Percent-encodes a repo path or ref for a URL; `/` stays a separator.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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

    fn request(&self, url: &str, accept: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        self.agent
            .get(url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    fn get(&self, url: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>> {
        self.request(url, accept).call().map_err(net)
    }

    /// GETs JSON. `Ok(None)` when the thing isn't there for this account (see `refusal`).
    fn json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<Option<T>> {
        let mut resp = self.get(url, "application/vnd.github+json")?;
        match resp.status().as_u16() {
            200 => resp.body_mut().read_json().map(Some).map_err(|e| Error::Other(format!("Unexpected answer from GitHub: {e}"))),
            s => refusal(s, resp.headers(), url).map_or(Ok(None), Err),
        }
    }

    /// The signed-in user. `Err(Expired)` when the token is no longer valid.
    pub fn user(&self) -> Result<User> {
        self.json(&format!("{API}/user"))?.ok_or_else(|| Error::Other("GitHub did not return the signed-in user.".into()))
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
            .json(&format!("{API}/repos/{full_name}/commits/{}", encode_path(branch)))?
            .ok_or_else(|| Error::Other(format!("{full_name} has no branch {branch}.")))?;
        Ok(Commit { sha: c.sha, date: c.commit.committer.date })
    }

    /// A file's raw bytes at `git_ref`, `None` if the repo doesn't have it.
    pub fn file(&self, full_name: &str, path: &str, git_ref: &str) -> Result<Option<Vec<u8>>> {
        let url = format!("{API}/repos/{full_name}/contents/{}?ref={}", encode_path(path.trim_start_matches("./")), encode_path(git_ref));
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
                .map_err(|e| Error::Offline(format!("Download from GitHub broke off: {e}"))),
            s => refusal(s, resp.headers(), url).map_or(Ok(None), Err),
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
        resp.body_mut().with_config().limit(40 << 20).read_to_vec().map(Some).map_err(|e| Error::Offline(e.to_string()))
    }

    /// Streams a release asset to `dest`, reporting (bytes so far, total).
    /// GitHub redirects to a signed storage URL; ureq drops the Authorization
    /// header on that redirect, which the storage host requires.
    /// A failed download leaves no `.part` file behind.
    pub fn download_asset(&self, full_name: &str, asset: &Asset, dest: &Path, progress: impl FnMut(u64, u64)) -> Result<()> {
        let part = dest.with_extension("part");
        let result = self
            .download_to(full_name, asset, &part, progress)
            .and_then(|()| std::fs::rename(&part, dest).map_err(|e| Error::Other(format!("Can't write {}: {e}", dest.display()))));
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result
    }

    fn download_to(&self, full_name: &str, asset: &Asset, part: &Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
        let url = format!("{API}/repos/{full_name}/releases/assets/{}", asset.id);
        // Installers are big: the body may take long, but not forever.
        let resp = self
            .request(&url, "application/octet-stream")
            .config()
            .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
            .build()
            .call()
            .map_err(net)?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(refusal(status, resp.headers(), &url)
                .unwrap_or_else(|| Error::Other(format!("GitHub answered {status} for {}", asset.name))));
        }
        let mut file = std::fs::File::create(part).map_err(|e| Error::Other(format!("Can't write {}: {e}", part.display())))?;
        let mut reader = resp.into_body().into_reader();
        let mut buf = vec![0u8; 256 * 1024];
        let mut done = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| Error::Offline(format!("Download of {} broke off: {e}", asset.name)))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| Error::Other(format!("Can't write {}: {e}", part.display())))?;
            done += n as u64;
            progress(done, asset.size);
        }
        file.flush().map_err(|e| Error::Other(e.to_string()))?;
        if asset.size != 0 && done != asset.size {
            return Err(Error::Offline(format!("Download of {} is incomplete ({done} of {} bytes).", asset.name, asset.size)));
        }
        Ok(())
    }
}

/// Device flow, step 1: ask GitHub for a code the user types in the browser.
pub fn request_device_code(client_id: &str) -> Result<DeviceCode> {
    let mut resp = agent()
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .send_form([("client_id", client_id), ("scope", "repo")])
        .map_err(net)?;
    let body: serde_json::Value = resp.body_mut().read_json().map_err(|e| Error::Other(format!("Unexpected answer from GitHub: {e}")))?;
    if let Some(err) = body.get("error_description").or(body.get("error")).and_then(|v| v.as_str()) {
        return Err(Error::Other(format!("GitHub refused the sign-in: {err}")));
    }
    serde_json::from_value(body).map_err(|e| Error::Other(format!("Unexpected answer from GitHub: {e}")))
}

pub enum Poll {
    Pending,
    SlowDown,
    Token(String),
}

/// Device flow, step 2: one poll for the token. `Err(Offline)` is worth
/// polling again; every other error ends the sign-in.
pub fn poll_device_token(client_id: &str, device_code: &str) -> Result<Poll> {
    let mut resp = agent()
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .send_form([("client_id", client_id), ("device_code", device_code), ("grant_type", "urn:ietf:params:oauth:grant-type:device_code")])
        .map_err(net)?;
    let body: serde_json::Value = resp.body_mut().read_json().map_err(|e| Error::Offline(format!("Unexpected answer from GitHub: {e}")))?;
    if let Some(token) = body.get("access_token").and_then(|v| v.as_str()) {
        return Ok(Poll::Token(token.to_string()));
    }
    match body.get("error").and_then(|v| v.as_str()) {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown),
        Some("expired_token") => Err(Error::Other("The code expired. Start the sign-in again.".into())),
        Some("access_denied") => Err(Error::Other("The sign-in was cancelled on GitHub.".into())),
        Some(other) => Err(Error::Other(format!("GitHub refused the sign-in: {other}"))),
        None => Err(Error::Other("Unexpected answer from GitHub.".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ureq::http::{HeaderMap, HeaderValue};

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn only_401_means_the_login_is_gone() {
        assert!(matches!(refusal(401, &headers(&[]), "u"), Some(Error::Expired)));
        assert!(refusal(403, &headers(&[]), "u").is_none());
        assert!(refusal(404, &headers(&[]), "u").is_none());
        assert!(matches!(refusal(500, &headers(&[]), "u"), Some(Error::Other(_))));
    }

    #[test]
    fn rate_limits_are_errors_not_missing_repos() {
        let limited = headers(&[("x-ratelimit-remaining", "0"), ("retry-after", "90")]);
        match refusal(403, &limited, "u") {
            Some(Error::RateLimited(m)) => assert!(m.contains("2 min"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(refusal(429, &headers(&[]), "u"), Some(Error::RateLimited(_))));
    }

    #[test]
    fn paths_are_percent_encoded() {
        assert_eq!(encode_path("docs/my shot.png"), "docs/my%20shot.png");
        assert_eq!(encode_path("a#b?c"), "a%23b%3Fc");
        assert_eq!(encode_path("release/1.0"), "release/1.0");
    }
}
