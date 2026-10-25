// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Help > Check for updates: the updater's window. It shows the running
//! version, how new versions are handled, and what the updater is doing
//! (checking, downloading, installed), with the actions that go with it.

use eframe::egui::{self, Align2, Ui};
use egui_phosphor::regular as icon;

use super::InkApp;
use super::menus::UPDATES_WINDOW;
use super::settings_ui::{hint, row, rows, section};
use crate::settings::{UpdateChannel, UpdateMode};
use crate::theme;
use crate::update::{self, State as UpdateState};

impl InkApp {
    /// Opens the updates window and asks for the latest release, unless one
    /// is already being fetched or installed.
    pub(super) fn check_for_updates(&mut self, ctx: &egui::Context) {
        self.help.updates = true;
        if !matches!(self.updater.state(), UpdateState::Installed(_)) {
            let install = self.settings.updates == UpdateMode::Install;
            self.updater.check(ctx, self.settings.update_channel, install);
        }
    }

    /// Help > Check for updates: the updater in a floating window.
    pub(super) fn updates_window(&mut self, ctx: &egui::Context) {
        if !self.help.updates {
            return;
        }
        let shown = theme::window(UPDATES_WINDOW, "Updates")
            .collapsible(false)
            // Above the notches and the toolbar.
            .order(egui::Order::Foreground)
            .resizable(false)
            .default_width(380.0)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| self.updates(ui));
        if shown.is_some_and(|w| theme::window_close_button(&w.response)) {
            self.help.updates = false;
        }
    }

    fn updates(&mut self, ui: &mut Ui) {
        let state = self.updater.state().clone();
        let mut check = false;
        let mut recheck = false;
        let mut install = false;
        let mut restart = false;
        let s = &mut self.settings;

        rows(ui, "updates", |ui| {
            row(ui, "Version", |ui| {
                ui.label(update::CURRENT);
                if ui
                    .add_enabled(!state.busy(), egui::Button::new(format!("{}  Check now", icon::MAGNIFYING_GLASS)))
                    .clicked()
                {
                    check = true;
                }
            });
            row(ui, "New versions", |ui| {
                egui::ComboBox::from_id_salt("update_mode").selected_text(s.updates.label()).show_ui(ui, |ui| {
                    for mode in [UpdateMode::Install, UpdateMode::Notify, UpdateMode::Off] {
                        ui.selectable_value(&mut s.updates, mode, mode.label());
                    }
                });
            });
            row(ui, "Channel", |ui| {
                egui::ComboBox::from_id_salt("update_channel").selected_text(s.update_channel.label()).show_ui(
                    ui,
                    |ui| {
                        for channel in UpdateChannel::ALL {
                            // Another channel: look again, without installing.
                            if ui.selectable_value(&mut s.update_channel, channel, channel.label()).changed() {
                                recheck = true;
                            }
                        }
                    },
                );
            });
        });

        section(ui, "Status");
        match &state {
            UpdateState::Idle => hint(ui, "Checked when Tratto starts, from its GitHub releases."),
            UpdateState::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    hint(ui, "Checking for updates...");
                });
            }
            UpdateState::UpToDate => hint(ui, "Tratto is up to date."),
            UpdateState::Available(release) => {
                hint(ui, &format!("Tratto {} is available.", release.version));
                if let Some(why) = &release.blocked {
                    hint(ui, why);
                }
                ui.horizontal(|ui| {
                    if release.installable()
                        && ui.add(theme::primary_button(format!("{}  Install", icon::DOWNLOAD_SIMPLE))).clicked()
                    {
                        install = true;
                    }
                    ui.hyperlink_to(format!("{}  Release notes", icon::ARROW_SQUARE_OUT), &release.page);
                });
            }
            UpdateState::Downloading(release) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    hint(ui, &format!("Downloading Tratto {}...", release.version));
                });
            }
            UpdateState::Installed(version) => {
                hint(ui, &format!("Tratto {version} is installed; it starts next time."));
                if ui.add(theme::primary_button(format!("{}  Restart now", icon::ARROW_CLOCKWISE))).clicked() {
                    restart = true;
                }
            }
            UpdateState::Failed(e) => hint(ui, e),
        }

        let channel = self.settings.update_channel;
        if check {
            let install = self.settings.updates == UpdateMode::Install;
            self.updater.check(ui.ctx(), channel, install);
        } else if recheck && !matches!(state, UpdateState::Installed(_)) {
            self.updater.check(ui.ctx(), channel, false);
        }
        if install {
            self.updater.install(ui.ctx());
        }
        if restart {
            self.restart(ui.ctx());
        }
    }
}
