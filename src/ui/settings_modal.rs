use std::path::Path;

use egui::{Align2, Color32, CornerRadius, Key, Margin, RichText, Stroke, Vec2};

use crate::ui::os_integration::OsIntegrationModal;
use crate::ui::settings::{AppSettings, FontPreference, LineSpacing, PageWidth};
use crate::ui::theme::{ThemeMode, WordTheme};

/// Tabs inside the unified Settings & Preferences modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    Appearance,
    Security,
    OsIntegration,
}

/// Result returned from `SettingsModal::show` when user edits preferences or triggers OS integration actions.
#[derive(Debug, Clone, Default)]
pub struct SettingsModalOutcome {
    pub settings_changed: bool,
    pub theme_changed: bool,
    pub status_message: Option<String>,
}

/// Unified tabbed Preferences, Security, and OS Integration dialog (`Ctrl+,`).
#[derive(Debug, Default)]
pub struct SettingsModal {
    pub open: bool,
    pub active_tab: SettingsTab,
}

impl SettingsModal {
    /// Opens the settings modal on the requested tab and synchronizes `os_modal` state.
    pub fn open_tab(
        &mut self,
        tab: SettingsTab,
        os_modal: &mut OsIntegrationModal,
        settings: &AppSettings,
    ) {
        self.open = true;
        self.active_tab = tab;
        os_modal.open_with_settings(settings);
        os_modal.open = false;
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: &WordTheme,
        settings: &mut AppSettings,
        settings_path: &Path,
        os_modal: &mut OsIntegrationModal,
    ) -> SettingsModalOutcome {
        let mut outcome = SettingsModalOutcome::default();
        if !self.open {
            return outcome;
        }

        let mut keep_open = true;
        let esc_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if esc_pressed {
            self.open = false;
            return outcome;
        }

        egui::Window::new("Settings & Preferences")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(590.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (tab, label) in [
                        (SettingsTab::Appearance, "Appearance & Fonts"),
                        (SettingsTab::Security, "Security"),
                        (SettingsTab::OsIntegration, "OS Integration"),
                    ] {
                        let active = self.active_tab == tab;
                        let btn = egui::Button::new(
                            RichText::new(label).size(12.5).strong().color(if active {
                                theme.accent
                            } else {
                                theme.text_primary
                            }),
                        )
                        .fill(if active {
                            theme.accent_soft
                        } else {
                            theme.pill_bg
                        })
                        .stroke(Stroke::new(
                            1.0_f32,
                            if active {
                                theme.accent
                            } else {
                                theme.border_subtle
                            },
                        ))
                        .corner_radius(CornerRadius::same(6));
                        if ui.add(btn).clicked() {
                            self.active_tab = tab;
                            if tab == SettingsTab::OsIntegration
                                && os_modal.exe_path_input.is_empty()
                            {
                                os_modal.open_with_settings(settings);
                                os_modal.open = false;
                            }
                        }
                    }
                });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

                match self.active_tab {
                    SettingsTab::Appearance => {
                        Self::show_appearance_tab(ui, theme, settings, &mut outcome);
                    }
                    SettingsTab::Security => {
                        Self::show_security_tab(ui, theme, settings, &mut outcome);
                    }
                    SettingsTab::OsIntegration => {
                        if let Some(msg) =
                            os_modal.show_contents(ui, theme, settings, settings_path, true)
                        {
                            outcome.settings_changed = true;
                            outcome.status_message = Some(msg);
                        }
                    }
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Changes save automatically.")
                            .size(11.5)
                            .color(theme.text_muted),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.open = false;
                        }
                    });
                });
            });

        if !keep_open {
            self.open = false;
        }
        outcome
    }

    fn show_appearance_tab(
        ui: &mut egui::Ui,
        theme: &WordTheme,
        settings: &mut AppSettings,
        outcome: &mut SettingsModalOutcome,
    ) {
        ui.label(
            RichText::new("Theme & Workspace")
                .strong()
                .size(13.0)
                .color(theme.heading_color),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Color Theme:").size(12.5));
            for (mode, lbl) in [
                (ThemeMode::Light, "Light"),
                (ThemeMode::Sepia, "Sepia"),
                (ThemeMode::Dark, "Dark"),
            ] {
                if ui
                    .selectable_label(settings.theme_mode == mode, lbl)
                    .clicked()
                    && settings.theme_mode != mode
                {
                    settings.theme_mode = mode;
                    settings.dark_mode = mode == ThemeMode::Dark;
                    outcome.settings_changed = true;
                    outcome.theme_changed = true;
                }
            }
        });

        ui.add_space(8.0);
        ui.label(
            RichText::new("Typography & Font Preferences")
                .strong()
                .size(13.0)
                .color(theme.heading_color),
        );
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(theme.ribbon_bg)
            .stroke(Stroke::new(1.0_f32, theme.border_subtle))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                egui::Grid::new("settings_fonts_grid")
                    .num_columns(3)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Menu / UI Font Size:").size(12.5));
                        if ui
                            .add(
                                egui::Slider::new(&mut settings.ui_font_size, 11.0..=16.0)
                                    .step_by(0.5)
                                    .suffix(" px"),
                            )
                            .changed()
                        {
                            outcome.settings_changed = true;
                        }
                        ui.label(
                            RichText::new("Toolbars, menus, panels")
                                .size(11.0)
                                .color(theme.text_muted),
                        );
                        ui.end_row();

                        ui.label(RichText::new("Source Editor Font:").size(12.5));
                        egui::ComboBox::from_id_salt("settings_editor_font_family")
                            .selected_text(settings.editor_font_family.label())
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for fam in [
                                    FontPreference::Monospace,
                                    FontPreference::SystemSans,
                                    FontPreference::Serif,
                                ] {
                                    if ui
                                        .selectable_label(
                                            settings.editor_font_family == fam,
                                            fam.label(),
                                        )
                                        .clicked()
                                    {
                                        settings.editor_font_family = fam;
                                        outcome.settings_changed = true;
                                    }
                                }
                            });
                        if ui
                            .add(
                                egui::Slider::new(&mut settings.editor_font_size, 11.0..=28.0)
                                    .step_by(0.5)
                                    .suffix(" px"),
                            )
                            .changed()
                        {
                            outcome.settings_changed = true;
                        }
                        ui.end_row();

                        ui.label(RichText::new("Reader / WYSIWYG Font:").size(12.5));
                        egui::ComboBox::from_id_salt("settings_reader_font_family")
                            .selected_text(settings.reader_font_family.label())
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for fam in [
                                    FontPreference::SystemSans,
                                    FontPreference::Serif,
                                    FontPreference::Monospace,
                                ] {
                                    if ui
                                        .selectable_label(
                                            settings.reader_font_family == fam,
                                            fam.label(),
                                        )
                                        .clicked()
                                    {
                                        settings.reader_font_family = fam;
                                        outcome.settings_changed = true;
                                    }
                                }
                            });
                        if ui
                            .add(
                                egui::Slider::new(&mut settings.reader_font_size, 12.0..=28.0)
                                    .step_by(0.5)
                                    .suffix(" px"),
                            )
                            .changed()
                        {
                            outcome.settings_changed = true;
                        }
                        ui.end_row();
                    });
            });

        ui.add_space(8.0);
        ui.label(
            RichText::new("Reading Layout & Behavior")
                .strong()
                .size(13.0)
                .color(theme.heading_color),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Page Width:").size(12.5));
            for (pw, lbl) in [
                (PageWidth::Narrow, "Narrow"),
                (PageWidth::Standard, "Standard"),
                (PageWidth::Wide, "Wide"),
            ] {
                if ui
                    .selectable_label(settings.page_width == pw, lbl)
                    .clicked()
                {
                    settings.page_width = pw;
                    outcome.settings_changed = true;
                }
            }
            ui.separator();
            ui.label(RichText::new("Line Spacing:").size(12.5));
            for (ls, lbl) in [
                (LineSpacing::Compact, "Compact"),
                (LineSpacing::Comfortable, "Comfortable"),
                (LineSpacing::Relaxed, "Relaxed"),
            ] {
                if ui
                    .selectable_label(settings.line_spacing == ls, lbl)
                    .clicked()
                {
                    settings.line_spacing = ls;
                    outcome.settings_changed = true;
                }
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut settings.sync_scroll, "Sync Scroll (Split Mode)")
                .changed()
            {
                outcome.settings_changed = true;
            }
            if ui
                .checkbox(&mut settings.show_markup, "Show Inline Markup (WYSIWYG)")
                .changed()
            {
                outcome.settings_changed = true;
            }
        });
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut settings.auto_calc_math, "Auto-Calculate Math")
                .changed()
            {
                outcome.settings_changed = true;
            }
            if ui
                .checkbox(&mut settings.auto_calc_tables, "Auto-Calculate Tables")
                .changed()
            {
                outcome.settings_changed = true;
            }
        });

        ui.add_space(6.0);
        if ui.button("Reset Appearance Defaults").clicked() {
            let def = AppSettings::default();
            outcome.theme_changed = settings.theme_mode != def.theme_mode;
            settings.theme_mode = def.theme_mode;
            settings.dark_mode = def.dark_mode;
            settings.ui_font_size = def.ui_font_size;
            settings.editor_font_family = def.editor_font_family;
            settings.editor_font_size = def.editor_font_size;
            settings.reader_font_family = def.reader_font_family;
            settings.reader_font_size = def.reader_font_size;
            settings.page_width = def.page_width;
            settings.line_spacing = def.line_spacing;
            settings.zoom_factor = def.zoom_factor;
            outcome.settings_changed = true;
            outcome.status_message =
                Some("Reset appearance and font preferences to defaults".to_string());
        }
    }

    fn show_security_tab(
        ui: &mut egui::Ui,
        theme: &WordTheme,
        settings: &mut AppSettings,
        outcome: &mut SettingsModalOutcome,
    ) {
        ui.label(
            RichText::new("Link & Network Content Security")
                .strong()
                .size(13.0)
                .color(theme.heading_color),
        );
        ui.add_space(2.0);
        ui.label(
            RichText::new("Control which link targets and remote resources Markdown Studio may open or fetch.")
                .size(12.0)
                .color(theme.text_muted),
        );
        ui.add_space(8.0);

        if ui
            .checkbox(
                &mut settings.allow_external_links,
                "Allow opening external links (http://, https://, mailto:)",
            )
            .changed()
        {
            outcome.settings_changed = true;
        }
        if ui
            .checkbox(
                &mut settings.allow_local_file_links,
                "Allow opening local document & file links",
            )
            .changed()
        {
            outcome.settings_changed = true;
        }
        if ui
            .checkbox(
                &mut settings.allow_executable_links,
                "Allow opening executable & script files (.exe, .bat, .cmd, .ps1, .sh, .msi, ...)",
            )
            .changed()
        {
            outcome.settings_changed = true;
        }
        if settings.allow_executable_links {
            egui::Frame::new()
                .fill(Color32::from_rgba_unmultiplied(
                    theme.warning_amber.r(),
                    theme.warning_amber.g(),
                    theme.warning_amber.b(),
                    26,
                ))
                .stroke(Stroke::new(1.0_f32, theme.warning_amber))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(
                            "Warning: Executable & script links are enabled. Clicking them in untrusted Markdown files launches programs directly via your OS.",
                        )
                        .size(11.5)
                        .color(theme.warning_amber),
                    );
                });
        }
        if ui
            .checkbox(
                &mut settings.allow_remote_images,
                "Allow loading remote images (http://, https://)",
            )
            .changed()
        {
            outcome.settings_changed = true;
        }

        ui.add_space(8.0);
        egui::Frame::new()
            .fill(theme.ribbon_bg)
            .stroke(Stroke::new(1.0_f32, theme.border_subtle))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(
                        "Always blocked for safety: UNC network shares (\\\\server\\share), NTFS Alternate Data Streams, javascript:, vbscript:, and data: URIs.",
                    )
                    .size(11.5)
                    .color(theme.text_muted),
                );
            });

        ui.add_space(8.0);
        if ui.button("Restore Safe Defaults").clicked() {
            let def = AppSettings::default();
            settings.allow_external_links = def.allow_external_links;
            settings.allow_local_file_links = def.allow_local_file_links;
            settings.allow_executable_links = def.allow_executable_links;
            settings.allow_remote_images = def.allow_remote_images;
            outcome.settings_changed = true;
            outcome.status_message = Some("Restored safe security defaults".to_string());
        }
    }
}
