// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! The design system: every colour, radius, spacing step and text size of the
//! UI, plus the few styled building blocks shared by several screens.
//!
//! [`install`] applies the tokens to egui's own style, so built-in widgets
//! (buttons, menus, windows, text fields) and custom painting come from the
//! same source. Nothing else in the UI should hard-code a colour or a size.
//!
//! Surfaces, from back to front:
//!
//! * [`color::DESK`]    the area around the pages and the library background
//! * [`color::CHROME`]  title bar, side panels, notches, toolbars, headers
//! * [`color::SURFACE`] windows, menus, popups, toasts
//!
//! One accent colour marks the selected / active / primary element.

use eframe::egui::{
    self, pos2, vec2, Button, Color32, FontData, FontDefinitions, FontFamily, FontId, Margin,
    Painter, Pos2, Rect, RichText, Rounding, Shadow, Stroke, TextStyle, Theme, ThemePreference, Ui,
    WidgetText,
};

pub mod color {
    use eframe::egui::Color32;

    /// Area around the pages, library background.
    pub const DESK: Color32 = Color32::from_rgb(43, 45, 49);
    /// Title bar, side panels, notches, floating toolbar, headers.
    pub const CHROME: Color32 = Color32::from_rgb(25, 26, 29);
    /// Windows, menus, popups, toasts.
    pub const SURFACE: Color32 = Color32::from_rgb(34, 36, 40);
    /// Button at rest.
    pub const RAISED: Color32 = Color32::from_rgb(47, 50, 55);
    pub const HOVER: Color32 = Color32::from_rgb(58, 61, 67);
    pub const PRESSED: Color32 = Color32::from_rgb(70, 74, 81);
    /// Text fields, slider rails.
    pub const INPUT: Color32 = Color32::from_rgb(18, 19, 21);
    /// Separators, outlines.
    pub const BORDER: Color32 = Color32::from_rgb(55, 58, 64);
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(82, 86, 93);

    pub const TEXT: Color32 = Color32::from_rgb(222, 225, 229);
    /// Emphasised text (strong labels, hovered widgets).
    pub const TEXT_STRONG: Color32 = Color32::from_rgb(246, 247, 249);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(156, 161, 168);
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(110, 115, 123);

    pub const ACCENT: Color32 = Color32::from_rgb(77, 139, 255);
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(106, 159, 255);
    /// Text and icons on an accent fill.
    pub const ON_ACCENT: Color32 = Color32::WHITE;
    pub const DANGER: Color32 = Color32::from_rgb(222, 70, 76);

    /// Paper and its rules.
    pub const PAGE: Color32 = Color32::from_rgb(252, 251, 247);
    pub const PAGE_RULE: Color32 = Color32::from_rgb(203, 216, 232);
    pub const PAGE_EDGE: Color32 = Color32::from_rgb(20, 21, 23);

    /// `ACCENT` with the given alpha (selection fills, outlines on paper).
    pub const fn accent_alpha(a: u8) -> Color32 {
        Color32::from_rgba_premultiplied(
            (77 * a as u32 / 255) as u8,
            (139 * a as u32 / 255) as u8,
            (255 * a as u32 / 255) as u8,
            a,
        )
    }
}

/// Corner radii.
pub mod radius {
    /// Buttons, inputs, chips, thumbnails.
    pub const SMALL: f32 = 6.0;
    /// Menus, windows, cards, toasts.
    pub const MEDIUM: f32 = 10.0;
    /// Floating toolbar, notches.
    pub const LARGE: f32 = 14.0;
}

/// Spacing steps (multiples of 4).
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 12.0;
    pub const L: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;
}

/// Text sizes.
pub mod size {
    pub const CAPTION: f32 = 11.5;
    pub const BODY: f32 = 13.5;
    pub const HEADING: f32 = 16.0;
    pub const TITLE: f32 = 21.0;
    /// Toolbar and window-button icons.
    pub const ICON: f32 = 18.0;
}

/// Semi-bold family, for headings and emphasis.
pub fn strong_family() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn font_strong(size: f32) -> FontId {
    FontId::new(size, strong_family())
}

/// Fonts (Inter + Phosphor icons) and the egui style; call once at startup.
/// The app is dark only: the theme does not follow the OS setting, so egui
/// widgets always match the custom-painted surfaces.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        FontData::from_static(include_bytes!("../assets/fonts/Inter-Regular.ttf")),
    );
    fonts.font_data.insert(
        "inter-semibold".into(),
        FontData::from_static(include_bytes!("../assets/fonts/Inter-SemiBold.ttf")),
    );
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // Inter first; then the icons (Unicode private use area, which the
    // bundled Inter leaves free, see assets/fonts/README.md) and egui's own
    // fonts (emoji, symbols) as fallbacks.
    let fallbacks: Vec<String> = fonts.families[&FontFamily::Proportional]
        .iter()
        .filter(|f| !matches!(f.as_str(), "inter" | "phosphor"))
        .cloned()
        .collect();
    let family = |main: &str| {
        [main.to_owned(), "phosphor".to_owned()].into_iter().chain(fallbacks.iter().cloned()).collect()
    };
    fonts.families.insert(FontFamily::Proportional, family("inter"));
    fonts.families.insert(strong_family(), family("inter-semibold"));
    ctx.set_fonts(fonts);

    ctx.set_theme(ThemePreference::Dark);
    ctx.style_mut_of(Theme::Dark, apply_style);
}

fn apply_style(style: &mut egui::Style) {
    use color::*;

    style.text_styles = [
        (TextStyle::Small, font(size::CAPTION)),
        (TextStyle::Body, font(size::BODY)),
        (TextStyle::Button, font(size::BODY)),
        (TextStyle::Heading, font_strong(size::HEADING)),
        (TextStyle::Monospace, FontId::monospace(12.5)),
    ]
    .into();

    let s = &mut style.spacing;
    s.item_spacing = vec2(space::S, 6.0);
    s.button_padding = vec2(10.0, 5.0);
    s.menu_margin = Margin::same(6.0);
    s.window_margin = Margin::same(space::L);
    s.interact_size = vec2(36.0, 28.0);
    s.slider_width = 120.0;
    s.icon_width = 16.0;
    s.icon_width_inner = 9.0;
    s.icon_spacing = 6.0;
    s.combo_width = 120.0;
    s.menu_width = 220.0;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = CHROME;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_rounding = Rounding::same(radius::MEDIUM);
    v.menu_rounding = Rounding::same(radius::MEDIUM);
    v.window_shadow = Shadow { offset: vec2(0.0, 10.0), blur: 28.0, spread: 0.0, color: Color32::from_black_alpha(120) };
    v.popup_shadow = Shadow { offset: vec2(0.0, 6.0), blur: 16.0, spread: 0.0, color: Color32::from_black_alpha(100) };
    v.window_highlight_topmost = false;
    v.extreme_bg_color = INPUT;
    v.faint_bg_color = Color32::from_white_alpha(6);
    v.code_bg_color = INPUT;
    v.hyperlink_color = ACCENT_HOVER;
    v.error_fg_color = DANGER;
    v.warn_fg_color = Color32::from_rgb(232, 170, 64);
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, ON_ACCENT);
    v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
    v.button_frame = true;
    // Sliders show their value as an accent fill, like the other selections.
    v.slider_trailing_fill = true;
    v.indent_has_left_vline = false;

    let rounding = Rounding::same(radius::SMALL);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = SURFACE;
    w.noninteractive.weak_bg_fill = SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.rounding = rounding;
    for (wv, fill, text) in [
        (&mut w.inactive, RAISED, TEXT),
        (&mut w.hovered, HOVER, TEXT_STRONG),
        (&mut w.active, PRESSED, TEXT_STRONG),
        (&mut w.open, HOVER, TEXT_STRONG),
    ] {
        wv.bg_fill = fill;
        wv.weak_bg_fill = fill;
        wv.bg_stroke = Stroke::NONE;
        wv.fg_stroke = Stroke::new(1.5, text);
        wv.rounding = rounding;
        wv.expansion = 0.0;
    }
}

// ------------------------------------------------------------ components

/// Glyph at icon size.
pub fn icon(glyph: &str) -> RichText {
    RichText::new(glyph).size(size::ICON)
}

/// Section caption: small, semi-bold, muted, upper case.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).font(font_strong(size::CAPTION)).color(color::TEXT_FAINT));
}

/// Screen or section heading.
pub fn heading(text: impl Into<String>) -> RichText {
    RichText::new(text).font(font_strong(size::HEADING)).color(color::TEXT_STRONG)
}

pub fn title(text: impl Into<String>) -> RichText {
    RichText::new(text).font(font_strong(size::TITLE)).color(color::TEXT_STRONG)
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).color(color::TEXT_MUTED)
}

/// The main action of a screen or dialog.
pub fn primary_button(text: impl Into<WidgetText>) -> Button<'static> {
    Button::new(text.into().color(color::ON_ACCENT)).fill(color::ACCENT)
}

/// An irreversible action.
pub fn danger_button(text: impl Into<WidgetText>) -> Button<'static> {
    Button::new(text.into().color(color::ON_ACCENT)).fill(color::DANGER)
}

/// Buttons without a fill at rest (toolbars, menus, headers). Only the
/// button fill changes; sliders and checkboxes keep theirs.
pub fn ghost_buttons(ui: &mut Ui) {
    ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
}

/// Shadow under cards (notes, notebooks) on the desk.
pub fn card_shadow() -> Shadow {
    Shadow { offset: vec2(0.0, 4.0), blur: 12.0, spread: 0.0, color: Color32::from_black_alpha(90) }
}

/// A borderless text field for renaming in place (note title, notebook
/// name): plain text at rest, a hover fill when it can be clicked, the input
/// look (dark fill, accent outline) while typing.
#[allow(clippy::too_many_arguments)]
pub fn inline_edit(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    text: &mut String,
    hint: &str,
    font: FontId,
    align: egui::Align,
    idle: Color32,
) -> egui::Response {
    let focused = ui.ctx().memory(|m| m.has_focus(id));
    let hovered = ui.rect_contains_pointer(rect);
    let background = ui.painter().add(egui::Shape::Noop);
    let resp = ui.put(
        rect,
        egui::TextEdit::singleline(text)
            .id(id)
            .font(font)
            .text_color(if focused || hovered { color::TEXT_STRONG } else { idle })
            .hint_text(hint)
            .horizontal_align(align)
            .vertical_align(egui::Align::Center)
            .margin(vec2(space::S, 2.0))
            .frame(false)
            .char_limit(120),
    );
    let rounding = Rounding::same(radius::SMALL);
    let fill = if resp.has_focus() {
        Some(color::INPUT)
    } else if resp.hovered() {
        Some(color::HOVER)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().set(background, egui::Shape::rect_filled(rect, rounding, fill));
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(rect, rounding, Stroke::new(1.0, color::ACCENT));
    }
    resp
}

/// Height of a [`list_row`].
const ROW_HEIGHT: f32 = 28.0;

/// A full-width list row (notes in the notebook panel): icon and label, a
/// hover fill, and for the selected row a soft accent fill with an accent bar.
/// Long labels end with "…".
pub fn list_row(ui: &mut Ui, glyph: &str, label: &str, selected: bool) -> egui::Response {
    // Draggable too (reordering); a click is still a click.
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), egui::Sense::click_and_drag());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = resp.hovered();
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, radius::SMALL, color::accent_alpha(46));
        let bar = Rect::from_min_size(rect.left_top() + vec2(0.0, 6.0), vec2(3.0, rect.height() - 12.0));
        p.rect_filled(bar, 1.5, color::ACCENT);
    } else if hovered {
        p.rect_filled(rect, radius::SMALL, color::HOVER);
    }
    let (icon_color, text_color) = match (selected, hovered) {
        (true, _) => (color::ACCENT_HOVER, color::TEXT_STRONG),
        (false, true) => (color::TEXT, color::TEXT_STRONG),
        (false, false) => (color::TEXT_MUTED, color::TEXT),
    };
    let y = rect.center().y;
    p.text(pos2(rect.left() + space::M, y), egui::Align2::LEFT_CENTER, glyph, font(size::BODY), icon_color);
    let text_left = rect.left() + space::M + 22.0;
    let mut job = egui::text::LayoutJob::simple_singleline(label.to_owned(), font(size::BODY), text_color);
    job.wrap = egui::text::TextWrapping::truncate_at_width((rect.right() - space::S - text_left).max(0.0));
    let galley = ui.fonts(|f| f.layout_job(job));
    ui.painter().galley(pos2(text_left, y - galley.size().y * 0.5), galley, text_color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A dashed "add" placeholder (add a page, a note): outline and label turn
/// accent on hover.
pub fn add_tile(ui: &mut Ui, rect: Rect, id: egui::Id, label: &str) -> egui::Response {
    let resp = ui.interact(rect, id, egui::Sense::click());
    let (line, text, fill) = if resp.hovered() {
        (color::ACCENT, color::ACCENT_HOVER, color::accent_alpha(18))
    } else {
        (color::BORDER_STRONG, color::TEXT_MUTED, Color32::TRANSPARENT)
    };
    let p = ui.painter();
    p.rect_filled(rect, radius::SMALL, fill);
    let r = rect.shrink(0.5);
    let outline = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    p.extend(egui::Shape::dashed_line(&outline, Stroke::new(1.0, line), 4.0, 3.0));
    let caption = format!("{}  {label}", egui_phosphor::regular::PLUS);
    p.text(rect.center(), egui::Align2::CENTER_CENTER, caption, font(size::BODY), text);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A short message pill (saved, exported, errors, drag labels) centred on `center`.
pub fn paint_toast(painter: &Painter, center: Pos2, text: &str) -> Rect {
    let galley = painter.layout_no_wrap(text.to_owned(), font(size::BODY), color::TEXT_STRONG);
    let rect = Rect::from_center_size(center, galley.size() + vec2(2.0 * space::L, 2.0 * space::S + 2.0));
    painter.add(Shadow { offset: vec2(0.0, 6.0), blur: 16.0, spread: 0.0, color: Color32::from_black_alpha(110) }
        .as_shape(rect, Rounding::same(radius::MEDIUM)));
    painter.rect(rect, radius::MEDIUM, color::SURFACE, Stroke::new(1.0, color::BORDER));
    painter.galley(rect.center() - galley.size() * 0.5, galley, color::TEXT_STRONG);
    rect
}
