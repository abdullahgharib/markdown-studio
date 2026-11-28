use std::path::Path;

use egui::{Align2, Color32, CornerRadius, Key, Margin, RichText, Stroke, Vec2};

use crate::APP_VERSION;
use crate::ui::fonts::installed_system_fonts;
use crate::ui::os_integration::OsIntegrationModal;
use crate::ui::settings::{AppSettings, FontPreference, LineSpacing, PageWidth};
use crate::ui::theme::{ThemeMode, WordTheme};
use crate::ui::updater::{render_release_notes_box, UpdateManager, UpdateStatus, RELEASES_PAGE_URL};

/// Tabs inside the unified Settings & Preferences modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    Appearance,
    Security,
    OsIntegration,
    Updates,
}

/// Result returned from `SettingsModal::show` when user edits preferences or triggers OS integration actions.
#[derive(Debug, Clone, Default)]
pub struct SettingsModalOutcome {
    pub settings_changed: bool,
    pub theme_changed: bool,
    pub status_message: Option<String>,
    pub check_updates_now: bool,
    pub open_update_modal: bool,
    pub open_release_page: Option<String>,
}

/// Unified tabbed Preferences, Security, and OS Integration dialog (`Ctrl+,`).
#[derive(Debug, Default)]
pub struct SettingsModal {
    pub open: bool,
    pub active_tab: SettingsTab,
    pub ui_font_search: String,
    pub editor_font_search: String,
    pub reader_font_search: String,
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

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: &WordTheme,
        settings: &mut AppSettings,
        settings_path: &Path,
        os_modal: &mut OsIntegrationModal,
        updater: &UpdateManager,
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
            .default_width(610.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (tab, label) in [
                        (SettingsTab::Appearance, "Appearance & Fonts"),
                        (SettingsTab::Security, "Security"),
                        (SettingsTab::OsIntegration, "OS Integration"),
                        (SettingsTab::Updates, "Updates"),
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

                egui::ScrollArea::vertical()
                    .max_height(460.0)
                    .show(ui, |ui| match self.active_tab {
                        SettingsTab::Appearance => {
                            self.show_appearance_tab(ui, theme, settings, &mut outcome);
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
                        SettingsTab::Updates => {
                            Self::show_updates_tab(ui, theme, settings, updater, &mut outcome);
                        }
                    });

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

    #[allow(clippy::too_many_arguments)]
    fn show_system_font_picker(
        ui: &mut egui::Ui,
        id_salt: &str,
        search_query: &mut String,
        custom_font_name: &mut Option<String>,
        mut preset_opt: Option<&mut FontPreference>,
        presets: &[FontPreference],
        default_label: &str,
        theme: &WordTheme,
    ) -> bool {
        let mut changed = false;
        let selected_label = if let Some(name) = custom_font_name.as_deref() {
            name.to_string()
        } else if let Some(ref p) = preset_opt {
            p.label().to_string()
        } else {
            default_label.to_string()
        };

        let popup_id = ui.make_persistent_id(id_salt);
        let btn = egui::Button::new(
            RichText::new(selected_label)
                .size(12.0)
                .color(theme.text_primary),
        )
        .min_size(Vec2::new(180.0, 22.0))
        .fill(theme.paper_bg)
        .stroke(Stroke::new(1.0_f32, theme.border))
        .corner_radius(CornerRadius::same(5));
        let resp = ui
            .add(btn)
            .on_hover_text("Choose any installed OS font or preset");
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }

        egui::popup_below_widget(
            ui,
            popup_id,
            &resp,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(250.0);
                ui.set_max_width(280.0);
                ui.add(
                    egui::TextEdit::singleline(search_query)
                        .hint_text("Search installed system fonts...")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);

                if let Some(ref mut preset_ref) = preset_opt {
                    for &p in presets {
                        let sel = custom_font_name.is_none() && **preset_ref == p;
                        if ui.selectable_label(sel, p.label()).clicked() {
                            *custom_font_name = None;
                            **preset_ref = p;
                            changed = true;
                            ui.memory_mut(|m| m.close_popup());
                        }
                    }
                } else {
                    let sel = custom_font_name.is_none();
                    if ui.selectable_label(sel, default_label).clicked() {
                        *custom_font_name = None;
                        changed = true;
                        ui.memory_mut(|m| m.close_popup());
                    }
                }

                let catalog = installed_system_fonts();
                ui.separator();
                ui.label(
                    RichText::new(format!("Installed System Fonts ({})", catalog.len()))
                        .size(11.0)
                        .strong()
                        .color(theme.text_muted),
                );

                let q = search_query.trim().to_lowercase();
                egui::ScrollArea::vertical()
                    .id_salt((id_salt, "scroll"))
                    .max_height(210.0)
                    .show(ui, |ui| {
                        let mut matches = 0usize;
                        for entry in catalog {
                            if !q.is_empty() && !entry.family_lower.contains(&q) {
                                continue;
                            }
                            matches += 1;
                            let is_sel = custom_font_name
                                .as_deref()
                                .is_some_and(|n| n.eq_ignore_ascii_case(&entry.family));
                            let label = if entry.is_monospace {
                                format!("{}  [Mono]", entry.family)
                            } else {
                                entry.family.clone()
                            };
                            if ui.selectable_label(is_sel, label).clicked() {
                                *custom_font_name = Some(entry.family.clone());
                                changed = true;
                                ui.memory_mut(|m| m.close_popup());
                            }
                        }
                        if matches == 0 {
                            ui.label(
                                RichText::new("(No matching system fonts)")
                                    .size(11.5)
                                    .color(theme.text_muted),
                            );
                        }
                    });
            },
        );
        changed
    }

    fn show_appearance_tab(
        &mut self,
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
                        ui.label(RichText::new("Menu / UI Font:").size(12.5));
                        if Self::show_system_font_picker(
                            ui,
                            "settings_ui_font_picker",
                            &mut self.ui_font_search,
                            &mut settings.ui_font_name,
                            None,
                            &[],
                            "System Default",
                            theme,
                        ) {
                            outcome.settings_changed = true;
                        }
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
                        ui.end_row();

                        ui.label(RichText::new("Source Editor Font:").size(12.5));
                        if Self::show_system_font_picker(
                            ui,
                            "settings_editor_font_picker",
                            &mut self.editor_font_search,
                            &mut settings.editor_font_name,
                            Some(&mut settings.editor_font_family),
                            &[
                                FontPreference::Monospace,
                                FontPreference::SystemSans,
                                FontPreference::Serif,
                            ],
                            "Monospace",
                            theme,
                        ) {
                            outcome.settings_changed = true;
                        }
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
                        if Self::show_system_font_picker(
                            ui,
                            "settings_reader_font_picker",
                            &mut self.reader_font_search,
                            &mut settings.reader_font_name,
                            Some(&mut settings.reader_font_family),
                            &[
                                FontPreference::SystemSans,
                                FontPreference::Serif,
                                FontPreference::Monospace,
                            ],
                            "System Sans",
                            theme,
                        ) {
                            outcome.settings_changed = true;
                        }
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
            settings.ui_font_name = None;
            settings.editor_font_family = def.editor_font_family;
            settings.editor_font_size = def.editor_font_size;
            settings.editor_font_name = None;
            settings.reader_font_family = def.reader_font_family;
            settings.reader_font_size = def.reader_font_size;
            settings.reader_font_name = None;
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
            RichText::new("Enable or disable every link category, URI scheme, and remote resource type individually.")
                .size(12.0)
                .color(theme.text_muted),
        );
        ui.add_space(8.0);

        ui.label(
            RichText::new("Standard Links & Media")
                .strong()
                .size(12.0)
                .color(theme.text_primary),
        );
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
                &mut settings.allow_remote_images,
                "Allow loading remote images (http://, https://)",
            )
            .changed()
        {
            outcome.settings_changed = true;
        }

        ui.add_space(8.0);
        ui.label(
            RichText::new("High-Risk Links & URI Schemes (Disabled by Default)")
                .strong()
                .size(12.0)
                .color(theme.text_primary),
        );
        if ui
            .checkbox(
                &mut settings.allow_executable_links,
                "Allow opening executable & script files (.exe, .bat, .cmd, .ps1, .sh, .msi, ...)",
            )
            .changed()
        {
            outcome.settings_changed = true;
        }
        if ui
            .checkbox(
                &mut settings.allow_unc_network_links,
                "Allow opening UNC network shares (\\\\server\\share, //server/share)",
            )
            .changed()
        {
            outcome.settings_changed = true;
        }
        if ui
            .checkbox(
                &mut settings.allow_ntfs_ads_links,
                "Allow opening NTFS Alternate Data Streams (file.txt:stream)",
            )
            .changed()
        {
            outcome.settings_changed = true;
        }
        if ui
            .checkbox(&mut settings.allow_javascript_uri, "Allow javascript: URIs")
            .changed()
        {
            outcome.settings_changed = true;
        }
        if ui
            .checkbox(&mut settings.allow_vbscript_uri, "Allow vbscript: URIs")
            .changed()
        {
            outcome.settings_changed = true;
        }
        if ui
            .checkbox(&mut settings.allow_data_uri, "Allow data: URIs")
            .changed()
        {
            outcome.settings_changed = true;
        }

        let any_high_risk = settings.allow_executable_links
            || settings.allow_unc_network_links
            || settings.allow_ntfs_ads_links
            || settings.allow_javascript_uri
            || settings.allow_vbscript_uri
            || settings.allow_data_uri;
        if any_high_risk {
            ui.add_space(6.0);
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
                            "Warning: One or more high-risk link/URI categories are enabled. Only click untrusted links in files you trust.",
                        )
                        .size(11.5)
                        .color(theme.warning_amber),
                    );
                });
        }

        ui.add_space(8.0);
        if ui.button("Restore Safe Defaults").clicked() {
            let def = AppSettings::default();
            settings.allow_external_links = def.allow_external_links;
            settings.allow_local_file_links = def.allow_local_file_links;
            settings.allow_executable_links = def.allow_executable_links;
            settings.allow_unc_network_links = def.allow_unc_network_links;
            settings.allow_ntfs_ads_links = def.allow_ntfs_ads_links;
            settings.allow_javascript_uri = def.allow_javascript_uri;
            settings.allow_vbscript_uri = def.allow_vbscript_uri;
            settings.allow_data_uri = def.allow_data_uri;
            settings.allow_remote_images = def.allow_remote_images;
            outcome.settings_changed = true;
            outcome.status_message = Some("Restored safe security defaults".to_string());
        }
    }

    fn show_updates_tab(
        ui: &mut egui::Ui,
        theme: &WordTheme,
        settings: &mut AppSettings,
        updater: &UpdateManager,
        outcome: &mut SettingsModalOutcome,
    ) {
        ui.label(
            RichText::new("GitHub Releases & Auto-Update")
                .strong()
                .size(13.5)
                .color(theme.text_primary),
        );
        ui.label(
            RichText::new(format!(
                "Installed version: v{APP_VERSION}  ·  Repository: github.com/abdullahgharib/markdown-studio"
            ))
            .size(11.5)
            .color(theme.text_muted),
        );
        ui.add_space(8.0);

        if ui
            .checkbox(
                &mut settings.check_updates_on_startup,
                "Check for updates automatically on startup",
            )
            .changed()
        {
            outcome.settings_changed = true;
        }

        if let Some(skipped) = settings.skipped_update_version.clone() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Skipped version: v{skipped}"))
                        .size(12.0)
                        .color(theme.text_muted),
                );
                if ui.small_button("Clear Skipped Version").clicked() {
                    settings.skipped_update_version = None;
                    outcome.settings_changed = true;
                    outcome.status_message = Some(format!("Cleared skipped version v{skipped}"));
                }
            });
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let busy = updater.is_busy();
            let check_btn = egui::Button::new(
                RichText::new(if matches!(updater.status, UpdateStatus::Checking { .. }) {
                    "Checking..."
                } else {
                    "Check for Updates Now"
                })
                .strong()
                .color(Color32::WHITE),
            )
            .fill(theme.accent)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(6));
            if ui.add_enabled(!busy, check_btn).clicked() {
                outcome.check_updates_now = true;
            }
            if ui.button("Open GitHub Releases").clicked() {
                outcome.open_release_page = Some(RELEASES_PAGE_URL.to_string());
            }
        });

        ui.add_space(10.0);
        match &updater.status {
            UpdateStatus::Idle => {
                ui.label(
                    RichText::new("Click \"Check for Updates Now\" to query GitHub Releases.")
                        .size(12.0)
                        .color(theme.text_muted),
                );
            }
            UpdateStatus::Checking { .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Checking GitHub Releases...")
                            .size(12.5)
                            .color(theme.text_primary),
                    );
                });
            }
            UpdateStatus::UpToDate { checked_version } => {
                ui.label(
                    RichText::new(format!(
                        "You are up to date (v{APP_VERSION}, latest on GitHub: v{checked_version})."
                    ))
                    .size(12.5)
                    .color(theme.accent),
                );
            }
            UpdateStatus::Available(info)
            | UpdateStatus::Downloading { info, .. }
            | UpdateStatus::ReadyToInstall { info, .. } => {
                ui.label(
                    RichText::new(format!("Update v{} is available — What's Changed:", info.version))
                        .strong()
                        .size(12.5)
                        .color(theme.accent),
                );
                ui.add_space(4.0);
                render_release_notes_box(ui, &info.body, theme, 180.0);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let install_btn = egui::Button::new(
                        RichText::new("View & Install Update...")
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(theme.accent)
                    .stroke(Stroke::NONE)
                    .corner_radius(CornerRadius::same(6));
                    if ui.add(install_btn).clicked() {
                        outcome.open_update_modal = true;
                    }
                    let is_skipped = settings.skipped_update_version.as_deref().is_some_and(|s| {
                        crate::ui::updater::normalize_version_str(s)
                            == crate::ui::updater::normalize_version_str(&info.version)
                    });
                    if !is_skipped && ui.button("Skip This Version").clicked() {
                        settings.skipped_update_version = Some(info.version.clone());
                        outcome.settings_changed = true;
                        outcome.status_message = Some(format!("Skipped update v{}", info.version));
                    }
                });
            }
            UpdateStatus::Error { message, .. } => {
                ui.label(
                    RichText::new(format!("Update check error: {message}"))
                        .size(12.0)
                        .color(theme.warning_amber),
                );
            }
        }
    }
}
