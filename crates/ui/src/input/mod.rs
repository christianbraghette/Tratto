// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Platform-specific input that winit does not provide: pens, file drops
//! on Wayland, and Windows' multi-finger gestures in fullscreen; touchpad
//! scrolling as egui should get it.

pub mod system_gestures;
pub mod touchpad;
pub mod wayland_drop;
pub mod wayland_pen;
#[cfg(target_arch = "wasm32")]
pub mod web_pen;
pub mod windows_pen;
