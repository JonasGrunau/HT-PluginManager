# HurenTohn Plugin Manager

## Purpose
Plugin manager (Rust, egui/eframe 0.36) for Jonas's JUCE plugins, on macOS and Windows. It signs in to GitHub (device flow, or the `gh` CLI token; the token goes into the keychain / Credential Manager through `keyring`) and lists the whitelisted repos the account can see; repos it can't see are left out silently. Per repo it reads `hurentohn.json` (README fallback), the latest release and the installed bundle versions. It installs the release's platform installer without a wizard, or builds `main` locally and installs the bundles. On macOS a root helper behind a one-command sudoers rule makes installs password-free after one prompt; on Windows each install shows one UAC prompt.

## Key files
| File | What |
|---|---|
| `src/app.rs` | State, worker threads (all I/O off the UI thread, results via `Msg` on a channel), and the sheet: title bar, masthead, sign-in, plugin rows, footer help line, snapshot mode |
| `src/theme.rs` | House style: palette "graphite", type scale, sheet geometry, Plex fonts, `wordmark`, `crop_marks`, `section_tag`, `key` (Lit / Outline / Secondary / Quiet), `window_key`, `hatch`, `progress` |
| `src/platform.rs` | Everything macOS and Windows do differently: folders, UI labels, the release asset suffix, opening links, `command()` (no console window on Windows), `place_traffic_lights` (macOS, via objc2-app-kit) |
| `src/privileged/` | `macos.rs`: the root helper script, its sudoers rule, `install_release` (pkg), `install_bundles`, the `osascript` admin-prompt fallback (command passed as argv, never interpolated). `windows.rs`: `Setup.exe /VERYSILENT` and an elevated PowerShell copy, both through `Start-Process -Verb RunAs` |
| `src/github.rs` | REST calls (ureq 3, `http_status_as_error(false)`): user, repo (404/403 → `None`), latest release, contents (raw), asset download, device flow |
| `src/build.rs` | Build of main: shallow fetch of the exact sha, CMake (Ninja on macOS, Visual Studio on Windows), progress from `[n/m]`, log file |
| `src/manifest.rs` | `hurentohn.json` schema, defaults, README fallback, wordmark split |
| `src/installed.rs` | Bundle scan (system + user plugin folders), version from Info.plist or the VST3 `moduleinfo.json` (JSON5, read by hand), lenient semver |
| `src/config.rs` | `config.json` (whitelist, client ID, JUCE dir, `helper_offered`), `installs.json` (provenance of builds of main) |
| `build.rs` | Cargo build script: the Windows exe's icon and version resource (not the build-of-main module) |
| `scripts/bundle.sh` | macOS `.app` (optionally universal), Developer ID signing (hardened runtime), DMG, notarization (CI env or 1Password), install |
| `packaging/windows/HurenTohn.iss` | Inno Setup installer, per user |
| `.github/workflows/release.yml` | Tag → DMG + Windows Setup.exe → GitHub release (the plugin repos' pipeline, same secrets) |
| `scripts/make-icon.py` | Draws `assets/AppIcon.{icns,ico,png}`: graphite sheet, centred H/T, crop marks |
| `start.sh` | Builds the signed bundle and runs it in the foreground |

## Invariants
- **No secrets in argv or files.** Git gets the token as `http.extraheader` through `GIT_CONFIG_*` env vars; checkouts' `.git/config` and the build log never contain it. Admin commands go to `osascript` as an argument.
- **The macOS helper only runs what it verified.** `pkg` copies the file into a root-owned temp dir and installs that copy, only if `spctl -a -t install` prints `origin=Developer ID Installer: … (JQ4STVWTQ3)`. `bundle` strips setuid/setgid and chowns to root before the bundle goes live. Changing `helper_script()` makes `helper_state()` report `Missing` until the user re-allows it (the on-disk script must equal the built-in one).
- **The macOS helper install reads no user-writable file.** The root command writes the helper and the sudoers rule from memory (`printf` into a root `mktemp -d`), checks the rule with `visudo -c`, then installs both.
- **Bundle names are checked before any install command.** `installed::valid_bundle_name` (the helper's rule plus a `.vst3`/`.component` suffix) runs in `Manifest::check`, `build_main` and `privileged::check_bundles`; the password-prompt path would otherwise `rm -rf` a whole plugin folder for a bundle named `..`.
- **Installs never run concurrently** (`INSTALL_LOCK`); installers refuse parallel runs. **Builds of main queue** behind `build::wait_turn`; the lock is let go before the install, so a password prompt doesn't hold up the next build.
- **Inaccessible repos are not errors.** `Gh::repo` maps 403/404/451 to `None`. `github::Error` tells the rest apart: `Offline`, `RateLimited` (403/429 with rate-limit headers), `Expired` (401), `Other`. Only `Expired` drops the stored login.
- **Workers always answer, the UI thread never scans.** `spawn` takes an `on_panic` that still sends the awaited message; installs and builds go through `spawn_job`, which ends in one `Msg::Finished` carrying an `Installed` scan made on the worker. Refresh and details results carry a `generation`; stale ones are dropped. A failed details fetch keeps what the row knew (`Details::resolve`); a release that was never fetched shows CAN'T CHECK, not UP TO DATE.
- **A `config.json` that doesn't parse is never overwritten** (`Config::read_only`). Config and `installs.json` are written atomically (temp file + rename).
- **Builds of main** are recognised by `installs.json` only while the installed bundle still has the recorded version; a release newer than the commit (ISO dates compare as strings) flips the row back to UPDATE.
- **Windows code is not run on this Mac.** Type-check it: copy the crate to a scratch dir, set `ureq = { version = "3", default-features = false, features = ["json"] }` (ring's C needs the Windows SDK), drop `build.rs`, then `cargo clippy --target x86_64-pc-windows-msvc --all-targets`. The release workflow is the real Windows build.

## UI rules (house style, audio-plugin SDK `references/ui-design.md` §0)
- Plex Sans Condensed (words) + Plex Mono (every number), flat drawing, one 2 px radius, crop marks, `[A]` section tags, `LEFT/RIGHT` wordmarks, one 880-wide sheet scaled as a whole (`fit_sheet` sets the egui zoom), footer help line drawn last so it shows this frame's hover. A hover help sentence takes the whole footer line; the state on the right comes back when the pointer leaves.
- Palette graphite: true greys (chroma 0) on the house contrast ladder; no hue, so it never reads as one of the plugins. **ACCENT MEANS: what needs you or is happening now** (update waiting, job running, failure) = solid ink fill with sheet-coloured text. Secondary actions (BUILD FROM MAIN) are a hairline outline with inkFaint text, an outline key only under the pointer; at rest things step back to inkFaint.
- Logo: `HUREN/TOHN` + `PLUGIN MANAGER` in the wordmark face and size, the second part in inkSoft. Sub-line `RELEASES AND BUILDS · HT-1 · vX.Y.Z`.
- Title bar: 36 px, `well` grey, hairline under it, no text. macOS keeps its system frame and traffic lights (`with_fullsize_content_view`, transparent title bar); `place_traffic_lights` centres them vertically every frame (10.5 pt above and below, macOS's own 9 pt left inset). Windows has no system frame: the app draws minimize / zoom / close keys on the sheet's right margin, a hairline border, and edge resize zones (`BeginResize`).
- The sign-in page is one centred block (heading, balanced two-line text, keys) with no footer; without an OAuth client ID only the gh CLI key shows, lit.
- UPDATE ALL is the masthead's one primary action, far right, only while updates wait. The account name is in the footer.
- Product pictures are grey at rest and fade to colour under the pointer. Every key sets a help sentence on hover.
- Rows are drawn in a child `Ui` per row: `allocate_rect` inside a row would otherwise move the list's cursor.

## Verify
1. `cargo test`, plus `HURENTOHN_TOKEN=$(gh auth token) cargo test --release -- --ignored builds_main` after touching `build.rs` or a manifest.
2. Screenshot loop after any UI change: `HURENTOHN_TOKEN=$(gh auth token) HURENTOHN_SNAPSHOT=<png> ./target/release/hurentohn`, then with `HURENTOHN_SNAPSHOT_STATE=busy`, with `HURENTOHN_SNAPSHOT_STATE=signed-out`, and with `HURENTOHN_SNAPSHOT_SIZE=2400x1300`. Read every PNG; fix through `theme.rs` tokens. The egui snapshot doesn't contain the system traffic lights: for those, and for `docs/screenshot.png`, capture the real window (`screencapture -o -l <window id>`, id from `CGWindowListCopyWindowInfo`).
3. A normal (non-snapshot) start with `helper_offered: false` triggers the one-time admin prompt; test runs should use snapshot mode.
4. Not testable here without a person at the prompt: `install_helper`, `install_release`, `install_bundles`. Their filter is checked by hand: `spctl -a -vv -t install <pkg>` must match the helper's grep.
