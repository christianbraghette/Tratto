// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Windows' three- and four-finger touch gestures, in fullscreen.
//!
//! In fullscreen the app takes the screen's edge swipes from the system
//! (see `chrome::own_edge_swipes`), and with them the system no longer
//! recognises the multi-finger gestures either. They are recognised here
//! instead and run as the shortcuts the system's gestures stand for
//! (Windows 11's defaults):
//!
//! | fingers | up        | down         | left / right        | tap           |
//! |---------|-----------|--------------|---------------------|---------------|
//! | 3       | Task view | Show desktop | Last app (Alt+Tab)  | Search        |
//! | 4       | Task view | Show desktop | Next / prev desktop | Notifications |
//!
//! [`FingerGestures`] only tracks the touches; [`run`] sends the keys (on
//! Windows; nothing elsewhere).

use std::collections::HashMap;

use eframe::egui::{Event, Pos2, TouchPhase, Vec2};

/// Fingers move this far together (points) to swipe...
const SWIPE_DIST: f32 = 80.0;
/// ...a tap moves less than this and lifts within `TAP_TIME` (s).
const TAP_SLOP: f32 = 20.0;
const TAP_TIME: f64 = 0.35;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    Swipe { fingers: u8, dir: Dir },
    Tap { fingers: u8 },
}

#[derive(Default)]
pub struct FingerGestures {
    /// The fingers down: where each started and is now.
    touches: HashMap<u64, (Pos2, Pos2)>,
    /// The most fingers down at once since the first one landed.
    most: usize,
    /// When the first one landed.
    start: f64,
    /// Furthest any finger moved (points).
    moved: f32,
    /// The gesture already ran (once per touch).
    done: bool,
}

impl FingerGestures {
    /// Three or more fingers are (or were, since the first landed) down: the
    /// canvas should ignore them until they all lift.
    pub fn active(&self) -> bool {
        !self.touches.is_empty() && self.most >= 3
    }

    /// Follows the touches in `events`; the gesture they complete, if any.
    pub fn update(&mut self, events: &[Event], now: f64) -> Option<Gesture> {
        let mut found = None;
        for e in events {
            let Event::Touch { id, phase, pos, .. } = e else { continue };
            match phase {
                TouchPhase::Start => {
                    if self.touches.is_empty() {
                        *self = Self { start: now, ..Self::default() };
                    }
                    self.touches.insert(id.0, (*pos, *pos));
                    self.most = self.most.max(self.touches.len());
                }
                TouchPhase::Move => {
                    if let Some((start, at)) = self.touches.get_mut(&id.0) {
                        *at = *pos;
                        self.moved = self.moved.max((*pos - *start).length());
                    }
                    found = found.or(self.swipe());
                }
                TouchPhase::End | TouchPhase::Cancel => {
                    let cancel = *phase == TouchPhase::Cancel;
                    self.touches.remove(&id.0);
                    if self.touches.is_empty() && !cancel {
                        found = found.or(self.tap(now));
                    }
                }
            }
        }
        found
    }

    fn fingers(&self) -> Option<u8> {
        matches!(self.most, 3 | 4).then_some(self.most as u8)
    }

    /// The fingers down moved far enough together.
    fn swipe(&mut self) -> Option<Gesture> {
        let fingers = self.fingers().filter(|_| !self.done)?;
        let sum = self.touches.values().fold(Vec2::ZERO, |acc, (start, at)| acc + (*at - *start));
        let d = sum / self.touches.len() as f32;
        if d.length() < SWIPE_DIST {
            return None;
        }
        self.done = true;
        let dir = if d.x.abs() > d.y.abs() {
            if d.x > 0.0 { Dir::Right } else { Dir::Left }
        } else if d.y > 0.0 {
            Dir::Down
        } else {
            Dir::Up
        };
        Some(Gesture::Swipe { fingers, dir })
    }

    /// All fingers lifted, quickly and without moving.
    fn tap(&mut self, now: f64) -> Option<Gesture> {
        let fingers = self.fingers().filter(|_| !self.done)?;
        self.done = true;
        (self.moved < TAP_SLOP && now - self.start <= TAP_TIME).then_some(Gesture::Tap { fingers })
    }
}

/// Runs `gesture` as the system would (Windows; nothing elsewhere).
pub fn run(gesture: Gesture) {
    #[cfg(windows)]
    keys::send(shortcut(gesture));
    #[cfg(not(windows))]
    let _ = gesture;
}

/// The keys of the shortcut `gesture` stands for, as virtual-key codes
/// pressed in order (and released in reverse).
#[cfg_attr(not(windows), allow(dead_code))]
fn shortcut(gesture: Gesture) -> &'static [u16] {
    const WIN: u16 = 0x5B;
    const CTRL: u16 = 0x11;
    const ALT: u16 = 0x12;
    const TAB: u16 = 0x09;
    const LEFT: u16 = 0x25;
    const RIGHT: u16 = 0x27;
    const D: u16 = 0x44;
    const N: u16 = 0x4E;
    const S: u16 = 0x53;
    match gesture {
        Gesture::Swipe { dir: Dir::Up, .. } => &[WIN, TAB],
        Gesture::Swipe { dir: Dir::Down, .. } => &[WIN, D],
        Gesture::Swipe { fingers: 3, .. } => &[ALT, TAB],
        // The desktops follow the fingers: swiping left brings the next one.
        Gesture::Swipe { dir: Dir::Left, .. } => &[WIN, CTRL, RIGHT],
        Gesture::Swipe { dir: Dir::Right, .. } => &[WIN, CTRL, LEFT],
        Gesture::Tap { fingers: 3 } => &[WIN, S],
        Gesture::Tap { .. } => &[WIN, N],
    }
}

#[cfg(windows)]
mod keys {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput,
    };

    /// Presses `keys` in order and releases them in reverse, in one batch.
    pub fn send(keys: &[u16]) {
        let key = |vk: u16, up: bool| {
            // The Windows key and the arrows are extended keys.
            let extended = matches!(vk, 0x5B | 0x25 | 0x27);
            let flags = if up { KEYEVENTF_KEYUP } else { 0 } | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
            }
        };
        let inputs: Vec<INPUT> =
            keys.iter().map(|&k| key(k, false)).chain(keys.iter().rev().map(|&k| key(k, true))).collect();
        // SAFETY: a slice of initialised keyboard INPUTs and its length.
        unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{TouchDeviceId, TouchId, pos2, vec2};

    fn touch(id: u64, phase: TouchPhase, pos: Pos2) -> Event {
        Event::Touch { device_id: TouchDeviceId(0), id: TouchId(id), phase, pos, force: None }
    }

    /// `n` fingers land side by side, move by `by` in `steps`, lift after
    /// `time` seconds; every gesture recognised on the way.
    fn gesture(n: u64, by: Vec2, steps: usize, time: f64) -> Vec<Gesture> {
        let mut g = FingerGestures::default();
        let at = |i: u64| pos2(100.0 + 30.0 * i as f32, 300.0);
        let mut found = Vec::new();
        found.extend(g.update(&(0..n).map(|i| touch(i, TouchPhase::Start, at(i))).collect::<Vec<_>>(), 0.0));
        assert_eq!(g.active(), n >= 3);
        for s in 1..=steps {
            let d = by * s as f32 / steps as f32;
            let moves: Vec<_> = (0..n).map(|i| touch(i, TouchPhase::Move, at(i) + d)).collect();
            found.extend(g.update(&moves, time * s as f64 / (steps + 1) as f64));
        }
        found.extend(g.update(&(0..n).map(|i| touch(i, TouchPhase::End, at(i) + by)).collect::<Vec<_>>(), time));
        assert!(!g.active());
        found
    }

    #[test]
    fn swipes_by_fingers_and_direction() {
        assert_eq!(gesture(3, vec2(0.0, -200.0), 10, 0.3), [Gesture::Swipe { fingers: 3, dir: Dir::Up }]);
        assert_eq!(gesture(4, vec2(-200.0, 20.0), 10, 0.3), [Gesture::Swipe { fingers: 4, dir: Dir::Left }]);
        // Once per touch, however far.
        assert_eq!(gesture(3, vec2(0.0, 600.0), 30, 0.5).len(), 1);
    }

    #[test]
    fn taps_are_quick_and_still() {
        assert_eq!(gesture(3, Vec2::ZERO, 0, 0.1), [Gesture::Tap { fingers: 3 }]);
        assert_eq!(gesture(4, vec2(5.0, 5.0), 2, 0.2), [Gesture::Tap { fingers: 4 }]);
        assert!(gesture(3, Vec2::ZERO, 0, 1.0).is_empty(), "held: no tap");
        assert!(gesture(3, vec2(40.0, 0.0), 4, 0.2).is_empty(), "moved, not far enough to swipe");
    }

    #[test]
    fn fewer_fingers_are_left_alone() {
        assert!(gesture(1, vec2(0.0, -200.0), 10, 0.3).is_empty());
        assert!(gesture(2, vec2(0.0, -200.0), 10, 0.3).is_empty());
        assert!(gesture(2, Vec2::ZERO, 0, 0.1).is_empty());
    }

    #[test]
    fn shortcuts() {
        assert_eq!(shortcut(Gesture::Swipe { fingers: 4, dir: Dir::Up }), [0x5B, 0x09]);
        assert_eq!(shortcut(Gesture::Swipe { fingers: 3, dir: Dir::Right }), [0x12, 0x09]);
        assert_eq!(shortcut(Gesture::Swipe { fingers: 4, dir: Dir::Left }), [0x5B, 0x11, 0x27]);
    }
}
