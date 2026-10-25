// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Updates from the project's GitHub releases.
//!
//! Each release carries, per platform, what each kind of installation
//! (`Install`) is updated from (see `.github/workflows/release.yml`):
//! * a folder (the portable archive, or where the Windows installer put
//!   the app): `tratto-<target>.zip`, holding what goes next to the
//!   executable: `tratto(.exe)`, the PDFium library and the licenses;
//! * a macOS app bundle: `tratto-<target>.app.zip`, holding `Tratto.app`;
//! * a Linux AppImage: `Tratto-<arch>.AppImage` itself.
//!
//! At startup the releases are asked for in the background and the newest
//! one of the chosen channel is picked (`UpdateChannel`: by the tag's
//! suffix, releases, `-rc<n>` and `-beta<n>`, or `-alpha<n>`); if it is newer, its file
//! is downloaded, checked against the SHA-256 digest GitHub publishes for
//! it, and swapped in. In a folder each file is renamed to `<name>.old` (a
//! running executable or loaded library cannot be overwritten on Windows,
//! but it can be renamed) and the new one takes its place; a bundle is
//! swapped whole (its signature covers all of it), the old one renamed to
//! `<name>.app.old`; an AppImage is replaced at once. The running app is
//! not affected; the next start runs the new version and removes the
//! `.old` files.
//!
//! Debug builds and installations the user cannot write to (installed by a
//! package manager) only report the new version.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use eframe::egui;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::settings::UpdateChannel;

/// `owner/name` of the GitHub repository the releases come from.
const REPO: &str = "christianbraghette/Tratto";
/// This version, with the pre-release suffix of the tag it was built from
/// (set by build.rs).
pub const CURRENT: &str = env!("TRATTO_VERSION");
/// The Rust target triple this executable was built for (set by build.rs).
const TARGET: &str = env!("TRATTO_TARGET");
/// Archives larger than this are refused.
const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;
const OLD: &str = ".old";
const NEW: &str = ".new";
/// The app bundle in a macOS release's `.app.zip`.
const BUNDLE: &str = "Tratto.app";
/// Releases looked at, most recent first (one page of the GitHub API).
const RELEASES: u32 = 100;

/// Set to start the app again once the window has closed (after an update).
pub static RESTART: AtomicBool = AtomicBool::new(false);

/// A release newer than the running version.
#[derive(Clone, Debug)]
pub struct Release {
    /// "1.2.0" (the tag without its `v`).
    pub version: String,
    /// The release on GitHub, for a manual download.
    pub page: String,
    /// The archive for this platform, if the release has one.
    archive: Option<Asset>,
    /// Why the app cannot install it itself (`None`: it can, there is an
    /// archive for this installation and the user may write to it).
    pub blocked: Option<String>,
}

impl Release {
    pub fn installable(&self) -> bool {
        self.blocked.is_none()
    }
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// `sha256:<hex>` (GitHub computes it on upload).
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    /// GitHub's pre-release flag: keeps the release off the Stable channel.
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    UpToDate,
    /// Newer and not installed: only notifying, or not installable here.
    Available(Release),
    Downloading(Release),
    /// Installed; used from the next start (the version).
    Installed(String),
    Failed(String),
}

impl State {
    pub fn busy(&self) -> bool {
        matches!(self, State::Checking | State::Downloading(_))
    }
}

/// The update check and installation, run in the background.
#[derive(Default)]
pub struct Updater {
    state: State,
    rx: Option<Receiver<State>>,
}

impl Updater {
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Asks for the latest release of `channel`; with `install`, a newer one
    /// is also downloaded and installed when possible.
    pub fn check(&mut self, ctx: &egui::Context, channel: UpdateChannel, install: bool) {
        if self.state.busy() {
            return;
        }
        self.state = State::Checking;
        self.run(ctx, move |send| match latest_release(channel) {
            Err(e) => send(State::Failed(e)),
            Ok(None) => send(State::UpToDate),
            Ok(Some(release)) if install && release.installable() => download_and_install(release, send),
            Ok(Some(release)) => send(State::Available(release)),
        });
    }

    /// Installs the release found by a check that only notified.
    pub fn install(&mut self, ctx: &egui::Context) {
        let State::Available(release) = &self.state else { return };
        if !release.installable() {
            return;
        }
        let release = release.clone();
        self.state = State::Downloading(release.clone());
        self.run(ctx, move |send| download_and_install(release, send));
    }

    /// Takes the progress of the background work; returns the new state if
    /// it changed.
    pub fn poll(&mut self) -> Option<&State> {
        let mut changed = false;
        if let Some(rx) = &self.rx {
            while let Ok(state) = rx.try_recv() {
                self.state = state;
                changed = true;
            }
        }
        if !self.state.busy() {
            self.rx = None;
        }
        changed.then_some(&self.state)
    }

    fn run(&mut self, ctx: &egui::Context, work: impl FnOnce(&dyn Fn(State)) + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            work(&|state| {
                let _ = tx.send(state);
                ctx.request_repaint();
            })
        });
    }
}

fn download_and_install(release: Release, send: &dyn Fn(State)) {
    send(State::Downloading(release.clone()));
    let (Some(asset), Ok(installed)) = (&release.archive, Install::find()) else {
        return send(State::Available(release));
    };
    match download(asset).and_then(|bytes| installed.install(&bytes)) {
        Ok(()) => send(State::Installed(release.version)),
        Err(e) => send(State::Failed(e)),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        // A whole archive: generous, but a stalled download still ends.
        .timeout_recv_body(Some(Duration::from_secs(600)))
        .user_agent(format!("Tratto/{CURRENT}"))
        .build()
        .into()
}

/// The latest release of `channel` if it is newer than this version
/// (`None`: up to date, or no release published yet).
fn latest_release(channel: UpdateChannel) -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page={RELEASES}");
    let mut response = match agent().get(&url).header("Accept", "application/vnd.github+json").call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(format!("Cannot check for updates: {e}")),
    };
    let releases: Vec<GhRelease> =
        response.body_mut().read_json().map_err(|e| format!("Unexpected reply from GitHub: {e}"))?;
    let newest = releases
        .into_iter()
        .filter(|r| !r.draft)
        .map(|r| (r.tag_name.trim_start_matches('v').to_owned(), r))
        .filter(|(v, r)| channel_of(v, r.prerelease).is_some_and(|c| c <= channel))
        .reduce(|a, b| if is_newer(&b.0, &a.0) { b } else { a });
    let Some((version, gh)) = newest.filter(|(v, _)| is_newer(v, CURRENT)) else {
        return Ok(None);
    };
    let name = Install::locate().map_or_else(|| format!("tratto-{TARGET}.zip"), |i| i.asset_name());
    let archive = gh.assets.into_iter().find(|a| a.name == name);
    let blocked = match (&archive, Install::find()) {
        (None, _) => Some(format!("This release has no {name} to update from.")),
        (Some(_), Err(why)) => Some(why),
        (Some(_), Ok(_)) => None,
    };
    Ok(Some(Release { version, page: gh.html_url, archive, blocked }))
}

/// `a` is a later version than `b` ("1.10.0" > "1.9.2"). Pre-releases come
/// before their release and are ordered by name, then number ("1.0.0-alpha2"
/// < "1.0.0-alpha10" < "1.0.0-beta0" < "1.0.0"); build suffixes are ignored,
/// unparsable versions are never newer.
fn is_newer(a: &str, b: &str) -> bool {
    matches!((parse_version(a), parse_version(b)), (Some(a), Some(b)) if a > b)
}

/// (major, minor, patch, is release, pre-release name, pre-release number)
fn parse_version(v: &str) -> Option<(u64, u64, u64, bool, String, u64)> {
    let v = v.split('+').next()?;
    let (core, pre) = v.split_once('-').unwrap_or((v, ""));
    let mut parts = core.split('.').map(|p| p.parse::<u64>());
    let major = parts.next()?.ok()?;
    let minor = parts.next().unwrap_or(Ok(0)).ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    let name = pre.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    let number = pre[name.len()..].trim_start_matches('.');
    let number = if number.is_empty() { 0 } else { number.parse().ok()? };
    Some((major, minor, patch, pre.is_empty(), name.to_owned(), number))
}

/// The steadiest channel a version is offered on, by its tag: releases on
/// Stable, release candidates and betas on Testing, alphas (and any other
/// pre-release) on Unstable. GitHub's pre-release flag (set by the release workflow from
/// the tag) only keeps a release off Stable, should it be set by hand.
/// `None` for an unparsable version.
fn channel_of(version: &str, prerelease: bool) -> Option<UpdateChannel> {
    let (_, _, _, release, name, _) = parse_version(version)?;
    Some(match name.as_str() {
        _ if release && prerelease => UpdateChannel::Testing,
        _ if release => UpdateChannel::Stable,
        "rc" | "beta" => UpdateChannel::Testing,
        _ => UpdateChannel::Unstable,
    })
}

fn download(asset: &Asset) -> Result<Vec<u8>, String> {
    if asset.size > MAX_DOWNLOAD {
        return Err(format!("The update is too large ({} MB)", asset.size / (1024 * 1024)));
    }
    let fail = |e: &dyn std::fmt::Display| format!("Cannot download the update: {e}");
    let response = agent().get(&asset.browser_download_url).call().map_err(|e| fail(&e))?;
    let mut bytes = Vec::with_capacity(asset.size as usize);
    response.into_body().into_reader().take(MAX_DOWNLOAD + 1).read_to_end(&mut bytes).map_err(|e| fail(&e))?;
    if bytes.len() as u64 != asset.size {
        return Err(fail(&"incomplete download"));
    }
    if let Some(expected) = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:"))
        && !hex(&Sha256::digest(&bytes)).eq_ignore_ascii_case(expected)
    {
        return Err("The downloaded update is damaged (checksum mismatch)".to_owned());
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// How the running app is installed: what an update replaces.
#[derive(Debug)]
enum Install {
    /// The folder of the executable (the portable archive, the Windows
    /// installer's): the archive's files go over those in it.
    Folder(PathBuf),
    /// macOS: the app bundle, replaced whole by the archive's `Tratto.app`.
    Bundle(PathBuf),
    /// Linux: the AppImage file, replaced by the release's.
    AppImage(PathBuf),
}

impl Install {
    /// The installation, if the app may update it itself; else why not.
    fn find() -> Result<Self, String> {
        if cfg!(debug_assertions) {
            return Err("This is a development build: it does not update itself.".to_owned());
        }
        let unknown = || "Tratto cannot tell where it is installed.".to_owned();
        let found = Self::locate().ok_or_else(unknown)?;
        let dir = match &found {
            Install::Folder(dir) => dir.clone(),
            Install::Bundle(path) | Install::AppImage(path) => path.parent().ok_or_else(unknown)?.to_path_buf(),
        };
        // Writable by this user: not installed by a package manager.
        let probe = dir.join(".tratto-update-probe");
        if std::fs::write(&probe, b"").is_err() {
            return Err(format!(
                "Tratto cannot write to {}. Move it to a folder of yours, or download the update.",
                dir.display()
            ));
        }
        let _ = std::fs::remove_file(&probe);
        Ok(found)
    }

    /// The installation, writable or not.
    fn locate() -> Option<Self> {
        if let Some(app_image) = app_image() {
            return Some(Install::AppImage(app_image));
        }
        let exe = std::env::current_exe().ok()?;
        if cfg!(target_os = "macos")
            && let Some(bundle) = bundle_of(&exe)
        {
            return Some(Install::Bundle(bundle));
        }
        Some(Install::Folder(exe.parent()?.to_path_buf()))
    }

    /// The release file it is updated from.
    fn asset_name(&self) -> String {
        match self {
            Install::Folder(_) => format!("tratto-{TARGET}.zip"),
            Install::Bundle(_) => format!("tratto-{TARGET}.app.zip"),
            Install::AppImage(_) => format!("Tratto-{}.AppImage", TARGET.split('-').next().unwrap_or(TARGET)),
        }
    }

    /// Puts the downloaded release file in place.
    fn install(&self, bytes: &[u8]) -> Result<(), String> {
        match self {
            Install::Folder(dir) => install(bytes, dir),
            Install::Bundle(app) => install_bundle(bytes, app),
            Install::AppImage(path) => install_app_image(bytes, path),
        }
    }
}

/// The AppImage the app runs from (its runtime sets `APPIMAGE`).
fn app_image() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_absolute() && p.is_file())
}

/// The app bundle `exe` is the executable of (`<name>.app/Contents/MacOS/<exe>`).
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && app.extension()? == "app")
        .then(|| app.to_path_buf())
}

/// What starts the app: the AppImage it runs from (the executable inside
/// is in a folder that goes away with the app), else the executable.
pub fn launcher() -> std::io::Result<PathBuf> {
    app_image().map_or_else(std::env::current_exe, Ok)
}

/// `path` with `suffix` appended to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Unpacks the release archive `zip` over the installation in `dir`.
fn install(zip: &[u8], dir: &Path) -> Result<(), String> {
    let exe_name = std::env::current_exe().ok().and_then(|p| p.file_name().map(ToOwned::to_owned));
    let fail = |e: &dyn std::fmt::Display| format!("Cannot install the update: {e}");
    let mut archive = zip::ZipArchive::new(Cursor::new(zip)).map_err(|e| fail(&e))?;

    // Unpack everything next to the files it replaces first: a failure here
    // leaves the installation untouched.
    let mut files = Vec::new();
    let unpacked = (|| -> Result<(), String> {
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| fail(&e))?;
            if entry.is_dir() {
                continue;
            }
            // Refuses absolute paths and `..`.
            let rel = entry
                .enclosed_name()
                .ok_or_else(|| fail(&format!("unsafe path {}", entry.name().unwrap_or_default())))?;
            let dest = dir.join(rel);
            let new = with_suffix(&dest, NEW);
            if let Some(parent) = new.parent() {
                std::fs::create_dir_all(parent).map_err(|e| fail(&e))?;
            }
            files.push(dest.clone());
            // The archive's permissions, else those of the file replaced (an
            // archive zipped on Windows has none: the executable must stay one).
            let mode = entry.unix_mode().or_else(|| mode_of(&dest));
            write_entry(&mut entry, &new, mode).map_err(|e| fail(&e))?;
        }
        let has_exe = exe_name
            .as_ref()
            .is_some_and(|n| files.iter().any(|f| f.parent() == Some(dir) && f.file_name() == Some(n)));
        if has_exe { Ok(()) } else { Err(fail(&"the archive has no Tratto executable")) }
    })();
    if let Err(e) = unpacked {
        for f in &files {
            let _ = std::fs::remove_file(with_suffix(f, NEW));
        }
        return Err(e);
    }

    // Swap them in; on a failure, put back what was already swapped.
    for (i, dest) in files.iter().enumerate() {
        let old = with_suffix(dest, OLD);
        let _ = std::fs::remove_file(&old);
        let had_old = dest.exists();
        let swapped = (|| {
            if had_old {
                std::fs::rename(dest, &old)?;
            }
            std::fs::rename(with_suffix(dest, NEW), dest)
        })();
        if let Err(e) = swapped {
            for done in files[..=i].iter().rev() {
                let old = with_suffix(done, OLD);
                if old.exists() {
                    let _ = std::fs::remove_file(done);
                    let _ = std::fs::rename(&old, done);
                }
            }
            for f in &files {
                let _ = std::fs::remove_file(with_suffix(f, NEW));
            }
            return Err(fail(&e));
        }
    }
    Ok(())
}

/// Unpacks the archive's `Tratto.app` next to the bundle `app` and swaps
/// it in whole, the old one renamed to `<app>.old`.
fn install_bundle(zip: &[u8], app: &Path) -> Result<(), String> {
    let exe_name = std::env::current_exe().ok().and_then(|p| p.file_name().map(ToOwned::to_owned));
    let fail = |e: &dyn std::fmt::Display| format!("Cannot install the update: {e}");
    let staging = with_suffix(app, NEW);
    let _ = std::fs::remove_dir_all(&staging);
    let unpacked = unpack(zip, &staging).map_err(|e| fail(&e)).and_then(|()| {
        let new_app = staging.join(BUNDLE);
        let has_exe = exe_name.is_some_and(|n| new_app.join("Contents/MacOS").join(n).is_file());
        if has_exe { Ok(new_app) } else { Err(fail(&"the archive has no Tratto app")) }
    });
    let swapped = unpacked.and_then(|new_app| {
        let old = with_suffix(app, OLD);
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(app, &old).map_err(|e| fail(&e))?;
        std::fs::rename(&new_app, app).map_err(|e| {
            let _ = std::fs::rename(&old, app);
            fail(&e)
        })
    });
    let _ = std::fs::remove_dir_all(&staging);
    swapped
}

/// Writes the downloaded AppImage next to the one at `path` and renames it
/// over it: the running app keeps reading the old one, already open.
fn install_app_image(bytes: &[u8], path: &Path) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("Cannot install the update: {e}");
    // An AppImage is an executable.
    if !bytes.starts_with(b"\x7fELF") {
        return Err(fail(&"the download is not an AppImage"));
    }
    let new = with_suffix(path, NEW);
    let written = write_entry(&mut &bytes[..], &new, Some(0o755)).and_then(|()| std::fs::rename(&new, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&new);
        return Err(fail(&e));
    }
    Ok(())
}

/// Unpacks all of `zip` into the new folder `dir`, which is left partly
/// filled on an error.
fn unpack(zip: &[u8], dir: &Path) -> std::io::Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        // Refuses absolute paths and `..`.
        let Some(rel) = entry.enclosed_name() else {
            return Err(std::io::Error::other(format!("unsafe path {}", entry.name().unwrap_or_default())));
        };
        let dest = dir.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            std::fs::create_dir_all(dest.parent().unwrap_or(dir))?;
            let mode = entry.unix_mode();
            write_entry(&mut entry, &dest, mode)?;
        }
    }
    Ok(())
}

/// Writes what `data` reads to a new file at `path`, with the Unix
/// permissions `mode` (if any).
fn write_entry(data: &mut impl Read, path: &Path, mode: Option<u32>) -> std::io::Result<()> {
    let mut out = std::fs::File::create(path)?;
    std::io::copy(data, &mut out)?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

/// The Unix permissions of the file at `path`.
fn mode_of(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).ok().map(|m| m.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Removes what the previous update left: the replaced files (`.old`) and
/// any unpacked but never swapped in (`.new`). Called at startup.
pub fn remove_leftovers() {
    let dir = match Install::locate() {
        Some(Install::Folder(dir)) => dir,
        Some(Install::Bundle(app)) => {
            for leftover in [with_suffix(&app, OLD), with_suffix(&app, NEW)] {
                let _ = std::fs::remove_dir_all(leftover);
            }
            return;
        }
        Some(Install::AppImage(path)) => {
            let _ = std::fs::remove_file(with_suffix(&path, NEW));
            return;
        }
        None => return,
    };
    fn sweep(dir: &Path, depth: u32) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if depth > 0 {
                    sweep(&path, depth - 1);
                }
            } else if name.ends_with(OLD) || name.ends_with(NEW) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    // The archive's files sit next to the executable or one folder down
    // (the licenses).
    sweep(&dir, 1);
}

/// Starts the app again if an update asked for it (`RESTART`), once the
/// window has closed and the settings are saved.
pub fn restart_if_requested() {
    if !RESTART.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(exe) = launcher() {
        let _ = std::process::Command::new(exe).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn versions_compare() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("1.10.0", "1.9.2"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta", "0.1.0"));
        assert!(is_newer("0.1.0", "0.1.0-beta3"));
        assert!(is_newer("0.1.0-alpha1", "0.1.0-alpha0"));
        assert!(is_newer("0.1.0-alpha10", "0.1.0-alpha2"));
        assert!(is_newer("0.1.0-beta0", "0.1.0-alpha9"));
        assert!(is_newer("0.1.0-beta.2", "0.1.0-beta.1"));
        assert!(is_newer("0.1.0-rc1", "0.1.0-beta9"));
        assert!(is_newer("0.1.0-rc2", "0.1.0-rc1"));
        assert!(is_newer("0.1.0", "0.1.0-rc2"));
        assert!(!is_newer("0.1.0-alpha0", "0.1.0-alpha0"));
        assert!(is_newer("0.2.0-alpha0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    #[test]
    fn versions_have_channels() {
        assert_eq!(channel_of("1.2.0", false), Some(UpdateChannel::Stable));
        assert_eq!(channel_of("1.2.0+build5", false), Some(UpdateChannel::Stable));
        assert_eq!(channel_of("1.2.0-beta1", false), Some(UpdateChannel::Testing));
        assert_eq!(channel_of("1.2.0-beta.2", false), Some(UpdateChannel::Testing));
        assert_eq!(channel_of("1.2.0-alpha0", false), Some(UpdateChannel::Unstable));
        assert_eq!(channel_of("1.2.0-rc1", false), Some(UpdateChannel::Testing));
        assert_eq!(channel_of("1.2.0-rc.2", true), Some(UpdateChannel::Testing));
        assert_eq!(channel_of("1.2.0-dev3", false), Some(UpdateChannel::Unstable));
        assert_eq!(channel_of("nightly", false), None);
        // Flagged as a pre-release on GitHub: never Stable, else by the tag.
        assert_eq!(channel_of("1.2.0", true), Some(UpdateChannel::Testing));
        assert_eq!(channel_of("1.2.0-beta1", true), Some(UpdateChannel::Testing));
        assert_eq!(channel_of("1.2.0-alpha0", true), Some(UpdateChannel::Unstable));
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            out.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            out.write_all(data).unwrap();
        }
        out.finish().unwrap().into_inner()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tratto-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn exe_name() -> String {
        std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn installs_over_the_old_files() {
        let dir = temp_dir("install");
        let exe = exe_name();
        std::fs::write(dir.join(&exe), b"old exe").unwrap();
        std::fs::write(dir.join("lib.dll"), b"old lib").unwrap();
        let zip = zip_of(&[(&exe, b"new exe"), ("lib.dll", b"new lib"), ("licenses/a.txt", b"license")]);
        install(&zip, &dir).unwrap();
        assert_eq!(std::fs::read(dir.join(&exe)).unwrap(), b"new exe");
        assert_eq!(std::fs::read(dir.join("lib.dll")).unwrap(), b"new lib");
        assert_eq!(std::fs::read(dir.join("licenses").join("a.txt")).unwrap(), b"license");
        assert_eq!(std::fs::read(with_suffix(&dir.join("lib.dll"), OLD)).unwrap(), b"old lib");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_archives_without_the_app_and_leaves_no_trace() {
        let dir = temp_dir("refuse");
        std::fs::write(dir.join("lib.dll"), b"old lib").unwrap();
        let zip = zip_of(&[("lib.dll", b"new lib")]);
        assert!(install(&zip, &dir).is_err());
        assert_eq!(std::fs::read(dir.join("lib.dll")).unwrap(), b"old lib");
        assert!(!with_suffix(&dir.join("lib.dll"), NEW).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn swaps_a_bundle_whole() {
        let dir = temp_dir("bundle");
        let app = dir.join("Tratto.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join(exe_name()), b"old exe").unwrap();
        std::fs::write(macos.join("gone"), b"only in the old version").unwrap();
        let exe = format!("Tratto.app/Contents/MacOS/{}", exe_name());
        let zip = zip_of(&[(&exe, b"new exe"), ("Tratto.app/Contents/Info.plist", b"plist")]);
        install_bundle(&zip, &app).unwrap();
        assert_eq!(std::fs::read(macos.join(exe_name())).unwrap(), b"new exe");
        assert!(app.join("Contents/Info.plist").exists());
        assert!(!macos.join("gone").exists());
        assert!(with_suffix(&app, OLD).join("Contents/MacOS/gone").exists());
        assert!(!with_suffix(&app, NEW).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_the_bundle_when_the_archive_has_no_app() {
        let dir = temp_dir("bundle-refuse");
        let app = dir.join("Tratto.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        let zip = zip_of(&[("Other.app/Contents/MacOS/other", b"x")]);
        assert!(install_bundle(&zip, &app).is_err());
        assert!(app.join("Contents/MacOS").is_dir());
        assert!(!with_suffix(&app, NEW).exists() && !with_suffix(&app, OLD).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_the_bundle_of_an_executable() {
        let exe = Path::new("/Applications/Tratto.app/Contents/MacOS/tratto");
        assert_eq!(bundle_of(exe), Some(PathBuf::from("/Applications/Tratto.app")));
        assert_eq!(bundle_of(Path::new("/opt/tratto/bin/tratto")), None);
    }

    #[test]
    fn replaces_an_app_image() {
        let dir = temp_dir("appimage");
        let path = dir.join("Tratto-x86_64.AppImage");
        std::fs::write(&path, b"\x7fELF old").unwrap();
        assert!(install_app_image(b"<html>not found</html>", &path).is_err());
        install_app_image(b"\x7fELF new", &path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"\x7fELF new");
        assert!(!with_suffix(&path, NEW).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_paths_outside_the_folder() {
        let dir = temp_dir("escape");
        let zip = zip_of(&[(&exe_name(), b"new exe"), ("../evil.txt", b"x")]);
        assert!(install(&zip, &dir).is_err());
        assert!(!dir.parent().unwrap().join("evil.txt.new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
