use std::path::PathBuf;

use egui::{Color32, CornerRadius, Margin, RichText, Sense, Shape, Stroke, Vec2};

use crate::document::editing::{ActiveFormatState, LineBlockKind};
use crate::document::math::{
    MATH_ACCENT_SNIPPETS, MATH_GREEK_SYMBOLS, MATH_OPERATOR_SYMBOLS, MATH_RELATION_SYMBOLS,
    MATH_TEXTBOOK_TEMPLATES,
};
use crate::document::unicode::BlockAlignment;
use crate::document::DocumentBuffer;
use crate::ui::settings::{LineSpacing, PageWidth};
use crate::ui::theme::{ThemeMode, WordTheme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Wysiwyg,
    Split,
    Reader,
    Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RibbonTab {
    #[default]
    Home,
    Equations,
    Tables,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CalcToolbarInfo {
    pub auto_calc_math: bool,
    pub auto_calc_tables: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ReadingToolbarInfo {
    pub page_width: PageWidth,
    pub line_spacing: LineSpacing,
    pub focus_mode: bool,
    pub reading_progress: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolbarAction {
    None,
    NewFile,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    OpenFile,
    OpenRecentFile(PathBuf),
    SaveFile,
    SaveFileAs,
    RenameFile(String),
    ExportPdf,
    ExportDocx,
    ExportHtml,
    ExportJson,
    PrintDoc,
    Undo,
    Redo,
    UndoSteps(usize),
    RedoSteps(usize),
    ToggleBold,
    ToggleItalic,
    ToggleUnderline,
    ToggleStrike,
    ToggleInlineCode,
    SetHeading(Option<u8>),
    ToggleBlock(LineBlockKind),
    SetAlignment(BlockAlignment),
    Indent,
    Outdent,
    InsertCodeBlock,
    InsertHorizontalRule,
    InsertMath,
    InsertInlineMath,
    InsertMathSnippet(&'static str),
    InsertMathTemplate(&'static str),
    CalculateMath,
    ToggleAutoCalcMath,
    InsertMermaid,
    InsertImage,
    OpenLinkDialog,
    InsertTable { rows: usize, cols: usize },
    OpenTableEditor,
    InsertTableFormula(&'static str),
    CalculateTables,
    ToggleAutoCalcTables,
    ToggleShowMarkup,
    ToggleFindBar,
    FocusFindBar,
    OpenReplaceBar,
    ToggleOutline,
    ToggleExplorer,
    ToggleTheme,
    SetThemeMode(ThemeMode),
    SetPageWidth(PageWidth),
    SetLineSpacing(LineSpacing),
    ToggleFocusMode,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    SetViewMode(ViewMode),
    OpenOsIntegrationModal,
}

/// Persistent state for interactive toolbar popups and responsive layout measurement.
#[derive(Debug, Default)]
pub struct ToolbarState {
    pub active_tab: RibbonTab,
    pub hover_rows: usize,
    pub hover_cols: usize,
    pub tier1_needed_w: f32,
    pub renaming_file: bool,
    pub rename_draft: String,
    pub rename_focus_requested: bool,
}

/// Renders the two-tier Word-inspired header & formatting ribbon.
#[allow(clippy::too_many_arguments)]
pub fn render_toolbar(
    ui: &mut egui::Ui,
    buf: &DocumentBuffer,
    view_mode: ViewMode,
    explorer_open: bool,
    outline_open: bool,
    show_markup: bool,
    recent_files: &[PathBuf],
    zoom_factor: f32,
    reading: ReadingToolbarInfo,
    calc: CalcToolbarInfo,
    active_fmt: &ActiveFormatState,
    theme: &WordTheme,
    export_busy: bool,
    state: &mut ToolbarState,
) -> ToolbarAction {
    let mut action = ToolbarAction::None;
    let read_only = view_mode == ViewMode::Reader;

    // Tier 1: Document Title, File Actions, View Switcher, and Export Group
    egui::Frame::new()
        .fill(theme.titlebar_bg)
        .inner_margin(Margin::symmetric(8, 5))
        .stroke(Stroke::new(1.0_f32, theme.border_subtle))
        .show(ui, |ui| {
            ui.spacing_mut().button_padding = egui::vec2(4.5, 4.0);
            let avail_w = ui.available_width();
            ui.set_min_width(avail_w);
            let single_row_threshold = if state.tier1_needed_w > 0.0 {
                state.tier1_needed_w
            } else {
                1160.0
            };
            let single_row = avail_w >= single_row_threshold;
            let mut left_w = 0.0_f32;
            let mut right_w = 0.0_f32;

            if single_row {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.5, 4.0);
                    left_w = render_tier1_left_controls(
                        ui,
                        buf,
                        read_only,
                        recent_files,
                        theme,
                        state,
                        &mut action,
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(2.5, 4.0);
                        right_w += render_export_buttons(ui, export_busy, theme, true, &mut action);
                        right_w += ui.separator().rect.width() + 2.5;
                        right_w += render_workspace_toggles(
                            ui,
                            explorer_open,
                            outline_open,
                            theme,
                            true,
                            &mut action,
                        );
                        right_w += ui.separator().rect.width() + 2.5;
                        right_w += render_view_mode_switcher(ui, view_mode, theme, true, &mut action)
                            .rect
                            .width()
                            + 2.5;
                    });
                });
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.5, 4.0);
                    left_w = render_tier1_left_controls(
                        ui,
                        buf,
                        read_only,
                        recent_files,
                        theme,
                        state,
                        &mut action,
                    );
                    ui.separator();
                    right_w += render_view_mode_switcher(ui, view_mode, theme, false, &mut action)
                        .rect
                        .width()
                        + 2.5;
                    right_w += ui.separator().rect.width() + 2.5;
                    right_w += render_workspace_toggles(
                        ui,
                        explorer_open,
                        outline_open,
                        theme,
                        false,
                        &mut action,
                    );
                    right_w += ui.separator().rect.width() + 2.5;
                    right_w += render_export_buttons(ui, export_busy, theme, false, &mut action);
                });
            }

            let measured_needed = left_w + right_w + 10.0;
            if (state.tier1_needed_w - measured_needed).abs() > 1.0 {
                let prev_single = state.tier1_needed_w > 0.0 && avail_w >= state.tier1_needed_w;
                let next_single = avail_w >= measured_needed;
                state.tier1_needed_w = measured_needed;
                if prev_single != next_single {
                    ui.ctx().request_repaint();
                }
            }
        });

    // Tier 2: Reading Comfort Bar (Reader mode) or Word-Style Formatting Ribbon
    let show_right_badge = read_only || view_mode == ViewMode::Wysiwyg;
    egui::Frame::new()
        .fill(theme.ribbon_bg)
        .inner_margin(Margin::symmetric(8, 4))
        .stroke(Stroke::new(1.0_f32, theme.border))
        .show(ui, |ui| {
            ui.spacing_mut().button_padding = egui::vec2(5.5, 4.0);
            let full_w = ui.available_width();
            ui.set_min_width(full_w);
            if read_only {
                if full_w >= 780.0 {
                    let left_w = (full_w - 280.0).max(200.0);
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_max_width(left_w);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(4.0, 3.0);
                                render_reader_comfort_left(ui, reading, theme, &mut action);
                            });
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            render_reader_comfort_right(
                                ui,
                                zoom_factor,
                                reading,
                                theme,
                                true,
                                &mut action,
                            );
                        });
                    });
                } else {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(4.0, 3.0);
                        render_reader_comfort_left(ui, reading, theme, &mut action);
                        ui.separator();
                        render_reader_comfort_right(
                            ui,
                            zoom_factor,
                            reading,
                            theme,
                            false,
                            &mut action,
                        );
                    });
                }
            } else if full_w >= if show_right_badge { 830.0 } else { 780.0 } {
                let reserved_right = if show_right_badge { 196.0 } else { 102.0 };
                let left_w = (full_w - reserved_right).max(200.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_max_width(left_w);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(2.5, 2.5);
                            render_formatting_ribbon_left(
                                ui,
                                active_fmt,
                                calc,
                                theme,
                                state,
                                &mut action,
                            );
                        });
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        render_formatting_ribbon_right(
                            ui,
                            view_mode,
                            show_markup,
                            zoom_factor,
                            theme,
                            true,
                            &mut action,
                        );
                    });
                });
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.5, 2.5);
                    render_formatting_ribbon_left(ui, active_fmt, calc, theme, state, &mut action);
                    ui.separator();
                    render_formatting_ribbon_right(
                        ui,
                        view_mode,
                        show_markup,
                        zoom_factor,
                        theme,
                        false,
                        &mut action,
                    );
                });
            }
        });

    action
}

fn rename_stem_char_len(name: &str) -> usize {
    match name.rfind('.') {
        Some(dot_idx) if dot_idx > 0 => name[..dot_idx].chars().count(),
        _ => name.chars().count(),
    }
}

fn render_tier1_left_controls(
    ui: &mut egui::Ui,
    buf: &DocumentBuffer,
    read_only: bool,
    recent_files: &[PathBuf],
    theme: &WordTheme,
    state: &mut ToolbarState,
    action: &mut ToolbarAction,
) -> f32 {
    let gap = ui.spacing().item_spacing.x;
    let mut sum_w = 0.0_f32;
    let mut track = |resp: &egui::Response| {
        sum_w += resp.rect.width() + gap;
    };

    // App mark badge
    let logo_resp = egui::Frame::new()
        .fill(theme.accent)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.label(
                RichText::new("M↓")
                    .strong()
                    .size(12.5)
                    .color(Color32::WHITE),
            );
        })
        .response
        .on_hover_text(format!("Markdown Studio v{}", crate::APP_VERSION));
    track(&logo_resp);

    let full_doc_name = buf
        .file_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Untitled.md".to_string());
    let display_name = if full_doc_name.chars().count() > 24 {
        let mut s: String = full_doc_name.chars().take(21).collect();
        s.push('…');
        s
    } else {
        full_doc_name.clone()
    };
    let name_tip = buf
        .file_path
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| full_doc_name.clone());
    if read_only {
        state.renaming_file = false;
        let name_resp = inline_label(
            ui,
            RichText::new(display_name)
                .strong()
                .size(13.0)
                .color(theme.text_primary),
        )
        .on_hover_text(name_tip);
        track(&name_resp);
    } else if state.renaming_file {
        let escape_pressed = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
        let edit_resp = ui.add(
            egui::TextEdit::singleline(&mut state.rename_draft)
                .id(egui::Id::new("toolbar_rename_input"))
                .desired_width(148.0)
                .font(egui::FontId::proportional(13.0))
                .margin(Margin::symmetric(6, 2)),
        );
        let just_requested = state.rename_focus_requested;
        if just_requested {
            edit_resp.request_focus();
            if let Some(mut text_state) = egui::TextEdit::load_state(ui.ctx(), edit_resp.id) {
                let stem_chars = rename_stem_char_len(&state.rename_draft);
                text_state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(stem_chars),
                    )));
                text_state.store(ui.ctx(), edit_resp.id);
            }
            state.rename_focus_requested = false;
        }
        if escape_pressed {
            state.renaming_file = false;
        } else if !just_requested && edit_resp.lost_focus() {
            let trimmed = state.rename_draft.trim().to_string();
            if !trimmed.is_empty() && trimmed != full_doc_name {
                *action = ToolbarAction::RenameFile(trimmed);
            }
            state.renaming_file = false;
        }
        track(&edit_resp);
    } else {
        let bg_slot = ui.painter().add(Shape::Noop);
        let name_resp = ui
            .add(
                egui::Label::new(
                    RichText::new(display_name)
                        .strong()
                        .size(13.0)
                        .color(theme.text_primary),
                )
                .sense(Sense::click())
                .wrap_mode(egui::TextWrapMode::Extend),
            )
            .on_hover_text(format!("{name_tip}\nClick to rename"));
        if name_resp.hovered() || name_resp.is_pointer_button_down_on() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            ui.painter().set(
                bg_slot,
                Shape::rect_filled(
                    name_resp.rect.expand2(Vec2::new(4.0, 2.0)),
                    CornerRadius::same(4),
                    theme.accent_soft,
                ),
            );
        }
        if name_resp.clicked() {
            state.renaming_file = true;
            state.rename_draft = full_doc_name;
            state.rename_focus_requested = true;
            ui.ctx().request_repaint();
        }
        track(&name_resp);
    }

    let (badge_text, badge_color) = if buf.dirty {
        ("● Unsaved", theme.warning_amber)
    } else {
        ("✓ Saved", theme.success_green)
    };
    let badge_resp = egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            badge_color.r(),
            badge_color.g(),
            badge_color.b(),
            28,
        ))
        .stroke(Stroke::new(
            1.0_f32,
            Color32::from_rgba_unmultiplied(
                badge_color.r(),
                badge_color.g(),
                badge_color.b(),
                90,
            ),
        ))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            inline_label(
                ui,
                RichText::new(badge_text)
                    .size(11.0)
                    .strong()
                    .color(badge_color),
            );
        })
        .response;
    track(&badge_resp);

    track(&ui.separator());

    let new_r = vivid_button(
        ui,
        RichText::new("New").size(12.0),
        Vec2::ZERO,
        6,
        false,
        !read_only,
        ToolbarButtonKind::Subtle,
        "New Document (Ctrl+N)",
        theme,
    );
    track(&new_r);
    if new_r.clicked() {
        *action = ToolbarAction::NewFile;
    }
    let open_r = ribbon_button(ui, "Open", "Open File (Ctrl+O)", theme);
    track(&open_r);
    if open_r.clicked() {
        *action = ToolbarAction::OpenFile;
    }

    let recent_popup_id = ui.make_persistent_id("recent_files_popup");
    let recent_btn = ribbon_button(ui, "Recent ▾", "Open Recent File", theme);
    track(&recent_btn);
    if recent_btn.clicked() {
        ui.memory_mut(|m| m.toggle_popup(recent_popup_id));
    }
    egui::popup_below_widget(
        ui,
        recent_popup_id,
        &recent_btn,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(220.0);
            if recent_files.is_empty() {
                ui.label(RichText::new("(No recent files)").weak().size(12.0));
            } else {
                for path in recent_files {
                    let label = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string());
                    let tip = path.to_string_lossy().to_string();
                    if ui.button(label).on_hover_text(tip).clicked() {
                        *action = ToolbarAction::OpenRecentFile(path.clone());
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
            }
        },
    );

    let save_r = ribbon_button(ui, "Save", "Save Document (Ctrl+S)", theme);
    track(&save_r);
    if save_r.clicked() {
        *action = ToolbarAction::SaveFile;
    }
    let save_as_r = ribbon_button(ui, "Save As", "Save As (Ctrl+Shift+S)", theme);
    track(&save_as_r);
    if save_as_r.clicked() {
        *action = ToolbarAction::SaveFileAs;
    }

    track(&ui.separator());

    let undo_depth = buf.undo_depth();
    let (uw, undo_act) = render_history_split_button(
        ui,
        "Undo",
        !read_only,
        "Undo (Ctrl+Z)",
        &format!("Undo History ({undo_depth} steps)"),
        "undo_history_popup",
        "(No undo history)",
        undo_depth,
        || buf.undo_history_labels(100),
        theme,
        ToolbarAction::Undo,
        ToolbarAction::UndoSteps,
    );
    sum_w += uw + gap;
    if let Some(a) = undo_act {
        *action = a;
    }

    let redo_depth = buf.redo_depth();
    let (rw, redo_act) = render_history_split_button(
        ui,
        "Redo",
        !read_only,
        "Redo (Ctrl+Y)",
        &format!("Redo History ({redo_depth} steps)"),
        "redo_history_popup",
        "(No redo history)",
        redo_depth,
        || buf.redo_history_labels(100),
        theme,
        ToolbarAction::Redo,
        ToolbarAction::RedoSteps,
    );
    sum_w += rw + gap;
    if let Some(a) = redo_act {
        *action = a;
    }

    sum_w
}

#[allow(clippy::too_many_arguments)]
fn render_history_split_button(
    ui: &mut egui::Ui,
    label: &str,
    editable: bool,
    tooltip: &str,
    dropdown_tooltip: &str,
    popup_id_str: &str,
    empty_label: &str,
    total_depth: usize,
    load_labels: impl FnOnce() -> Vec<String>,
    theme: &WordTheme,
    single_action: ToolbarAction,
    multi_action: impl Fn(usize) -> ToolbarAction,
) -> (f32, Option<ToolbarAction>) {
    let mut chosen = None;
    let enabled_btn = editable && ui.is_enabled() && total_depth > 0;
    let enabled_arrow = editable && ui.is_enabled();

    let main_r = vivid_button(
        ui,
        RichText::new(label).size(12.0),
        Vec2::ZERO,
        6,
        false,
        enabled_btn,
        ToolbarButtonKind::Subtle,
        tooltip,
        theme,
    );
    if main_r.clicked() {
        chosen = Some(single_action);
    }

    ui.add_space(-3.0);

    let popup_id = ui.make_persistent_id(popup_id_str);
    let popup_open = ui.memory(|m| m.is_popup_open(popup_id));
    let prev_pad = ui.spacing().button_padding;
    ui.spacing_mut().button_padding = egui::vec2(3.0, 4.0);
    let arrow_r = vivid_button(
        ui,
        RichText::new("▾").size(11.0),
        Vec2::new(14.0, main_r.rect.height()),
        5,
        popup_open,
        enabled_arrow,
        ToolbarButtonKind::Subtle,
        dropdown_tooltip,
        theme,
    );
    ui.spacing_mut().button_padding = prev_pad;
    if arrow_r.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }

    egui::popup_below_widget(
        ui,
        popup_id,
        &arrow_r,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(240.0);
            ui.set_max_width(320.0);
            ui.label(
                RichText::new(format!("{label} History ({total_depth})"))
                    .strong()
                    .size(12.0)
                    .color(theme.accent),
            );
            ui.separator();
            if total_depth == 0 {
                ui.label(RichText::new(empty_label).weak().size(12.0));
            } else {
                let items = load_labels();
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for (idx, desc) in items.iter().enumerate() {
                            let steps = idx + 1;
                            let row_lbl = format!("{steps}. {desc}");
                            let row_btn = ui
                                .scope(|ui| {
                                    ui.visuals_mut().widgets.inactive.bg_fill =
                                        Color32::TRANSPARENT;
                                    ui.visuals_mut().widgets.inactive.weak_bg_fill =
                                        Color32::TRANSPARENT;
                                    ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::NONE;
                                    ui.add_sized(
                                        [ui.available_width(), 22.0],
                                        egui::Button::new(
                                            RichText::new(row_lbl)
                                                .size(12.0)
                                                .color(theme.text_primary),
                                        ),
                                    )
                                })
                                .inner
                                .on_hover_text(format!(
                                    "{label} {steps} step{}",
                                    if steps == 1 { "" } else { "s" }
                                ));
                            if row_btn.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if row_btn.clicked() {
                                chosen = Some(multi_action(steps));
                                ui.memory_mut(|m| m.close_popup());
                            }
                        }
                        if total_depth > items.len() {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(format!(
                                    "... and {} older step(s)",
                                    total_depth - items.len()
                                ))
                                .weak()
                                .size(11.0),
                            );
                        }
                    });
            }
        },
    );

    let total_w = main_r.rect.width() + arrow_r.rect.width();
    (total_w, chosen)
}

fn render_view_mode_switcher(
    ui: &mut egui::Ui,
    view_mode: ViewMode,
    theme: &WordTheme,
    rtl: bool,
    action: &mut ToolbarAction,
) -> egui::Response {
    let modes = [
        (ViewMode::Wysiwyg, "WYSIWYG", "Word-Style Inline Editor (Ctrl+1)"),
        (ViewMode::Split, "Split", "Synchronized Split Source & Preview (Ctrl+2)"),
        (ViewMode::Reader, "Reader", "Read-Only Editorial Reader (Ctrl+3)"),
        (ViewMode::Source, "Source", "Raw Markdown Source Editor (Ctrl+4)"),
    ];
    egui::Frame::new()
        .fill(theme.pill_bg)
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::same(3))
        .stroke(Stroke::new(1.0_f32, theme.border_subtle))
        .show(ui, |ui| {
            if rtl {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                    for (mode, label, tip) in modes.into_iter().rev() {
                        if view_mode_pill_button(ui, label, view_mode == mode, tip, theme)
                            .clicked()
                        {
                            *action = ToolbarAction::SetViewMode(mode);
                        }
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                    for (mode, label, tip) in modes {
                        if view_mode_pill_button(ui, label, view_mode == mode, tip, theme)
                            .clicked()
                        {
                            *action = ToolbarAction::SetViewMode(mode);
                        }
                    }
                });
            }
        })
        .response
}

fn render_workspace_toggles(
    ui: &mut egui::Ui,
    explorer_open: bool,
    outline_open: bool,
    theme: &WordTheme,
    rtl: bool,
    action: &mut ToolbarAction,
) -> f32 {
    let gap = ui.spacing().item_spacing.x;
    let mut sum_w = 0.0_f32;
    let order = if rtl { [4, 3, 2, 1, 0] } else { [0, 1, 2, 3, 4] };
    for idx in order {
        let resp = match idx {
            0 => {
                let r = ribbon_button(ui, "Find", "Find & Replace (Ctrl+F)", theme);
                if r.clicked() {
                    *action = ToolbarAction::ToggleFindBar;
                }
                r
            }
            1 => {
                let r = toggle_pill_button(
                    ui,
                    "Explorer",
                    explorer_open,
                    "Toggle File Explorer Sidebar",
                    theme,
                );
                if r.clicked() {
                    *action = ToolbarAction::ToggleExplorer;
                }
                r
            }
            2 => {
                let r = toggle_pill_button(
                    ui,
                    "Outline",
                    outline_open,
                    "Toggle Document Outline Sidebar",
                    theme,
                );
                if r.clicked() {
                    *action = ToolbarAction::ToggleOutline;
                }
                r
            }
            3 => {
                let theme_label = match theme.mode {
                    ThemeMode::Light => "Sepia Mode",
                    ThemeMode::Sepia => "Dark Mode",
                    ThemeMode::Dark => "Light Mode",
                };
                let r = ribbon_button(
                    ui,
                    theme_label,
                    "Switch Theme (Light → Sepia → Dark)",
                    theme,
                );
                if r.clicked() {
                    *action = ToolbarAction::ToggleTheme;
                }
                r
            }
            _ => {
                let r = ribbon_button(
                    ui,
                    "OS Menu",
                    "Windows Right-Click Context Menu & .exe Path Setup",
                    theme,
                );
                if r.clicked() {
                    *action = ToolbarAction::OpenOsIntegrationModal;
                }
                r
            }
        };
        sum_w += resp.rect.width() + gap;
    }
    sum_w
}

fn render_export_buttons(
    ui: &mut egui::Ui,
    export_busy: bool,
    theme: &WordTheme,
    rtl: bool,
    action: &mut ToolbarAction,
) -> f32 {
    let gap = ui.spacing().item_spacing.x;
    let mut sum_w = 0.0_f32;
    let order = if rtl { [3, 2, 1, 0] } else { [0, 1, 2, 3] };
    for idx in order {
        let resp = match idx {
            0 => {
                let r = export_accent_soft_button(
                    ui,
                    "PDF",
                    !export_busy,
                    "Export paginated vector PDF",
                    theme,
                );
                if r.clicked() {
                    *action = ToolbarAction::ExportPdf;
                }
                r
            }
            1 => {
                let r = export_primary_button(
                    ui,
                    "Word (.docx)",
                    !export_busy,
                    "Export native OpenXML Word document",
                    theme,
                );
                if r.clicked() {
                    *action = ToolbarAction::ExportDocx;
                }
                r
            }
            2 => {
                let popup_id = ui.make_persistent_id("export_more_formats_popup");
                let r = export_accent_soft_button(
                    ui,
                    "Export ▾",
                    !export_busy,
                    "Export to HTML, JSON, PDF, or Word",
                    theme,
                );
                if r.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(popup_id));
                }
                egui::popup_below_widget(
                    ui,
                    popup_id,
                    &r,
                    egui::PopupCloseBehavior::CloseOnClickOutside,
                    |ui| {
                        ui.set_min_width(175.0);
                        ui.label(
                            RichText::new("Export Format")
                                .strong()
                                .size(11.5)
                                .color(theme.accent),
                        );
                        ui.separator();
                        for (label, act) in [
                            ("Export HTML (.html)", ToolbarAction::ExportHtml),
                            ("Export JSON (.json)", ToolbarAction::ExportJson),
                            ("Export PDF (.pdf)", ToolbarAction::ExportPdf),
                            ("Export Word (.docx)", ToolbarAction::ExportDocx),
                        ] {
                            let btn = ui.add_sized(
                                [ui.available_width(), 22.0],
                                egui::Button::new(
                                    RichText::new(label).size(12.0).color(theme.text_primary),
                                ),
                            );
                            if btn.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if btn.clicked() {
                                *action = act;
                                ui.memory_mut(|m| m.close_popup());
                            }
                        }
                    },
                );
                r
            }
            _ => {
                let r = export_print_button(
                    ui,
                    "Print",
                    !export_busy,
                    "Print via native OS spooler (Ctrl+P)",
                    theme,
                );
                if r.clicked() {
                    *action = ToolbarAction::PrintDoc;
                }
                r
            }
        };
        sum_w += resp.rect.width() + gap;
    }
    sum_w
}

fn render_reader_comfort_left(
    ui: &mut egui::Ui,
    reading: ReadingToolbarInfo,
    theme: &WordTheme,
    action: &mut ToolbarAction,
) {
    inline_label(
        ui,
        RichText::new("Width:")
            .size(11.5)
            .strong()
            .color(theme.text_muted),
    );
    for (pw, lbl, tip) in [
        (PageWidth::Narrow, "Narrow", "Narrow editorial column (680 px)"),
        (PageWidth::Standard, "Standard", "Standard reading page (820 px)"),
        (PageWidth::Wide, "Wide", "Wide page view (1040 px)"),
    ] {
        if toggle_pill_button(ui, lbl, reading.page_width == pw, tip, theme).clicked() {
            *action = ToolbarAction::SetPageWidth(pw);
        }
    }

    ui.separator();

    inline_label(
        ui,
        RichText::new("Spacing:")
            .size(11.5)
            .strong()
            .color(theme.text_muted),
    );
    for (ls, lbl, tip) in [
        (LineSpacing::Compact, "Compact", "Compact 1.28x line spacing"),
        (
            LineSpacing::Comfortable,
            "Comfortable",
            "Comfortable 1.52x editorial spacing",
        ),
        (LineSpacing::Relaxed, "Relaxed", "Relaxed 1.74x airy spacing"),
    ] {
        if toggle_pill_button(ui, lbl, reading.line_spacing == ls, tip, theme).clicked() {
            *action = ToolbarAction::SetLineSpacing(ls);
        }
    }

    ui.separator();

    inline_label(
        ui,
        RichText::new("Paper:")
            .size(11.5)
            .strong()
            .color(theme.text_muted),
    );
    for (tm, lbl, tip) in [
        (ThemeMode::Light, "Light", "Classic Paper light palette"),
        (ThemeMode::Sepia, "Sepia", "Warm Sepia eye-comfort reading palette"),
        (ThemeMode::Dark, "Dark", "Midnight Studio dark palette"),
    ] {
        if toggle_pill_button(ui, lbl, theme.mode == tm, tip, theme).clicked() {
            *action = ToolbarAction::SetThemeMode(tm);
        }
    }

    ui.separator();

    if toggle_pill_button(
        ui,
        "Focus (F11)",
        reading.focus_mode,
        "Distraction-free full-page reading mode (F11)",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::ToggleFocusMode;
    }
}

fn render_zoom_controls(
    ui: &mut egui::Ui,
    zoom_factor: f32,
    theme: &WordTheme,
    rtl: bool,
    action: &mut ToolbarAction,
) {
    let zoom_pct = format!("{}%", (zoom_factor * 100.0).round() as usize);
    let order = if rtl { [2, 1, 0] } else { [0, 1, 2] };
    for idx in order {
        match idx {
            0 => {
                if ribbon_button(ui, "-", "Zoom Out (Ctrl+- or Ctrl+Scroll Down)", theme).clicked()
                {
                    *action = ToolbarAction::ZoomOut;
                }
            }
            1 => {
                if ribbon_button(ui, &zoom_pct, "Reset Zoom (Ctrl+0)", theme).clicked() {
                    *action = ToolbarAction::ZoomReset;
                }
            }
            _ => {
                if ribbon_button(ui, "+", "Zoom In (Ctrl+= or Ctrl+Scroll Up)", theme).clicked() {
                    *action = ToolbarAction::ZoomIn;
                }
            }
        }
    }
}

fn render_reader_comfort_right(
    ui: &mut egui::Ui,
    zoom_factor: f32,
    reading: ReadingToolbarInfo,
    theme: &WordTheme,
    rtl: bool,
    action: &mut ToolbarAction,
) {
    let draw_badge = |ui: &mut egui::Ui| {
        egui::Frame::new()
            .fill(theme.accent_soft)
            .stroke(Stroke::new(1.0_f32, theme.accent))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(9, 2))
            .show(ui, |ui| {
                inline_label(
                    ui,
                    RichText::new("Read-Only Mode")
                        .size(11.5)
                        .strong()
                        .color(theme.accent),
                );
            });
    };
    let draw_progress = |ui: &mut egui::Ui| {
        if let Some(p) = reading.reading_progress {
            let pct = (p * 100.0).round().clamp(0.0, 100.0) as usize;
            inline_label(
                ui,
                RichText::new(format!("{pct}%"))
                    .size(11.5)
                    .strong()
                    .color(theme.text_muted),
            );
        }
    };

    if rtl {
        render_zoom_controls(ui, zoom_factor, theme, true, action);
        draw_progress(ui);
        draw_badge(ui);
    } else {
        draw_badge(ui);
        draw_progress(ui);
        render_zoom_controls(ui, zoom_factor, theme, false, action);
    }
}

fn render_formatting_ribbon_right(
    ui: &mut egui::Ui,
    view_mode: ViewMode,
    show_markup: bool,
    zoom_factor: f32,
    theme: &WordTheme,
    rtl: bool,
    action: &mut ToolbarAction,
) {
    let draw_markup = |ui: &mut egui::Ui, action: &mut ToolbarAction| {
        if view_mode == ViewMode::Wysiwyg
            && toggle_pill_button(
                ui,
                "Show Markup",
                show_markup,
                "Show raw Markdown syntax in WYSIWYG mode",
                theme,
            )
            .clicked()
        {
            *action = ToolbarAction::ToggleShowMarkup;
        }
    };

    if rtl {
        render_zoom_controls(ui, zoom_factor, theme, true, action);
        draw_markup(ui, action);
    } else {
        draw_markup(ui, action);
        render_zoom_controls(ui, zoom_factor, theme, false, action);
    }
}

fn render_ribbon_tab_switcher(
    ui: &mut egui::Ui,
    theme: &WordTheme,
    state: &mut ToolbarState,
) {
    let tabs = [
        (RibbonTab::Home, "✎ Text", "Text, formatting, lists & document blocks"),
        (
            RibbonTab::Equations,
            "∫ Math",
            "Math symbols, textbook formulas & math calculator",
        ),
        (
            RibbonTab::Tables,
            "⊞ Tables",
            "Tables & Excel-like formulas and recalculation",
        ),
    ];
    egui::Frame::new()
        .fill(theme.pill_bg)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(2))
        .stroke(Stroke::new(1.0_f32, theme.border_subtle))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                for (tab, label, tip) in tabs {
                    if view_mode_pill_button(ui, label, state.active_tab == tab, tip, theme)
                        .clicked()
                    {
                        state.active_tab = tab;
                    }
                }
            });
        });
}

fn render_formatting_ribbon_left(
    ui: &mut egui::Ui,
    active_fmt: &ActiveFormatState,
    calc: CalcToolbarInfo,
    theme: &WordTheme,
    state: &mut ToolbarState,
    action: &mut ToolbarAction,
) {
    render_ribbon_tab_switcher(ui, theme, state);
    ui.separator();

    match state.active_tab {
        RibbonTab::Home => render_home_ribbon_controls(ui, active_fmt, theme, action),
        RibbonTab::Equations => render_equations_ribbon_controls(ui, calc, theme, action),
        RibbonTab::Tables => render_tables_ribbon_controls(ui, calc, theme, state, action),
    }
}

fn render_home_ribbon_controls(
    ui: &mut egui::Ui,
    active_fmt: &ActiveFormatState,
    theme: &WordTheme,
    action: &mut ToolbarAction,
) {
    // Heading selector H1-H6 & active format snapshot
    let fmt = active_fmt;
    let cur_h = fmt.heading_level;
    let heading_label = match cur_h {
        Some(lvl) => format!("Heading {lvl}"),
        None => "Normal Text".to_string(),
    };
    egui::ComboBox::from_id_salt("heading_selector")
        .selected_text(
            RichText::new(heading_label)
                .size(12.0)
                .strong()
                .color(theme.text_primary),
        )
        .width(102.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(
                    cur_h.is_none(),
                    RichText::new("Normal Paragraph").color(theme.text_primary),
                )
                .clicked()
            {
                *action = ToolbarAction::SetHeading(None);
            }
            ui.separator();
            for lvl in 1..=6u8 {
                let label = format!("Heading {lvl}");
                let size = match lvl {
                    1 => 18.0,
                    2 => 16.0,
                    3 => 14.5,
                    4 => 13.5,
                    _ => 12.5,
                };
                if ui
                    .selectable_label(
                        cur_h == Some(lvl),
                        RichText::new(label)
                            .strong()
                            .size(size)
                            .color(theme.text_primary),
                    )
                    .clicked()
                {
                    *action = ToolbarAction::SetHeading(Some(lvl));
                }
            }
        });

    ui.separator();

    // Inline formatting cluster (with live active toggle highlights)
    egui::Frame::new()
        .fill(theme.pill_bg)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                if format_chip_active(
                    ui,
                    RichText::new("B").strong().size(13.0),
                    fmt.bold,
                    "Bold (Ctrl+B)",
                    theme,
                )
                .clicked()
                {
                    *action = ToolbarAction::ToggleBold;
                }
                if format_chip_active(
                    ui,
                    RichText::new("I").italics().size(13.0),
                    fmt.italic,
                    "Italic (Ctrl+I)",
                    theme,
                )
                .clicked()
                {
                    *action = ToolbarAction::ToggleItalic;
                }
                if format_chip_active(
                    ui,
                    RichText::new("U").underline().size(13.0),
                    fmt.underline,
                    "Underline (Ctrl+U)",
                    theme,
                )
                .clicked()
                {
                    *action = ToolbarAction::ToggleUnderline;
                }
                if format_chip_active(
                    ui,
                    RichText::new("S").strikethrough().size(13.0),
                    fmt.strikethrough,
                    "Strikethrough",
                    theme,
                )
                .clicked()
                {
                    *action = ToolbarAction::ToggleStrike;
                }
                if format_chip_active(
                    ui,
                    RichText::new("</>").monospace().size(12.0),
                    fmt.inline_code,
                    "Inline Code",
                    theme,
                )
                .clicked()
                {
                    *action = ToolbarAction::ToggleInlineCode;
                }
            });
        });

    ui.separator();

    // Block Alignment & Logical Start/End cluster
    egui::Frame::new()
        .fill(theme.pill_bg)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                for (align, label, tip) in [
                    (BlockAlignment::Left, "L", "Align Left"),
                    (BlockAlignment::Center, "C", "Align Center"),
                    (BlockAlignment::Right, "R", "Align Right"),
                    (BlockAlignment::Justify, "J", "Justify"),
                    (BlockAlignment::Start, "Start", "Logical Start (LTR Left / RTL Right)"),
                    (BlockAlignment::End, "End", "Logical End (LTR Right / RTL Left)"),
                ] {
                    if format_chip_active(
                        ui,
                        RichText::new(label).size(11.5).strong(),
                        fmt.alignment == align,
                        tip,
                        theme,
                    )
                    .clicked()
                    {
                        *action = ToolbarAction::SetAlignment(align);
                    }
                }
            });
        });

    if ribbon_button(ui, "Outdent", "Decrease Logical Indentation", theme).clicked() {
        *action = ToolbarAction::Outdent;
    }
    if toggle_pill_button(
        ui,
        "Indent",
        fmt.indent_level > 0,
        "Increase Logical Indentation",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::Indent;
    }

    ui.separator();

    // Lists & Block Elements cluster
    if toggle_pill_button(
        ui,
        "• Bullet",
        fmt.block_kind == Some(LineBlockKind::BulletList),
        "Toggle Bulleted List",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::ToggleBlock(LineBlockKind::BulletList);
    }
    if toggle_pill_button(
        ui,
        "1. List",
        fmt.block_kind == Some(LineBlockKind::NumberedList),
        "Toggle Numbered List",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::ToggleBlock(LineBlockKind::NumberedList);
    }
    if toggle_pill_button(
        ui,
        "[x] Task",
        fmt.block_kind == Some(LineBlockKind::TaskList),
        "Toggle Task Checklist",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::ToggleBlock(LineBlockKind::TaskList);
    }
    if toggle_pill_button(
        ui,
        "> Quote",
        fmt.block_kind == Some(LineBlockKind::BlockQuote),
        "Toggle Blockquote",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::ToggleBlock(LineBlockKind::BlockQuote);
    }
    if ribbon_button(ui, "{ } Code", "Insert Fenced Code Block", theme).clicked() {
        *action = ToolbarAction::InsertCodeBlock;
    }
    if ribbon_button(
        ui,
        "― HR",
        "Insert Horizontal Rule (---)",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::InsertHorizontalRule;
    }
    if ribbon_button(
        ui,
        "∑ Math",
        "Insert LaTeX Math ($...$ inline or $$...$$ block)",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::InsertMath;
    }
    if ribbon_button(
        ui,
        "◇ Mermaid",
        "Insert Mermaid Diagram (```mermaid)",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::InsertMermaid;
    }
    if ribbon_button(ui, "Image", "Insert Image (![alt](path))", theme).clicked() {
        *action = ToolbarAction::InsertImage;
    }
    if ribbon_button(ui, "Link", "Insert Hyperlink (Ctrl+K)", theme).clicked() {
        *action = ToolbarAction::OpenLinkDialog;
    }
}

#[allow(clippy::too_many_arguments)]
fn render_math_symbol_palette_popup(
    ui: &mut egui::Ui,
    button_label: &str,
    tooltip: &str,
    popup_salt: &str,
    items: &'static [(&'static str, &'static str)],
    cols: usize,
    theme: &WordTheme,
    action: &mut ToolbarAction,
) {
    let popup_id = ui.make_persistent_id(popup_salt);
    let open = ui.memory(|m| m.is_popup_open(popup_id));
    let btn = toggle_pill_button(ui, button_label, open, tooltip, theme);
    if btn.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }
    egui::popup_below_widget(
        ui,
        popup_id,
        &btn,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(if cols > 1 { 220.0 } else { 190.0 });
            egui::Grid::new(ui.id().with(popup_salt))
                .spacing([4.0, 4.0])
                .show(ui, |ui| {
                    for (idx, &(glyph, snippet)) in items.iter().enumerate() {
                        let cell_btn = egui::Button::new(
                            RichText::new(glyph).size(13.0).color(theme.text_primary),
                        )
                        .min_size(if cols > 1 {
                            Vec2::new(30.0, 26.0)
                        } else {
                            Vec2::new(176.0, 24.0)
                        });
                        if ui.add(cell_btn).on_hover_text(snippet).clicked() {
                            *action = ToolbarAction::InsertMathSnippet(snippet);
                            ui.memory_mut(|m| m.close_popup());
                        }
                        if (idx + 1) % cols == 0 {
                            ui.end_row();
                        }
                    }
                });
        },
    );
}

fn render_equations_ribbon_controls(
    ui: &mut egui::Ui,
    calc: CalcToolbarInfo,
    theme: &WordTheme,
    action: &mut ToolbarAction,
) {
    if ribbon_button(ui, "Inline $x$", "Wrap selection or insert inline LaTeX math ($...$)", theme)
        .clicked()
    {
        *action = ToolbarAction::InsertInlineMath;
    }
    if ribbon_button(ui, "Block $$", "Insert display equation block ($$ ... $$)", theme).clicked() {
        *action = ToolbarAction::InsertMath;
    }

    ui.separator();

    for (label, snippet, tip) in [
        ("a/b", r"\frac{a}{b}", "Insert Fraction \\frac{a}{b}"),
        ("√x", r"\sqrt{x}", "Insert Square Root \\sqrt{x}"),
        ("x²", "^{2}", "Insert Superscript / Power"),
        ("xᵢ", "_{i}", "Insert Subscript"),
        ("∑", r"\sum_{i=1}^{n}", "Insert Summation \\sum_{i=1}^{n}"),
        ("∫", r"\int_{a}^{b}", "Insert Definite Integral \\int_{a}^{b}"),
        ("lim", r"\lim_{x \to 0}", "Insert Limit \\lim_{x \\to 0}"),
        (
            "[Matrix]",
            "\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}",
            "Insert 2×2 Matrix",
        ),
        ("Box", r"\boxed{x}", "Insert Boxed Equation \\boxed{x}"),
    ] {
        if ribbon_button(ui, label, tip, theme).clicked() {
            *action = ToolbarAction::InsertMathSnippet(snippet);
        }
    }

    ui.separator();

    render_math_symbol_palette_popup(
        ui,
        "Greek ▾",
        "Greek letters (α, β, γ, Δ, Σ, Ω...)",
        "math_greek_popup",
        MATH_GREEK_SYMBOLS,
        7,
        theme,
        action,
    );
    render_math_symbol_palette_popup(
        ui,
        "Operators ▾",
        "Mathematical & calculus operators (±, ×, ∑, ∫, ∂, ∇...)",
        "math_operators_popup",
        MATH_OPERATOR_SYMBOLS,
        6,
        theme,
        action,
    );
    render_math_symbol_palette_popup(
        ui,
        "Relations ▾",
        "Relations, logic & set symbols (≤, ≥, ≠, ≈, ∈, ∀, ⇒...)",
        "math_relations_popup",
        MATH_RELATION_SYMBOLS,
        7,
        theme,
        action,
    );
    render_math_symbol_palette_popup(
        ui,
        "Accents ▾",
        "Accents, vectors & blackboard sets (x̂, x⃗, ℝ, ℕ, ℂ...)",
        "math_accents_popup",
        MATH_ACCENT_SNIPPETS,
        2,
        theme,
        action,
    );

    let tpl_popup_id = ui.make_persistent_id("math_textbook_tpl_popup");
    let tpl_open = ui.memory(|m| m.is_popup_open(tpl_popup_id));
    let tpl_btn = toggle_pill_button(
        ui,
        "Textbook ▾",
        tpl_open,
        "Insert ready-made textbook equation blocks",
        theme,
    );
    if tpl_btn.clicked() {
        ui.memory_mut(|m| m.toggle_popup(tpl_popup_id));
    }
    egui::popup_below_widget(
        ui,
        tpl_popup_id,
        &tpl_btn,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(210.0);
            for &(name, tex) in MATH_TEXTBOOK_TEMPLATES {
                if ui.button(name).on_hover_text(tex).clicked() {
                    *action = ToolbarAction::InsertMathTemplate(tex);
                    ui.memory_mut(|m| m.close_popup());
                }
            }
        },
    );

    ui.separator();

    if vivid_button(
        ui,
        RichText::new("= Calculate").size(12.0).strong(),
        Vec2::ZERO,
        6,
        false,
        true,
        ToolbarButtonKind::AccentSoft,
        "Evaluate the LaTeX / numeric expression or block at cursor and insert/update '= result'",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::CalculateMath;
    }
    if toggle_pill_button(
        ui,
        "⚡ Auto-Calc",
        calc.auto_calc_math,
        "Automatically evaluate math expressions and display live results on equation cards",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::ToggleAutoCalcMath;
    }
}

fn render_tables_ribbon_controls(
    ui: &mut egui::Ui,
    calc: CalcToolbarInfo,
    theme: &WordTheme,
    state: &mut ToolbarState,
    action: &mut ToolbarAction,
) {
    render_table_grid_inserter_button(ui, "tables_tab_grid_popup", theme, state, action);

    if ribbon_button(
        ui,
        "Edit Table",
        "Open Excel-like interactive Table & Formula Editor for table at cursor",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::OpenTableEditor;
    }

    ui.separator();

    for (label, formula, tip) in [
        ("∑ SUM", "=SUM(ABOVE)", "Insert Excel SUM formula (=SUM(ABOVE))"),
        ("x̄ AVG", "=AVERAGE(ABOVE)", "Insert Excel AVERAGE formula (=AVERAGE(ABOVE))"),
        ("↓ MIN", "=MIN(ABOVE)", "Insert Excel MIN formula (=MIN(ABOVE))"),
        ("↑ MAX", "=MAX(ABOVE)", "Insert Excel MAX formula (=MAX(ABOVE))"),
        ("# COUNT", "=COUNT(ABOVE)", "Insert Excel COUNT formula (=COUNT(ABOVE))"),
        ("IF", "=IF(A1>0, \"Yes\", \"No\")", "Insert Excel conditional IF formula"),
    ] {
        if ribbon_button(ui, label, tip, theme).clicked() {
            *action = ToolbarAction::InsertTableFormula(formula);
        }
    }

    ui.separator();

    if vivid_button(
        ui,
        RichText::new("fx Recalculate").size(12.0).strong(),
        Vec2::ZERO,
        6,
        false,
        true,
        ToolbarButtonKind::AccentSoft,
        "Recalculate Excel formulas in the current table (or all tables in the document)",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::CalculateTables;
    }
    if toggle_pill_button(
        ui,
        "⚡ Auto-Calc",
        calc.auto_calc_tables,
        "Automatically evaluate Excel-like table formulas live during rendering",
        theme,
    )
    .clicked()
    {
        *action = ToolbarAction::ToggleAutoCalcTables;
    }
}

fn render_table_grid_inserter_button(
    ui: &mut egui::Ui,
    popup_salt: &str,
    theme: &WordTheme,
    state: &mut ToolbarState,
    action: &mut ToolbarAction,
) {
    // Word-style Interactive Table Grid Picker popup
    let table_popup_id = ui.make_persistent_id(popup_salt);
    let table_btn = ribbon_button(
        ui,
        "Table ▾",
        "Interactive GFM Table Grid Inserter",
        theme,
    );
    if table_btn.clicked() {
        ui.memory_mut(|m| m.toggle_popup(table_popup_id));
    }
    egui::popup_below_widget(
        ui,
        table_popup_id,
        &table_btn,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(196.0);
            let rows = state.hover_rows.max(2);
            let cols = state.hover_cols.max(1);
            ui.label(
                RichText::new(if state.hover_rows > 0 {
                    format!("Insert {rows} × {cols} Table")
                } else {
                    "Hover Grid to Pick Size".to_string()
                })
                .strong()
                .color(theme.accent),
            );
            ui.add_space(6.0);

            let grid_max = 8usize;
            let cell_sz = 19.0_f32;
            let gap = 3.0_f32;
            let total_w = grid_max as f32 * (cell_sz + gap);
            let (grid_rect, _) = ui.allocate_exact_size(
                Vec2::new(total_w, total_w),
                Sense::hover(),
            );

            let pointer_pos = ui.input(|i| i.pointer.hover_pos());
            let mut clicked_dims: Option<(usize, usize)> = None;
            state.hover_rows = 0;
            state.hover_cols = 0;

            for r in 0..grid_max {
                for c in 0..grid_max {
                    let x = grid_rect.left() + c as f32 * (cell_sz + gap);
                    let y = grid_rect.top() + r as f32 * (cell_sz + gap);
                    let cell_rect = egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        Vec2::splat(cell_sz),
                    );
                    if let Some(pos) = pointer_pos {
                        if cell_rect.contains(pos) {
                            state.hover_rows = r + 1;
                            state.hover_cols = c + 1;
                        }
                    }
                }
            }

            let painter = ui.painter_at(grid_rect);
            for r in 0..grid_max {
                for c in 0..grid_max {
                    let x = grid_rect.left() + c as f32 * (cell_sz + gap);
                    let y = grid_rect.top() + r as f32 * (cell_sz + gap);
                    let cell_rect = egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        Vec2::splat(cell_sz),
                    );
                    let active = r < state.hover_rows && c < state.hover_cols;
                    let fill = if active {
                        theme.accent_soft
                    } else {
                        theme.paper_bg
                    };
                    let stroke_col = if active {
                        theme.accent
                    } else {
                        theme.border
                    };
                    painter.rect_filled(cell_rect, 3.0, fill);
                    painter.rect_stroke(
                        cell_rect,
                        3.0,
                        Stroke::new(1.0_f32, stroke_col),
                        egui::StrokeKind::Inside,
                    );

                    let resp = ui.interact(
                        cell_rect,
                        ui.id().with(("tcell", popup_salt, r, c)),
                        Sense::click(),
                    );
                    if resp.clicked() {
                        clicked_dims = Some(((r + 1).max(2), c + 1));
                    }
                }
            }

            if let Some((ins_r, ins_c)) = clicked_dims {
                *action = ToolbarAction::InsertTable {
                    rows: ins_r,
                    cols: ins_c,
                };
                ui.memory_mut(|m| m.close_popup());
            }

            ui.separator();
            if ui.button("Edit Table at Cursor...").clicked() {
                *action = ToolbarAction::OpenTableEditor;
                ui.memory_mut(|m| m.close_popup());
            }
        },
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolbarButtonKind {
    Subtle,
    Secondary,
    ViewPill,
    AccentSoft,
    Primary,
}

fn inline_label(ui: &mut egui::Ui, text: RichText) -> egui::Response {
    ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn vivid_button(
    ui: &mut egui::Ui,
    text: RichText,
    min_size: Vec2,
    radius: u8,
    active: bool,
    enabled: bool,
    kind: ToolbarButtonKind,
    tooltip: &str,
    theme: &WordTheme,
) -> egui::Response {
    let hover_tint = Color32::from_rgba_unmultiplied(
        theme.accent.r(),
        theme.accent.g(),
        theme.accent.b(),
        if theme.dark { 58 } else { 42 },
    );
    let pressed_tint = Color32::from_rgba_unmultiplied(
        theme.accent.r(),
        theme.accent.g(),
        theme.accent.b(),
        if theme.dark { 96 } else { 78 },
    );

    let (idle_fill, idle_stroke, idle_fg, hover_fill, hover_stroke, hover_fg, pressed_fill, pressed_fg) =
        match kind {
            ToolbarButtonKind::Subtle => {
                if active {
                    (
                        theme.accent_soft,
                        Stroke::new(1.0_f32, theme.accent),
                        theme.accent,
                        hover_tint,
                        Stroke::new(1.3_f32, theme.accent),
                        theme.accent,
                        pressed_tint,
                        theme.accent,
                    )
                } else {
                    (
                        Color32::TRANSPARENT,
                        Stroke::NONE,
                        theme.text_primary,
                        theme.accent_soft,
                        Stroke::new(1.2_f32, theme.accent),
                        if theme.dark {
                            Color32::WHITE
                        } else {
                            theme.accent_hover
                        },
                        pressed_tint,
                        theme.accent,
                    )
                }
            }
            ToolbarButtonKind::Secondary => (
                theme.pill_bg,
                Stroke::new(1.0_f32, theme.border),
                theme.text_primary,
                theme.accent_soft,
                Stroke::new(1.25_f32, theme.accent),
                if theme.dark {
                    Color32::WHITE
                } else {
                    theme.accent_hover
                },
                pressed_tint,
                theme.accent,
            ),
            ToolbarButtonKind::ViewPill => {
                if active {
                    (
                        theme.paper_bg,
                        Stroke::new(1.0_f32, theme.border),
                        theme.accent,
                        theme.accent_soft,
                        Stroke::new(1.25_f32, theme.accent),
                        theme.accent,
                        pressed_tint,
                        theme.accent,
                    )
                } else {
                    (
                        Color32::TRANSPARENT,
                        Stroke::NONE,
                        theme.text_muted,
                        theme.accent_soft,
                        Stroke::new(1.1_f32, theme.accent),
                        theme.text_primary,
                        pressed_tint,
                        theme.accent,
                    )
                }
            }
            ToolbarButtonKind::AccentSoft => (
                theme.accent_soft,
                Stroke::new(1.0_f32, theme.accent),
                theme.accent,
                hover_tint,
                Stroke::new(1.4_f32, theme.accent),
                if theme.dark {
                    Color32::WHITE
                } else {
                    theme.accent_hover
                },
                pressed_tint,
                theme.accent,
            ),
            ToolbarButtonKind::Primary => (
                theme.accent,
                Stroke::NONE,
                Color32::WHITE,
                theme.accent_hover,
                Stroke::new(1.2_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 160)),
                Color32::WHITE,
                theme.accent_hover,
                Color32::WHITE,
            ),
        };

    // Mutate visuals directly on `ui` (without `ui.scope`) so `horizontal_wrapped`
    // keeps its wrapping layout and measures the single-line extended button intact.
    let (prev_inactive, prev_hovered, prev_active) = {
        let w = &ui.visuals().widgets;
        (w.inactive, w.hovered, w.active)
    };
    {
        let widgets = &mut ui.visuals_mut().widgets;
        widgets.inactive.bg_fill = idle_fill;
        widgets.inactive.weak_bg_fill = idle_fill;
        widgets.inactive.bg_stroke = idle_stroke;
        widgets.inactive.fg_stroke = Stroke::new(1.0_f32, idle_fg);
        widgets.inactive.corner_radius = CornerRadius::same(radius);
        widgets.inactive.expansion = 0.0;

        widgets.hovered.bg_fill = hover_fill;
        widgets.hovered.weak_bg_fill = hover_fill;
        widgets.hovered.bg_stroke = hover_stroke;
        widgets.hovered.fg_stroke = Stroke::new(1.2_f32, hover_fg);
        widgets.hovered.corner_radius = CornerRadius::same(radius);
        widgets.hovered.expansion = 1.0;

        widgets.active.bg_fill = pressed_fill;
        widgets.active.weak_bg_fill = pressed_fill;
        widgets.active.bg_stroke = Stroke::new(1.5_f32, theme.accent);
        widgets.active.fg_stroke = Stroke::new(1.4_f32, pressed_fg);
        widgets.active.corner_radius = CornerRadius::same(radius);
        widgets.active.expansion = 0.0;
    }

    let mut btn = egui::Button::new(text)
        .wrap_mode(egui::TextWrapMode::Extend)
        .corner_radius(CornerRadius::same(radius));
    if min_size != Vec2::ZERO {
        btn = btn.min_size(min_size);
    }
    let resp = ui.add_enabled(enabled, btn);

    {
        let widgets = &mut ui.visuals_mut().widgets;
        widgets.inactive = prev_inactive;
        widgets.hovered = prev_hovered;
        widgets.active = prev_active;
    }

    if enabled && ui.is_enabled() && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        let glow_color = Color32::from_rgba_unmultiplied(
            theme.accent.r(),
            theme.accent.g(),
            theme.accent.b(),
            if theme.dark { 105 } else { 80 },
        );
        ui.painter().rect_stroke(
            resp.rect.expand(1.5),
            CornerRadius::same(radius.saturating_add(1)),
            Stroke::new(1.5_f32, glow_color),
            egui::StrokeKind::Outside,
        );
        if kind != ToolbarButtonKind::Primary && resp.rect.width() >= 14.0 {
            let bar_y = resp.rect.bottom() - 1.5;
            let bar_inset = 5.0_f32.min(resp.rect.width() * 0.25);
            ui.painter().line_segment(
                [
                    egui::pos2(resp.rect.left() + bar_inset, bar_y),
                    egui::pos2(resp.rect.right() - bar_inset, bar_y),
                ],
                Stroke::new(2.0_f32, theme.accent),
            );
        }
    }

    if tooltip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tooltip)
    }
}

fn ribbon_button(
    ui: &mut egui::Ui,
    label: &str,
    tooltip: &str,
    theme: &WordTheme,
) -> egui::Response {
    vivid_button(
        ui,
        RichText::new(label).size(12.0),
        Vec2::ZERO,
        6,
        false,
        true,
        ToolbarButtonKind::Subtle,
        tooltip,
        theme,
    )
}

fn toggle_pill_button(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    tooltip: &str,
    theme: &WordTheme,
) -> egui::Response {
    vivid_button(
        ui,
        RichText::new(label).size(12.0),
        Vec2::ZERO,
        6,
        active,
        true,
        ToolbarButtonKind::Subtle,
        tooltip,
        theme,
    )
}

fn format_chip_active(
    ui: &mut egui::Ui,
    text: RichText,
    active: bool,
    tooltip: &str,
    theme: &WordTheme,
) -> egui::Response {
    vivid_button(
        ui,
        text,
        Vec2::new(24.0, 22.0),
        5,
        active,
        true,
        ToolbarButtonKind::Subtle,
        tooltip,
        theme,
    )
}

fn view_mode_pill_button(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    tooltip: &str,
    theme: &WordTheme,
) -> egui::Response {
    vivid_button(
        ui,
        RichText::new(label).size(12.0).strong(),
        Vec2::ZERO,
        5,
        active,
        true,
        ToolbarButtonKind::ViewPill,
        tooltip,
        theme,
    )
}

fn export_print_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    tooltip: &str,
    theme: &WordTheme,
) -> egui::Response {
    vivid_button(
        ui,
        RichText::new(label).size(12.5),
        Vec2::ZERO,
        6,
        false,
        enabled,
        ToolbarButtonKind::Secondary,
        tooltip,
        theme,
    )
}

fn export_accent_soft_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    tooltip: &str,
    theme: &WordTheme,
) -> egui::Response {
    vivid_button(
        ui,
        RichText::new(label).size(12.5).strong(),
        Vec2::ZERO,
        6,
        false,
        enabled,
        ToolbarButtonKind::AccentSoft,
        tooltip,
        theme,
    )
}

fn export_primary_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    tooltip: &str,
    theme: &WordTheme,
) -> egui::Response {
    vivid_button(
        ui,
        RichText::new(label).size(12.5).strong(),
        Vec2::ZERO,
        6,
        false,
        enabled,
        ToolbarButtonKind::Primary,
        tooltip,
        theme,
    )
}
