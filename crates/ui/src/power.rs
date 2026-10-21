// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whether the computer runs on battery, and whether the battery is low, for
//! the battery saver (`Settings::battery_saver`).
//!
//! A thread reads the power source every few seconds, started the first
//! time it is asked, and wakes the UI only when an answer changes: an idle
//! window stays idle. Where it cannot be read (a desktop, an unsupported
//! system), the answer is "not on battery".

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui;

/// How often the power source is read.
const POLL: Duration = Duration::from_secs(10);
/// At this charge (percent) or less, on battery, the battery is low.
pub const LOW_PERCENT: u8 = 20;

static ON_BATTERY: AtomicBool = AtomicBool::new(false);
static LOW: AtomicBool = AtomicBool::new(false);

/// The power source as read.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Status {
    on_battery: bool,
    /// The battery's charge, if known.
    percent: Option<u8>,
}

impl Status {
    fn low(self) -> bool {
        self.on_battery && self.percent.is_some_and(|p| p <= LOW_PERCENT)
    }
}

/// Stores `status`; true if an answer changed.
fn publish(status: Status) -> bool {
    let battery = ON_BATTERY.swap(status.on_battery, Ordering::Relaxed) != status.on_battery;
    let low = LOW.swap(status.low(), Ordering::Relaxed) != status.low();
    battery || low
}

/// Starts reading the power source (once).
fn watch(ctx: &egui::Context) {
    static WATCHER: OnceLock<()> = OnceLock::new();
    WATCHER.get_or_init(|| {
        // The first reading before the first answer.
        let Some(now) = read() else { return };
        publish(now);
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new().name("power".into()).spawn(move || {
            loop {
                std::thread::sleep(POLL);
                let Some(now) = read() else { return };
                if publish(now) {
                    ctx.request_repaint();
                }
            }
        });
    });
}

/// The computer runs on battery now (as last read).
pub fn on_battery(ctx: &egui::Context) -> bool {
    watch(ctx);
    ON_BATTERY.load(Ordering::Relaxed)
}

/// The computer runs on battery, at `LOW_PERCENT` or less (as last read).
pub fn battery_low(ctx: &egui::Context) -> bool {
    watch(ctx);
    LOW.load(Ordering::Relaxed)
}

#[cfg(windows)]
fn read() -> Option<Status> {
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
    let on_battery = match status.ac_line_status {
        0 => true,
        1 => false,
        _ => return None,
    };
    // 255: unknown.
    let percent = (status.battery_life_percent <= 100).then_some(status.battery_life_percent);
    Some(Status { on_battery, percent })
}

/// On battery when a battery is there and no charger is online; the charge
/// of all the batteries together.
#[cfg(target_os = "linux")]
fn read() -> Option<Status> {
    let read = |p: std::path::PathBuf| std::fs::read_to_string(p).map(|s| s.trim().to_owned()).unwrap_or_default();
    let (mut batteries, mut mains) = (Vec::new(), false);
    for supply in std::fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        let dir = supply.path();
        match read(dir.join("type")).as_str() {
            // A peripheral's battery (a mouse, a pen) has `scope` Device.
            "Battery" if read(dir.join("scope")) != "Device" => batteries.push(read(dir.join("capacity")).parse().ok()),
            "Battery" => {}
            _ => mains |= read(dir.join("online")) == "1",
        }
    }
    let known: Vec<u32> = batteries.iter().flatten().copied().collect();
    let percent = (!known.is_empty()).then(|| (known.iter().sum::<u32>() / known.len() as u32).min(100) as u8);
    Some(Status { on_battery: !batteries.is_empty() && !mains, percent })
}

#[cfg(target_os = "macos")]
fn read() -> Option<Status> {
    let out = std::process::Command::new("pmset").args(["-g", "batt"]).output().ok()?;
    parse_pmset(&String::from_utf8_lossy(&out.stdout))
}

/// `pmset -g batt`: "Now drawing from 'Battery Power'" (or 'AC Power'),
/// then a line per battery with its charge ("... 85%; discharging; ...").
#[cfg(any(target_os = "macos", test))]
fn parse_pmset(text: &str) -> Option<Status> {
    let mut lines = text.lines();
    let on_battery = lines.next()?.contains("Battery Power");
    let percent = lines.find_map(|line| {
        let end = line.find('%')?;
        let start = line[..end].rfind(|c: char| !c.is_ascii_digit()).map_or(0, |i| i + 1);
        line[start..end].parse::<u8>().ok()
    });
    Some(Status { on_battery, percent })
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn read() -> Option<Status> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_without_failing() {
        // Whatever this machine runs on; it must not panic or hang.
        let _ = read();
    }

    #[test]
    fn low_only_on_battery_at_the_threshold_or_less() {
        let status = |on_battery, percent| Status { on_battery, percent };
        assert!(status(true, Some(LOW_PERCENT)).low());
        assert!(status(true, Some(5)).low());
        assert!(!status(true, Some(LOW_PERCENT + 1)).low());
        assert!(!status(false, Some(5)).low(), "charging");
        assert!(!status(true, None).low(), "charge unknown");
    }

    #[test]
    fn reads_pmset() {
        let battery = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=4653155)\t18%; discharging; 1:02 remaining present: true\n";
        assert_eq!(parse_pmset(battery), Some(Status { on_battery: true, percent: Some(18) }));
        let mains = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=4653155)\t100%; charged; 0:00 remaining present: true\n";
        assert_eq!(parse_pmset(mains), Some(Status { on_battery: false, percent: Some(100) }));
        assert_eq!(parse_pmset("Now drawing from 'AC Power'\n"), Some(Status { on_battery: false, percent: None }));
    }
}
