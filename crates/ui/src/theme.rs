// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The design system: every colour, radius, spacing step and text size of the
//! UI, plus the few styled building blocks shared by several screens.
//!
//! [`install`] applies the tokens to egui's own style, so built-in widgets
//! (buttons, menus, windows, text fields) and custom painting come from the
//! same source. Nothing else in the UI should hard-code a colour or a size.
//!
//! Surfaces, from back to front:
//!
//! * [`color::desk()`]    the area around the pages and the library background
//! * [`color::chrome()`]  title bar, side panels, notches, toolbars, headers
//! * [`color::surface()`] windows, menus, popups, toasts
//!
//! One accent colour marks the selected / active / primary element.

use std::sync::Arc;

use eframe::egui::{
    self, Button, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Margin, Painter,
    Pos2, Rect, RichText, Shadow, Stroke, StrokeKind, TextStyle, Theme, ThemePreference, Ui, WidgetText, pos2, vec2,
};

/// The colours of the interface, of the dark or the light theme: the one
/// [`follow`] picked for this frame.
pub mod color {
    use std::sync::atomic::{AtomicBool, Ordering};

    use eframe::egui::Color32;

    /// Every colour of one theme.
    pub struct Palette {
        /// Area around the pages, library background.
        pub desk: Color32,
        /// Title bar, side panels, notches, floating toolbar, headers.
        pub chrome: Color32,
        /// Windows, menus, popups, toasts.
        pub surface: Color32,
        /// Button at rest.
        pub raised: Color32,
        pub hover: Color32,
        pub pressed: Color32,
        /// Text fields, slider rails.
        pub input: Color32,
        /// Separators, outlines.
        pub border: Color32,
        pub border_strong: Color32,
        pub text: Color32,
        /// Emphasised text (strong labels, hovered widgets).
        pub text_strong: Color32,
        pub text_muted: Color32,
        pub text_faint: Color32,
        pub accent: Color32,
        pub accent_hover: Color32,
        /// Text and icons on an accent fill.
        pub on_accent: Color32,
        pub danger: Color32,
        /// Paper and its rules (the same paper in both themes).
        pub page: Color32,
        pub page_rule: Color32,
        /// The dots of dotted paper (darker than rules: they are smaller).
        pub page_dot: Color32,
        /// The page's outline on the desk.
        pub page_edge: Color32,
    }

    pub const DARK: Palette = Palette {
        desk: Color32::from_rgb(43, 45, 49),
        chrome: Color32::from_rgb(25, 26, 29),
        surface: Color32::from_rgb(34, 36, 40),
        raised: Color32::from_rgb(47, 50, 55),
        hover: Color32::from_rgb(58, 61, 67),
        pressed: Color32::from_rgb(70, 74, 81),
        input: Color32::from_rgb(18, 19, 21),
        border: Color32::from_rgb(55, 58, 64),
        border_strong: Color32::from_rgb(82, 86, 93),
        text: Color32::from_rgb(222, 225, 229),
        text_strong: Color32::from_rgb(246, 247, 249),
        text_muted: Color32::from_rgb(156, 161, 168),
        text_faint: Color32::from_rgb(110, 115, 123),
        accent: Color32::from_rgb(77, 139, 255),
        accent_hover: Color32::from_rgb(106, 159, 255),
        on_accent: Color32::WHITE,
        danger: Color32::from_rgb(222, 70, 76),
        page: Color32::WHITE,
        page_rule: Color32::from_rgb(203, 216, 232),
        page_dot: Color32::from_rgb(160, 176, 198),
        page_edge: Color32::from_rgb(20, 21, 23),
    };

    pub const LIGHT: Palette = Palette {
        desk: Color32::from_rgb(222, 225, 230),
        chrome: Color32::from_rgb(245, 246, 248),
        surface: Color32::from_rgb(255, 255, 255),
        raised: Color32::from_rgb(232, 234, 238),
        hover: Color32::from_rgb(222, 225, 230),
        pressed: Color32::from_rgb(207, 211, 218),
        input: Color32::from_rgb(240, 242, 245),
        border: Color32::from_rgb(214, 217, 223),
        border_strong: Color32::from_rgb(178, 184, 192),
        text: Color32::from_rgb(38, 41, 46),
        text_strong: Color32::from_rgb(14, 16, 19),
        text_muted: Color32::from_rgb(92, 98, 107),
        text_faint: Color32::from_rgb(130, 136, 145),
        accent: Color32::from_rgb(37, 99, 235),
        accent_hover: Color32::from_rgb(29, 78, 216),
        on_accent: Color32::WHITE,
        danger: Color32::from_rgb(205, 43, 51),
        page: Color32::WHITE,
        page_rule: Color32::from_rgb(203, 216, 232),
        page_dot: Color32::from_rgb(160, 176, 198),
        page_edge: Color32::from_rgb(170, 176, 185),
    };

    static LIGHT_ON: AtomicBool = AtomicBool::new(false);

    /// Use the light palette (else the dark one) from now on.
    pub(super) fn set_light(light: bool) {
        LIGHT_ON.store(light, Ordering::Relaxed);
    }

    /// The palette in use.
    pub fn palette() -> &'static Palette {
        if LIGHT_ON.load(Ordering::Relaxed) { &LIGHT } else { &DARK }
    }

    macro_rules! colors {
        ($($name:ident),* $(,)?) => {$(
            pub fn $name() -> Color32 {
                palette().$name
            }
        )*};
    }

    colors!(
        desk,
        chrome,
        surface,
        raised,
        hover,
        pressed,
        input,
        border,
        border_strong,
        text,
        text_strong,
        text_muted,
        text_faint,
        accent,
        accent_hover,
        on_accent,
        danger,
        page,
        page_rule,
        page_dot,
        page_edge,
    );

    /// `accent` with the given alpha (selection fills, outlines on paper).
    pub fn accent_alpha(a: u8) -> Color32 {
        let [r, g, b, _] = accent().to_array();
        let k = |c: u8| (c as u32 * a as u32 / 255) as u8;
        Color32::from_rgba_premultiplied(k(r), k(g), k(b), a)
    }

    // The colours below are drawn on the paper or over the whole window, so
    // they are the same in both themes.

    /// Dims the window behind a modal state (loading, a file dragged in).
    pub fn scrim() -> Color32 {
        Color32::from_black_alpha(140)
    }

    /// The soft drop shadow under a page on the desk.
    pub fn page_shadow() -> Color32 {
        Color32::from_black_alpha(60)
    }

    /// Fill of the resize / rotate handles on the paper (outlined in `accent`).
    pub fn handle() -> Color32 {
        Color32::WHITE
    }

    /// Behind the text box being edited: the paper, a little see-through.
    pub fn edit_backdrop() -> Color32 {
        Color32::from_white_alpha(215)
    }

    /// Search matches, like a yellow highlighter.
    pub fn search_match() -> Color32 {
        Color32::from_rgba_unmultiplied(255, 210, 0, 100)
    }

    /// The current search match, in orange.
    pub fn search_current() -> Color32 {
        Color32::from_rgba_unmultiplied(255, 140, 0, 130)
    }

    /// A picture's place while it decodes.
    pub fn picture_pending() -> Color32 {
        Color32::from_gray(242)
    }

    /// The place of a picture that cannot be read.
    pub fn picture_unreadable() -> Color32 {
        Color32::from_gray(225)
    }

    /// The faint ring around the pen cursor, which keeps light inks visible.
    pub fn cursor_ring() -> Color32 {
        Color32::from_black_alpha(70)
    }

    /// The eraser cursor's fill and outline.
    pub fn eraser_fill() -> Color32 {
        Color32::from_rgba_unmultiplied(120, 120, 120, 40)
    }

    pub fn eraser_ring() -> Color32 {
        Color32::from_gray(80)
    }
}

/// Which theme the interface takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ThemeMode {
    /// The computer's light or dark mode.
    System,
    Light,
    #[default]
    Dark,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [Self::Dark, Self::Light, Self::System];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Same as the system",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

/// Takes the theme `mode` asks for (egui's widgets, and the palette of the
/// custom-painted surfaces). Call at the start of every frame: the system's
/// mode may change while the app runs.
pub fn follow(ctx: &egui::Context, mode: ThemeMode) {
    let preference = match mode {
        ThemeMode::System => ThemePreference::System,
        ThemeMode::Light => ThemePreference::Light,
        ThemeMode::Dark => ThemePreference::Dark,
    };
    if ctx.options(|o| o.theme_preference) != preference {
        ctx.set_theme(preference);
    }
    color::set_light(ctx.theme() == Theme::Light);
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

/// Inter's own italics, for text boxes' Markdown (egui's slanted upright
/// glyphs blur): medium for regular text, bold for semibold.
pub fn italic_family(bold: bool) -> FontFamily {
    FontFamily::Name(if bold { "semibold-italic" } else { "italic" }.into())
}

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn font_strong(size: f32) -> FontId {
    FontId::new(size, strong_family())
}

/// Fonts (Inter + Phosphor icons) and the egui style of both themes; call
/// once at startup, then [`follow`] every frame.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    // Inter's glyphs on whole pixels: sharper than egui's default of four
    // fractional offsets, for slightly less even spacing.
    let inter = |bytes: &'static [u8]| {
        Arc::new(FontData::from_static(bytes).tweak(FontTweak { subpixel_binning: Some(false), ..Default::default() }))
    };
    fonts.font_data.insert("inter".into(), inter(include_bytes!("../assets/fonts/Inter-Regular.ttf")));
    fonts.font_data.insert("inter-semibold".into(), inter(include_bytes!("../assets/fonts/Inter-SemiBold.ttf")));
    // Italics a weight heavier than their upright: slanted strokes cross the
    // pixel grid and look lighter.
    fonts.font_data.insert("inter-italic".into(), inter(include_bytes!("../assets/fonts/Inter-MediumItalic.ttf")));
    fonts.font_data.insert("inter-bold-italic".into(), inter(include_bytes!("../assets/fonts/Inter-BoldItalic.ttf")));
    // The icon font's bytes (egui-phosphor's `add_to_fonts` is built for
    // another egui version).
    let phosphor = FontData::from_static(egui_phosphor::Variant::Regular.font_bytes());
    fonts.font_data.insert("phosphor".into(), Arc::new(phosphor));
    // Inter first; then the icons (Unicode private use area, which the
    // bundled Inter leaves free, see assets/fonts/README.md) and egui's own
    // fonts (emoji, symbols) as fallbacks.
    let fallbacks: Vec<String> = fonts.families[&FontFamily::Proportional]
        .iter()
        .filter(|f| !matches!(f.as_str(), "inter" | "phosphor"))
        .cloned()
        .collect();
    let family =
        |main: &str| [main.to_owned(), "phosphor".to_owned()].into_iter().chain(fallbacks.iter().cloned()).collect();
    fonts.families.insert(FontFamily::Proportional, family("inter"));
    fonts.families.insert(strong_family(), family("inter-semibold"));
    fonts.families.insert(italic_family(false), family("inter-italic"));
    fonts.families.insert(italic_family(true), family("inter-bold-italic"));
    ctx.set_fonts(fonts);

    ctx.set_theme(ThemePreference::Dark);
    ctx.style_mut_of(Theme::Dark, |s| apply_style(s, &color::DARK, true));
    ctx.style_mut_of(Theme::Light, |s| apply_style(s, &color::LIGHT, false));
}

fn apply_style(style: &mut egui::Style, p: &color::Palette, dark: bool) {
    let color::Palette {
        chrome,
        surface,
        raised,
        hover,
        pressed,
        input,
        border,
        text,
        text_strong,
        accent,
        accent_hover,
        on_accent,
        danger,
        ..
    } = *p;

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
    s.menu_margin = Margin::same(6);
    s.window_margin = Margin::from(space::L);
    s.interact_size = vec2(36.0, 28.0);
    s.slider_width = 120.0;
    s.icon_width = 16.0;
    s.icon_width_inner = 9.0;
    s.icon_spacing = 6.0;
    s.combo_width = 120.0;
    s.menu_width = 220.0;
    // Narrower than egui's 500: a hint stays inside the Settings window
    // instead of running past its edge.
    s.tooltip_width = 320.0;

    let v = &mut style.visuals;
    v.dark_mode = dark;
    v.panel_fill = chrome;
    v.window_fill = surface;
    v.window_stroke = Stroke::new(1.0, border);
    v.window_corner_radius = CornerRadius::from(radius::MEDIUM);
    v.menu_corner_radius = CornerRadius::from(radius::MEDIUM);
    // Softer on a light interface.
    let shadow = |a: u8| Color32::from_black_alpha(if dark { a } else { a / 3 });
    v.window_shadow = Shadow { offset: [0, 10], blur: 28, spread: 0, color: shadow(120) };
    v.popup_shadow = Shadow { offset: [0, 6], blur: 16, spread: 0, color: shadow(100) };
    v.window_highlight_topmost = false;
    v.extreme_bg_color = input;
    v.faint_bg_color = if dark { Color32::from_white_alpha(6) } else { Color32::from_black_alpha(6) };
    v.code_bg_color = input;
    v.hyperlink_color = accent_hover;
    v.error_fg_color = danger;
    v.warn_fg_color = Color32::from_rgb(232, 170, 64);
    v.selection.bg_fill = accent;
    v.selection.stroke = Stroke::new(1.0, on_accent);
    v.text_cursor.stroke = Stroke::new(2.0, accent);
    v.button_frame = true;
    // Sliders show their value as an accent fill, like the other selections.
    v.slider_trailing_fill = true;
    v.indent_has_left_vline = false;

    let rounding = CornerRadius::from(radius::SMALL);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = surface;
    w.noninteractive.weak_bg_fill = surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, text);
    w.noninteractive.corner_radius = rounding;
    for (wv, fill, text) in [
        (&mut w.inactive, raised, text),
        (&mut w.hovered, hover, text_strong),
        (&mut w.active, pressed, text_strong),
        (&mut w.open, hover, text_strong),
    ] {
        wv.bg_fill = fill;
        wv.weak_bg_fill = fill;
        wv.bg_stroke = Stroke::NONE;
        wv.fg_stroke = Stroke::new(1.5, text);
        wv.corner_radius = rounding;
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
    ui.label(RichText::new(text.to_uppercase()).font(font_strong(size::CAPTION)).color(color::text_faint()));
}

/// Screen or section heading.
pub fn heading(text: impl Into<String>) -> RichText {
    RichText::new(text).font(font_strong(size::HEADING)).color(color::text_strong())
}

pub fn title(text: impl Into<String>) -> RichText {
    RichText::new(text).font(font_strong(size::TITLE)).color(color::text_strong())
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).color(color::text_muted())
}

/// The main action of a screen or dialog.
pub fn primary_button(text: impl Into<WidgetText>) -> Button<'static> {
    Button::new(text.into().color(color::on_accent())).fill(color::accent())
}

/// An irreversible action.
pub fn danger_button(text: impl Into<WidgetText>) -> Button<'static> {
    Button::new(text.into().color(color::on_accent())).fill(color::danger())
}

/// Buttons without a fill at rest (toolbars, menus, headers). Only the
/// button fill changes; sliders and checkboxes keep theirs.
pub fn ghost_buttons(ui: &mut Ui) {
    ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
}

/// Shadow under cards (notes, notebooks) on the desk.
pub fn card_shadow() -> Shadow {
    Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(90) }
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
            .text_color(if focused || hovered { color::text_strong() } else { idle })
            .hint_text(hint)
            .align(egui::Align2([align, egui::Align::Center]))
            // A custom frame: no background of its own (painted below).
            .frame(egui::Frame::NONE.inner_margin(Margin::symmetric(space::S as i8, 2)))
            .char_limit(120),
    );
    let rounding = CornerRadius::from(radius::SMALL);
    let fill = if resp.has_focus() {
        Some(color::input())
    } else if resp.hovered() {
        Some(color::hover())
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().set(background, egui::Shape::rect_filled(rect, rounding, fill));
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(rect, rounding, Stroke::new(1.0, color::accent()), StrokeKind::Middle);
    }
    resp
}

/// Height of a [`list_row`].
pub const ROW_HEIGHT: f32 = 28.0;
/// Width of a [`row_button`].
pub const ROW_BUTTON: f32 = 24.0;

/// A full-width list row (notes in the notebook panel): icon and label, a
/// hover fill, and for the selected row a soft accent fill with an accent bar.
/// Long labels end with "…". While hovered, the label keeps clear of the last
/// `actions` points, where the caller puts its [`row_button`]s.
pub fn list_row(ui: &mut Ui, glyph: &str, label: &str, selected: bool, actions: f32) -> egui::Response {
    // Draggable too (reordering); a click is still a click.
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), egui::Sense::click_and_drag());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    // Over the row's own buttons it still counts as hovered.
    let hovered = ui.rect_contains_pointer(rect);
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, radius::SMALL, color::accent_alpha(46));
        let bar = Rect::from_min_size(rect.left_top() + vec2(0.0, 6.0), vec2(3.0, rect.height() - 12.0));
        p.rect_filled(bar, 1.5, color::accent());
    } else if hovered {
        p.rect_filled(rect, radius::SMALL, color::hover());
    }
    let (icon_color, text_color) = match (selected, hovered) {
        (true, _) => (color::accent_hover(), color::text_strong()),
        (false, true) => (color::text(), color::text_strong()),
        (false, false) => (color::text_muted(), color::text()),
    };
    let y = rect.center().y;
    p.text(pos2(rect.left() + space::M, y), egui::Align2::LEFT_CENTER, glyph, font(size::BODY), icon_color);
    let text_left = rect.left() + space::M + 22.0;
    let right = rect.right() - space::S - if hovered { actions } else { 0.0 };
    let mut job = egui::text::LayoutJob::simple_singleline(label.to_owned(), font(size::BODY), text_color);
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - text_left).max(0.0));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(pos2(text_left, y - galley.size().y * 0.5), galley, text_color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small icon button at the right end of a row: `slot` 0 is the last one,
/// 1 the one before it, and so on. `danger` turns it red on hover.
pub fn row_button(
    ui: &mut Ui,
    row: Rect,
    slot: usize,
    id: egui::Id,
    glyph: &str,
    tip: &str,
    danger: bool,
) -> egui::Response {
    let center = pos2(row.right() - space::XS - ROW_BUTTON * (slot as f32 + 0.5), row.center().y);
    let rect = Rect::from_center_size(center, vec2(ROW_BUTTON - 2.0, ROW_BUTTON - 2.0));
    let resp = ui.interact(rect, id, egui::Sense::click());
    let fg = match (resp.hovered(), danger) {
        (true, true) => color::danger(),
        (true, false) => color::text_strong(),
        (false, _) => color::text_muted(),
    };
    if resp.hovered() {
        ui.painter().rect_filled(rect, radius::SMALL, color::pressed());
    }
    ui.painter().text(center, egui::Align2::CENTER_CENTER, glyph, font(size::BODY), fg);
    resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A window that appears and disappears at once, closed with
/// [`window_close_button`] (egui's own is a bare "×").
pub fn window<'a>(id: &str, title: &'a str) -> egui::Window<'a> {
    egui::Window::new(title).id(egui::Id::unique(id)).fade_in(false)
}

/// The close button in the title bar of a [`window`]: `window` is its
/// response. A hover fill like [`row_button`]'s; true when clicked.
pub fn window_close_button(window: &egui::Response) -> bool {
    let ctx = &window.ctx;
    let id = window.id;
    // Where egui puts its own: a heading-high square inside the window margin.
    let style = ctx.global_style();
    let margin = style.spacing.window_margin;
    let h = ctx.fonts_mut(|f| f.row_height(&TextStyle::Heading.resolve(&style)));
    let center = pos2(window.rect.right() - margin.rightf() - h * 0.5, window.rect.top() + margin.topf() + h * 0.5);
    let rect = Rect::from_center_size(center, vec2(ROW_BUTTON, ROW_BUTTON));
    // On the window's layer, over its title bar.
    let builder = egui::UiBuilder::new().layer_id(window.layer_id).max_rect(rect);
    let ui = Ui::new(ctx.clone(), id.with("close_ui"), builder);
    let resp = ui.interact(rect, id.with("close"), egui::Sense::click());
    let fg = if resp.hovered() { color::text_strong() } else { color::text_muted() };
    if resp.hovered() {
        ui.painter().rect_filled(rect, radius::SMALL, color::pressed());
    }
    ui.painter().text(center, egui::Align2::CENTER_CENTER, egui_phosphor::regular::X, font(size::BODY), fg);
    resp.on_hover_text("Close (Esc)").on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// A dashed "add" placeholder (add a page, a note): outline and label turn
/// accent on hover.
pub fn add_tile(ui: &mut Ui, rect: Rect, id: egui::Id, label: &str) -> egui::Response {
    let resp = ui.interact(rect, id, egui::Sense::click());
    let (line, text, fill) = if resp.hovered() {
        (color::accent(), color::accent_hover(), color::accent_alpha(18))
    } else {
        (color::border_strong(), color::text_muted(), Color32::TRANSPARENT)
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
    let galley = painter.layout_no_wrap(text.to_owned(), font(size::BODY), color::text_strong());
    let rect = Rect::from_center_size(center, galley.size() + vec2(2.0 * space::L, 2.0 * space::S + 2.0));
    painter.add(
        Shadow { offset: [0, 6], blur: 16, spread: 0, color: Color32::from_black_alpha(110) }
            .as_shape(rect, CornerRadius::from(radius::MEDIUM)),
    );
    painter.rect(rect, radius::MEDIUM, color::surface(), Stroke::new(1.0, color::border()), StrokeKind::Middle);
    painter.galley(rect.center() - galley.size() * 0.5, galley, color::text_strong());
    rect
}
