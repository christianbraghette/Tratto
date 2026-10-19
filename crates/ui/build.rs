// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows: embeds the app icon in the .exe, so Explorer, the taskbar and
//! shortcuts show it (the window icon itself is set at runtime, in main.rs).

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
}
