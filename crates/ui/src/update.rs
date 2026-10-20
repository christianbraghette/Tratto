// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Updates from the project's GitHub releases.
//!
//! Each release carries one archive per platform, `tratto-<target>.zip`
//! (see `.github/workflows/release.yml`), holding what goes next to the
//! executable: `tratto(.exe)`, the PDFium library and the licenses. At
//! startup the latest release is asked for in the background; if it is
//! newer, its archive is downloaded, checked against the SHA-256 digest
//! GitHub publishes for it, and swapped in: each file is renamed to
//! `<name>.old` (a running executable or loaded library cannot be
//! overwritten on Windows, but it can be renamed) and the new one takes its
//! place. The running app is not affected; the next start runs the new
//! version and removes the `.old` files.
//!
//! Debug builds and installations in a folder the user cannot write to
//! (installed by a package manager) only report the new version.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use eframe::egui;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// `owner/name` of the GitHub repository the releases come from.
const REPO: &str = "christianbraghette/inklav";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// The Rust target triple this executable was built for (set by build.rs).
const TARGET: &str = env!("TRATTO_TARGET");
/// Archives larger than this are refused.
const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;
const OLD: &str = ".old";
const NEW: &str = ".new";

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
    /// The app can install it itself: there is an archive for this platform
    /// and the installation folder is writable.
    pub installable: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// "sha256:<hex>" (GitHub computes it on upload).
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
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

    /// Asks for the latest release; with `install`, a newer one is also
    /// downloaded and installed when possible.
    pub fn check(&mut self, ctx: &egui::Context, install: bool) {
        if self.state.busy() {
            return;
        }
        self.state = State::Checking;
        self.run(ctx, move |send| match latest_release() {
            Err(e) => send(State::Failed(e)),
            Ok(None) => send(State::UpToDate),
            Ok(Some(release)) if install && release.installable => download_and_install(release, send),
            Ok(Some(release)) => send(State::Available(release)),
        });
    }

    /// Installs the release found by a check that only notified.
    pub fn install(&mut self, ctx: &egui::Context) {
        let State::Available(release) = &self.state else { return };
        if !release.installable {
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
    let (Some(asset), Some(dir)) = (&release.archive, install_dir()) else {
        return send(State::Available(release));
    };
    match download(asset).and_then(|bytes| install(&bytes, &dir)) {
        Ok(()) => send(State::Installed(release.version)),
        Err(e) => send(State::Failed(e)),
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .user_agent(&format!("Tratto/{CURRENT}"))
        .build()
}

/// The latest release if it is newer than this version (`None`: up to date,
/// or no release published yet).
fn latest_release() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let response = match agent().get(&url).set("Accept", "application/vnd.github+json").call() {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(e) => return Err(format!("Cannot check for updates: {e}")),
    };
    let gh: GhRelease = response.into_json().map_err(|e| format!("Unexpected reply from GitHub: {e}"))?;
    let version = gh.tag_name.trim_start_matches('v').to_owned();
    if !is_newer(&version, CURRENT) {
        return Ok(None);
    }
    let name = archive_name();
    let archive = gh.assets.into_iter().find(|a| a.name == name);
    let installable = archive.is_some() && install_dir().is_some();
    Ok(Some(Release { version, page: gh.html_url, archive, installable }))
}

fn archive_name() -> String {
    format!("tratto-{TARGET}.zip")
}

/// `a` is a later version than `b` ("1.10.0" > "1.9.2"; pre-release and
/// build suffixes are ignored, unparsable versions are never newer).
fn is_newer(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let core = v.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>());
        let major = parts.next()?.ok()?;
        let minor = parts.next().unwrap_or(Ok(0)).ok()?;
        let patch = parts.next().unwrap_or(Ok(0)).ok()?;
        Some((major, minor, patch))
    }
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

fn download(asset: &Asset) -> Result<Vec<u8>, String> {
    if asset.size > MAX_DOWNLOAD {
        return Err(format!("The update is too large ({} MB)", asset.size / (1024 * 1024)));
    }
    let fail = |e: &dyn std::fmt::Display| format!("Cannot download the update: {e}");
    let response = agent().get(&asset.browser_download_url).call().map_err(|e| fail(&e))?;
    let mut bytes = Vec::with_capacity(asset.size as usize);
    response.into_reader().take(MAX_DOWNLOAD + 1).read_to_end(&mut bytes).map_err(|e| fail(&e))?;
    if bytes.len() as u64 != asset.size {
        return Err(fail(&"incomplete download"));
    }
    if let Some(expected) = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")) {
        if !hex(&Sha256::digest(&bytes)).eq_ignore_ascii_case(expected) {
            return Err("The downloaded update is damaged (checksum mismatch)".to_owned());
        }
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The folder of the running executable, if the app may update itself there.
fn install_dir() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        return None;
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // Writable by this user: not installed by a package manager.
    let probe = dir.join(".tratto-update-probe");
    std::fs::write(&probe, b"").ok()?;
    let _ = std::fs::remove_file(&probe);
    Some(dir)
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
            let rel = entry.enclosed_name().ok_or_else(|| fail(&format!("unsafe path {}", entry.name())))?;
            let dest = dir.join(rel);
            let new = with_suffix(&dest, NEW);
            if let Some(parent) = new.parent() {
                std::fs::create_dir_all(parent).map_err(|e| fail(&e))?;
            }
            let mut out = std::fs::File::create(&new).map_err(|e| fail(&e))?;
            files.push(dest);
            std::io::copy(&mut entry, &mut out).map_err(|e| fail(&e))?;
            // The archive's permissions, else those of the file replaced (an
            // archive zipped on Windows has none: the executable must stay one).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = entry.unix_mode().or_else(|_| std::fs::metadata(&dest).ok().map(|m| m.permissions().mode()));
                if let Some(mode) = mode {
                    std::fs::set_permissions(&new, std::fs::Permissions::from_mode(mode)).map_err(|e| fail(&e))?;
                }
            }
        }
        let has_exe = exe_name.as_ref().is_some_and(|n| files.iter().any(|f| f.parent() == Some(dir) && f.file_name() == Some(n)));
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

/// Removes what the previous update left: the replaced files (`.old`) and
/// any unpacked but never swapped in (`.new`). Called at startup.
pub fn remove_leftovers() {
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) else {
        return;
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
    if let Ok(exe) = std::env::current_exe() {
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
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
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
    fn refuses_paths_outside_the_folder() {
        let dir = temp_dir("escape");
        let zip = zip_of(&[(&exe_name(), b"new exe"), ("../evil.txt", b"x")]);
        assert!(install(&zip, &dir).is_err());
        assert!(!dir.parent().unwrap().join("evil.txt.new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
