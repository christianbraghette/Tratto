// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Work that must not hold up a frame: on a thread of its own on the
//! desktop. The web has no threads: there it is done at once, before
//! `spawn` returns (its results are on their channels already).

/// Runs `work` on a thread named `name` (the desktop), or now (the web).
pub fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    return std::thread::Builder::new().name(name.to_owned()).spawn(work).map(drop);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = name;
        work();
        Ok(())
    }
}
