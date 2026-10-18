//! Large, pen-friendly scrollbars.
//!
//! The track is 16 points wide and the thumb never gets shorter than 56 points,
//! so it is easy to hit with a stylus. Dragging the thumb scrolls with a fixed
//! grab offset; tapping or pressing on the empty track makes the thumb jump
//! there and follow the pen. Interaction (`update`) and painting (`paint`) are
//! separate so the scroll offset is already final when the page is drawn.

use eframe::egui::{pos2, vec2, Color32, FontId, Id, Painter, Rect, Sense, Ui};

pub const THICKNESS: f32 = 16.0;
const MIN_THUMB: f32 = 56.0;
const INSET: f32 = 3.0;

pub struct BarState {
    pub thumb: Rect,
    pub active: bool,
    pub hovered: bool,
    pub enabled: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn update(
    ui: &mut Ui,
    id: Id,
    track: Rect,
    vertical: bool,
    scroll: &mut f32,
    max_scroll: f32,
    view_len: f32,
    content_len: f32,
    grab: &mut Option<f32>,
) -> BarState {
    let (track_start, track_len) = if vertical {
        (track.top(), track.height())
    } else {
        (track.left(), track.width())
    };
    let usable = (track_len - 2.0 * INSET).max(1.0);
    let thumb_len = (usable * view_len / content_len.max(1.0)).clamp(MIN_THUMB.min(usable), usable);
    let travel = (usable - thumb_len).max(0.0);
    let enabled = max_scroll > 0.0 && travel > 0.0;

    let resp = ui.interact(track, id, Sense::drag());
    let thumb_start = |scroll: f32| {
        let frac = if max_scroll > 0.0 { (scroll / max_scroll).clamp(0.0, 1.0) } else { 0.0 };
        track_start + INSET + frac * travel
    };

    let mut active = false;
    if enabled && resp.dragged() {
        if let Some(ptr) = resp.interact_pointer_pos() {
            let along = if vertical { ptr.y } else { ptr.x };
            if resp.drag_started() || grab.is_none() {
                let ts = thumb_start(*scroll);
                *grab = Some(if along >= ts && along <= ts + thumb_len {
                    along - ts // grabbed the thumb: keep the offset
                } else {
                    thumb_len * 0.5 // pressed the track: centre the thumb on the pen
                });
            }
            let g = grab.unwrap_or(thumb_len * 0.5);
            let start = (along - g - track_start - INSET).clamp(0.0, travel);
            *scroll = start / travel * max_scroll;
            active = true;
        }
    }
    if !resp.dragged() {
        *grab = None;
    }

    let ts = thumb_start(*scroll);
    let thumb = if vertical {
        Rect::from_min_size(pos2(track.left() + INSET, ts), vec2(track.width() - 2.0 * INSET, thumb_len))
    } else {
        Rect::from_min_size(pos2(ts, track.top() + INSET), vec2(thumb_len, track.height() - 2.0 * INSET))
    };
    BarState { thumb, active, hovered: resp.hovered(), enabled }
}

pub fn paint(painter: &Painter, track: Rect, vertical: bool, s: &BarState, label: Option<&str>) {
    painter.rect_filled(track, 0.0, Color32::from_gray(38));
    let color = if !s.enabled {
        Color32::from_gray(70)
    } else if s.active || s.hovered {
        Color32::from_gray(200)
    } else {
        Color32::from_gray(150)
    };
    painter.rect_filled(s.thumb, 6.0, color);

    // Floating label (e.g. "Page 3 / 12") next to the thumb while dragging.
    if let (true, true, Some(text)) = (s.active, vertical, label) {
        let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(15.0), Color32::WHITE);
        let size = galley.size();
        let pill = Rect::from_min_size(
            pos2(track.left() - 10.0 - size.x - 16.0, s.thumb.center().y - size.y * 0.5 - 6.0),
            size + vec2(16.0, 12.0),
        );
        painter.rect_filled(pill, 8.0, Color32::from_black_alpha(215));
        painter.galley(pill.min + vec2(8.0, 6.0), galley, Color32::WHITE);
    }
}
