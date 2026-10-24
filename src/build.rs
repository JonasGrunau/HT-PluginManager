//! Building a plug-in's main branch on this Mac: shallow checkout in
//! Application Support, CMake + Ninja, bundles out of the artefacts folder.

use crate::manifest::Manifest;
use crate::platform;
use base64::Engine;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BUILD_DIR: &str = "build-hurentohn";

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

    let src = source_dir(full_name);
    step(Step::Fetching);
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
    let mut bundles = Vec::new();
    for name in manifest.bundles.iter().filter(|b| cfg!(target_os = "macos") || !platform::is_au(b)) {
        let sub = if name.ends_with(".component") { "AU" } else { "VST3" };
        let path = artefacts.join(sub).join(name);
        if !path.is_dir() {
            return Err(fail(format!("The build finished but {} is missing.", path.display())));
        }
        bundles.push(path);
    }
    Ok(Built { bundles, log })
}

/// Git with the token as an HTTP header passed through the environment: it
/// never lands in `.git/config` or in the process list.
fn git(token: &str, dir: &Path, args: &[&str], log: &mut std::fs::File) -> Result<(), String> {
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    let mut cmd = platform::command("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
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
    for line in BufReader::new(reader).lines() {
        let line = line.unwrap_or_default();
        let _ = writeln!(log, "{line}");
        on_line(&line);
        if !line.trim().is_empty() {
            last = line;
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
    /// `HURENTOHN_TOKEN=$(gh auth token) cargo test --release -- --ignored builds_main`
    #[test]
    #[ignore = "clones and compiles a plug-in (minutes)"]
    fn builds_main_of_a_real_plugin() {
        let token = std::env::var("HURENTOHN_TOKEN").expect("HURENTOHN_TOKEN");
        let full = std::env::var("HURENTOHN_TEST_REPO").unwrap_or_else(|_| "JonasGrunau/PhaseFucker".into());
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
    fn tempfile_log() -> std::fs::File {
        let p = std::env::temp_dir().join(format!("hurentohn-log-{}", std::process::id()));
        std::fs::File::create(p).unwrap()
    }
}
