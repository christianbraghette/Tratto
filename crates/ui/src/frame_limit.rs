// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Frame rate cap for the no-vsync presentation.
//!
//! Without vsync a continuously repainting UI (drawing, scrolling, momentum)
//! renders as fast as the GPU allows: thousands of frames per second, most
//! never shown. The cap waits at the start of a frame, before the pen input is
//! read, so latency stays at most one frame interval and the frame draws the
//! freshest samples. Idle frames are unaffected: egui only repaints on demand.
//!
//! The cap is `Settings::fps_limit` (File > Settings); `TRATTO_FPS=<n>`
//! overrides it, `0` removes it. With vsync the display paces the frames and
//! there is no cap.

use std::time::{Duration, Instant};

/// The default of `Settings::fps_limit`.
pub const DEFAULT_FPS: u32 = 120;

/// The OS sleep can overshoot by about this much; the rest is spun.
const SPIN: Duration = Duration::from_micros(1500);

pub struct FrameLimiter {
    /// The window presents with vsync (set at startup): no cap.
    vsync: bool,
    /// `TRATTO_FPS`, which wins over the setting.
    env_fps: Option<u32>,
    /// When the next frame may start.
    next: Option<Instant>,
}

impl FrameLimiter {
    pub fn new(vsync: bool) -> Self {
        let env_fps = std::env::var("TRATTO_FPS").ok().and_then(|v| v.trim().parse().ok());
        Self { vsync, env_fps, next: None }
    }

    /// The window presents with vsync.
    pub fn vsync(&self) -> bool {
        self.vsync
    }

    /// `TRATTO_FPS` is set: the setting has no effect.
    pub fn overridden(&self) -> bool {
        self.env_fps.is_some()
    }

    /// Waits until the next frame may start, at most `fps` frames per second
    /// (`0`: no cap).
    pub fn wait(&mut self, fps: u32) {
        let fps = self.env_fps.unwrap_or(fps);
        if self.vsync || fps == 0 {
            self.next = None;
            return;
        }
        let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
        if let Some(next) = self.next {
            let now = Instant::now();
            if now + SPIN < next {
                std::thread::sleep(next - now - SPIN);
            }
            while Instant::now() < next {
                std::hint::spin_loop();
            }
        }
        let start = Instant::now();
        // Keep a steady cadence; after an idle pause or a slow frame, restart
        // from now instead of rushing to catch up.
        self.next = Some(match self.next {
            Some(next) if start < next + interval => next + interval,
            _ => start + interval,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(vsync: bool) -> FrameLimiter {
        FrameLimiter { vsync, env_fps: None, next: None }
    }

    #[test]
    fn caps_the_rate() {
        let mut limiter = limiter(false);
        let start = Instant::now();
        for _ in 0..11 {
            limiter.wait(200);
        }
        // 10 intervals of 5 ms after the first frame.
        assert!(start.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn no_cap_at_zero_or_with_vsync() {
        for (vsync, fps) in [(false, 0), (true, 10)] {
            let mut limiter = limiter(vsync);
            let start = Instant::now();
            for _ in 0..11 {
                limiter.wait(fps);
            }
            assert!(start.elapsed() < Duration::from_millis(500));
        }
    }
}
