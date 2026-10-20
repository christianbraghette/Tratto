// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! * Windows: embeds the app icon in the .exe, so Explorer, the taskbar and
//!   shortcuts show it (the window icon itself is set at runtime, in main.rs).
//! * Copies the PDFium library (for writing on PDFs) from `pdfium/<platform>`
//!   next to the built executable, where the app looks for it. See
//!   `pdfium/README.md` for how to fetch it; without it the app still builds
//!   and runs, only PDFs cannot be shown.
//! * Passes the target triple to the updater (`TRATTO_TARGET`): it picks
//!   the release archive built for it.
//! * Passes the version shown and compared by the updater (`TRATTO_VERSION`):
//!   the one in the environment when set (the release workflow sets it from
//!   the tag, e.g. `0.1.0-alpha0`), else the crate version.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/icon.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/icon.ico");
        if let Err(e) = res.compile() {
            // A missing resource compiler only costs the .exe its icon.
            println!("cargo:warning=cannot embed the app icon: {e}");
        }
    }
    copy_pdfium();
    println!("cargo:rustc-env=TRATTO_TARGET={}", std::env::var("TARGET").expect("cargo sets TARGET"));
    println!("cargo:rerun-if-env-changed=TRATTO_VERSION");
    let version = std::env::var("TRATTO_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION"));
    println!("cargo:rustc-env=TRATTO_VERSION={version}");
}

/// The PDFium library of the target platform, as laid out by the
/// pdfium-binaries archives.
fn pdfium_library() -> Option<&'static str> {
    let os = std::env::var("CARGO_CFG_TARGET_OS").ok()?;
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").ok()?;
    Some(match (os.as_str(), arch.as_str()) {
        ("windows", "x86_64") => "win-x64/bin/pdfium.dll",
        ("windows", "aarch64") => "win-arm64/bin/pdfium.dll",
        ("linux", "x86_64") => "linux-x64/lib/libpdfium.so",
        ("linux", "aarch64") => "linux-arm64/lib/libpdfium.so",
        ("macos", "x86_64") => "mac-x64/lib/libpdfium.dylib",
        ("macos", "aarch64") => "mac-arm64/lib/libpdfium.dylib",
        _ => return None,
    })
}

fn copy_pdfium() {
    let Some(lib) = pdfium_library() else { return };
    let src = Path::new("pdfium").join(lib);
    println!("cargo:rerun-if-changed={}", src.display());
    if !src.exists() {
        println!("cargo:warning=PDFium not found at crates/ui/{}: writing on PDFs will not work (see crates/ui/pdfium/README.md)", src.display());
        return;
    }
    // OUT_DIR is <target>/<profile>/build/<crate>-<hash>/out; the executable
    // goes to <target>/<profile>.
    let Some(profile_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from).and_then(|p| p.ancestors().nth(3).map(Path::to_path_buf)) else {
        return;
    };
    let name = src.file_name().expect("a file name");
    if let Err(e) = std::fs::copy(&src, profile_dir.join(name)) {
        println!("cargo:warning=cannot copy PDFium next to the executable: {e}");
    }
}
