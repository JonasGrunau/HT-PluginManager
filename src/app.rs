//! App state, the background jobs and the sheet.

use crate::config::{self, Config, InstallRecord, Installs};
use crate::github::{self, DeviceCode, Gh, Release, Repo};
use crate::installed::{self, Installed};
use crate::manifest::{self, Manifest};
use crate::platform::{self, open_url};
use crate::privileged::{self, HelperState};
use crate::theme::{self, Face, KeyKind, col, sheet, size};
use crate::{auth, build};
use egui::{Align2, Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, Ui, pos2, vec2};
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const SUBLINE: &str = concat!("RELEASES AND BUILDS · HT-1 · v", env!("CARGO_PKG_VERSION"));
const ROW_H: f32 = 200.0;
const IMG_W: f32 = 240.0;
const IMG_H: f32 = 160.0;
const ACTION_W: f32 = 196.0;
/// Depth of the shadows at the top and bottom of a scrolled list.
const SHADOW_H: f32 = 40.0;
/// How long the offline screen waits before it tries the stored login again by itself.
const RETRY: Duration = Duration::from_secs(30);
/// How long COPY CODE reads COPIED after a click.
const COPIED_FOR: Duration = Duration::from_secs(2);
/// What a worker reports when it panicked, so nothing waits on it forever.
const CRASHED: &str = "HurenTohn hit an internal error. Try again.";

// ---------------------------------------------------------------------------
// Messages from the worker threads

// A handful of messages per refresh; boxing the big ones buys nothing.
#[allow(clippy::large_enum_variant)]
enum Msg {
    SignedIn {
        gh: Gh,
        login: String,
    },
    SignedOut(Option<String>),
    Offline(String),
    /// GitHub turned the login down (401) in the middle of a session.
    Expired {
        generation: u64,
    },
    Device(DeviceCode),
    Repos {
        generation: u64,
        result: Result<Vec<Repo>, github::Error>,
    },
    Details {
        generation: u64,
        full: String,
        details: Details,
    },
    Image {
        full: String,
        source: String,
        color: ColorImage,
        gray: ColorImage,
    },
    ImageMissing(String),
    /// Progress of a running job.
    Job {
        full: String,
        job: Job,
    },
    /// A job ended. `installed` is what is on disk now, read on the worker.
    Finished {
        full: String,
        job: Job,
        installed: Installed,
    },
    Record {
        full: String,
        record: InstallRecord,
    },
    Helper(HelperState, Option<String>),
}

#[derive(Clone)]
struct Sink {
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Sink {
    fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
        self.ctx.request_repaint();
    }

    fn job(&self, full: &str, job: Job) {
        self.send(Msg::Job { full: full.to_string(), job });
    }

    fn working(&self, full: &str, title: &str, frac: Option<f32>, detail: impl Into<String>) {
        self.job(full, Job::Working { title: title.into(), frac, detail: detail.into() });
    }
}

/// Runs `f` on a worker thread. If it panics, `on_panic` still sends the
/// message the UI is waiting for, so no row or key stays busy forever.
fn spawn(f: impl FnOnce() + Send + 'static, on_panic: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        if std::panic::catch_unwind(AssertUnwindSafe(f)).is_err() {
            on_panic();
        }
    });
}

/// Asks GitHub whose `token` this is (on a worker thread). `save` stores it
/// once GitHub accepts it. Only a 401 drops a stored login: a rate limit or
/// an outage leaves it alone and shows the app offline instead.
fn check_token(sink: &Sink, token: String, save: bool) {
    let gh = Gh::new(token);
    match gh.user() {
        Ok(user) => {
            if save && let Err(e) = auth::save_token(gh.token()) {
                return sink.send(Msg::SignedOut(Some(e)));
            }
            sink.send(Msg::SignedIn { gh, login: user.login });
        }
        Err(github::Error::Expired) => {
            if !save {
                auth::forget_token();
            }
            sink.send(Msg::SignedOut(Some(github::Error::Expired.to_string())));
        }
        Err(e) if save => sink.send(Msg::SignedOut(Some(e.to_string()))),
        Err(e) => sink.send(Msg::Offline(e.to_string())),
    }
}

// ---------------------------------------------------------------------------
// State

#[derive(Clone)]
enum Job {
    Idle,
    Working { title: String, frac: Option<f32>, detail: String },
    Failed { message: String, log: Option<PathBuf> },
    Done(String),
}

/// How a job ends: the sentence for the footer, or what broke and its log.
type Outcome = Result<String, (String, Option<PathBuf>)>;

enum Auth {
    Checking,
    Offline(String),
    SignedOut { device: Option<DeviceCode>, busy: bool, error: Option<String> },
    SignedIn { gh: Gh, login: String },
}

#[derive(PartialEq, Clone, Copy)]
enum Status {
    Loading,
    NoRelease,
    NotInstalled,
    Update,
    UpToDate,
    Ahead,
    MainBuild,
    /// The release request failed and no earlier answer is known.
    Unknown,
}

struct Plugin {
    repo: Repo,
    manifest: Option<Manifest>,
    release: Option<Release>,
    /// A release request got an answer ("no release" counts).
    release_checked: bool,
    details_error: Option<String>,
    installed: Installed,
    image: Option<(TextureHandle, TextureHandle)>,
    /// The manifest's `image` the textures were made from.
    image_source: Option<String>,
    image_missing: bool,
    job: Job,
}

/// What a row already shows, handed to the details worker so a failed
/// request keeps it instead of blanking the row.
#[derive(Clone, Default)]
struct Known {
    manifest: Option<Manifest>,
    /// `Some` once a release request got an answer.
    release: Option<Option<Release>>,
    /// The picture on screen, which isn't fetched again.
    image: Option<String>,
}

/// What one look at a repo found. Where a request failed, the row keeps what
/// it knew: a network blip must not turn UPDATE AVAILABLE into UP TO DATE.
struct Details {
    manifest: Manifest,
    release: Option<Release>,
    release_checked: bool,
    installed: Installed,
    error: Option<String>,
}

impl Details {
    fn resolve(repo_name: &str, manifest: Result<Manifest, String>, release: Result<Option<Release>, String>, known: &Known) -> Self {
        let mut error = None;
        let manifest = manifest.unwrap_or_else(|e| {
            error = Some(e);
            known.manifest.clone().unwrap_or_else(|| Manifest::from_readme("", repo_name))
        });
        let (release, release_checked) = match release {
            Ok(r) => (r, true),
            Err(e) => {
                error.get_or_insert(e);
                (known.release.clone().flatten(), known.release.is_some())
            }
        };
        Self { manifest, release, release_checked, installed: Installed::default(), error }
    }
}

impl Plugin {
    fn new(repo: Repo) -> Self {
        Self {
            repo,
            manifest: None,
            release: None,
            release_checked: false,
            details_error: None,
            installed: Installed::default(),
            image: None,
            image_source: None,
            image_missing: false,
            job: Job::Idle,
        }
    }

    fn known(&self) -> Known {
        Known {
            manifest: self.manifest.clone(),
            release: self.release_checked.then(|| self.release.clone()),
            image: self.image.as_ref().and(self.image_source.clone()),
        }
    }

    fn name(&self) -> &str {
        self.manifest.as_ref().map(|m| m.name.as_str()).unwrap_or(&self.repo.name)
    }

    fn busy(&self) -> bool {
        matches!(self.job, Job::Working { .. })
    }

    /// A build of main is recognised by the record HurenTohn wrote when it
    /// installed it, as long as the bundle on disk still has that version
    /// (compared as versions: the plist may say `0.3`, the scan `0.3.0`).
    fn main_build<'a>(&self, record: Option<&'a InstallRecord>) -> Option<(&'a str, &'a str)> {
        match record? {
            InstallRecord::Main { sha, committed_at, bundle_version } => {
                let recorded = installed::parse_version(bundle_version)?;
                (Some(recorded) == self.installed.version()).then_some((sha.as_str(), committed_at.as_str()))
            }
            InstallRecord::Release { .. } => None,
        }
    }

    fn status(&self, record: Option<&InstallRecord>) -> Status {
        if self.manifest.is_none() {
            return Status::Loading;
        }
        if !self.release_checked {
            return Status::Unknown;
        }
        let release = self.release.as_ref().and_then(|r| installed::parse_version(r.version()));
        if let Some((_, committed_at)) = self.main_build(record) {
            let published = self.release.as_ref().and_then(|r| r.published_at.as_deref()).unwrap_or("");
            return if !published.is_empty() && published > committed_at { Status::Update } else { Status::MainBuild };
        }
        match (self.installed.version(), release) {
            (None, None) => Status::NoRelease,
            (None, Some(_)) => Status::NotInstalled,
            (Some(_), None) => Status::UpToDate,
            (Some(i), Some(r)) if i < r => Status::Update,
            (Some(i), Some(r)) if i == r => Status::UpToDate,
            _ => Status::Ahead,
        }
    }
}

struct Snapshot {
    path: PathBuf,
    settled_frames: u32,
    requested: bool,
    /// `HURENTOHN_SNAPSHOT_STATE=busy` stages an update, a build and a failure
    /// on the real data, so the screenshot loop sees every row state.
    busy: bool,
}

pub struct App {
    cfg: Config,
    installs: Installs,
    sink: Sink,
    rx: Receiver<Msg>,
    auth: Auth,
    plugins: Vec<Plugin>,
    repos_loading: bool,
    repos_error: Option<String>,
    checked_at: Option<Instant>,
    helper: Option<HelperState>,
    helper_busy: bool,
    device_cancel: Arc<AtomicBool>,
    /// Bumped by every refresh and sign-out; worker results from an older
    /// one are dropped when they arrive.
    generation: u64,
    /// Repos with an install or build on a worker. Survives a sign-out, so a
    /// second job for the same repo can't start next to it.
    running: HashSet<String>,
    /// When the offline screen tries the stored login again.
    retry_at: Option<Instant>,
    /// When the device code was last copied by hand.
    copied_at: Option<Instant>,
    /// The rows with an update waiting, worked out once per frame.
    waiting: Vec<usize>,
    /// What the hovered control does, collected while drawing; the footer shows it.
    help: Option<String>,
    /// The last thing that happened, for the footer at rest.
    notice: Option<String>,
    snapshot: Option<Snapshot>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let (tx, rx) = channel();
        let sink = Sink { tx, ctx: cc.egui_ctx.clone() };
        let (cfg, cfg_error) = Config::load();
        let mut app = Self {
            cfg,
            installs: Installs::load(),
            sink,
            rx,
            auth: Auth::Checking,
            plugins: Vec::new(),
            repos_loading: false,
            repos_error: None,
            checked_at: None,
            helper: None,
            helper_busy: false,
            device_cancel: Arc::new(AtomicBool::new(false)),
            generation: 0,
            running: HashSet::new(),
            retry_at: None,
            copied_at: None,
            waiting: Vec::new(),
            help: None,
            notice: cfg_error,
            snapshot: std::env::var_os("HURENTOHN_SNAPSHOT").map(|p| Snapshot {
                path: p.into(),
                settled_frames: 0,
                requested: false,
                busy: std::env::var("HURENTOHN_SNAPSHOT_STATE").is_ok_and(|s| s == "busy"),
            }),
        };
        app.check_helper();
        // HURENTOHN_SNAPSHOT_STATE=signed-out renders the sign-in page without touching the stored login,
        // =device the page with a (made-up) device code.
        let shot_state = std::env::var("HURENTOHN_SNAPSHOT_STATE").ok().filter(|_| app.snapshot.is_some());
        match shot_state.as_deref() {
            Some("signed-out") => app.auth = Auth::SignedOut { device: None, busy: false, error: None },
            Some("device") => {
                let device = DeviceCode {
                    device_code: String::new(),
                    user_code: "WDJB-MJHT".into(),
                    verification_uri: "https://github.com/login/device".into(),
                    expires_in: 899,
                    interval: 5,
                };
                app.auth = Auth::SignedOut { device: Some(device), busy: false, error: None };
            }
            _ => app.validate_stored(),
        }
        app
    }

    // -----------------------------------------------------------------------
    // Workers

    fn check_helper(&self) {
        let sink = self.sink.clone();
        spawn(move || sink.send(Msg::Helper(privileged::helper_state(), None)), || {});
    }

    fn install_helper(&mut self) {
        self.helper_busy = true;
        let (sink, crash) = (self.sink.clone(), self.sink.clone());
        spawn(
            move || {
                let note = privileged::install_helper().err();
                sink.send(Msg::Helper(privileged::helper_state(), note));
            },
            move || crash.send(Msg::Helper(HelperState::Missing, Some(CRASHED.into()))),
        );
    }

    /// Signs in with the stored login. The keychain is read on the worker:
    /// it can stall on a system prompt.
    fn validate_stored(&mut self) {
        self.auth = Auth::Checking;
        self.retry_at = None;
        let (sink, crash) = (self.sink.clone(), self.sink.clone());
        spawn(
            move || match auth::load_token() {
                Some(token) => check_token(&sink, token, false),
                None => sink.send(Msg::SignedOut(None)),
            },
            move || crash.send(Msg::Offline(CRASHED.into())),
        );
    }

    fn start_device_login(&mut self) {
        let client_id = self.cfg.client_id().to_string();
        self.auth = Auth::SignedOut { device: None, busy: true, error: None };
        self.device_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.device_cancel.clone();
        let (sink, crash) = (self.sink.clone(), self.sink.clone());
        spawn(
            move || {
                let code = match github::request_device_code(&client_id) {
                    Ok(c) => c,
                    Err(e) => return sink.send(Msg::SignedOut(Some(e.to_string()))),
                };
                sink.send(Msg::Device(code.clone()));
                let mut interval = code.interval.max(5);
                let deadline = Instant::now() + Duration::from_secs(code.expires_in);
                while Instant::now() < deadline {
                    std::thread::sleep(Duration::from_secs(interval));
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    match github::poll_device_token(&client_id, &code.device_code) {
                        Ok(github::Poll::Pending) => {}
                        Ok(github::Poll::SlowDown) => interval += 5,
                        // A dropped connection isn't a refusal: ask again until the code expires.
                        Err(github::Error::Offline(_)) => {}
                        Ok(github::Poll::Token(token)) => {
                            // Cancelled while this poll was under way: the user has moved on.
                            if cancel.load(Ordering::Relaxed) {
                                return;
                            }
                            return check_token(&sink, token, true);
                        }
                        Err(e) => return sink.send(Msg::SignedOut(Some(e.to_string()))),
                    }
                }
                if !cancel.load(Ordering::Relaxed) {
                    sink.send(Msg::SignedOut(Some("The code expired. Start the sign-in again.".into())));
                }
            },
            move || crash.send(Msg::SignedOut(Some(CRASHED.into()))),
        );
    }

    /// `gh auth token` runs on the worker: it can take a while, and the UI
    /// thread doesn't wait for subprocesses.
    fn gh_cli_login(&mut self) {
        self.auth = Auth::SignedOut { device: None, busy: true, error: None };
        let (sink, crash) = (self.sink.clone(), self.sink.clone());
        spawn(
            move || match auth::gh_cli_token() {
                Some(token) => check_token(&sink, token, true),
                None => {
                    sink.send(Msg::SignedOut(Some("No GitHub CLI login found. Run `gh auth login` first, or sign in with GitHub.".into())))
                }
            },
            move || crash.send(Msg::SignedOut(Some(CRASHED.into()))),
        );
    }

    fn sign_out(&mut self) {
        // Here, not on a worker: a sign-in right after must not race the delete.
        auth::forget_token();
        self.drop_session(None);
        self.notice = Some("Signed out. The login is gone from the keychain.".into());
    }

    /// GitHub turned the login down (401): forget it and go back to sign-in.
    fn expired(&mut self) {
        auth::forget_token();
        self.drop_session(Some(github::Error::Expired.to_string()));
    }

    /// Back to the sign-in page. Results still on their way from this
    /// session are dropped when they arrive (`generation`).
    fn drop_session(&mut self, error: Option<String>) {
        self.generation += 1;
        self.plugins.clear();
        self.checked_at = None;
        self.repos_loading = false;
        self.repos_error = None;
        self.auth = Auth::SignedOut { device: None, busy: false, error };
    }

    fn gh(&self) -> Option<Gh> {
        match &self.auth {
            Auth::SignedIn { gh, .. } => Some(gh.clone()),
            _ => None,
        }
    }

    /// Asks GitHub which whitelisted repos this account can see. The ones it
    /// can't are simply not listed.
    fn refresh(&mut self) {
        let Some(gh) = self.gh() else { return };
        self.generation += 1;
        let generation = self.generation;
        self.repos_loading = true;
        self.repos_error = None;
        // An entry that isn't owner/repo would fail the whole request: skip it and say so.
        let (whitelist, junk): (Vec<String>, Vec<String>) =
            self.cfg.whitelist.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).partition(|s| config::is_repo_name(s));
        if !junk.is_empty() {
            self.notice = Some(format!("Skipped {} in the whitelist of config.json: not owner/repo.", junk.join(", ")));
        }
        let (sink, crash) = (self.sink.clone(), self.sink.clone());
        spawn(
            move || {
                let results: Vec<github::Result<Option<Repo>>> = std::thread::scope(|s| {
                    let handles: Vec<_> = whitelist.iter().map(|full| s.spawn(|| gh.repo(full))).collect();
                    handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(github::Error::Other(CRASHED.into())))).collect()
                });
                let mut repos: Vec<Repo> = Vec::new();
                for r in results {
                    match r {
                        // The same repo twice (another spelling in the whitelist) is one row.
                        Ok(Some(repo)) if !repos.iter().any(|known| known.full_name == repo.full_name) => repos.push(repo),
                        Ok(_) => {}
                        Err(e) => return sink.send(Msg::Repos { generation, result: Err(e) }),
                    }
                }
                sink.send(Msg::Repos { generation, result: Ok(repos) });
            },
            move || crash.send(Msg::Repos { generation, result: Err(github::Error::Other(CRASHED.into())) }),
        );
    }

    /// Reads a repo's manifest, latest release and installed bundles, then
    /// its picture unless that is already on screen. Always answers with
    /// `Details`, even after a panic, so the row never stays CHECKING.
    fn fetch_details(&self, repo: Repo, known: Known) {
        let Some(gh) = self.gh() else { return };
        let generation = self.generation;
        let (sink, crash) = (self.sink.clone(), self.sink.clone());
        let (crash_repo, crash_known) = (repo.clone(), known.clone());
        spawn(
            move || {
                let full = repo.full_name.clone();
                let manifest = match gh.file(&full, manifest::FILE, &repo.default_branch) {
                    Ok(Some(bytes)) => Ok(Manifest::parse(&bytes, &repo.name)),
                    Ok(None) => gh.readme(&full).map(|readme| {
                        let mut m = Manifest::from_readme(&readme.unwrap_or_default(), &repo.name);
                        if m.description.is_empty() {
                            m.description = repo.description.clone().unwrap_or_default();
                        }
                        Ok(m)
                    }),
                    Err(e) => Err(e),
                };
                let release = gh.latest_release(&full);
                if matches!(manifest, Err(github::Error::Expired)) || matches!(release, Err(github::Error::Expired)) {
                    return sink.send(Msg::Expired { generation });
                }
                // An unreachable GitHub and a broken hurentohn.json both keep what the row knew.
                let manifest = manifest.map_err(String::from).and_then(|m| m);
                let mut details = Details::resolve(&repo.name, manifest, release.map_err(String::from), &known);
                details.installed = Installed::scan(&details.manifest.bundles);
                let image = details.manifest.image.clone();
                sink.send(Msg::Details { generation, full: full.clone(), details });

                if known.image.as_deref() == Some(image.as_str()) {
                    return;
                }
                let bytes = if image.is_empty() {
                    None
                } else if image.starts_with("https://") || image.starts_with("http://") {
                    gh.url_bytes(&image).ok().flatten()
                } else {
                    gh.file(&full, &image, &repo.default_branch).ok().flatten()
                };
                match bytes.and_then(|b| decode(&b)) {
                    Some((color, gray)) => sink.send(Msg::Image { full, source: image, color, gray }),
                    None => sink.send(Msg::ImageMissing(full)),
                }
            },
            move || {
                let mut details = Details::resolve(&crash_repo.name, Err(CRASHED.into()), Err(CRASHED.into()), &crash_known);
                details.installed = Installed::scan(&details.manifest.bundles);
                crash.send(Msg::Details { generation, full: crash_repo.full_name, details });
            },
        );
    }

    /// Runs an install or build on a worker. However it ends, a panic
    /// included, the row gets one `Finished` with what is on disk now, read
    /// on the worker rather than the UI thread.
    fn spawn_job(&mut self, full: String, bundles: Vec<String>, body: impl FnOnce(&Sink) -> Outcome + Send + 'static) {
        self.running.insert(full.clone());
        let sink = self.sink.clone();
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| body(&sink))).unwrap_or_else(|_| Err((CRASHED.into(), None)));
            let job = match outcome {
                Ok(text) => Job::Done(text),
                Err((message, log)) => Job::Failed { message, log },
            };
            let installed = Installed::scan(&bundles);
            sink.send(Msg::Finished { full, job, installed });
        });
    }

    fn start_install(&mut self, idx: usize) {
        let Some(gh) = self.gh() else { return };
        let p = &mut self.plugins[idx];
        if self.running.contains(&p.repo.full_name) {
            return;
        }
        let (Some(release), Some(manifest)) = (p.release.clone(), p.manifest.as_ref()) else { return };
        let bundles = manifest.bundles.clone();
        let Some(asset) = release.installer().cloned() else {
            let message = format!("Release {} has no *{} to install.", release.tag_name, platform::INSTALLER_SUFFIX);
            p.job = Job::Failed { message, log: None };
            return;
        };
        let full = p.repo.full_name.clone();
        let name = p.name().to_string();
        p.job = Job::Working { title: "DOWNLOADING".into(), frac: Some(0.0), detail: asset.name.clone() };
        self.spawn_job(full.clone(), bundles, move |sink| {
            let dir = config::download_dir();
            let _ = std::fs::create_dir_all(&dir);
            let dest = dir.join(&asset.name);
            let cached = asset.size > 0 && std::fs::metadata(&dest).is_ok_and(|m| m.len() == asset.size);
            if !cached {
                let mut last = -1i32;
                gh.download_asset(&full, &asset, &dest, |done, total| {
                    let pct = (done as f64 / total.max(1) as f64 * 100.0) as i32;
                    if pct != last {
                        last = pct;
                        sink.working(&full, "DOWNLOADING", Some(pct as f32 / 100.0), asset.name.clone());
                    }
                })
                .map_err(|e| (e.to_string(), None))?;
            }
            sink.working(&full, "INSTALLING", None, platform::INSTALL_DETAIL);
            let result = privileged::install_release(&dest, &name);
            // Downloaded again next time: a damaged file isn't retried forever, and the cache doesn't grow.
            let _ = std::fs::remove_file(&dest);
            result.map_err(|message| (message, None))?;
            privileged::refresh_audio_units();
            let record = InstallRecord::Release { tag: release.tag_name.clone(), bundle_version: release.version().to_string() };
            sink.send(Msg::Record { full: full.clone(), record });
            Ok(format!("{name} {} is installed.", release.version()))
        });
    }

    fn start_uninstall(&mut self, idx: usize) {
        let p = &mut self.plugins[idx];
        if self.running.contains(&p.repo.full_name) {
            return;
        }
        let Some(manifest) = p.manifest.as_ref() else { return };
        let bundles = manifest.bundles.clone();
        let full = p.repo.full_name.clone();
        let name = p.name().to_string();
        p.job = Job::Working {
            title: "UNINSTALLING".into(),
            frac: None,
            detail: format!("{} from {}", bundles.join(", "), platform::SYSTEM_DIR_LABEL),
        };
        self.spawn_job(full, bundles.clone(), move |_| {
            privileged::uninstall_bundles(&bundles, &name).map_err(|message| (message, None))?;
            privileged::refresh_audio_units();
            Ok(format!("{name} is uninstalled."))
        });
    }

    fn start_build(&mut self, idx: usize) {
        let Some(gh) = self.gh() else { return };
        let juce = self.cfg.juce();
        let p = &mut self.plugins[idx];
        if self.running.contains(&p.repo.full_name) {
            return;
        }
        let Some(manifest) = p.manifest.clone() else { return };
        let full = p.repo.full_name.clone();
        let branch = p.repo.default_branch.clone();
        let name = p.name().to_string();
        p.job = Job::Working { title: "FETCHING MAIN".into(), frac: None, detail: format!("{full}@{branch}") };
        let bundles = manifest.bundles.clone();
        self.spawn_job(full.clone(), bundles, move |sink| {
            let head = gh.branch_head(&full, &branch).map_err(|e| (e.to_string(), None))?;
            let short = |sha: &str| sha[..7.min(sha.len())].to_string();
            let built = {
                // One build at a time; the lock is let go before the install,
                // so a password prompt doesn't hold up the next build.
                let _turn = build::wait_turn(|| {
                    sink.working(&full, "WAITING FOR BUILD", None, "Another plugin is building; this one starts after it.")
                });
                // Ninja prints a line per step; the row needs a few updates a second, not thousands.
                let mut sent: Option<Instant> = None;
                build::build_main(gh.token(), &full, &branch, &head.sha, &manifest, juce.as_deref(), |step| {
                    let frequent = matches!(step, build::Step::Compiling { done, total, .. } if done < total)
                        || matches!(step, build::Step::Output(_));
                    if frequent && sent.is_some_and(|t| t.elapsed() < Duration::from_millis(100)) {
                        return;
                    }
                    sent = Some(Instant::now());
                    match step {
                        build::Step::Fetching => sink.working(&full, "FETCHING MAIN", None, format!("{branch} @ {}", short(&head.sha))),
                        build::Step::Configuring => sink.working(&full, "CONFIGURING", None, "cmake"),
                        build::Step::Output(line) => sink.working(&full, "BUILDING", None, line),
                        build::Step::Compiling { done, total, line } => {
                            let detail = line.split_once("] ").map(|(_, rest)| rest).unwrap_or(&line).to_string();
                            sink.working(&full, &format!("COMPILING {done}/{total}"), Some(done as f32 / total.max(1) as f32), detail);
                        }
                    }
                })
            }
            .map_err(|e| (e.message, Some(e.log)))?;
            sink.working(&full, "INSTALLING", None, format!("into {}", platform::SYSTEM_DIR_LABEL));
            privileged::install_bundles(&built.bundles, &name).map_err(|message| (message, Some(built.log.clone())))?;
            privileged::refresh_audio_units();
            let bundle_version = built.bundles.first().and_then(|b| installed::bundle_version(b)).unwrap_or_default();
            let record = InstallRecord::Main { sha: built.sha.clone(), committed_at: head.date, bundle_version };
            sink.send(Msg::Record { full: full.clone(), record });
            Ok(format!("{name} from main @ {} is installed.", short(&built.sha)))
        });
    }

    // -----------------------------------------------------------------------
    // Messages

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::SignedIn { gh, login } => {
                    self.auth = Auth::SignedIn { gh, login };
                    self.refresh();
                    if !self.cfg.helper_offered && self.helper == Some(HelperState::Missing) && self.snapshot.is_none() {
                        self.cfg.helper_offered = true;
                        self.cfg.save();
                        self.install_helper();
                    }
                }
                Msg::SignedOut(error) => self.auth = Auth::SignedOut { device: None, busy: false, error },
                Msg::Offline(e) => self.auth = Auth::Offline(e),
                Msg::Expired { generation } => {
                    if generation == self.generation {
                        self.expired();
                    }
                }
                Msg::Device(code) => {
                    ctx.copy_text(code.user_code.clone());
                    open_url(&code.verification_uri);
                    self.auth = Auth::SignedOut { device: Some(code), busy: false, error: None };
                }
                // From a refresh before the last refresh or sign-out.
                Msg::Repos { generation, .. } if generation != self.generation => {}
                Msg::Repos { result: Ok(repos), .. } => {
                    self.repos_loading = false;
                    self.checked_at = Some(Instant::now());
                    let mut old: Vec<Plugin> = std::mem::take(&mut self.plugins);
                    for repo in repos {
                        let mut p = match old.iter().position(|p| p.repo.full_name == repo.full_name) {
                            Some(i) => old.swap_remove(i),
                            None => Plugin::new(repo.clone()),
                        };
                        // Started before a sign-out and still going: the row says so until it ends.
                        if self.running.contains(&repo.full_name) && !p.busy() {
                            p.job = Job::Working {
                                title: "STILL RUNNING".into(),
                                frac: None,
                                detail: "Started before the last sign-in.".into(),
                            };
                        }
                        p.repo = repo.clone();
                        let known = p.known();
                        self.plugins.push(p);
                        self.fetch_details(repo, known);
                    }
                }
                Msg::Repos { result: Err(github::Error::Expired), .. } => self.expired(),
                Msg::Repos { result: Err(e), .. } => {
                    self.repos_loading = false;
                    // Rows on screen stay; the footer says the refresh failed.
                    if self.plugins.is_empty() {
                        self.repos_error = Some(e.to_string());
                    } else {
                        self.notice = Some(format!("Refresh failed: {e}"));
                    }
                }
                Msg::Details { generation, full, details } => {
                    if generation == self.generation
                        && let Some(p) = self.plugin(&full)
                    {
                        p.manifest = Some(details.manifest);
                        p.release = details.release;
                        p.release_checked = details.release_checked;
                        p.installed = details.installed;
                        p.details_error = details.error;
                    }
                }
                Msg::Image { full, source, color, gray } => {
                    let c = ctx.load_texture(format!("{full}-color"), color, egui::TextureOptions::LINEAR);
                    let g = ctx.load_texture(format!("{full}-gray"), gray, egui::TextureOptions::LINEAR);
                    if let Some(p) = self.plugin(&full) {
                        p.image = Some((c, g));
                        p.image_source = Some(source);
                        p.image_missing = false;
                    }
                }
                Msg::ImageMissing(full) => {
                    if let Some(p) = self.plugin(&full) {
                        p.image_missing = true;
                    }
                }
                Msg::Job { full, job } => {
                    if let Some(p) = self.plugin(&full) {
                        p.job = job;
                    }
                }
                Msg::Finished { full, job, installed } => {
                    self.running.remove(&full);
                    if let Job::Done(text) = &job {
                        self.notice = Some(text.clone());
                    }
                    // A record of how a plug-in got installed outlives no uninstall.
                    if installed.system.is_empty() && self.installs.0.remove(&full).is_some() {
                        self.installs.save();
                    }
                    if let Some(p) = self.plugin(&full) {
                        p.installed = installed;
                        p.job = if matches!(job, Job::Done(_)) { Job::Idle } else { job };
                    }
                }
                Msg::Record { full, record } => {
                    self.installs.0.insert(full, record);
                    self.installs.save();
                }
                Msg::Helper(state, note) => {
                    self.helper = Some(state);
                    self.helper_busy = false;
                    if let Some(note) = note {
                        self.notice = Some(format!("Silent installs are off: {note} Every install asks for the password instead."));
                    } else if state == HelperState::Ready && self.notice.is_none() {
                        self.notice = Some("Silent installs are on: signed releases install without a password.".into());
                    }
                    // Signed in before the helper check returned: offer it now.
                    if state == HelperState::Missing
                        && !self.cfg.helper_offered
                        && matches!(self.auth, Auth::SignedIn { .. })
                        && self.snapshot.is_none()
                    {
                        self.cfg.helper_offered = true;
                        self.cfg.save();
                        self.install_helper();
                    }
                }
            }
        }
    }

    fn plugin(&mut self, full: &str) -> Option<&mut Plugin> {
        self.plugins.iter_mut().find(|p| p.repo.full_name == full)
    }

    fn updates(&self) -> Vec<usize> {
        (0..self.plugins.len())
            .filter(|&i| {
                let p = &self.plugins[i];
                !p.busy() && p.status(self.installs.0.get(&p.repo.full_name)) == Status::Update
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Drawing

    /// One 880-wide sheet scaled as a whole: the zoom follows the window width.
    fn fit_sheet(&self, ctx: &egui::Context) {
        let width = ctx.input(|i| i.content_rect().width()) * ctx.zoom_factor();
        let target = (width / sheet::W).clamp(0.75, 2.0);
        if (target - ctx.zoom_factor()).abs() > 0.002 {
            ctx.set_zoom_factor(target);
        }
    }

    fn masthead(&mut self, ui: &mut Ui, x0: f32) {
        let painter = ui.painter().clone();
        let top = ui.max_rect().top();
        // The logo: the LEFT/RIGHT wordmark, then what it is, in the same face and size.
        let mark = theme::wordmark(&painter, pos2(x0 + sheet::LEFT, top + 14.0), "HUREN", "TOHN", size::WORDMARK);
        let name_font = theme::font(Face::Label, size::WORDMARK);
        theme::text(
            &painter,
            pos2(mark.right() + 9.0, top + 14.0),
            Align2::LEFT_TOP,
            "PLUGIN MANAGER",
            name_font,
            col::INK_SOFT,
            -0.01 * size::WORDMARK,
        );
        theme::mono(&painter, pos2(x0 + sheet::LEFT, top + 46.0), Align2::LEFT_TOP, SUBLINE, size::SUBLINE, col::INK_SOFT);
        painter.hline(x0 + sheet::LEFT..=x0 + sheet::RIGHT, top + sheet::MAST_RULE_Y, Stroke::new(1.5, col::INK_FAINT));

        let Auth::SignedIn { login, .. } = &self.auth else { return };
        let login = login.clone();
        // Right to left: the one primary action (UPDATE ALL, only while
        // something waits) far right, REFRESH, then the account keys. Who is
        // signed in is in the footer.
        let y = top + 24.0;
        let mut x = x0 + sheet::RIGHT;
        let mut place = |label: &str, gap: f32| {
            let w = theme::key_width(&painter, label);
            let r = Rect::from_min_size(pos2(x - w, y), vec2(w, 26.0));
            x = r.left() - gap;
            r
        };
        let updates = self.waiting.clone();
        if !updates.is_empty() {
            let r = place("UPDATE ALL", sheet::SM);
            let resp = theme::key(ui, r, "UPDATE ALL", KeyKind::Lit, true);
            if resp.hovered() {
                self.help = Some(format!("Installs all {} updates, one after another, without installer windows.", updates.len()));
            }
            if resp.clicked() {
                for &i in &updates {
                    self.start_install(i);
                }
            }
        }
        let refresh_label = if self.repos_loading { "CHECKING" } else { "REFRESH" };
        let r = theme::key(ui, place(refresh_label, sheet::GROUP_GAP), refresh_label, KeyKind::Outline, !self.repos_loading);
        if r.hovered() {
            self.help = Some("Asks GitHub again for new releases and re-reads what is installed.".into());
        }
        if r.clicked() {
            self.refresh();
        }
        // A job keeps running after a sign-out but its row would be gone: wait for it.
        let busy = self.plugins.iter().any(Plugin::busy);
        let r = theme::key(ui, place("SIGN OUT", sheet::SM), "SIGN OUT", KeyKind::Quiet, !busy);
        if r.hovered() {
            self.help = Some(if busy {
                "Wait for the running install or build to finish before signing out.".into()
            } else {
                format!("Signs {login} out and forgets the login stored on this computer.")
            });
        }
        if r.clicked() {
            self.sign_out();
            return;
        }
        if self.helper == Some(HelperState::Missing) {
            let label = if self.helper_busy { "WAITING FOR PASSWORD" } else { "ALLOW SILENT INSTALLS" };
            let resp = theme::key(ui, place(label, sheet::SM), label, KeyKind::Outline, !self.helper_busy);
            if resp.hovered() {
                self.help = Some("Asks for your password once; after that signed releases install without asking.".into());
            }
            if resp.clicked() {
                self.install_helper();
            }
        }
    }

    /// The app's own title bar: drag to move, double-click to zoom, and on
    /// Windows house-style minimize / zoom / close keys on the sheet's right margin.
    fn title_bar(&mut self, ui: &mut Ui, x0: f32) {
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();
        let painter = ui.painter().clone();
        // Window chrome doesn't scale with the sheet: sizes here are window points.
        let pt = 1.0 / ctx.zoom_factor();
        painter.rect_filled(rect, egui::CornerRadius::ZERO, col::WELL);
        painter.hline(rect.x_range(), rect.bottom() - 0.5 * pt, Stroke::new(pt, col::HAIRLINE));
        // No title text: the masthead right under it already says what this is.
        // macOS puts its traffic lights on the left of this bar; Windows gets our keys on the right.

        let bar = ui.interact(rect, egui::Id::new("title-bar"), Sense::click_and_drag());
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if bar.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if bar.double_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }

        if cfg!(target_os = "macos") {
            return;
        }
        let size = vec2(26.0, 18.0) * pt;
        let mut x = x0 + sheet::RIGHT;
        for glyph in [theme::WindowGlyph::Close, theme::WindowGlyph::Zoom, theme::WindowGlyph::Minimize] {
            let r = Rect::from_min_size(pos2(x - size.x, rect.center().y - size.y / 2.0), size);
            x = r.left() - sheet::XS * pt;
            let resp = theme::window_key(ui, r, glyph, pt);
            if resp.hovered() {
                self.help = Some(
                    match glyph {
                        theme::WindowGlyph::Close => "Closes HurenTohn. Running downloads and builds stop.",
                        theme::WindowGlyph::Zoom => "Fills the screen with the window, or brings it back.",
                        theme::WindowGlyph::Minimize => "Puts the window away into the Dock / taskbar.",
                    }
                    .into(),
                );
            }
            if resp.clicked() {
                ctx.send_viewport_cmd(match glyph {
                    theme::WindowGlyph::Close => egui::ViewportCommand::Close,
                    theme::WindowGlyph::Zoom => egui::ViewportCommand::Maximized(!maximized),
                    theme::WindowGlyph::Minimize => egui::ViewportCommand::Minimized(true),
                });
            }
        }
    }

    /// The sign-in page: one block centred in the space between masthead and
    /// footer, so a big window doesn't leave it in a corner.
    fn signed_out(&mut self, ui: &mut Ui, x0: f32) {
        let painter = ui.painter().clone();
        let (device, busy, error) = match &self.auth {
            Auth::SignedOut { device, busy, error } => (device.clone(), *busy, error.clone()),
            _ => return,
        };
        let area = ui.max_rect();
        let cx = x0 + sheet::W / 2.0;
        let width = 520.0;
        let centred = |text: &str, font: egui::FontId, colour: Color32, rows: usize| {
            // Balanced lines: as many as the text needs at `width`, each about
            // as long as the others, so no word hangs alone on the last one.
            let one_line = theme::text_width(&painter, text, font.clone(), 0.0);
            let lines = (one_line / width).ceil().max(1.0);
            let max_width = (one_line / lines + 24.0).min(width);
            let mut job = egui::text::LayoutJob::default();
            job.append(text, 0.0, egui::TextFormat { font_id: font, color: colour, ..Default::default() });
            job.wrap = egui::text::TextWrapping { max_width, max_rows: rows, break_anywhere: false, overflow_character: Some('…') };
            job.halign = egui::Align::Center;
            painter.layout_job(job)
        };
        let heading = centred("Sign in with GitHub to see your plugins.", theme::font(Face::Text, 22.0), col::INK, 1);
        let body = centred(
            "All your plugins in one place. See what's new, update with one click, or try the latest build before it's released.",
            theme::font(Face::Text, size::BODY),
            col::INK_SOFT,
            4,
        );
        // Device sign-in needs the OAuth app's client ID; until there is one,
        // the gh CLI login is the only (and so the lit) way in.
        let has_client = !self.cfg.client_id().is_empty();
        let error = error.map(|e| centred(&e, theme::font(Face::Text, size::BODY), col::INK, 3));

        // Heights first, so the block can be centred as a whole.
        let action_h = if device.is_some() {
            72.0 + sheet::MD + 14.0 + sheet::LG + 30.0
        } else if has_client {
            30.0 + sheet::SM + 30.0
        } else {
            30.0
        };
        let mut total = heading.size().y + sheet::MD + body.size().y + sheet::XL + action_h;
        if let Some(g) = &error {
            total += sheet::LG + g.size().y;
        }
        let mut y = (area.center().y - total / 2.0).max(area.top() + sheet::XL).round();
        let put = |g: std::sync::Arc<egui::Galley>, colour: Color32, y: &mut f32, gap_after: f32| {
            let h = g.size().y;
            // halign = Center lays rows out around x = 0.
            painter.galley(pos2(cx, *y), g, colour);
            *y += h + gap_after;
        };
        put(heading, col::INK, &mut y, sheet::MD);
        put(body, col::INK_SOFT, &mut y, sheet::XL);

        // A row of keys, centred.
        let row = |labels: &[(&str, f32)], y: f32| {
            let total: f32 = labels.iter().map(|(_, w)| w).sum::<f32>() + sheet::SM * (labels.len() as f32 - 1.0);
            let mut x = cx - total / 2.0;
            labels
                .iter()
                .map(|(_, w)| {
                    let r = Rect::from_min_size(pos2(x, y), vec2(*w, 30.0));
                    x = r.right() + sheet::SM;
                    r
                })
                .collect::<Vec<_>>()
        };

        if let Some(code) = device {
            // The code copies on a click, on the well or on COPY CODE: the copy
            // made when it arrived is gone once something else is copied.
            let well = Rect::from_min_size(pos2(cx - 160.0, y), vec2(320.0, 72.0));
            let well_r = ui.interact(well, ui.id().with("device_code"), Sense::click());
            if well_r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let edge = if well_r.hovered() { col::INK_FAINT } else { col::HAIRLINE };
            painter.rect_filled(well, theme::radius(), col::WELL);
            painter.rect_stroke(well, theme::radius(), Stroke::new(1.0, edge), egui::StrokeKind::Inside);
            theme::text(
                &painter,
                well.center(),
                Align2::CENTER_CENTER,
                &code.user_code,
                theme::font(Face::MonoStrong, size::READOUT),
                col::INK,
                2.0,
            );
            y = well.bottom() + sheet::MD;
            theme::caption(
                &painter,
                pos2(cx, y),
                Align2::CENTER_TOP,
                "Paste this code on the GitHub page that just opened. It is already on your clipboard.",
                size::CONTROL,
                col::INK_SOFT,
            );
            y += 14.0 + sheet::LG;
            let copied = self.copied_at.is_some_and(|t| t.elapsed() < COPIED_FOR);
            let copy_label = if copied { "COPIED" } else { "COPY CODE" };
            let keys = row(
                &[
                    (copy_label, theme::key_width(&painter, "COPY CODE")),
                    ("OPEN GITHUB AGAIN", theme::key_width(&painter, "OPEN GITHUB AGAIN")),
                    ("CANCEL", theme::key_width(&painter, "CANCEL")),
                ],
                y,
            );
            if theme::key(ui, keys[0], copy_label, KeyKind::Outline, true).clicked() || well_r.clicked() {
                ui.ctx().copy_text(code.user_code.clone());
                self.copied_at = Some(Instant::now());
            }
            if let Some(t) = self.copied_at.filter(|t| t.elapsed() < COPIED_FOR) {
                ui.ctx().request_repaint_after(COPIED_FOR.saturating_sub(t.elapsed()));
            }
            if theme::key(ui, keys[1], "OPEN GITHUB AGAIN", KeyKind::Outline, true).clicked() {
                open_url(&code.verification_uri);
            }
            if theme::key(ui, keys[2], "CANCEL", KeyKind::Quiet, true).clicked() {
                self.device_cancel.store(true, Ordering::Relaxed);
                self.auth = Auth::SignedOut { device: None, busy: false, error: None };
            }
            self.help = self.help.take().or(Some(format!("Waiting for GitHub. The code works for {} minutes.", code.expires_in / 60)));
            return;
        }

        // The keys stack, one width: GitHub on top, the gh CLI under it.
        let label = if busy { "SIGNING IN" } else { "SIGN IN WITH GITHUB" };
        let key_w = theme::key_width(&painter, "SIGN IN WITH GITHUB").max(theme::key_width(&painter, "USE GH CLI LOGIN")).max(220.0);
        let mut keys = vec![Rect::from_min_size(pos2(cx - key_w / 2.0, y), vec2(key_w, 30.0))];
        if has_client {
            keys.push(keys[0].translate(vec2(0.0, 30.0 + sheet::SM)));
            y += 30.0 + sheet::SM;
            let r = theme::key(ui, keys[0], label, KeyKind::Lit, !busy);
            if r.hovered() {
                self.help = Some("Opens GitHub in your browser with a one-time code; no password goes through this app.".into());
            }
            if r.clicked() {
                self.start_device_login();
            }
        }
        let cli_kind = if has_client { KeyKind::Secondary } else { KeyKind::Lit };
        let r = theme::key(ui, *keys.last().unwrap(), "USE GH CLI LOGIN", cli_kind, !busy);
        if r.hovered() {
            self.help = Some("Takes the token of your `gh auth login` session and keeps a copy on this computer.".into());
        }
        if r.clicked() {
            self.gh_cli_login();
        }
        y += 30.0 + sheet::LG;
        if let Some(g) = error {
            put(g, col::INK, &mut y, 0.0);
        }
    }

    fn centered_sentence(&mut self, ui: &mut Ui, x0: f32, sentence: &str, retry: bool) {
        let painter = ui.painter().clone();
        let rect = ui.max_rect();
        let c = pos2(x0 + sheet::W / 2.0, rect.top() + rect.height() * 0.4);
        theme::text(&painter, c, Align2::CENTER_CENTER, sentence, theme::font(Face::Text, size::EMPTY), col::INK_SOFT, 0.0);
        if retry {
            let r = Rect::from_center_size(c + vec2(0.0, 40.0), vec2(110.0, 30.0));
            let resp = theme::key(ui, r, "TRY AGAIN", KeyKind::Outline, true);
            if resp.hovered() {
                self.help = Some(format!("Signs in again now. HurenTohn also tries by itself every {} seconds.", RETRY.as_secs()));
            }
            if resp.clicked() {
                self.validate_stored();
            }
        }
    }

    fn plugin_list(&mut self, ui: &mut Ui, x0: f32) {
        let painter = ui.painter().clone();
        let top = ui.max_rect().top() + (sheet::TAG_Y - sheet::MAST_H);
        let left = x0 + sheet::LEFT;
        theme::section_tag(&painter, pos2(left, top), "A", "Plugins");

        // Legend row: the counts. UPDATE ALL lives in the masthead.
        let x = x0 + sheet::RIGHT;
        let count = match (self.plugins.len(), self.waiting.len()) {
            (n, 0) => format!("{n} PLUGINS"),
            (n, 1) => format!("{n} PLUGINS · 1 UPDATE"),
            (n, u) => format!("{n} PLUGINS · {u} UPDATES"),
        };
        if !self.plugins.is_empty() {
            theme::mono(&painter, pos2(x, top + 6.5), Align2::RIGHT_CENTER, &count, size::INFO, col::INK_SOFT);
        }

        let list_top = top + sheet::TAG_TO_CONTENT;
        // The list ends in the right margin, so its scrollbar sits there and
        // not on the window edge next to the crop marks.
        let area = Rect::from_min_max(pos2(ui.max_rect().left(), list_top), pos2(x0 + sheet::RIGHT + 14.0, ui.max_rect().bottom()));
        let mut list_ui = ui.new_child(egui::UiBuilder::new().max_rect(area));
        if self.plugins.is_empty() {
            let sentence = match (&self.repos_error, self.repos_loading) {
                (Some(e), _) => e.clone(),
                (None, true) => "Asking GitHub which plugins you can see…".into(),
                (None, false) => "This GitHub account can't see any plugin on the whitelist.".into(),
            };
            self.centered_sentence(&mut list_ui, x0, &sentence, false);
            return;
        }
        let scroll = egui::ScrollArea::vertical().auto_shrink(false).show(&mut list_ui, |ui| {
            for i in 0..self.plugins.len() {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
                if ui.is_rect_visible(rect) {
                    // A child per row: its keys and links neither move the
                    // list's cursor nor share ids with other rows.
                    let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("row", i)));
                    self.plugin_row(&mut row, i, rect, x0);
                }
            }
        });
        // The legend above and the footer below cast a shadow onto rows that
        // slide under them, so the list reads as going on, not cut off. Each
        // fades in over the first SHADOW_H of hidden list.
        let view = scroll.inner_rect;
        let above = scroll.state.offset.y;
        let below = scroll.content_size.y - above - view.height();
        // As wide as the rows, not the window.
        let band = |top: f32| Rect::from_min_max(pos2(left, top), pos2(x0 + sheet::RIGHT, top + SHADOW_H));
        theme::edge_shadow(&painter, band(view.top()), true, above / SHADOW_H);
        theme::edge_shadow(&painter, band(view.bottom() - SHADOW_H), false, below / SHADOW_H);
    }

    fn plugin_row(&mut self, ui: &mut Ui, idx: usize, rect: Rect, x0: f32) {
        let painter = ui.painter().clone();
        let record = self.installs.0.get(&self.plugins[idx].repo.full_name).cloned();
        let p = &self.plugins[idx];
        let status = p.status(record.as_ref());
        let y0 = rect.top();
        let left = x0 + sheet::LEFT;
        if idx + 1 < self.plugins.len() {
            painter.hline(left..=x0 + sheet::RIGHT, y0 + IMG_H + sheet::GROUP_GAP + 0.5, Stroke::new(1.0, col::HAIRLINE));
        }
        let hovered = ui.rect_contains_pointer(rect);
        let t = ui.ctx().animate_bool_with_time(egui::Id::new(("row-colour", idx)), hovered, 0.18);

        // Picture: graphite at rest, colour under the pointer.
        let well = Rect::from_min_size(pos2(left, y0), vec2(IMG_W, IMG_H));
        painter.rect_filled(well, theme::radius(), col::WELL);
        match &p.image {
            Some((colour, gray)) => {
                let img = fit(colour.size_vec2(), well.shrink(1.0));
                let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
                painter.image(gray.id(), img, uv, Color32::WHITE);
                if t > 0.0 {
                    painter.image(colour.id(), img, uv, Color32::from_white_alpha((t * 255.0) as u8));
                }
            }
            None if p.image_missing => {
                theme::hatch(&painter, well.shrink(1.0), col::GRID);
                theme::caption(&painter, well.center(), Align2::CENTER_CENTER, "No picture", size::CONTROL, col::INK_FAINT);
            }
            None => {
                theme::caption(&painter, well.center(), Align2::CENTER_CENTER, "Loading", size::CONTROL, col::INK_FAINT);
            }
        }
        painter.rect_stroke(well, theme::radius(), Stroke::new(1.0, col::HAIRLINE), egui::StrokeKind::Inside);

        // Text column.
        let tx = well.right() + sheet::GROUP_GAP;
        let ax = x0 + sheet::RIGHT - ACTION_W;
        let tw = ax - sheet::GROUP_GAP - tx;
        let fallback;
        let m = match &p.manifest {
            Some(m) => m,
            None => {
                fallback = Manifest { name: p.repo.name.clone(), ..Default::default() };
                &fallback
            }
        };
        let (l, r) = m.wordmark();
        theme::wordmark(&painter, pos2(tx, y0 - 6.5), &l, &r, size::PRODUCT);
        let mut ty = y0 + 25.0;
        if !m.category.is_empty() {
            theme::caption(&painter, pos2(tx, ty), Align2::LEFT_TOP, &m.category, size::CONTROL, col::INK_SOFT);
            ty += 20.0;
        }
        // Bottom lines of the text column: what went wrong, else where it lives
        // (and a local build, if any). The description gets the rows above them.
        let bottom = y0 + IMG_H;
        let meta_h = theme::layout(&painter, "Ag", theme::font(Face::Mono, size::SUBLINE), col::INK_FAINT, 0.0, None).size().y;
        let meta_top = bottom - meta_h;
        let failed = matches!(p.job, Job::Failed { .. });
        let has_note = p.installed.user_version().is_some() && !failed;
        let lines_top = if failed { bottom - 17.0 } else { meta_top - if has_note { 16.0 } else { 0.0 } };
        if !m.description.is_empty() {
            let body = theme::font(Face::Text, size::BODY);
            let line_h = theme::layout(&painter, "Ag", body.clone(), col::INK_SOFT, 0.0, None).size().y;
            let bottom = lines_top - sheet::SM;
            let rows = (((bottom - ty) / line_h).floor() as usize).max(1);
            let g = theme::layout(&painter, &m.description, body, col::INK_SOFT, 0.0, Some((tw, rows)));
            painter.galley(pos2(tx, ty), g, col::INK_SOFT);
        }

        let failure = match &p.job {
            Job::Failed { message, log } => Some((message.clone(), log.clone())),
            _ => None,
        };
        if let Some((message, log)) = failure {
            // A lit FAILED tag (the accent: it needs you), then what broke.
            let tag = Rect::from_min_size(pos2(tx, bottom - 17.0), vec2(theme::key_width(&painter, "FAILED") - 8.0, 17.0));
            painter.rect_filled(tag, theme::radius(), col::LIT);
            theme::caption(&painter, tag.center() + vec2(0.5, 0.5), Align2::CENTER_CENTER, "FAILED", size::CONTROL, col::SHEET);
            let mx = tag.right() + sheet::SM;
            let g = theme::layout(&painter, &message, theme::font(Face::Mono, size::INFO), col::INK, 0.0, Some((tw - (mx - tx), 1)));
            painter.galley(pos2(mx, tag.center().y - g.size().y / 2.0), g, col::INK);
            let resp = ui.allocate_rect(Rect::from_min_size(tag.min, vec2(tw, 17.0)), Sense::click());
            if resp.hovered() {
                self.help = Some(match &log {
                    Some(l) => format!("{message} Click to open the log ({}).", l.display()),
                    None => message.clone(),
                });
            }
            if resp.clicked()
                && let Some(l) = &log
            {
                open_url(&l.to_string_lossy());
            }
        } else {
            let mut meta = p.repo.full_name.clone();
            if let Some(rel) = &p.release {
                meta.push_str(&format!(" · {} released {}", rel.tag_name, rel.date()));
            }
            if let Some(e) = &p.details_error {
                meta = e.clone();
            }
            let g = theme::layout(&painter, &meta, theme::font(Face::Mono, size::SUBLINE), col::INK_FAINT, 0.0, Some((tw, 1)));
            let link = Rect::from_min_size(pos2(tx, meta_top), g.size());
            let resp = ui.allocate_rect(link, Sense::click());
            painter.galley(link.min, g, if resp.hovered() { col::INK } else { col::INK_FAINT });
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                self.help = Some(format!("Opens {} on GitHub.", p.repo.full_name));
            }
            if resp.clicked() {
                open_url(p.release.as_ref().map(|r| r.html_url.as_str()).unwrap_or(&p.repo.html_url));
            }
            if let Some(v) = p.installed.user_version() {
                let note = format!("+ LOCAL BUILD {v} IN {}", platform::USER_DIR_SHORT);
                let r = theme::mono(&painter, pos2(tx, meta_top - 16.0), Align2::LEFT_TOP, &note, size::SUBLINE, col::INK_FAINT);
                if ui.rect_contains_pointer(r) {
                    self.help = Some(format!(
                        "A copy from a local build sits in {}. Hosts may list it next to the managed one in {}.",
                        platform::USER_DIR_LABEL,
                        platform::SYSTEM_DIR_LABEL
                    ));
                }
            }
        }

        // Action column: status, versions, keys.
        let status_label = match status {
            Status::Loading => "CHECKING",
            Status::NoRelease => "NO RELEASE YET",
            Status::NotInstalled => "NOT INSTALLED",
            Status::Update => "UPDATE AVAILABLE",
            Status::UpToDate => "UP TO DATE",
            Status::Ahead => "NEWER THAN RELEASE",
            Status::MainBuild => "BUILD OF MAIN",
            Status::Unknown => "CAN'T CHECK",
        };
        if status == Status::Update {
            let w = theme::key_width(&painter, status_label) - 8.0;
            let tag = Rect::from_min_size(pos2(ax, y0), vec2(w, 17.0));
            painter.rect_filled(tag, theme::radius(), col::LIT);
            theme::caption(&painter, tag.center() + vec2(0.5, 0.5), Align2::CENTER_CENTER, status_label, size::CONTROL, col::SHEET);
        } else {
            let c = if status == Status::UpToDate { col::INK_FAINT } else { col::INK_SOFT };
            theme::caption(&painter, pos2(ax, y0 + 2.5), Align2::LEFT_CENTER, status_label, size::CONTROL, c);
        }
        let mut vy = y0 + 30.0;
        let mut version_row = |label: &str, value: &str, lit: bool| {
            theme::caption(&painter, pos2(ax, vy + 7.0), Align2::LEFT_CENTER, label, size::TAG_LETTER, col::INK_FAINT);
            theme::mono(
                &painter,
                pos2(ax + ACTION_W, vy + 7.0),
                Align2::RIGHT_CENTER,
                value,
                size::VALUE,
                if lit { col::INK } else { col::INK_SOFT },
            );
            painter.hline(ax..=ax + ACTION_W, vy + 16.5, Stroke::new(1.0, col::GRID));
            vy += 19.0;
        };
        let main = p.main_build(record.as_ref());
        let installed_v = p.installed.version_string().unwrap_or_else(|| "—".into());
        version_row("Installed", &installed_v, status != Status::Update);
        let release_v = p.release.as_ref().map(|r| r.version().to_string()).unwrap_or_else(|| "—".into());
        version_row("Latest release", &release_v, status == Status::Update);
        if let Some((sha, _)) = main {
            version_row("Main build", &sha[..7.min(sha.len())], false);
        }

        let keys_y = y0 + IMG_H - 26.0 - sheet::SM - 30.0;
        let mut primary = Rect::from_min_size(pos2(ax, keys_y), vec2(ACTION_W, 30.0));
        let secondary = Rect::from_min_size(pos2(ax, keys_y + 30.0 + sheet::SM), vec2(ACTION_W, 26.0));
        if let Job::Working { title, frac, detail } = p.job.clone() {
            let time = ui.input(|i| i.time);
            theme::caption(&painter, pos2(ax, keys_y + 4.0), Align2::LEFT_TOP, &title, size::CONTROL, col::INK);
            if let Some(f) = frac {
                theme::mono(
                    &painter,
                    pos2(ax + ACTION_W, keys_y + 3.0),
                    Align2::RIGHT_TOP,
                    &format!("{:.0} %", f * 100.0),
                    size::INFO,
                    col::INK,
                );
            }
            theme::progress(&painter, Rect::from_min_size(pos2(ax, keys_y + 22.0), vec2(ACTION_W, 3.0)), frac, time);
            let g = theme::layout(&painter, &detail, theme::font(Face::Mono, size::SUBLINE), col::INK_FAINT, 0.0, Some((ACTION_W, 2)));
            painter.galley(pos2(ax, keys_y + 34.0), g, col::INK_FAINT);
            if ui.rect_contains_pointer(Rect::from_min_max(primary.min, secondary.max)) {
                self.help = Some(detail.clone());
            }
            if frac.is_none() {
                ui.ctx().request_repaint_after(Duration::from_millis(33));
            }
            return;
        }

        let name = p.name().to_string();
        let can_install = p.release.as_ref().and_then(|r| r.installer()).is_some();
        // The label without its version, for when UNINSTALL shares the line.
        let (mut label, short, kind) = match (status, &p.release) {
            (Status::Update, Some(r)) => (format!("UPDATE TO {}", r.version()), "UPDATE", KeyKind::Lit),
            (Status::NotInstalled, Some(r)) => (format!("INSTALL {}", r.version()), "INSTALL", KeyKind::Outline),
            (Status::UpToDate, Some(r)) => (format!("REINSTALL {}", r.version()), "REINSTALL", KeyKind::Quiet),
            (Status::Ahead | Status::MainBuild, Some(r)) => (format!("INSTALL RELEASE {}", r.version()), "INSTALL RELEASE", KeyKind::Quiet),
            (Status::Unknown, _) => ("RELEASE UNKNOWN".into(), "RELEASE UNKNOWN", KeyKind::Quiet),
            _ => ("NO RELEASE".into(), "NO RELEASE", KeyKind::Quiet),
        };
        // Installed: UNINSTALL sits right of the main key, which drops its
        // version (it is in the rows above) if the two don't fit side by side.
        let uninstall = (!p.installed.system.is_empty()).then(|| {
            let w = theme::key_width(&painter, "UNINSTALL");
            let r = Rect::from_min_size(pos2(ax + ACTION_W - w, keys_y), vec2(w, 30.0));
            primary.max.x = r.left() - sheet::SM;
            if theme::key_width(&painter, &label) > primary.width() {
                label = short.into();
            }
            r
        });
        let enabled = can_install && status != Status::Loading;
        let resp = theme::key(ui, primary, &label, kind, enabled);
        if resp.hovered() {
            self.help = Some(match &p.release {
                Some(r) if can_install => {
                    format!("Downloads {name} {}'s installer from GitHub and {}.", r.version(), platform::INSTALL_HOW)
                }
                Some(r) => format!("Release {} has no installer for this computer.", r.tag_name),
                None if status == Status::Unknown => {
                    format!("GitHub didn't say which release of {name} is the latest. REFRESH asks again.")
                }
                None => format!("{name} has no release yet. BUILD FROM MAIN builds it from source."),
            });
        }
        if resp.clicked() {
            self.start_install(idx);
        }
        if let Some(r) = uninstall {
            let resp = theme::key(ui, r, "UNINSTALL", KeyKind::Quiet, status != Status::Loading);
            if resp.hovered() {
                let p = &self.plugins[idx];
                let mut help = format!("Removes {name} from {}; {}.", platform::SYSTEM_DIR_LABEL, platform::UNINSTALL_HOW);
                if p.installed.user_version().is_some() {
                    help.push_str(&format!(" The local build in {} stays.", platform::USER_DIR_LABEL));
                }
                self.help = Some(help);
            }
            if resp.clicked() {
                self.start_uninstall(idx);
            }
        }
        let resp = theme::key(ui, secondary, "BUILD FROM MAIN", KeyKind::Secondary, status != Status::Loading);
        if resp.hovered() {
            self.help = Some(format!(
                "Builds the current {} branch of {name} on this Mac and installs it over the release. Takes a few minutes.",
                self.plugins[idx].repo.default_branch
            ));
        }
        if resp.clicked() {
            self.start_build(idx);
        }
    }

    fn footer(&mut self, painter: &egui::Painter, area: Rect, x0: f32) {
        // On the list's clip edge, so a row cut off by the footer meets a rule.
        let rule_y = area.top() + 0.5;
        painter.hline(x0 + sheet::LEFT..=x0 + sheet::RIGHT, rule_y, Stroke::new(1.0, col::HAIRLINE));
        let y = area.top() + 4.0 + 11.0;
        // A help sentence gets the whole line; the state comes back when the pointer leaves.
        let mut rw = 0.0;
        if self.help.is_none() {
            let right = self.footer_state();
            rw = theme::text_width(painter, &right, theme::font(Face::Mono, size::FOOT_RIGHT), 0.0) + sheet::LG;
            theme::mono(painter, pos2(x0 + sheet::RIGHT, y), Align2::RIGHT_CENTER, &right, size::FOOT_RIGHT, col::INK_FAINT);
        }
        let (line, colour) = match (&self.help, &self.notice) {
            (Some(h), _) => (h.clone(), col::INK),
            (None, Some(n)) => (n.clone(), col::INK_SOFT),
            (None, None) => (self.resting_sentence(), col::INK_SOFT),
        };
        let width = sheet::CONTENT_W - rw;
        let g = theme::layout(painter, &line, theme::font(Face::Mono, size::HELP), colour, 0.0, Some((width, 1)));
        let h = g.size().y;
        painter.galley(pos2(x0 + sheet::LEFT, y - h / 2.0), g, colour);
    }

    fn resting_sentence(&self) -> String {
        match &self.auth {
            Auth::Checking => "Signing in with the login from your keychain…".into(),
            Auth::Offline(e) => e.clone(),
            Auth::SignedOut { .. } => "Signed out. Sign in to see your plugins.".into(),
            Auth::SignedIn { .. } if self.repos_loading => "Checking GitHub for releases…".into(),
            Auth::SignedIn { .. } => {
                let missing =
                    self.plugins.iter().filter(|p| p.status(self.installs.0.get(&p.repo.full_name)) == Status::NotInstalled).count();
                match (self.waiting.len(), missing) {
                    (0, 0) => "Every plugin is up to date.".into(),
                    (0, 1) => format!("1 plugin is not in {} yet. INSTALL puts its release there.", platform::SYSTEM_DIR_LABEL),
                    (0, m) => format!("{m} plugins are not in {} yet. INSTALL puts a release there.", platform::SYSTEM_DIR_LABEL),
                    (1, _) => "1 update waiting. Hover a key to see what it does.".into(),
                    (n, _) => format!("{n} updates waiting. UPDATE ALL installs them one after another."),
                }
            }
        }
    }

    fn footer_state(&self) -> String {
        let mut parts = Vec::new();
        if let Auth::SignedIn { login, .. } = &self.auth {
            parts.push(format!("SIGNED IN AS {login}"));
        }
        if let Some(t) = self.checked_at {
            let mins = t.elapsed().as_secs() / 60;
            parts.push(if mins == 0 { "CHECKED JUST NOW".to_string() } else { format!("CHECKED {mins} MIN AGO") });
        }
        parts.push(
            match self.helper {
                Some(HelperState::Ready) => "SILENT INSTALLS",
                Some(HelperState::Missing) => "INSTALLS ASK FOR PASSWORD",
                Some(HelperState::Unsupported) => "INSTALLS ASK FOR ADMIN RIGHTS",
                None => "",
            }
            .to_string(),
        );
        parts.retain(|p| !p.is_empty());
        parts.join(" · ")
    }

    fn snapshot_tick(&mut self, ctx: &egui::Context) {
        let settled = match &self.auth {
            Auth::SignedIn { .. } => {
                !self.repos_loading && self.plugins.iter().all(|p| p.manifest.is_some() && (p.image.is_some() || p.image_missing))
            }
            Auth::SignedOut { busy, .. } => !busy,
            _ => false,
        };
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(snap) = &mut self.snapshot else { return };
        if let Some(image) = shot {
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(png) = ::image::RgbaImage::from_raw(image.size[0] as u32, image.size[1] as u32, bytes) {
                let _ = png.save(&snap.path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        snap.settled_frames = if settled { snap.settled_frames + 1 } else { 0 };
        if snap.settled_frames > 20 && snap.busy {
            snap.busy = false;
            snap.settled_frames = 0;
            self.stage_busy_state();
            return;
        }
        if snap.settled_frames > 20 && !snap.requested {
            snap.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        ctx.request_repaint_after(Duration::from_millis(30));
    }
}

impl App {
    fn stage_busy_state(&mut self) {
        let older = |p: &Plugin| {
            let v = p.release.as_ref().and_then(|r| installed::parse_version(r.version()))?;
            Some(semver::Version::new(v.major, v.minor, v.patch.saturating_sub(1)).to_string())
        };
        if let Some(p) = self.plugins.get_mut(0) {
            p.job = Job::Working {
                title: "COMPILING 412/1530".into(),
                frac: Some(412.0 / 1530.0),
                detail: "Building CXX object CMakeFiles/PsyBox.dir/src/core/dsp/Filters.cpp.o".into(),
            };
        }
        if let Some(p) = self.plugins.get_mut(1) {
            let v = older(p).unwrap_or_else(|| "0.1.0".into());
            p.installed.system = vec![installed::Copy { version: Some(v) }];
        }
        if let Some(p) = self.plugins.get_mut(2) {
            p.job = Job::Failed {
                message: "cmake failed: ninja: build stopped: subcommand failed.".into(),
                log: Some(build::log_path(&p.repo.full_name)),
            };
        }
        if let Some(p) = self.plugins.get_mut(3) {
            p.job = Job::Working {
                title: "WAITING FOR BUILD".into(),
                frac: None,
                detail: "Another plugin is building; this one starts after it.".into(),
            };
        }
        if let Some(p) = self.plugins.get_mut(4) {
            p.release = None;
            p.release_checked = false;
            p.details_error = Some("GitHub is not reachable: timeout".into());
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain(&ctx);
        self.waiting = self.updates();
        // Offline: try the stored login again by itself every RETRY.
        if matches!(self.auth, Auth::Offline(_)) {
            let at = *self.retry_at.get_or_insert_with(|| Instant::now() + RETRY);
            let now = Instant::now();
            if now >= at {
                self.validate_stored();
            } else {
                ctx.request_repaint_after(at - now);
            }
        }
        self.fit_sheet(&ctx);
        // The title bar is window chrome: TITLE_H window points whatever the zoom.
        let title_h = sheet::TITLE_H / ctx.zoom_factor();
        #[cfg(target_os = "macos")]
        platform::place_traffic_lights(frame, sheet::TITLE_H as f64, 1.0);
        #[cfg(not(target_os = "macos"))]
        let _ = frame;
        self.help = None;

        let x0_of = |ui: &Ui| ui.max_rect().left() + ((ui.max_rect().width() - sheet::W) / 2.0).max(0.0);
        egui::Panel::top("title-bar").exact_size(title_h).frame(egui::Frame::NONE.fill(col::WELL)).show_separator_line(false).show(
            ui,
            |ui| {
                let x0 = x0_of(ui);
                self.title_bar(ui, x0);
            },
        );
        egui::Panel::top("masthead").exact_size(sheet::MAST_H).frame(egui::Frame::NONE.fill(col::SHEET)).show_separator_line(false).show(
            ui,
            |ui| {
                let x0 = x0_of(ui);
                self.masthead(ui, x0);
            },
        );
        let footer = egui::Panel::bottom("footer")
            .exact_size(sheet::FOOTER_H)
            .frame(egui::Frame::NONE.fill(col::SHEET))
            .show_separator_line(false)
            .show(ui, |ui| ui.max_rect())
            .inner;
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(col::SHEET)).show(ui, |ui| {
            let x0 = x0_of(ui);
            match &self.auth {
                Auth::Checking => self.centered_sentence(ui, x0, "Signing in…", false),
                Auth::Offline(e) => {
                    let e = e.clone();
                    self.centered_sentence(ui, x0, &e, true)
                }
                Auth::SignedOut { .. } => self.signed_out(ui, x0),
                Auth::SignedIn { .. } => self.plugin_list(ui, x0),
            }
        });

        // Drawn last so the footer shows what the pointer is over this frame.
        let screen = ctx.content_rect();
        let x0 = screen.left() + ((screen.width() - sheet::W) / 2.0).max(0.0);
        let top = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("sheet-top")));
        // The sign-in screen stays bare: no footer line until there is something to manage.
        if matches!(self.auth, Auth::SignedIn { .. } | Auth::Offline(_)) {
            self.footer(&top, footer, x0);
        }
        theme::crop_marks(&top, Rect::from_min_max(pos2(screen.left(), screen.top() + title_h), screen.max));
        // Windows: the window's own border, since there is no system frame.
        // macOS keeps its native frame (rounded corners, shadow, resizing).
        #[cfg(windows)]
        {
            top.rect_stroke(screen, egui::CornerRadius::ZERO, Stroke::new(1.0, col::HAIRLINE), egui::StrokeKind::Inside);
            resize_edges(ui, screen);
        }

        if self.checked_at.is_some() {
            // Keeps "CHECKED n MIN AGO" current.
            ctx.request_repaint_after(Duration::from_secs(30));
        }
        if self.snapshot.is_some() {
            self.snapshot_tick(&ctx);
        }
    }
}

/// Fits `size` into `area` keeping the aspect ratio, centred.
fn fit(size: egui::Vec2, area: Rect) -> Rect {
    let scale = (area.width() / size.x).min(area.height() / size.y);
    Rect::from_center_size(area.center(), size * scale)
}

/// Decodes a product picture at list size: the colour image and a graphite
/// version (Rec. 709 luma, dimmed to step back at rest).
fn decode(bytes: &[u8]) -> Option<(ColorImage, ColorImage)> {
    let img = ::image::load_from_memory(bytes).ok()?.thumbnail(720, 480).to_rgba8();
    // `fit` divides by the size.
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    let size = [img.width() as usize, img.height() as usize];
    let colour = ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let gray: Vec<u8> = img
        .pixels()
        .flat_map(|p| {
            let [r, g, b, a] = p.0;
            let y = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) * 0.82;
            let y = y.round().clamp(0.0, 255.0) as u8;
            [y, y, y, a]
        })
        .collect();
    Some((colour, ColorImage::from_rgba_unmultiplied(size, &gray)))
}

/// Undecorated windows on Windows lose their resize border; these edge
/// zones hand the drag to the system instead. (macOS keeps native resizing.)
#[cfg(windows)]
fn resize_edges(ui: &mut Ui, r: Rect) {
    use egui::CursorIcon as C;
    use egui::viewport::ResizeDirection as D;
    let e = 5.0;
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    let zones = [
        (Rect::from_min_max(pos2(l + e, t), pos2(rt - e, t + e)), D::North, C::ResizeVertical),
        (Rect::from_min_max(pos2(l + e, b - e), pos2(rt - e, b)), D::South, C::ResizeVertical),
        (Rect::from_min_max(pos2(l, t + e), pos2(l + e, b - e)), D::West, C::ResizeHorizontal),
        (Rect::from_min_max(pos2(rt - e, t + e), pos2(rt, b - e)), D::East, C::ResizeHorizontal),
        (Rect::from_min_max(pos2(l, t), pos2(l + e, t + e)), D::NorthWest, C::ResizeNwSe),
        (Rect::from_min_max(pos2(rt - e, b - e), pos2(rt, b)), D::SouthEast, C::ResizeNwSe),
        (Rect::from_min_max(pos2(rt - e, t), pos2(rt, t + e)), D::NorthEast, C::ResizeNeSw),
        (Rect::from_min_max(pos2(l, b - e), pos2(l + e, b)), D::SouthWest, C::ResizeNeSw),
    ];
    for (i, (zone, dir, cursor)) in zones.into_iter().enumerate() {
        let resp = ui.interact(zone, egui::Id::new(("resize-edge", i)), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(cursor);
        }
        if resp.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> Repo {
        Repo { full_name: "o/X".into(), name: "X".into(), default_branch: "main".into(), html_url: String::new(), description: None }
    }

    fn release(tag: &str) -> Release {
        Release { tag_name: tag.into(), html_url: String::new(), published_at: None, assets: Vec::new() }
    }

    #[test]
    fn a_failed_check_keeps_what_the_row_knew() {
        let known = Known {
            manifest: Some(Manifest { name: "X".into(), category: "Delay".into(), ..Default::default() }),
            release: Some(Some(release("v1.2.0"))),
            image: None,
        };
        let d = Details::resolve("X", Err("offline".into()), Err("offline".into()), &known);
        assert_eq!(d.manifest.category, "Delay");
        assert_eq!(d.release.map(|r| r.tag_name).as_deref(), Some("v1.2.0"));
        assert!(d.release_checked);
        assert_eq!(d.error.as_deref(), Some("offline"));
    }

    #[test]
    fn a_first_check_that_fails_is_unknown_not_up_to_date() {
        let d = Details::resolve("X", Err("offline".into()), Err("offline".into()), &Known::default());
        let mut p = Plugin::new(repo());
        p.manifest = Some(d.manifest);
        p.release = d.release;
        p.release_checked = d.release_checked;
        p.installed.system = vec![installed::Copy { version: Some("1.0.0".into()) }];
        assert!(p.status(None) == Status::Unknown);
    }

    #[test]
    fn builds_of_main_match_two_part_versions() {
        let mut p = Plugin::new(repo());
        p.manifest = Some(Manifest::default());
        p.release_checked = true;
        p.installed.system = vec![installed::Copy { version: Some("0.3".into()) }];
        let record = InstallRecord::Main { sha: "abc".into(), committed_at: "2026-10-01T00:00:00Z".into(), bundle_version: "0.3".into() };
        assert!(p.status(Some(&record)) == Status::MainBuild);
        // An unreadable recorded version matches nothing, not even "nothing installed".
        p.installed.system.clear();
        let unreadable = InstallRecord::Main { sha: "abc".into(), committed_at: String::new(), bundle_version: String::new() };
        assert!(p.main_build(Some(&unreadable)).is_none());
    }
}
