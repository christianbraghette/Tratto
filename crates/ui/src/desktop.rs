// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The window buttons of the desktop the app runs on: which ones, on which
//! side and in which style, so the custom title bar matches the other
//! windows.
//!
//! Windows and macOS: minimize, maximize and close on the right, Windows
//! style. Linux: on KDE, KWin's own settings (`kwinrc`) and the Breeze
//! style; elsewhere GNOME's `button-layout` setting, read through the
//! desktop portal (GNOME and the other GTK desktops answer it), and the
//! Adwaita style. The setting is read on a background thread at startup;
//! until it is known, the desktop's default layout is used.

use eframe::egui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Minimize,
    Maximize,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum Style {
    /// Full-height square buttons, the close one red when hovered.
    Windows,
    /// GNOME / libadwaita: small round buttons, always filled.
    Adwaita,
    /// KDE / Breeze: bare icons, a round highlight when hovered.
    Breeze,
}

/// The window buttons, each side listed from left to right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowButtons {
    pub style: Style,
    pub left: Vec<Button>,
    pub right: Vec<Button>,
}

impl WindowButtons {
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    fn windows() -> Self {
        Self { style: Style::Windows, left: Vec::new(), right: vec![Button::Minimize, Button::Maximize, Button::Close] }
    }
}

/// The window buttons of this desktop. Cheap: call it every frame.
pub fn window_buttons(ctx: &egui::Context) -> WindowButtons {
    #[cfg(target_os = "linux")]
    return linux::window_buttons(ctx);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = ctx;
        WindowButtons::windows()
    }
}

/// GNOME's `button-layout` ("appmenu:minimize,maximize,close"): the buttons
/// left and right of the colon. Other entries (`appmenu`, `icon`, `spacer`)
/// are not window buttons; without a colon all buttons are on the left.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_gnome_layout(layout: &str) -> (Vec<Button>, Vec<Button>) {
    let side = |s: &str| {
        s.split(',')
            .filter_map(|name| match name.trim() {
                "minimize" => Some(Button::Minimize),
                "maximize" => Some(Button::Maximize),
                "close" => Some(Button::Close),
                _ => None,
            })
            .collect()
    };
    let (left, right) = layout.split_once(':').unwrap_or((layout, ""));
    (side(left), side(right))
}

/// KWin's `ButtonsOnLeft` / `ButtonsOnRight`: one letter per button
/// (`I` minimize, `A` maximize, `X` close; the others are not drawn here).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_kwin_buttons(letters: &str) -> Vec<Button> {
    letters
        .chars()
        .filter_map(|c| match c {
            'I' => Some(Button::Minimize),
            'A' => Some(Button::Maximize),
            'X' => Some(Button::Close),
            _ => None,
        })
        .collect()
}

/// A key of a section of an INI-style file (KDE's config files).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ini_value<'a>(text: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut in_section = false;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = name == section;
        } else if in_section
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim());
        }
    }
    None
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::PathBuf;
    use std::sync::{Once, OnceLock};

    use eframe::egui;

    use super::{Style, WindowButtons, ini_value, parse_gnome_layout, parse_kwin_buttons};

    /// The layout read from the desktop's settings.
    static READ: OnceLock<WindowButtons> = OnceLock::new();
    static ASKED: Once = Once::new();

    pub fn window_buttons(ctx: &egui::Context) -> WindowButtons {
        if let Some(read) = READ.get() {
            return read.clone();
        }
        ASKED.call_once(|| {
            let ctx = ctx.clone();
            let _ = std::thread::Builder::new().name("window-buttons".into()).spawn(move || {
                let _ = READ.set(read_settings());
                ctx.request_repaint();
            });
        });
        default_layout()
    }

    /// `XDG_CURRENT_DESKTOP` names the desktop, e.g. "KDE" or "ubuntu:GNOME".
    fn desktop_is(name: &str) -> bool {
        std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|d| d.eq_ignore_ascii_case(name)))
    }

    /// What each desktop shows out of the box.
    fn default_layout() -> WindowButtons {
        if desktop_is("KDE") {
            kwin_layout(None)
        } else if desktop_is("GNOME") {
            adwaita("appmenu:close")
        } else {
            adwaita("appmenu:minimize,maximize,close")
        }
    }

    fn adwaita(layout: &str) -> WindowButtons {
        let (left, right) = parse_gnome_layout(layout);
        WindowButtons { style: Style::Adwaita, left, right }
    }

    fn read_settings() -> WindowButtons {
        if desktop_is("KDE") {
            let path = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .map(|dir| dir.join("kwinrc"));
            let text = path.and_then(|p| std::fs::read_to_string(p).ok());
            return kwin_layout(text.as_deref());
        }
        match portal_button_layout() {
            Some(layout) => adwaita(&layout),
            None => default_layout(),
        }
    }

    /// KWin's buttons from the text of `kwinrc` (its defaults where unset).
    fn kwin_layout(kwinrc: Option<&str>) -> WindowButtons {
        const SECTION: &str = "org.kde.kdecoration2";
        let get = |key, default| kwinrc.and_then(|t| ini_value(t, SECTION, key)).unwrap_or(default);
        WindowButtons {
            style: Style::Breeze,
            left: parse_kwin_buttons(get("ButtonsOnLeft", "MS")),
            right: parse_kwin_buttons(get("ButtonsOnRight", "HIAX")),
        }
    }

    /// `org.gnome.desktop.wm.preferences button-layout` through the portal's
    /// Settings interface (also outside GNOME, and inside a sandbox).
    fn portal_button_layout() -> Option<String> {
        use zbus::zvariant::{OwnedValue, Value};

        fn string_in(value: &Value<'_>) -> Option<String> {
            match value {
                Value::Str(s) => Some(s.to_string()),
                // `Read` wraps the setting in one more variant.
                Value::Value(inner) => string_in(inner),
                _ => None,
            }
        }

        let conn = zbus::blocking::Connection::session().ok()?;
        let reply = conn
            .call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.portal.Settings"),
                "Read",
                &("org.gnome.desktop.wm.preferences", "button-layout"),
            )
            .ok()?;
        let value: OwnedValue = reply.body().deserialize().ok()?;
        string_in(&value)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::desktop::Button::*;

        #[test]
        fn kwin_buttons_come_from_kwinrc_or_its_defaults() {
            let rc =
                "[Compositing]\nButtonsOnLeft=X\n\n[org.kde.kdecoration2]\nButtonsOnLeft=XIA\nlibrary=org.kde.breeze\n";
            let b = kwin_layout(Some(rc));
            assert_eq!((b.left, b.right), (vec![Close, Minimize, Maximize], vec![Minimize, Maximize, Close]));
            let b = kwin_layout(None);
            assert_eq!((b.left, b.right), (vec![], vec![Minimize, Maximize, Close]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Button::*;

    #[test]
    fn gnome_layouts_parse() {
        assert_eq!(parse_gnome_layout("appmenu:close"), (vec![], vec![Close]));
        assert_eq!(parse_gnome_layout("close,minimize,maximize:appmenu"), (vec![Close, Minimize, Maximize], vec![]));
        assert_eq!(
            parse_gnome_layout("icon:minimize,spacer,maximize,close"),
            (vec![], vec![Minimize, Maximize, Close])
        );
        assert_eq!(parse_gnome_layout("close"), (vec![Close], vec![]));
    }

    #[test]
    fn kwin_letters_parse() {
        assert_eq!(parse_kwin_buttons("HIAX"), vec![Minimize, Maximize, Close]);
        assert_eq!(parse_kwin_buttons("MS"), vec![]);
    }
}
