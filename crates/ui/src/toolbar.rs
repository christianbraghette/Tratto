// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The editor's toolbars, all drawn as notches attached to the canvas edges.
//!
//! * the tools (tools, styles, undo / redo) centred on the top or bottom edge;
//!   for a PDF in the reader the reading bar (import, pages, zoom) in their place
//! * the joins of the sidebar's strip (notebook, pages, contents, search,
//!   note settings) with the title bar and the bottom edge
//!
//! Buttons show Phosphor icons with the name as a tooltip. Colours and sizes
//! come from [`crate::theme`].

use std::ops::RangeInclusive;

use eframe::egui::{self, Align2, Color32, Margin, Order, Pos2, Rect, Sense, Shape, Slider, Stroke, pos2, vec2};
use egui_phosphor::regular as icon;

use crate::scrollbar;
use crate::settings::{EraserMode, SelectMode, Settings, Sidebar, SidebarSide, ToolStyle, ToolbarPosition, ZoomMode};
use crate::theme::{self, color, radius, space};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Moves around the note like the PDF reader: drag to scroll, tap to
    /// follow a PDF link; writes nothing.
    Pointer,
    Pen,
    Highlighter,
    Eraser,
    /// Lasso select + move.
    Select,
    /// Click to type text.
    Text,
}

pub enum ToolbarEvent {
    Undo,
    Redo,
    ZoomIn,
    ZoomOut,
    /// Make the PDF in the reader a note to write on.
    Import,
    PrevPage,
    NextPage,
    /// Go to this page (0-based).
    GoToPage(u32),
    /// Back to where the view was before following a link.
    Back,
    /// A tool was clicked (already selected in `tool`).
    Tool(Tool),
    /// A colour for what is selected.
    Recolor([u8; 3]),
    /// Turn the pointer tool's laser on or off.
    Laser,
    /// Start presentation mode.
    Present,
    /// End presentation mode.
    EndPresentation,
}

impl Tool {
    pub const ALL: [Tool; 6] = [Tool::Pointer, Tool::Pen, Tool::Highlighter, Tool::Eraser, Tool::Select, Tool::Text];

    pub fn icon(self) -> &'static str {
        match self {
            Tool::Pointer => icon::CURSOR,
            Tool::Pen => icon::PEN_NIB,
            Tool::Highlighter => icon::HIGHLIGHTER,
            Tool::Eraser => icon::ERASER,
            Tool::Select => icon::SELECTION_PLUS,
            Tool::Text => icon::TEXT_T,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Pointer => "Pointer",
            Tool::Pen => "Pen",
            Tool::Highlighter => "Highlighter",
            Tool::Eraser => "Eraser",
            Tool::Select => "Select (lasso)",
            Tool::Text => "Text",
        }
    }
}

const PEN_PALETTE: [[u8; 3]; 6] =
    [[20, 20, 20], [30, 80, 220], [210, 40, 40], [30, 150, 70], [240, 140, 20], [130, 50, 170]];
const HIGHLIGHTER_PALETTE: [[u8; 3]; 5] =
    [[255, 235, 0], [120, 230, 90], [255, 140, 200], [100, 210, 255], [255, 170, 60]];

/// Shows the tool bar as a notch centred on the top edge (below the title
/// bar) or on the bottom edge of the canvas; returns a clicked action.
///
/// Call with the root `ui` after the title bar and the side panels, so it is
/// centred over the canvas (what is left of `ui`). When the window is too
/// narrow for it to fit at the chosen edge, it moves to the other edge.
/// `bottom_inset` keeps it clear of the horizontal scrollbar when that is visible.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &egui::Ui,
    tool: &mut Tool,
    settings: &mut Settings,
    can_undo: bool,
    can_redo: bool,
    nav: &Navigation,
    selected: Option<([u8; 3], bool)>,
    bottom_inset: f32,
) -> Option<ToolbarEvent> {
    let position = settings.toolbar_position;
    edge_bar(ui, "tool_notch", position, bottom_inset, 1.0, |ui| {
        contents(ui, tool, settings, can_undo, can_redo, nav, selected)
    })
}

/// Where the view is, for the page controls of the reader's bar and of the
/// pointer tool.
pub struct Navigation {
    /// The current page (0-based) of `count`.
    pub page: u32,
    pub count: u32,
    /// The document is a PDF, or a note written on one: it may have contents.
    pub contents: bool,
    /// A link was followed: Back can return.
    pub can_go_back: bool,
    /// The pointer tool's laser is on.
    pub laser: bool,
    /// Presentation mode is on (its bar has its own way out).
    pub presenting: bool,
}

/// The reader's bar, in the tool bar's place: import (to write on the PDF),
/// the laser, back after a link, the pages and the zoom (the table of
/// contents is in the sidebar).
pub fn show_reader(
    ui: &egui::Ui,
    settings: &mut Settings,
    nav: &Navigation,
    bottom_inset: f32,
) -> Option<ToolbarEvent> {
    let position = settings.toolbar_position;
    edge_bar(ui, "reader_notch", position, bottom_inset, 1.0, |ui| {
        let mut event = None;
        let import = egui::Button::new(format!("{}  Import", icon::FILE_ARROW_DOWN));
        let tip = "Make it a note to write on, in the notebook or section open in the library";
        if ui.add(import).on_hover_text(tip).clicked() {
            event = Some(ToolbarEvent::Import);
        }
        ui.separator();
        let pages = page_controls(ui, settings, nav, true);
        ui.separator();
        zoom_controls(ui, &mut settings.zoom).or(pages).or(event)
    })
}

/// Presentation mode's bar, on the tool bar's edge: the laser, Back after a
/// link, the pages, its own `zoom` and the way out. Hidden, it slides in like the title bar
/// in full screen: when the pointer touches that edge of the screen, or on a
/// swipe in from it, and stays while the pointer is on it.
pub fn show_presenting(
    ui: &egui::Ui,
    settings: &mut Settings,
    zoom: &mut ZoomMode,
    nav: &Navigation,
) -> Option<ToolbarEvent> {
    let position = settings.toolbar_position;
    let reveal = presenting_reveal(ui.ctx(), position);
    if reveal <= 0.0 {
        return None;
    }
    edge_bar(ui, PRESENTING_BAR, position, 0.0, reveal, |ui| {
        let pages = page_controls(ui, settings, nav, true);
        ui.separator();
        let zoomed = zoom_controls(ui, zoom);
        ui.separator();
        let end = egui::Button::new(theme::icon(icon::X));
        if ui.add(end).on_hover_text("End presentation (Escape)").clicked() {
            return Some(ToolbarEvent::EndPresentation);
        }
        zoomed.or(pages)
    })
}

/// Presentation mode's bar (its area's id).
const PRESENTING_BAR: &str = "presenting_notch";
/// How close to its edge the pointer reveals the presentation bar, and how
/// far past the bar it stays (points).
const REVEAL_EDGE: f32 = 2.0;
const KEEP_PAST: f32 = 12.0;
/// A swipe that reveals it starts this close to its edge (points) and goes
/// at least this far in.
pub const SWIPE_EDGE: f32 = 24.0;
const SWIPE_IN: f32 = 48.0;
/// Its slide in / out time, in seconds.
const REVEAL_TIME: f32 = 0.15;

/// Hides the presentation bar at once (presenting starts with it away).
pub fn hide_presenting_bar(ctx: &egui::Context) {
    let id = egui::Id::new("presenting_bar_reveal");
    ctx.data_mut(|d| {
        d.insert_temp(id, false);
        d.insert_temp(egui::Id::new("presenting_bar_swiped"), false);
    });
    ctx.animate_bool_with_time(id, false, 0.0);
}

/// How far `p` is from the screen edge of the presentation bar.
pub fn from_bar_edge(ctx: &egui::Context, position: ToolbarPosition, p: Pos2) -> f32 {
    let screen = ctx.viewport_rect();
    match position {
        ToolbarPosition::Top => p.y - screen.top(),
        ToolbarPosition::Bottom => screen.bottom() - p.y,
    }
}

/// How far the presentation bar is shown (0 hidden, 1 fully in); see
/// `show_presenting`. A finger leaves no pointer to stay near it: after a
/// swipe it stays until a press away from it. It also stays while a field
/// in it (the page number) is being typed into.
fn presenting_reveal(ctx: &egui::Context, position: ToolbarPosition) -> f32 {
    let id = egui::Id::new("presenting_bar_reveal");
    let swipe_id = egui::Id::new("presenting_bar_swiped");
    let shown = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let mut swiped = ctx.data(|d| d.get_temp::<bool>(swipe_id)).unwrap_or(false);
    let height = ctx.memory(|m| m.area_rect(egui::Id::new(PRESENTING_BAR))).map_or(48.0, |r| r.height());
    let edge = |p: Pos2| from_bar_edge(ctx, position, p);
    let (pointer, origin, down, pressed) = ctx.input(|i| {
        (i.pointer.hover_pos(), i.pointer.press_origin(), i.pointer.primary_down(), i.pointer.any_pressed())
    });
    let reach = if shown { height + KEEP_PAST } else { REVEAL_EDGE };
    let near = pointer.is_some_and(|p| edge(p) <= reach);
    if let (Some(o), Some(p), true) = (origin, pointer, down)
        && edge(o) <= SWIPE_EDGE
        && edge(p) - edge(o) >= SWIPE_IN
    {
        swiped = true;
    } else if pressed && origin.is_some_and(|o| edge(o) > height + KEEP_PAST) {
        swiped = false;
    }
    let typing = ctx.memory(|m| m.focused().is_some());
    let show = near || swiped || typing;
    ctx.data_mut(|d| {
        d.insert_temp(id, show);
        d.insert_temp(swipe_id, swiped);
    });
    ctx.animate_bool_with_time(id, show, REVEAL_TIME)
}

/// The laser (the pointer tool, `laser`) or else the table of contents (if
/// any), Back (after a link) and the pages: previous, the current one (to
/// type or drag), next.
fn page_controls(ui: &mut egui::Ui, settings: &mut Settings, nav: &Navigation, laser: bool) -> Option<ToolbarEvent> {
    let mut event = None;
    if laser && !nav.presenting {
        let present = egui::Button::new(theme::icon(icon::PRESENTATION));
        if ui.add(present).on_hover_text("Present: the page alone, full screen (F5)").clicked() {
            event = Some(ToolbarEvent::Present);
        }
    }
    if laser {
        let tip = if nav.laser { "Laser pointer: on (drag on the page to point)" } else { "Laser pointer, to present" };
        if ui.add(egui::Button::selectable(nav.laser, theme::icon(icon::RECORD))).on_hover_text(tip).clicked() {
            event = Some(ToolbarEvent::Laser);
        }
    } else if nav.contents && outline_button(ui, settings.sidebar == Sidebar::Outline) {
        settings.sidebar.toggle(Sidebar::Outline);
    }
    if nav.can_go_back && ui.button(theme::icon(icon::ARROW_BEND_UP_LEFT)).on_hover_text("Back (Alt+Left)").clicked() {
        event = Some(ToolbarEvent::Back);
    }
    if laser || nav.contents || nav.can_go_back {
        ui.separator();
    }
    let (page, count) = (nav.page, nav.count);
    let prev = egui::Button::new(theme::icon(icon::CARET_LEFT));
    if ui.add_enabled(page > 0, prev).on_hover_text("Previous page (Left, Page Up)").clicked() {
        event = Some(ToolbarEvent::PrevPage);
    }
    let mut shown = page + 1;
    let field = egui::DragValue::new(&mut shown).range(1..=count.max(1)).speed(0.05);
    if ui.add(field).on_hover_text("Page: click to type a number, or drag").changed() {
        event = Some(ToolbarEvent::GoToPage(shown - 1));
    }
    ui.label(theme::muted(format!("/ {count}")));
    let next = egui::Button::new(theme::icon(icon::CARET_RIGHT));
    if ui.add_enabled(page + 1 < count, next).on_hover_text("Next page (Right, Space, Page Down)").clicked() {
        event = Some(ToolbarEvent::NextPage);
    }
    event
}

/// A bar in a notch centred on the top or bottom edge of what is left of the
/// root `ui` (see `show`). `slide` is how far it is in from that edge (1:
/// all of it).
fn edge_bar<R>(
    ui: &egui::Ui,
    id: &str,
    position: ToolbarPosition,
    bottom_inset: f32,
    slide: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let ctx = ui.ctx();
    let free = ui.available_rect_before_wrap();
    let id = egui::Id::new(id);
    // Size from the previous frame (its height a guess the first time).
    let size = ctx.memory(|m| m.area_rect(id)).map_or(vec2(0.0, 48.0), |r| r.size());
    let (width, hidden) = (size.x, size.y * (1.0 - slide));
    let fits = width + 2.0 * CORNER_ZONE <= free.width();
    let top = match position {
        ToolbarPosition::Top => fits,
        ToolbarPosition::Bottom => !fits,
    };
    let (at, pos) = if top {
        (Edge::Top, pos2(free.center().x, free.top() - hidden))
    } else {
        (Edge::Bottom, pos2(free.center().x, free.bottom() - bottom_inset + hidden))
    };

    egui::Area::new(id)
        .order(Order::Foreground)
        // Sliding in, it starts past the edge of the screen.
        .constrain(false)
        .pivot(if top { Align2::CENTER_TOP } else { Align2::CENTER_BOTTOM })
        .fixed_pos(pos)
        .show(ctx, |ui| {
            let background = ui.painter().add(Shape::Noop);
            let mut margin = notch_margin(top);
            margin.left = space::M as i8;
            margin.right = space::M as i8;
            let shown = egui::Frame::NONE.inner_margin(margin).show(ui, |ui| {
                tool_buttons_style(ui);
                ui.horizontal(add_contents).inner
            });
            ui.painter().set(background, edge_notch_shape(shown.response.rect, at, color::chrome()));
            shown.inner
        })
        .inner
}

/// The edge a centred notch is attached to.
#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Top,
    Bottom,
}

/// A centred notch: both free corners rounded, concave fillets at both ends
/// of the attached edge. One polygon, fanned from the middle of the attached
/// edge, from which every point is visible.
fn edge_notch_shape(rect: Rect, at: Edge, fill: Color32) -> Shape {
    let (big, small) = (NOTCH_ROUNDING, NOTCH_FILLET);
    let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
    // Attached to the top, clockwise; edges along the border reach 1 px past it.
    let mut pts = vec![pos2(rect.center().x, t - 1.0), pos2(r + small, t - 1.0)];
    arc(&mut pts, pos2(r + small, t + small), small, -90.0, -180.0);
    arc(&mut pts, pos2(r - big, b - big), big, 0.0, 90.0);
    arc(&mut pts, pos2(l + big, b - big), big, 90.0, 180.0);
    arc(&mut pts, pos2(l - small, t + small), small, 0.0, -90.0);
    pts.push(pos2(l - small, t - 1.0));
    if at == Edge::Bottom {
        for p in &mut pts {
            p.y = t + b - p.y;
        }
    }
    Shape::convex_polygon(clockwise(pts), fill, Stroke::NONE)
}

/// The concave joins of the sidebar's strip with the title bar and the
/// bottom edge, so the strip reads as a notch grown along the window edge.
///
/// Call with the root `ui` after the sidebar: what is left of it is the
/// canvas area then. `bottom_inset` keeps the bottom join above the
/// horizontal scrollbar.
pub fn show_joins(ui: &egui::Ui, side: SidebarSide, bottom_inset: f32) {
    let ctx = ui.ctx();
    let free = ui.available_rect_before_wrap();

    // Above the canvas, but below menus and popups (which are foreground
    // areas).
    let joins = ctx.layer_painter(egui::LayerId::new(Order::Middle, egui::Id::new("panel_joins")));

    // On the right, against the vertical scrollbar, which always occupies the
    // right edge of the canvas (and, being the same surface, joins the
    // sidebar).
    let right = side == SidebarSide::Right;
    let (top, bottom) = if right {
        let x = free.right() - scrollbar::THICKNESS;
        (pos2(x, free.top()), pos2(x, free.bottom() - bottom_inset))
    } else {
        (free.left_top(), free.left_bottom())
    };
    let sx = if right { -1.0 } else { 1.0 };
    joins.add(fillet(top, sx, 1.0, color::chrome()));
    joins.add(fillet(bottom, sx, -1.0, color::chrome()));
}

/// Fullscreen, with the sidebar hidden: a notch in the top corner on the
/// sidebar's `side` that shows it again, with `last`, the view it last
/// showed. Returns true when clicked. Its button sits where the strip's
/// first button is.
pub fn show_sidebar_notch(ui: &egui::Ui, side: SidebarSide, last: Sidebar) -> bool {
    let free = ui.available_rect_before_wrap();
    let (at, corner) = match side {
        SidebarSide::Left => (Corner::TopLeft, free.left_top()),
        SidebarSide::Right => (Corner::TopRight, pos2(free.right() - scrollbar::THICKNESS, free.top())),
    };
    let mut clicked = false;
    notch(ui.ctx(), "fullscreen_sidebar_notch", at, corner, |ui| clicked = sidebar_button(ui, last));
    clicked
}

/// Shows the hidden sidebar again, with `last`, the view it last showed.
/// Returns true when clicked.
fn sidebar_button(ui: &mut egui::Ui, last: Sidebar) -> bool {
    let view = match last {
        Sidebar::Notebook => "the notebook",
        Sidebar::Outline => "the contents",
        Sidebar::Search => "the search",
        Sidebar::Settings => "the note settings",
        Sidebar::Pages | Sidebar::Closed => "the pages",
    };
    let tip = format!("Show the sidebar ({view})");
    ui.add(egui::Button::new(theme::icon(icon::SIDEBAR_SIMPLE))).on_hover_text(tip).clicked()
}

/// Width of the sidebar's button strip: one button and its margins.
pub const STRIP_WIDTH: f32 = 2.0 * space::S + NOTCH_BUTTON;
/// Size of a notch's buttons.
const NOTCH_BUTTON: f32 = 32.0;

/// Toggles the notebook view of the sidebar. Returns true when clicked;
/// `shown` highlights it.
pub fn notebook_button(ui: &mut egui::Ui, shown: bool) -> bool {
    let tip = if shown { "Hide the notebook" } else { "Notebook: its sections and notes" };
    ui.add(egui::Button::selectable(shown, theme::icon(icon::BOOKS))).on_hover_text(tip).clicked()
}

/// Toggles the pages view of the sidebar. Returns true when clicked;
/// `shown` highlights it.
pub fn pages_button(ui: &mut egui::Ui, shown: bool) -> bool {
    let tip = if shown { "Hide pages" } else { "Pages: jump, reorder, delete" };
    ui.add(egui::Button::selectable(shown, theme::icon(icon::FILES))).on_hover_text(tip).clicked()
}

/// Toggles the outline (table of contents) of a note written on a PDF.
/// Returns true when clicked; `shown` highlights it.
pub fn outline_button(ui: &mut egui::Ui, shown: bool) -> bool {
    let tip = if shown { "Hide the contents" } else { "Contents: the PDF's outline" };
    ui.add(egui::Button::selectable(shown, theme::icon(icon::LIST_BULLETS))).on_hover_text(tip).clicked()
}

/// Toggles the search view. Returns true when
/// clicked; `shown` highlights it.
pub fn search_button(ui: &mut egui::Ui, shown: bool) -> bool {
    let tip = if shown { "Hide the search" } else { "Search: the note, its section or notebook (Ctrl+F)" };
    ui.add(egui::Button::selectable(shown, theme::icon(icon::MAGNIFYING_GLASS))).on_hover_text(tip).clicked()
}

/// Returns true when clicked; `shown` highlights it.
pub fn settings_button(ui: &mut egui::Ui, shown: bool) -> bool {
    let tip = if shown { "Hide note settings" } else { "Note settings: paper, pages, export" };
    ui.add(egui::Button::selectable(shown, theme::icon(icon::GEAR_SIX))).on_hover_text(tip).clicked()
}

/// Margins around the buttons of a notch attached to the top edge (`top`) or
/// to the bottom edge. The sidebar uses the same ones, so its buttons sit
/// exactly where the notch buttons were.
pub fn notch_margin(top: bool) -> Margin {
    // A little more room on the attached side keeps the buttons clear of the
    // title bar (and its window buttons) and of the bottom edge.
    let (attached, free) = (space::S as i8, 6);
    Margin {
        left: space::S as i8,
        right: space::S as i8,
        top: if top { attached } else { free },
        bottom: if top { free } else { attached },
    }
}

/// Button style of the notches, for the same buttons inside the sidebar.
pub fn notch_buttons_style(ui: &mut egui::Ui) {
    tool_buttons_style(ui);
}

/// Radius of the notches' free corners.
const NOTCH_ROUNDING: f32 = radius::LARGE;
/// Width kept free at each side of the tool notch (the joins of the sidebar,
/// the scrollbar).
const CORNER_ZONE: f32 = 32.0;
/// Radius of the concave joins between the notch and the window edges.
const NOTCH_FILLET: f32 = 10.0;

/// The window corner a notch is attached to.
#[derive(Clone, Copy, PartialEq)]
enum Corner {
    TopLeft,
    TopRight,
}

/// A tab attached to the top edge, flush with the side edge, with its outer
/// corner at `corner`.
fn notch(ctx: &egui::Context, id: &str, at: Corner, corner: Pos2, add_contents: impl FnOnce(&mut egui::Ui)) {
    let pivot = match at {
        Corner::TopLeft => Align2::LEFT_TOP,
        Corner::TopRight => Align2::RIGHT_TOP,
    };
    egui::Area::new(egui::Id::new(id)).order(Order::Foreground).pivot(pivot).fixed_pos(corner).show(ctx, |ui| {
        // Reserve the background slot first so it is painted below the buttons.
        let background = ui.painter().add(Shape::Noop);
        let rect = egui::Frame::NONE
            .inner_margin(notch_margin(true))
            .show(ui, |ui| {
                tool_buttons_style(ui);
                ui.horizontal(add_contents);
            })
            .response
            .rect;
        ui.painter().set(background, notch_shape(rect, at, color::chrome()));
    });
}

/// The notch outline as one polygon: the body with its free corner rounded,
/// plus concave fillets that blend it into the two window edges.
///
/// The outline is not convex, but every point is visible from the window
/// corner, its first vertex; epaint fans the fill out from the first vertex,
/// so it is filled correctly, and a single shape has no anti-aliasing seams.
fn notch_shape(rect: Rect, at: Corner, fill: Color32) -> Shape {
    let (big, small) = (NOTCH_ROUNDING, NOTCH_FILLET);
    let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
    // Outline of the top-left notch, clockwise. The edge along the side of the
    // window reaches 1 px past it, so its anti-aliasing fringe is not visible.
    let mut pts = vec![pos2(l - 1.0, t), pos2(r + small, t)];
    arc(&mut pts, pos2(r + small, t + small), small, -90.0, -180.0);
    arc(&mut pts, pos2(r - big, b - big), big, 0.0, 90.0);
    arc(&mut pts, pos2(l + small, b + small), small, -90.0, -180.0);
    pts.push(pos2(l - 1.0, b + small));
    if at == Corner::TopRight {
        for p in &mut pts {
            p.x = l + r - p.x;
        }
    }
    Shape::convex_polygon(clockwise(pts), fill, Stroke::NONE)
}

/// A single concave join at `corner`, filling towards +x·`sx` and +y·`sy`
/// (like the fillets of a notch, without the notch).
fn fillet(corner: Pos2, sx: f32, sy: f32, fill: Color32) -> Shape {
    let r = NOTCH_FILLET;
    let mut pts = vec![pos2(-1.0, -1.0), pos2(r, -1.0)];
    arc(&mut pts, pos2(r, r), r, -90.0, -180.0);
    pts.push(pos2(-1.0, r));
    let pts = pts.into_iter().map(|p| corner + vec2(p.x * sx, p.y * sy)).collect();
    Shape::convex_polygon(clockwise(pts), fill, Stroke::NONE)
}

/// Makes the polygon clockwise on screen (as epaint prefers), keeping the
/// first vertex first (the fill fans out from it).
fn clockwise(mut pts: Vec<Pos2>) -> Vec<Pos2> {
    let n = pts.len();
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    if area < 0.0 {
        pts[1..].reverse();
    }
    pts
}

/// Appends a circular arc from angle `from` to `to` (degrees, y down).
fn arc(pts: &mut Vec<Pos2>, center: Pos2, radius: f32, from: f32, to: f32) {
    const SEGMENTS: usize = 8;
    for i in 0..=SEGMENTS {
        let a = (from + (to - from) * i as f32 / SEGMENTS as f32).to_radians();
        pts.push(center + radius * vec2(a.cos(), a.sin()));
    }
}

/// Square icon buttons without a fill at rest, shared by all toolbars.
fn tool_buttons_style(ui: &mut egui::Ui) {
    theme::ghost_buttons(ui);
    let s = ui.spacing_mut();
    s.item_spacing = vec2(space::XS, space::XS);
    s.button_padding = vec2(7.0, 5.0);
    s.interact_size = vec2(NOTCH_BUTTON, NOTCH_BUTTON);
}

fn contents(
    ui: &mut egui::Ui,
    tool: &mut Tool,
    settings: &mut Settings,
    can_undo: bool,
    can_redo: bool,
    nav: &Navigation,
    selected: Option<([u8; 3], bool)>,
) -> Option<ToolbarEvent> {
    let mut event = None;

    let mut picked = None;
    for t in Tool::ALL {
        let tip = match t {
            Tool::Pointer => "Pointer: scroll, turn pages, follow links",
            _ => t.label(),
        };
        if ui.selectable_value(tool, t, theme::icon(t.icon())).on_hover_text(tip).clicked() {
            picked = Some(ToolbarEvent::Tool(t));
        }
    }
    ui.separator();

    ui.spacing_mut().slider_width = 100.0;
    // The size bubble goes towards the middle of the window.
    let above = settings.toolbar_position == ToolbarPosition::Bottom;
    match *tool {
        Tool::Pointer => event = page_controls(ui, settings, nav, true),
        Tool::Pen => style_controls(ui, &mut settings.pen, &PEN_PALETTE, 0.5..=12.0, 0.5, above),
        Tool::Highlighter => {
            style_controls(ui, &mut settings.highlighter, &HIGHLIGHTER_PALETTE, 4.0..=40.0, 2.0, above)
        }
        Tool::Eraser => {
            let modes = [
                (EraserMode::Partial, icon::ERASER, "Partial: rubs out only the ink under it"),
                (EraserMode::Stroke, icon::SCRIBBLE, "Stroke: erases every stroke it touches, whole"),
            ];
            for (mode, glyph, tip) in modes {
                ui.selectable_value(&mut settings.eraser_mode, mode, theme::icon(glyph)).on_hover_text(tip);
            }
            size_slider(ui, &mut settings.eraser_radius, 4.0..=60.0, 2.0, above);
        }
        Tool::Select => {
            let modes = [
                (SelectMode::Lasso, icon::LASSO, "Lasso: draw around what to select, then drag it"),
                (
                    SelectMode::Rectangle,
                    icon::SELECTION_ALL,
                    "Rectangle: drag a box around what to select, then drag it",
                ),
            ];
            for (mode, glyph, tip) in modes {
                ui.selectable_value(&mut settings.select_mode, mode, theme::icon(glyph)).on_hover_text(tip);
            }
            // Something selected: its colour (the first stroke's or text box's).
            if let Some((current, highlighter)) = selected {
                ui.separator();
                let palette: &[[u8; 3]] = if highlighter { &HIGHLIGHTER_PALETTE } else { &PEN_PALETTE };
                for c in palette {
                    if swatch(ui, *c, current == *c).on_hover_text("Colour of the selection").clicked() {
                        event = Some(ToolbarEvent::Recolor(*c));
                    }
                }
            }
        }
        Tool::Text => style_controls(ui, &mut settings.text, &PEN_PALETTE, 8.0..=96.0, 2.0, above),
    }
    ui.separator();

    let undo = egui::Button::new(theme::icon(icon::ARROW_U_UP_LEFT));
    if ui.add_enabled(can_undo, undo).on_hover_text("Undo (Ctrl+Z)").clicked() {
        event = Some(ToolbarEvent::Undo);
    }
    let redo = egui::Button::new(theme::icon(icon::ARROW_U_UP_RIGHT));
    if ui.add_enabled(can_redo, redo).on_hover_text("Redo (Ctrl+Y)").clicked() {
        event = Some(ToolbarEvent::Redo);
    }
    ui.separator();
    zoom_controls(ui, &mut settings.zoom).or(event).or(picked)
}

/// Zoom: out, the presets, in.
fn zoom_controls(ui: &mut egui::Ui, zoom: &mut ZoomMode) -> Option<ToolbarEvent> {
    let mut event = None;
    if ui.button(theme::icon(icon::MAGNIFYING_GLASS_MINUS)).on_hover_text("Zoom out (Ctrl+Minus)").clicked() {
        event = Some(ToolbarEvent::ZoomOut);
    }
    egui::ComboBox::from_id_salt("toolbar_zoom")
        .selected_text(zoom.label())
        .width(84.0)
        .show_ui(ui, |ui| {
            for z in ZoomMode::PRESETS {
                ui.selectable_value(zoom, z, z.label());
            }
        })
        .response
        .on_hover_text("Zoom (Ctrl + mouse wheel or pinch zooms around the pointer)");
    if ui.button(theme::icon(icon::MAGNIFYING_GLASS_PLUS)).on_hover_text("Zoom in (Ctrl+Plus)").clicked() {
        event = Some(ToolbarEvent::ZoomIn);
    }
    event
}

fn style_controls(
    ui: &mut egui::Ui,
    style: &mut ToolStyle,
    palette: &[[u8; 3]],
    range: RangeInclusive<f32>,
    step: f32,
    above: bool,
) {
    for c in palette {
        if swatch(ui, *c, style.color == *c).clicked() {
            style.color = *c;
        }
    }
    egui::color_picker::color_edit_button_srgb(ui, &mut style.color);
    size_slider(ui, &mut style.width, range, step, above);
}

/// A tool size slider that snaps to `step`. While hovered or dragged, a
/// bubble over the handle (`above` it, or below) shows the size.
fn size_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    step: f32,
    above: bool,
) -> egui::Response {
    let (min, max) = (*range.start(), *range.end());
    let resp = ui.add(Slider::new(value, range).step_by(step.into()).show_value(false));
    if resp.hovered() || resp.dragged() {
        // Where egui puts the handle: the rail inset by the handle's radius.
        let r = resp.rect;
        let inset = r.height() / 2.5;
        let t = ((*value - min) / (max - min)).clamp(0.0, 1.0);
        let x = egui::lerp(r.left() + inset..=r.right() - inset, t);
        let (anchor, pivot) = if above {
            (pos2(x, r.top() - space::XS), Align2::CENTER_BOTTOM)
        } else {
            (pos2(x, r.bottom() + space::XS), Align2::CENTER_TOP)
        };
        egui::Area::new(resp.id.with("size_bubble"))
            .order(Order::Tooltip)
            .fixed_pos(anchor)
            .pivot(pivot)
            .interactable(false)
            .show(ui.ctx(), |ui| {
                // Never wrap: the area has no width of its own to wrap to.
                egui::Frame::popup(ui.style()).show(ui, |ui| ui.add(egui::Label::new(size_label(*value)).extend()));
            });
    }
    resp
}

/// "2.5 px", "12 px".
fn size_label(v: f32) -> String {
    if v.fract().abs() < 0.05 { format!("{v:.0} px") } else { format!("{v:.1} px") }
}

fn swatch(ui: &mut egui::Ui, rgb: [u8; 3], selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 32.0), Sense::click());
    let painter = ui.painter();
    let c = rect.center();
    // The thin ring keeps dark colours visible on the dark toolbar.
    painter.circle(c, 8.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]), Stroke::new(1.0, color::border_strong()));
    if selected {
        painter.circle_stroke(c, 11.0, Stroke::new(2.0, color::accent()));
    } else if resp.hovered() {
        painter.circle_stroke(c, 11.0, Stroke::new(1.5, color::text_faint()));
    }
    resp
}
