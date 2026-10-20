// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Platform-specific input that winit does not provide: pens, and file
//! drops on Wayland.

pub mod wayland_drop;
pub mod wayland_pen;
pub mod windows_pen;
