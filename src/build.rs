//! Building a plug-in's main branch on this Mac: shallow checkout in
//! Application Support, CMake + Ninja, bundles out of the artefacts folder.

use crate::manifest::Manifest;
use crate::platform;
use base64::Engine;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, TryLockError};

const BUILD_DIR: &str = "build-ht-plugin-manager";

/// One build at a time: each one uses every core, and JUCE builds take a
/// gigabyte or two of memory apiece.
static BUILD_LOCK: Mutex<()> = Mutex::new(());

/// Waits for the build before this one to finish. `waiting` runs once, only
/// if there is a build to wait for.
pub fn wait_turn(waiting: impl FnOnce()) -> MutexGuard<'static, ()> {
    match BUILD_LOCK.try_lock() {
        Ok(turn) => turn,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        Err(TryLockError::WouldBlock) => {
            waiting();
            BUILD_LOCK.lock().unwrap_or_else(|e| e.into_inner())
        }
    }
}

pub enum Step {
    Fetching,
    Configuring,
    /// Ninja's `[done/total]` and the line it printed.
    Compiling {
        done: u32,
        total: u32,
        line: String,
    },
    /// A line from a build without a step count (MSBuild on Windows).
    Output(String),
}

pub struct Built {
    pub bundles: Vec<PathBuf>,
    pub log: PathBuf,
    /// The commit that was built. The one asked for, unless GitHub refused
    /// to fetch it by sha and the branch had moved on.
    pub sha: String,
}

pub struct BuildError {
    pub message: String,
    pub log: PathBuf,
}

pub fn source_dir(full_name: &str) -> PathBuf {
    crate::config::support_dir().join("src").join(full_name.replace('/', "__"))
}

pub fn log_path(full_name: &str) -> PathBuf {
    crate::config::log_dir().join(format!("{}-build.log", full_name.replace('/', "__")))
}

/// Builds `branch` at `sha` and returns the finished bundles.
pub fn build_main(
    token: &str,
    full_name: &str,
    branch: &str,
    sha: &str,
    manifest: &Manifest,
    juce: Option<&Path>,
    mut step: impl FnMut(Step),
) -> Result<Built, BuildError> {
    let log = log_path(full_name);
    let fail = |message: String| BuildError { message, log: log.clone() };
    let _ = std::fs::create_dir_all(crate::config::log_dir());
    let mut log_file = std::fs::File::create(&log).map_err(|e| fail(format!("Can't write the build log: {e}")))?;
    manifest.check().map_err(&fail)?;

    let src = source_dir(full_name);
    step(Step::Fetching);
    if src.join(".git").is_dir() {
        // Builds run one at a time, so a lock file here is left over from one
        // that was killed; git would refuse to touch the checkout forever.
        for lock in ["index.lock", "shallow.lock"] {
            let _ = std::fs::remove_file(src.join(".git").join(lock));
        }
        // A checkout whose git config no longer reads (an interrupted first
        // fetch, a damaged disk) is started over.
        if git(token, &src, &["remote", "get-url", "origin"], &mut log_file).is_err() {
            let _ = writeln!(log_file, "# the checkout is damaged; starting it over");
            let _ = std::fs::remove_dir_all(&src);
        }
    }
    if !src.join(".git").is_dir() {
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(&src).map_err(|e| fail(e.to_string()))?;
        git(token, &src, &["init", "-q"], &mut log_file).map_err(&fail)?;
        let url = format!("https://github.com/{full_name}.git");
        git(token, &src, &["remote", "add", "origin", &url], &mut log_file).map_err(&fail)?;
    }
    // The exact commit the UI showed, not whatever main moved to meanwhile.
    git(token, &src, &["fetch", "-q", "--depth", "1", "origin", sha], &mut log_file)
        .or_else(|_| git(token, &src, &["fetch", "-q", "--depth", "1", "origin", branch], &mut log_file))
        .map_err(&fail)?;
    git(token, &src, &["checkout", "-q", "--force", "FETCH_HEAD"], &mut log_file).map_err(&fail)?;
    let built_sha = head_sha(&src).unwrap_or_else(|| sha.to_string());
    // Keeps ignored files, so the build dir and its downloaded JUCE survive.
    git(token, &src, &["clean", "-q", "-fd"], &mut log_file).map_err(&fail)?;
    if src.join(".gitmodules").is_file() {
        git(token, &src, &["submodule", "update", "-q", "--init", "--recursive", "--depth", "1"], &mut log_file).map_err(&fail)?;
    }

    step(Step::Configuring);
    // Ninja where it is on the PATH by itself (macOS); Visual Studio on
    // Windows, where Ninja would need a developer prompt for the compiler.
    let generator: &[&str] = if cfg!(windows) { &["-A", "x64"] } else { &["-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release"] };
    let mut configure: Vec<String> = ["-S", ".", "-B", BUILD_DIR].iter().chain(generator).map(|s| s.to_string()).collect();
    if let Some(juce) = juce {
        configure.push(format!("-DFETCHCONTENT_SOURCE_DIR_JUCE={}", juce.display()));
    }
    configure.extend(manifest.build.cmake_args.iter().cloned());
    let mut cmd = platform::command("cmake");
    cmd.args(&configure).current_dir(&src);
    run_streamed(cmd, &mut log_file, |_| {}).map_err(&fail)?;

    let mut build: Vec<String> =
        ["--build", BUILD_DIR, "--config", "Release", "--parallel", "--target"].iter().map(|s| s.to_string()).collect();
    // Audio Units only exist on macOS.
    build.extend(manifest.build.targets.iter().filter(|t| cfg!(target_os = "macos") || !t.ends_with("_AU")).cloned());
    let mut cmd = platform::command("cmake");
    cmd.args(&build).current_dir(&src);
    // JUCE's VST3 manifest helper is a nested Ninja build with its own
    // `[1/2]`; only the build with the largest step count is the real one.
    let mut biggest = 0;
    run_streamed(cmd, &mut log_file, |line| match ninja_progress(line) {
        Some((done, total)) if total >= biggest => {
            biggest = total;
            step(Step::Compiling { done, total, line: line.to_string() });
        }
        Some(_) => {}
        None if biggest == 0 && !line.trim().is_empty() => step(Step::Output(line.trim().to_string())),
        None => {}
    })
    .map_err(&fail)?;

    let artefacts = src.join(BUILD_DIR).join(&manifest.build.artefacts);
    // `check` made sure every name is a bare bundle file name.
    let mut bundles = Vec::new();
    for name in manifest.bundles.iter().filter(|b| cfg!(target_os = "macos") || !platform::is_au(b)) {
        let sub = if name.ends_with(".component") { "AU" } else { "VST3" };
        let path = artefacts.join(sub).join(name);
        if !path.is_dir() {
            return Err(fail(format!("The build finished but {} is missing.", path.display())));
        }
        bundles.push(path);
    }
    if bundles.is_empty() {
        return Err(fail(format!("{} names no bundle this computer can install.", crate::manifest::FILE)));
    }
    Ok(Built { bundles, log, sha: built_sha })
}

/// The commit the checkout is at.
fn head_sha(dir: &Path) -> Option<String> {
    let out =
        platform::command("git").args(["rev-parse", "HEAD"]).current_dir(dir).env("PATH", crate::config::tool_path()).output().ok()?;
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !sha.is_empty()).then_some(sha)
}

/// Git with the token as an HTTP header passed through the environment: it
/// never lands in `.git/config` or in the process list.
fn git(token: &str, dir: &Path, args: &[&str], log: &mut std::fs::File) -> Result<(), String> {
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    let mut cmd = platform::command("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        // A stalled connection fails after a minute instead of hanging the row.
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
        .env("GIT_HTTP_LOW_SPEED_TIME", "60")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
        .env("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: basic {basic}"));
    run_streamed(cmd, log, |_| {})
}

/// Runs a tool with stdout and stderr merged into the log, handing every line
/// to `on_line`. The error is the tool's last line, which says what broke.
fn run_streamed(mut cmd: Command, log: &mut std::fs::File, mut on_line: impl FnMut(&str)) -> Result<(), String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let _ = writeln!(log, "\n$ {program} {}", cmd.get_args().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" "));
    let (reader, writer) = std::io::pipe().map_err(|e| e.to_string())?;
    let mut child = cmd
        .env("PATH", crate::config::tool_path())
        .env("CLICOLOR_FORCE", "0")
        .stdin(Stdio::null())
        .stdout(writer.try_clone().map_err(|e| e.to_string())?)
        .stderr(writer)
        .spawn()
        .map_err(|e| format!("Can't start {program}: {e}. Is it installed?"))?;
    // The Command still holds the write end; drop it so the read ends at exit.
    drop(cmd);
    let mut last = String::new();
    // By bytes, not `lines()`: a compiler quoting a file in another encoding
    // must not turn its lines into blanks.
    for line in BufReader::new(reader).split(b'\n') {
        let Ok(bytes) = line else { break };
        let line = String::from_utf8_lossy(&bytes);
        let line = line.trim_end_matches('\r');
        let _ = writeln!(log, "{line}");
        on_line(line);
        if !line.trim().is_empty() {
            last = line.to_string();
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("{program} failed: {}", last.trim())) }
}

/// `[12/345] Building CXX object …` → (12, 345).
fn ninja_progress(line: &str) -> Option<(u32, u32)> {
    let inner = line.strip_prefix('[')?.split(']').next()?;
    let (done, total) = inner.split_once('/')?;
    Some((done.trim().parse().ok()?, total.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ninja_progress() {
        assert_eq!(ninja_progress("[12/345] Building CXX object x.o"), Some((12, 345)));
        assert_eq!(ninja_progress("-- Configuring done"), None);
    }

    #[cfg(unix)]
    #[test]
    fn streamed_errors_carry_the_last_line() {
        let mut log = tempfile_log();
        let mut lines = Vec::new();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo one; echo two >&2; exit 3"]);
        let err = run_streamed(cmd, &mut log, |l| lines.push(l.to_string())).unwrap_err();
        assert_eq!(lines, ["one", "two"]);
        assert!(err.ends_with("two"), "{err}");
    }

    /// The whole path against GitHub and a real plug-in, minus the install:
    /// `HTPM_TOKEN=$(gh auth token) cargo test --release -- --ignored builds_main`
    #[test]
    #[ignore = "clones and compiles a plug-in (minutes)"]
    fn builds_main_of_a_real_plugin() {
        let token = std::env::var("HTPM_TOKEN").expect("HTPM_TOKEN");
        let full = std::env::var("HTPM_TEST_REPO").unwrap_or_else(|_| "JonasGrunau/PhaseFucker".into());
        let gh = crate::github::Gh::new(token.clone());
        let repo = gh.repo(&full).unwrap().expect("repo visible");
        let bytes = gh.file(&full, crate::manifest::FILE, &repo.default_branch).unwrap().expect("manifest");
        let manifest = Manifest::parse(&bytes, &repo.name).unwrap();
        let head = gh.branch_head(&full, &repo.default_branch).unwrap();
        let juce = crate::config::Config::default().juce();
        let built = build_main(&token, &full, &repo.default_branch, &head.sha, &manifest, juce.as_deref(), |step| {
            if let Step::Compiling { done, total, .. } = step
                && (done % 100 == 0 || done == total)
            {
                eprintln!("[{done}/{total}]");
            }
        })
        .unwrap_or_else(|e| panic!("{} (log: {})", e.message, e.log.display()));
        assert_eq!(built.bundles.len(), 2);
        for b in &built.bundles {
            assert!(b.join("Contents/Info.plist").is_file(), "{}", b.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn streamed_lines_survive_other_encodings() {
        let mut log = tempfile_log();
        let mut lines = Vec::new();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", r"printf 'caf\351\nnext\n'"]);
        run_streamed(cmd, &mut log, |l| lines.push(l.to_string())).unwrap();
        assert_eq!(lines, ["caf\u{FFFD}", "next"]);
    }

    #[test]
    fn builds_wait_their_turn() {
        let first = wait_turn(|| panic!("nothing to wait for"));
        let (tx, rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            let _turn = wait_turn(|| tx.send(()).unwrap());
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the second build waits");
        drop(first);
        second.join().unwrap();
    }

    #[cfg(unix)]
    fn tempfile_log() -> std::fs::File {
        let p = std::env::temp_dir().join(format!("ht-plugin-manager-log-{}", std::process::id()));
        std::fs::File::create(p).unwrap()
    }
}
