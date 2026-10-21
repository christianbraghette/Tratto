// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whether the computer runs on battery, for the battery saver
//! (`Settings::battery_saver`).
//!
//! A thread reads the power source every few seconds, started the first
//! time it is asked, and wakes the UI only when it changes: an idle window
//! stays idle. Where it cannot be read (a desktop, an unsupported system),
//! the answer is "not on battery".

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui;

/// How often the power source is read.
const POLL: Duration = Duration::from_secs(10);

static ON_BATTERY: AtomicBool = AtomicBool::new(false);

/// The computer runs on battery now (as last read).
pub fn on_battery(ctx: &egui::Context) -> bool {
    static WATCHER: OnceLock<()> = OnceLock::new();
    WATCHER.get_or_init(|| {
        // The first reading before the first answer.
        let Some(now) = read() else { return };
        ON_BATTERY.store(now, Ordering::Relaxed);
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new().name("power".into()).spawn(move || {
            loop {
                std::thread::sleep(POLL);
                let Some(now) = read() else { return };
                if ON_BATTERY.swap(now, Ordering::Relaxed) != now {
                    ctx.request_repaint();
                }
            }
        });
    });
    ON_BATTERY.load(Ordering::Relaxed)
}

/// On battery (`Some(true)`), on mains (`Some(false)`), or unknown.
#[cfg(windows)]
fn read() -> Option<bool> {
    #[repr(C)]
    #[derive(Default)]
    struct SystemPowerStatus {
        ac_line_status: u8,
        battery_flag: u8,
        battery_life_percent: u8,
        system_status_flag: u8,
        battery_life_time: u32,
        battery_full_life_time: u32,
    }
    // Declared here so it does not need an optional windows-sys feature.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemPowerStatus(status: *mut SystemPowerStatus) -> i32;
    }
    let mut status = SystemPowerStatus::default();
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return None;
    }
    // 0: offline, 1: online, 255: unknown.
    match status.ac_line_status {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// On battery when a battery is there and no charger is online.
#[cfg(target_os = "linux")]
fn read() -> Option<bool> {
    let read = |p: std::path::PathBuf| std::fs::read_to_string(p).map(|s| s.trim().to_owned()).unwrap_or_default();
    let (mut battery, mut mains) = (false, false);
    for supply in std::fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        let dir = supply.path();
        match read(dir.join("type")).as_str() {
            // A peripheral's battery (a mouse, a pen) has `scope` Device.
            "Battery" => battery |= read(dir.join("scope")) != "Device",
            _ => mains |= read(dir.join("online")) == "1",
        }
    }
    Some(battery && !mains)
}

/// `pmset -g ps` starts with "Now drawing from 'Battery Power'" or "'AC Power'".
#[cfg(target_os = "macos")]
fn read() -> Option<bool> {
    let out = std::process::Command::new("pmset").args(["-g", "ps"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next()?;
    Some(first.contains("Battery Power"))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn read() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_without_failing() {
        // Whatever this machine runs on; it must not panic or hang.
        let _ = super::read();
    }
}
