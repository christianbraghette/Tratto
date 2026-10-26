// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The zoom the page is made at while the view zooms.
//!
//! Making the page again at a new zoom costs: the strokes are tessellated
//! again, the PDF pages rasterised again, the pictures decoded again, the
//! text laid out (and its glyphs rasterised) in a new size. While the view
//! zooms, what was made at the zoom before is drawn scaled instead, and it is
//! made again only when the zoom ends, as Firefox's PDF viewer does: when the
//! fingers pinching lift, or when a wheel or a touchpad has not zoomed for
//! `END_SECS`.

use std::time::Duration;

use eframe::egui;

/// A zoom (wheel, touchpad, menu) has ended once it has not changed for
/// this long (seconds); a pinch on a touch screen ends when it lifts.
const END_SECS: f64 = 0.2;

/// Decides the zoom the page is made at, once per frame (see the module
/// docs). One for every view that zooms.
#[derive(Default)]
pub struct ZoomRender {
    /// The zoom the page is made at (points per document unit).
    made: Option<f32>,
    /// The zoom of the last frame, and when it last changed (egui's time).
    last: f32,
    changed_at: f64,
    /// Fingers were pinching in the last frame.
    pinched: bool,
}

impl ZoomRender {
    /// The zoom to make the page at this frame. `scale` is the zoom shown,
    /// `pinching` whether fingers are pinching (the zoom goes on however
    /// long they hold still).
    pub fn update(&mut self, ctx: &egui::Context, scale: f32, pinching: bool) -> f32 {
        let now = ctx.input(|i| i.time);
        if scale != self.last {
            self.last = scale;
            self.changed_at = now;
        }
        let lifted = std::mem::replace(&mut self.pinched, pinching) && !pinching;
        let made = self.made.get_or_insert(scale);
        if *made == scale {
            return scale;
        }
        let still = now - self.changed_at;
        if lifted || !pinching && still >= END_SECS {
            *made = scale;
        } else if !pinching {
            // A frame when it ends, to make the page again.
            ctx.request_repaint_after(Duration::from_secs_f64(END_SECS - still));
        }
        *made
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::RawInput;

    /// The zoom made at, at time `t` with zoom `scale`.
    fn made(ctx: &egui::Context, z: &mut ZoomRender, t: f64, scale: f32, pinching: bool) -> f32 {
        let mut out = 0.0;
        let at = RawInput { time: Some(t), ..RawInput::default() };
        crate::test_frame(ctx, at, |ui| out = z.update(ui.ctx(), scale, pinching));
        out
    }

    #[test]
    fn made_again_once_the_zoom_ends() {
        let ctx = egui::Context::default();
        let mut z = ZoomRender::default();
        assert_eq!(made(&ctx, &mut z, 0.0, 1.0, false), 1.0);
        // A wheel zooming: drawn scaled, however far it goes.
        assert_eq!(made(&ctx, &mut z, 0.05, 1.2, false), 1.0);
        assert_eq!(made(&ctx, &mut z, 0.10, 4.0, false), 1.0);
        // A pause shorter than the end: still zooming.
        assert_eq!(made(&ctx, &mut z, 0.10 + END_SECS * 0.5, 4.0, false), 1.0);
        // Ended: made at the zoom shown.
        assert_eq!(made(&ctx, &mut z, 0.10 + END_SECS, 4.0, false), 4.0);
    }

    #[test]
    fn a_pinch_held_still_goes_on_until_it_lifts() {
        let ctx = egui::Context::default();
        let mut z = ZoomRender::default();
        made(&ctx, &mut z, 0.0, 1.0, false);
        assert_eq!(made(&ctx, &mut z, 0.05, 3.0, true), 1.0);
        // Held still for long: the fingers are still down.
        assert_eq!(made(&ctx, &mut z, 5.0, 3.0, true), 1.0);
        // Lifted.
        assert_eq!(made(&ctx, &mut z, 5.01, 3.0, false), 3.0);
        // Lifted right after zooming: made again at once all the same.
        assert_eq!(made(&ctx, &mut z, 5.02, 1.1, true), 3.0);
        assert_eq!(made(&ctx, &mut z, 5.03, 1.1, false), 1.1);
    }
}
