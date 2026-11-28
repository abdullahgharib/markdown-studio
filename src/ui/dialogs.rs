use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;

use egui::{Align2, Color32, CornerRadius, FontId, Key, ProgressBar, RichText, Sense, Stroke, Vec2};
use ropey::Rope;
use rustc_hash::{FxHashMap, FxHasher};

use crate::document::ast::TableData;
use crate::document::calc;
use crate::document::editing::{apply_table_edit, word_range_in_line};
use crate::document::search::{
    find_all_matches, find_all_matches_in_rope_cancellable, is_unicode_word_char,
    replace_all_matches, replace_single_match_with_opts, SearchOptions, SearchResult,
};
use crate::document::unicode::{
    build_visual_bidi_line_map, detect_paragraph_direction, grapheme_char_boundaries,
    next_grapheme_char_idx, prev_grapheme_char_idx, snap_to_grapheme_end, snap_to_grapheme_start,
    TextDirection, VisualBidiLineMap,
};
use crate::document::DocumentBuffer;
use crate::export::ir::TableAlign;
use crate::export::print::{
    list_os_printers, parse_page_range, DuplexMode, PageOrientation, PaperSize, PrintJobOptions,
    PrinterInfo,
};
use crate::export::BackgroundExporter;
use crate::ui::theme::{ThemeMode, WordTheme};
use crate::ui::toolbar::{vivid_button, ToolbarButtonKind, ViewMode};

/// State for the interactive GFM Table Editor modal.
#[derive(Debug, Default)]
pub struct TableEditorModal {
    pub open: bool,
    pub table: Option<TableData>,
    pub selected_cell: Option<(usize, usize)>,
    eval_revision: u64,
    cached_eval_revision: u64,
    cached_evaluated: Vec<Vec<String>>,
}

impl TableEditorModal {
    pub fn open_with(&mut self, mut table: TableData) {
        for row in &mut table.rows {
            for cell in row.iter_mut() {
                if cell.trim_start().starts_with("\\=")
                    && let (Some(f), _) = calc::extract_table_cell_formula(cell) {
                        *cell = f;
                    }
            }
        }
        self.selected_cell = if !table.rows.is_empty() && table.col_count() > 0 {
            Some((0, 0))
        } else {
            None
        };
        self.table = Some(table);
        self.open = true;
        self.eval_revision = self.eval_revision.wrapping_add(1);
        self.cached_eval_revision = 0;
        self.cached_evaluated.clear();
    }

    pub fn show(&mut self, ctx: &egui::Context, buf: &mut DocumentBuffer) {
        if !self.open {
            return;
        }
        let Some(mut table) = self.table.take() else {
            self.open = false;
            return;
        };

        let mut keep_open = true;
        let mut cancel_clicked = false;
        let mut apply = false;
        let mut mark_dirty = false;

        egui::Window::new("Interactive GFM Table Editor")
            .open(&mut keep_open)
            .resizable(true)
            .default_width(700.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("+ Add Row").clicked() {
                        table.add_row(None);
                        mark_dirty = true;
                    }
                    if ui.button("+ Add Column").clicked() {
                        table.add_column(None);
                        mark_dirty = true;
                    }
                    if ui
                        .button("fx Recalculate")
                        .on_hover_text("Evaluate all Excel formulas (=SUM, =AVERAGE, ...) in cells")
                        .clicked()
                    {
                        let _ = calc::recalculate_table(&mut table);
                        mark_dirty = true;
                    }
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "{} rows × {} columns",
                            table.rows.len() + 1,
                            table.col_count()
                        ))
                        .weak(),
                    );
                });

                // Ensure all body rows have consistent column count for Excel indexing
                let cols_norm = table.col_count();
                for row in &mut table.rows {
                    while row.len() < cols_norm {
                        row.push(String::new());
                    }
                }
                while table.header.len() < cols_norm {
                    table.header.push(String::new());
                }

                // Formula Bar for selected body cell
                if !table.rows.is_empty() && cols_norm > 0 {
                    let (sel_r, sel_c) = self.selected_cell.unwrap_or((0, 0));
                    let sel_r = sel_r.min(table.rows.len().saturating_sub(1));
                    let sel_c = sel_c.min(cols_norm.saturating_sub(1));
                    self.selected_cell = Some((sel_r, sel_c));

                    ui.horizontal_wrapped(|ui| {
                        let cell_ref = format!("{}{}", calc::col_index_to_letters(sel_c), sel_r + 1);
                        ui.label(RichText::new(format!("[{cell_ref}]")).strong().monospace());
                        ui.label(RichText::new("fx").strong().italics());
                        let cell_mut = &mut table.rows[sel_r][sel_c];
                        if ui
                            .add(
                                egui::TextEdit::singleline(cell_mut)
                                    .desired_width(210.0)
                                    .hint_text("Value or =SUM(A1:B2) / =SUM(ABOVE)"),
                            )
                            .changed()
                        {
                            mark_dirty = true;
                        }
                        let dir_arg = if sel_r > 0 {
                            "ABOVE"
                        } else if sel_c > 0 {
                            "LEFT"
                        } else {
                            "A1"
                        };
                        for &(lbl, tmpl) in &[
                            ("∑ SUM", "=SUM"),
                            ("AVG", "=AVERAGE"),
                            ("MIN", "=MIN"),
                            ("MAX", "=MAX"),
                            ("COUNT", "=COUNT"),
                        ] {
                            if ui
                                .small_button(lbl)
                                .on_hover_text(format!("Insert {tmpl}({dir_arg}) into {cell_ref}"))
                                .clicked()
                            {
                                *cell_mut = format!("{tmpl}({dir_arg})");
                                mark_dirty = true;
                            }
                        }
                        if ui
                            .small_button("IF")
                            .on_hover_text(format!("Insert =IF(...) condition into {cell_ref}"))
                            .clicked()
                        {
                            *cell_mut = "=IF(A1>0, 1, 0)".to_string();
                            mark_dirty = true;
                        }
                    });
                }

                ui.separator();

                if mark_dirty || self.cached_eval_revision != self.eval_revision {
                    self.cached_evaluated = calc::evaluate_table_grid(&table.rows);
                    self.cached_eval_revision = self.eval_revision;
                }
                let evaluated = &self.cached_evaluated;

                let scroll_id = ui.make_persistent_id("table_editor_both_scroll");
                let scroll_y = egui::scroll_area::State::load(ctx, scroll_id)
                    .map(|s| s.offset.y)
                    .unwrap_or(0.0);
                let total_rows = table.rows.len();
                let est_row_h = 28.0_f32;
                let virt_range = if total_rows > 80 {
                    let body_scroll = (scroll_y - 90.0).max(0.0);
                    let start = ((body_scroll / est_row_h).floor() as usize).saturating_sub(6);
                    let end = (start + 32).min(total_rows);
                    start..end
                } else {
                    0..total_rows
                };

                egui::ScrollArea::both()
                    .id_salt("table_editor_both_scroll")
                    .max_height(360.0)
                    .show(ui, |ui| {
                        let cols = table.col_count();
                        egui::Grid::new("table_editor_grid")
                            .striped(true)
                            .spacing([6.0, 6.0])
                            .show(ui, |ui| {
                                // Excel Column Letters (A, B, C, ...)
                                ui.label("");
                                for c in 0..cols {
                                    ui.label(
                                        RichText::new(calc::col_index_to_letters(c))
                                            .strong()
                                            .size(11.0),
                                    );
                                }
                                ui.label("");
                                ui.end_row();

                                // Alignment & Delete column controls
                                ui.label(RichText::new("Align").strong());
                                let mut col_to_delete: Option<usize> = None;
                                for c in 0..cols {
                                    ui.horizontal(|ui| {
                                        let cur_align = table
                                            .alignments
                                            .get(c)
                                            .copied()
                                            .unwrap_or(TableAlign::None);
                                        let label = match cur_align {
                                            TableAlign::None => "Auto",
                                            TableAlign::Left => "Left",
                                            TableAlign::Center => "Center",
                                            TableAlign::Right => "Right",
                                        };
                                        if ui.small_button(label).clicked() {
                                            let next = match cur_align {
                                                TableAlign::None => TableAlign::Left,
                                                TableAlign::Left => TableAlign::Center,
                                                TableAlign::Center => TableAlign::Right,
                                                TableAlign::Right => TableAlign::None,
                                            };
                                            table.set_alignment(c, next);
                                            mark_dirty = true;
                                        }
                                        if cols > 1
                                            && ui
                                                .small_button("×")
                                                .on_hover_text("Delete column")
                                                .clicked()
                                        {
                                            col_to_delete = Some(c);
                                        }
                                    });
                                }
                                if let Some(c) = col_to_delete {
                                    table.delete_column(c);
                                    mark_dirty = true;
                                    if let Some((sr, sc)) = self.selected_cell {
                                        self.selected_cell = Some((sr, sc.saturating_sub(if sc >= c { 1 } else { 0 })));
                                    }
                                }
                                ui.label("");
                                ui.end_row();

                                // Header row
                                let cur_cols = table.col_count();
                                while table.header.len() < cur_cols {
                                    table.header.push(String::new());
                                }
                                ui.label(RichText::new("Header").strong());
                                for cell in table.header.iter_mut().take(cur_cols) {
                                    if ui
                                        .add(egui::TextEdit::singleline(cell).desired_width(120.0))
                                        .changed()
                                    {
                                        mark_dirty = true;
                                    }
                                }
                                ui.label("");
                                ui.end_row();

                                if virt_range.start > 0 {
                                    ui.add_space(virt_range.start as f32 * est_row_h);
                                    for _ in 0..=cur_cols {
                                        ui.label("");
                                    }
                                    ui.end_row();
                                }

                                // Visible body rows
                                let mut row_to_delete: Option<usize> = None;
                                for r_idx in virt_range.clone() {
                                    let row = &mut table.rows[r_idx];
                                    while row.len() < cur_cols {
                                        row.push(String::new());
                                    }
                                    ui.label(
                                        RichText::new(format!("{}", r_idx + 1))
                                            .strong()
                                            .size(11.0),
                                    );
                                    for (c_idx, cell) in row.iter_mut().take(cur_cols).enumerate() {
                                        let (formula_opt, _) = calc::extract_table_cell_formula(cell);
                                        let has_formula = formula_opt.is_some();
                                        let eval_badge = if has_formula {
                                            evaluated
                                                .get(r_idx)
                                                .and_then(|r| r.get(c_idx))
                                                .cloned()
                                        } else {
                                            None
                                        };
                                        ui.horizontal(|ui| {
                                            let te = ui.add(
                                                egui::TextEdit::singleline(cell).desired_width(
                                                    if eval_badge.is_some() { 82.0 } else { 120.0 },
                                                ),
                                            );
                                            if te.changed() {
                                                mark_dirty = true;
                                            }
                                            if te.gained_focus() || te.clicked() || te.has_focus() {
                                                self.selected_cell = Some((r_idx, c_idx));
                                            }
                                            if let Some(val_str) = eval_badge {
                                                ui.label(
                                                    RichText::new(format!("→ {val_str}"))
                                                        .size(10.5)
                                                        .strong()
                                                        .color(Color32::from_rgb(37, 99, 235)),
                                                );
                                            }
                                        });
                                    }
                                    if ui.small_button("Delete").clicked() {
                                        row_to_delete = Some(r_idx);
                                    }
                                    ui.end_row();
                                }
                                if virt_range.end < total_rows {
                                    ui.add_space((total_rows - virt_range.end) as f32 * est_row_h);
                                    for _ in 0..=cur_cols {
                                        ui.label("");
                                    }
                                    ui.end_row();
                                }
                                if let Some(r_idx) = row_to_delete {
                                    table.delete_row(r_idx);
                                    mark_dirty = true;
                                    if table.rows.is_empty() {
                                        self.selected_cell = None;
                                    } else if let Some((sr, sc)) = self.selected_cell {
                                        self.selected_cell = Some((
                                            sr.min(table.rows.len().saturating_sub(1)),
                                            sc,
                                        ));
                                    }
                                }
                            });
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    let apply_btn = egui::Button::new(
                        RichText::new("Apply Changes")
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(37, 99, 235));
                    if ui.add(apply_btn).clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_clicked = true;
                    }
                });
            });

        if mark_dirty {
            self.eval_revision = self.eval_revision.wrapping_add(1);
        }

        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            cancel_clicked = true;
        } else if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::Enter)) {
            apply = true;
        }

        if apply {
            let _ = calc::recalculate_table(&mut table);
            apply_table_edit(buf, &table);
            self.open = false;
            self.table = None;
            self.selected_cell = None;
            self.cached_evaluated.clear();
        } else if keep_open && !cancel_clicked {
            self.table = Some(table);
        } else {
            self.open = false;
            self.table = None;
            self.selected_cell = None;
            self.cached_evaluated.clear();
        }
    }
}

/// Modal dialog for inserting a Markdown hyperlink `[text](url)`.
#[derive(Debug, Default)]
pub struct InsertLinkModal {
    pub open: bool,
    pub text: String,
    pub url: String,
    pub focus_url: bool,
}

impl InsertLinkModal {
    pub fn open_with(&mut self, prefilled_text: String) {
        self.open = true;
        self.text = prefilled_text;
        self.url.clear();
        self.focus_url = true;
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: &WordTheme) -> Option<(String, String)> {
        if !self.open {
            return None;
        }
        let mut keep_open = true;
        let mut cancel = false;
        let mut confirm = false;

        egui::Window::new("Insert Hyperlink")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Link Text:").size(13.0).color(theme.text_primary));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.text)
                            .hint_text("Display label (optional)")
                            .desired_width(250.0),
                    );
                });
                ui.add_space(6.0);
                let url_id = egui::Id::new("insert_link_url_input");
                ui.horizontal(|ui| {
                    ui.label(RichText::new("URL / Target:").size(13.0).color(theme.text_primary));
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.url)
                            .id(url_id)
                            .hint_text("https://example.com or #heading")
                            .desired_width(240.0),
                    );
                    if self.focus_url {
                        self.focus_url = false;
                        resp.request_focus();
                    }
                });
                if ui.input(|i| i.key_pressed(Key::Enter)) && !self.url.trim().is_empty() {
                    confirm = true;
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let can_insert = !self.url.trim().is_empty();
                    let insert_btn = egui::Button::new(
                        RichText::new("Insert Link")
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(if can_insert {
                        theme.accent
                    } else {
                        theme.text_muted
                    });
                    if ui.add_enabled(can_insert, insert_btn).clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if confirm && !self.url.trim().is_empty() {
            self.open = false;
            Some((std::mem::take(&mut self.text), std::mem::take(&mut self.url)))
        } else if !keep_open || cancel {
            self.open = false;
            None
        } else {
            None
        }
    }
}

/// Pending document-discarding action waiting for user Save/Discard confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingDiscardAction {
    NewFile,
    OpenFile,
    OpenSpecificFile(PathBuf),
    CloseTab(usize),
    CloseTabs(Vec<u64>),
    CloseWindow,
    InstallUpdate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsavedPromptChoice {
    Save,
    DontSave,
    Cancel,
}

/// Modal prompt shown when clicking New, Open, or closing with unsaved changes.
#[derive(Debug, Default)]
pub struct UnsavedChangesModal {
    pub pending: Option<PendingDiscardAction>,
}

impl UnsavedChangesModal {
    pub fn open(&mut self, action: PendingDiscardAction) {
        self.pending = Some(action);
    }

    pub fn is_open(&self) -> bool {
        self.pending.is_some()
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        doc_name: &str,
        theme: &WordTheme,
    ) -> Option<(PendingDiscardAction, UnsavedPromptChoice)> {
        let pending = self.pending.clone()?;
        let mut keep_open = true;
        let mut choice: Option<UnsavedPromptChoice> = None;

        let action_verb = match &pending {
            PendingDiscardAction::NewFile => "creating a new document",
            PendingDiscardAction::OpenFile | PendingDiscardAction::OpenSpecificFile(_) => {
                "opening another file"
            }
            PendingDiscardAction::CloseTab(_) | PendingDiscardAction::CloseTabs(_) => {
                "closing this tab"
            }
            PendingDiscardAction::CloseWindow => "closing Markdown Studio",
            PendingDiscardAction::InstallUpdate => "restarting to install the update",
        };

        egui::Window::new("Unsaved Changes")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(410.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Save changes to \"{doc_name}\" before {action_verb}?"))
                        .strong()
                        .size(14.5)
                        .color(theme.text_primary),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Your unsaved edits will be lost if you don't save them.")
                        .size(12.5)
                        .color(theme.text_muted),
                );
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    let save_btn = egui::Button::new(
                        RichText::new("Save")
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(theme.accent)
                    .stroke(Stroke::NONE);
                    if ui.add(save_btn).clicked() {
                        choice = Some(UnsavedPromptChoice::Save);
                    }

                    let discard_btn = egui::Button::new(
                        RichText::new("Don't Save")
                            .strong()
                            .color(theme.warning_amber),
                    )
                    .fill(Color32::from_rgba_unmultiplied(
                        theme.warning_amber.r(),
                        theme.warning_amber.g(),
                        theme.warning_amber.b(),
                        24,
                    ))
                    .stroke(Stroke::new(1.0_f32, theme.warning_amber));
                    if ui.add(discard_btn).clicked() {
                        choice = Some(UnsavedPromptChoice::DontSave);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            choice = Some(UnsavedPromptChoice::Cancel);
                        }
                    });
                });
            });

        if !keep_open && choice.is_none() {
            choice = Some(UnsavedPromptChoice::Cancel);
        }

        if let Some(c) = choice {
            self.pending = None;
            Some((pending, c))
        } else {
            None
        }
    }
}

/// Modal dialog for renaming a file or folder from the File Explorer context menu.
#[derive(Debug, Default)]
pub struct ExplorerRenameModal {
    pub open: bool,
    pub target_path: Option<PathBuf>,
    pub is_dir: bool,
    pub draft: String,
    pub focus_requested: bool,
    pub error_message: Option<String>,
}

impl ExplorerRenameModal {
    pub fn open_for(&mut self, path: PathBuf, is_dir: bool) {
        self.open = true;
        self.draft = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.target_path = Some(path);
        self.is_dir = is_dir;
        self.focus_requested = true;
        self.error_message = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.target_path = None;
        self.draft.clear();
        self.focus_requested = false;
        self.error_message = None;
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: &WordTheme) -> Option<(PathBuf, String)> {
        if !self.open {
            return None;
        }
        let target_path = self.target_path.clone()?;
        let mut keep_open = true;
        let mut submit = false;
        let mut cancel = false;
        let kind_label = if self.is_dir { "Folder" } else { "File" };

        egui::Window::new(format!("Rename {kind_label}"))
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Enter a new name for {}:", target_path.display()))
                        .size(12.5)
                        .color(theme.text_muted),
                );
                ui.add_space(6.0);
                let resp = ui.add_sized(
                    Vec2::new(350.0, 26.0),
                    egui::TextEdit::singleline(&mut self.draft).hint_text("New name..."),
                );
                if self.focus_requested {
                    resp.request_focus();
                    self.focus_requested = false;
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    submit = true;
                }
                if let Some(ref err) = self.error_message {
                    ui.add_space(4.0);
                    ui.label(RichText::new(err).size(12.0).color(theme.warning_amber));
                }
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ok_btn = egui::Button::new(
                        RichText::new("Rename").strong().color(Color32::WHITE),
                    )
                    .fill(theme.accent)
                    .stroke(Stroke::NONE);
                    if ui.add(ok_btn).clicked() {
                        submit = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            cancel = true;
                        }
                    });
                });
            });

        if cancel || !keep_open {
            self.close();
            return None;
        }
        if submit {
            match crate::ui::context_menu::validate_explorer_entry_name(&self.draft) {
                Ok(valid) => {
                    self.close();
                    return Some((target_path, valid));
                }
                Err(msg) => {
                    self.error_message = Some(msg.to_string());
                }
            }
        }
        None
    }
}

/// Confirmation dialog shown before deleting a file or folder from the File Explorer.
#[derive(Debug, Default)]
pub struct ExplorerDeleteConfirmModal {
    pub open: bool,
    pub target_path: Option<PathBuf>,
    pub is_dir: bool,
}

impl ExplorerDeleteConfirmModal {
    pub fn open_for(&mut self, path: PathBuf, is_dir: bool) {
        self.open = true;
        self.target_path = Some(path);
        self.is_dir = is_dir;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.target_path = None;
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: &WordTheme) -> Option<PathBuf> {
        if !self.open {
            return None;
        }
        let target_path = self.target_path.clone()?;
        let mut keep_open = true;
        let mut confirm = false;
        let mut cancel = false;
        let kind_label = if self.is_dir { "Folder" } else { "File" };
        let name = target_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| target_path.display().to_string());

        egui::Window::new(format!("Delete {kind_label}"))
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(400.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Delete \"{name}\"?"))
                        .strong()
                        .size(14.5)
                        .color(theme.text_primary),
                );
                ui.add_space(4.0);
                let detail = if self.is_dir {
                    format!(
                        "This will permanently delete the folder and its contents:\n{}",
                        target_path.display()
                    )
                } else {
                    format!(
                        "This will permanently delete from disk:\n{}",
                        target_path.display()
                    )
                };
                ui.label(RichText::new(detail).size(12.0).color(theme.text_muted));
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let del_btn = egui::Button::new(
                        RichText::new("Delete")
                            .strong()
                            .color(theme.warning_amber),
                    )
                    .fill(Color32::from_rgba_unmultiplied(
                        theme.warning_amber.r(),
                        theme.warning_amber.g(),
                        theme.warning_amber.b(),
                        24,
                    ))
                    .stroke(Stroke::new(1.0_f32, theme.warning_amber));
                    if ui.add(del_btn).clicked() {
                        confirm = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            cancel = true;
                        }
                    });
                });
            });

        if cancel || !keep_open {
            self.close();
            return None;
        }
        if confirm {
            self.close();
            return Some(target_path);
        }
        None
    }
}

/// Interactive Print Settings dialog allowing printer, copies, paper size, orientation, and range selection.
#[derive(Default)]
pub struct PrintDialogModal {
    pub open: bool,
    pub doc_title: String,
    pub options: PrintJobOptions,
    pub printers: Vec<PrinterInfo>,
    pub loading_printers: bool,
    pub error_message: Option<String>,
    printer_rx: Option<Receiver<Vec<PrinterInfo>>>,
}

impl PrintDialogModal {
    pub fn open_for(&mut self, title: &str) {
        self.open_for_impl(title, None);
    }

    pub fn open_for_with_ctx(&mut self, title: &str, ctx: egui::Context) {
        self.open_for_impl(title, Some(ctx));
    }

    fn open_for_impl(&mut self, title: &str, ctx: Option<egui::Context>) {
        self.open = true;
        self.doc_title = title.to_string();
        self.error_message = None;
        self.loading_printers = true;
        let (tx, rx) = mpsc::channel();
        self.printer_rx = Some(rx);
        thread::Builder::new()
            .name("printer-enum".to_string())
            .spawn(move || {
                let _ = tx.send(list_os_printers());
                if let Some(c) = ctx {
                    c.request_repaint();
                }
            })
            .ok();
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: &WordTheme) -> Option<PrintJobOptions> {
        if !self.open {
            return None;
        }
        if let Some(ref rx) = self.printer_rx
            && let Ok(list) = rx.try_recv() {
                self.printers = list;
                self.loading_printers = false;
                self.printer_rx = None;
                if self.options.printer_name.is_none() {
                    self.options.printer_name = self
                        .printers
                        .iter()
                        .find(|p| p.is_default)
                        .or_else(|| self.printers.first())
                        .map(|p| p.name.clone());
                }
            }

        let mut keep_open = true;
        let mut submit_job: Option<PrintJobOptions> = None;
        let mut cancel = false;

        egui::Window::new("Print Document")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(450.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!("Document: {}", self.doc_title))
                        .size(12.5)
                        .color(theme.text_muted),
                );
                ui.add_space(6.0);

                egui::Grid::new("print_dialog_grid")
                    .num_columns(2)
                    .spacing([14.0, 10.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Printer:").strong());
                        ui.horizontal(|ui| {
                            let selected_label = if self.loading_printers && self.printers.is_empty() {
                                "Detecting printers...".to_string()
                            } else {
                                self.options
                                    .printer_name
                                    .clone()
                                    .unwrap_or_else(|| "System Default Printer".to_string())
                            };
                            egui::ComboBox::from_id_salt("print_printer_combo")
                                .selected_text(selected_label)
                                .width(270.0)
                                .show_ui(ui, |ui| {
                                    if self.printers.is_empty() {
                                        let is_sel = self.options.printer_name.is_none();
                                        if ui
                                            .selectable_label(is_sel, "System Default Printer")
                                            .clicked()
                                        {
                                            self.options.printer_name = None;
                                        }
                                    } else {
                                        for p in &self.printers {
                                            let label = if p.is_default {
                                                format!("{} (Default)", p.name)
                                            } else {
                                                p.name.clone()
                                            };
                                            let selected =
                                                self.options.printer_name.as_deref() == Some(&p.name);
                                            if ui.selectable_label(selected, label).clicked() {
                                                self.options.printer_name = Some(p.name.clone());
                                            }
                                        }
                                    }
                                });
                        });
                        ui.end_row();

                        ui.label(RichText::new("Copies:").strong());
                        ui.horizontal(|ui| {
                            if ui.small_button("−").clicked() && self.options.copies > 1 {
                                self.options.copies -= 1;
                            }
                            ui.add(
                                egui::DragValue::new(&mut self.options.copies)
                                    .range(1..=99)
                                    .speed(0.15),
                            );
                            if ui.small_button("+").clicked() && self.options.copies < 99 {
                                self.options.copies += 1;
                            }
                            ui.add_space(10.0);
                            ui.checkbox(&mut self.options.collate, "Collate");
                        });
                        ui.end_row();

                        ui.label(RichText::new("Paper Size:").strong());
                        egui::ComboBox::from_id_salt("print_paper_size_combo")
                            .selected_text(self.options.paper_size.label())
                            .width(270.0)
                            .show_ui(ui, |ui| {
                                for size in PaperSize::ALL {
                                    if ui
                                        .selectable_label(
                                            self.options.paper_size == size,
                                            size.label(),
                                        )
                                        .clicked()
                                    {
                                        self.options.paper_size = size;
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label(RichText::new("Orientation:").strong());
                        ui.horizontal(|ui| {
                            ui.radio_value(
                                &mut self.options.orientation,
                                PageOrientation::Portrait,
                                "Portrait",
                            );
                            ui.radio_value(
                                &mut self.options.orientation,
                                PageOrientation::Landscape,
                                "Landscape",
                            );
                        });
                        ui.end_row();

                        ui.label(RichText::new("Pages:").strong());
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.radio_value(&mut self.options.all_pages, true, "All Pages");
                                ui.radio_value(&mut self.options.all_pages, false, "Custom Range");
                            });
                            if !self.options.all_pages {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.options.page_range)
                                        .id_source("print_page_range_input")
                                        .desired_width(220.0)
                                        .hint_text("e.g. 1-3, 5"),
                                );
                            }
                        });
                        ui.end_row();

                        ui.label(RichText::new("Sides:").strong());
                        egui::ComboBox::from_id_salt("print_duplex_combo")
                            .selected_text(self.options.duplex.label())
                            .width(270.0)
                            .show_ui(ui, |ui| {
                                for mode in [
                                    DuplexMode::Simplex,
                                    DuplexMode::LongEdge,
                                    DuplexMode::ShortEdge,
                                ] {
                                    if ui
                                        .selectable_label(self.options.duplex == mode, mode.label())
                                        .clicked()
                                    {
                                        self.options.duplex = mode;
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label(RichText::new("Color Mode:").strong());
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut self.options.color, true, "Color");
                            ui.radio_value(&mut self.options.color, false, "Grayscale");
                        });
                        ui.end_row();

                        ui.label(RichText::new("Header:").strong());
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut self.options.header_footer.show_header,
                                "Show",
                            );
                            ui.add_enabled(
                                self.options.header_footer.show_header,
                                egui::TextEdit::singleline(
                                    &mut self.options.header_footer.header_template,
                                )
                                .id_source("print_header_tmpl")
                                .desired_width(210.0)
                                .hint_text("{title}"),
                            );
                        });
                        ui.end_row();

                        ui.label(RichText::new("Footer:").strong());
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.checkbox(
                                    &mut self.options.header_footer.show_footer,
                                    "Show",
                                );
                                ui.add_enabled(
                                    self.options.header_footer.show_footer,
                                    egui::TextEdit::singleline(
                                        &mut self.options.header_footer.footer_template,
                                    )
                                    .id_source("print_footer_tmpl")
                                    .desired_width(210.0)
                                    .hint_text("Page {page} of {pages}"),
                                );
                            });
                            ui.label(
                                RichText::new("Tokens: {title}, {page}, {pages}, {date}")
                                    .size(10.5)
                                    .color(theme.text_muted),
                            );
                        });
                        ui.end_row();
                    });

                if let Some(ref err) = self.error_message {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(err)
                            .size(12.0)
                            .color(Color32::from_rgb(200, 48, 48)),
                    );
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    let print_btn = egui::Button::new(
                        RichText::new("Print")
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(theme.accent)
                    .stroke(Stroke::NONE);
                    if ui.add(print_btn).clicked() {
                        if !self.options.all_pages {
                            match parse_page_range(&self.options.page_range, None) {
                                Ok(_) => {
                                    self.error_message = None;
                                    submit_job = Some(self.options.clone());
                                }
                                Err(e) => {
                                    self.error_message = Some(e);
                                }
                            }
                        } else {
                            self.error_message = None;
                            submit_job = Some(self.options.clone());
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            cancel = true;
                        }
                    });
                });
            });

        if submit_job.is_some() || !keep_open || cancel {
            self.open = false;
            self.printer_rx = None;
        }
        submit_job
    }
}

#[derive(Clone, Debug)]
struct BidiInputLayoutCache {
    text_hash: u64,
    text_len: usize,
    preedit: String,
    cursor_when_preedit: usize,
    font_size_bits: u32,
    ppp_bits: u32,
    theme_mode: ThemeMode,
    atlas: Weak<egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>>,
    display_logical: String,
    char_len: usize,
    bidi_map: Arc<VisualBidiLineMap>,
    galley: Option<Arc<egui::Galley>>,
}

/// Persistent caret/selection/IME state for BiDi-aware singleline text inputs (Find & Replace).
#[derive(Debug, Default)]
pub struct BidiInputState {
    pub cursor_char: usize,
    pub selection_anchor: Option<usize>,
    pub horiz_offset: f32,
    pub dragging: bool,
    pub ime_composing: bool,
    pub ime_preedit: String,
    layout_cache: Option<BidiInputLayoutCache>,
}

impl BidiInputState {
    fn clamp_cursor(&mut self, char_len: usize) {
        self.cursor_char = self.cursor_char.min(char_len);
        if let Some(a) = self.selection_anchor
            && a > char_len {
                self.selection_anchor = Some(char_len);
            }
    }

    fn selection_range(&self, char_len: usize) -> Option<Range<usize>> {
        let a = self.selection_anchor?.min(char_len);
        let c = self.cursor_char.min(char_len);
        if a == c {
            None
        } else if a < c {
            Some(a..c)
        } else {
            Some(c..a)
        }
    }

    fn set_cursor(&mut self, pos: usize, extend: bool, char_len: usize) {
        let clamped = pos.min(char_len);
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_char.min(char_len));
            }
            self.cursor_char = clamped;
        } else {
            self.selection_anchor = None;
            self.cursor_char = clamped;
        }
    }
}

fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

fn replace_char_range_str(s: &mut String, range: Range<usize>, insert: &str) -> usize {
    let char_len = s.chars().count();
    let start_c = range.start.min(char_len);
    let end_c = range.end.min(char_len).max(start_c);
    let b0 = char_to_byte_idx(s, start_c);
    let b1 = char_to_byte_idx(s, end_c);
    s.replace_range(b0..b1, insert);
    start_c + insert.chars().count()
}

fn prev_word_char_idx(s: &str, col: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() || col == 0 {
        return 0;
    }
    let bounds = grapheme_char_boundaries(s);
    let mut b_idx = bounds.partition_point(|&b| b < col.min(chars.len()));
    while b_idx > 0 {
        let prev_b = bounds[b_idx - 1];
        if is_unicode_word_char(chars[prev_b]) {
            break;
        }
        b_idx -= 1;
    }
    while b_idx > 0 {
        let prev_b = bounds[b_idx - 1];
        if !is_unicode_word_char(chars[prev_b]) {
            break;
        }
        b_idx -= 1;
    }
    bounds[b_idx]
}

fn next_word_char_idx(s: &str, col: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    if len == 0 || col >= len {
        return len;
    }
    let bounds = grapheme_char_boundaries(s);
    let mut b_idx = bounds.partition_point(|&b| b < col);
    if b_idx >= bounds.len().saturating_sub(1) {
        return len;
    }
    while b_idx + 1 < bounds.len() && !is_unicode_word_char(chars[bounds[b_idx]]) {
        b_idx += 1;
    }
    while b_idx + 1 < bounds.len() && is_unicode_word_char(chars[bounds[b_idx]]) {
        b_idx += 1;
    }
    bounds[b_idx]
}

/// Renders a single-line BiDi-aware text input supporting Arabic shaping, RTL alignment,
/// multi-language IME input, and multi-codepoint emoji grapheme navigation.
#[allow(clippy::too_many_arguments)]
fn render_bidi_singleline_input(
    ui: &mut egui::Ui,
    id_salt: &str,
    text: &mut String,
    state: &mut BidiInputState,
    desired_width: f32,
    hint_text: &str,
    theme: &WordTheme,
    request_focus: bool,
) -> (egui::Response, bool) {
    let input_id = egui::Id::new(id_salt);
    let height = 24.0_f32;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(desired_width, height), Sense::hover());
    let response = ui.interact(rect, input_id, Sense::click_and_drag());
    ui.memory_mut(|m| m.interested_in_focus(input_id, ui.layer_id()));

    if request_focus || response.clicked() || response.drag_started() {
        ui.memory_mut(|m| m.request_focus(input_id));
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }

    let focused = ui.memory(|m| m.has_focus(input_id));
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                input_id,
                egui::EventFilter {
                    tab: false,
                    horizontal_arrows: true,
                    vertical_arrows: false,
                    escape: false,
                },
            );
        });
    }

    let mut changed = false;
    if focused {
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            let cur_len = text.chars().count();
            state.clamp_cursor(cur_len);
            match ev {
                egui::Event::Ime(ime) => match ime {
                    egui::ImeEvent::Enabled => state.ime_composing = true,
                    egui::ImeEvent::Preedit(p) => {
                        state.ime_composing = !p.is_empty();
                        state.ime_preedit = p;
                    }
                    egui::ImeEvent::Commit(c) => {
                        state.ime_preedit.clear();
                        state.ime_composing = false;
                        let clean: String = c.chars().filter(|ch| *ch != '\r' && *ch != '\n').collect();
                        if !clean.is_empty() {
                            let r = state
                                .selection_range(cur_len)
                                .unwrap_or(state.cursor_char..state.cursor_char);
                            state.cursor_char = replace_char_range_str(text, r, &clean);
                            state.selection_anchor = None;
                            changed = true;
                        }
                    }
                    egui::ImeEvent::Disabled => {
                        state.ime_preedit.clear();
                        state.ime_composing = false;
                    }
                },
                egui::Event::Text(txt) => {
                    if state.ime_composing {
                        state.ime_preedit.clear();
                        state.ime_composing = false;
                    }
                    let clean: String = txt
                        .chars()
                        .filter(|c| !c.is_control() && *c != '\r' && *c != '\n')
                        .collect();
                    if !clean.is_empty() {
                        let r = state
                            .selection_range(cur_len)
                            .unwrap_or(state.cursor_char..state.cursor_char);
                        state.cursor_char = replace_char_range_str(text, r, &clean);
                        state.selection_anchor = None;
                        changed = true;
                    }
                }
                egui::Event::Paste(pasted) => {
                    let clean: String = pasted
                        .chars()
                        .filter(|c| *c != '\r' && *c != '\n')
                        .collect();
                    if !clean.is_empty() {
                        let r = state
                            .selection_range(cur_len)
                            .unwrap_or(state.cursor_char..state.cursor_char);
                        state.cursor_char = replace_char_range_str(text, r, &clean);
                        state.selection_anchor = None;
                        changed = true;
                    }
                }
                egui::Event::Copy => {
                    if let Some(r) = state.selection_range(cur_len) {
                        let b0 = char_to_byte_idx(text, r.start);
                        let b1 = char_to_byte_idx(text, r.end);
                        ui.ctx().copy_text(text[b0..b1].to_string());
                    }
                }
                egui::Event::Cut => {
                    if let Some(r) = state.selection_range(cur_len) {
                        let b0 = char_to_byte_idx(text, r.start);
                        let b1 = char_to_byte_idx(text, r.end);
                        ui.ctx().copy_text(text[b0..b1].to_string());
                        state.cursor_char = replace_char_range_str(text, r, "");
                        state.selection_anchor = None;
                        changed = true;
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let cmd = modifiers.command;
                    let shift = modifiers.shift;
                    match key {
                        Key::A if cmd => {
                            state.selection_anchor = Some(0);
                            state.cursor_char = cur_len;
                        }
                        Key::Backspace => {
                            if let Some(r) = state.selection_range(cur_len) {
                                state.cursor_char = replace_char_range_str(text, r, "");
                                state.selection_anchor = None;
                                changed = true;
                            } else if state.cursor_char > 0 {
                                let prev = if cmd {
                                    prev_word_char_idx(text, state.cursor_char)
                                } else {
                                    prev_grapheme_char_idx(text, state.cursor_char)
                                };
                                state.cursor_char =
                                    replace_char_range_str(text, prev..state.cursor_char, "");
                                changed = true;
                            }
                        }
                        Key::Delete => {
                            if let Some(r) = state.selection_range(cur_len) {
                                state.cursor_char = replace_char_range_str(text, r, "");
                                state.selection_anchor = None;
                                changed = true;
                            } else if state.cursor_char < cur_len {
                                let next = if cmd {
                                    next_word_char_idx(text, state.cursor_char)
                                } else {
                                    next_grapheme_char_idx(text, state.cursor_char)
                                };
                                state.cursor_char =
                                    replace_char_range_str(text, state.cursor_char..next, "");
                                changed = true;
                            }
                        }
                        Key::ArrowLeft => {
                            let is_rtl = detect_paragraph_direction(text) == TextDirection::Rtl;
                            if is_rtl {
                                if !shift && state.selection_range(cur_len).is_some() {
                                    let r = state.selection_range(cur_len).unwrap();
                                    state.set_cursor(r.end, false, cur_len);
                                } else if state.cursor_char < cur_len {
                                    let next = if cmd {
                                        next_word_char_idx(text, state.cursor_char)
                                    } else {
                                        next_grapheme_char_idx(text, state.cursor_char)
                                    };
                                    state.set_cursor(next, shift, cur_len);
                                }
                            } else if !shift && state.selection_range(cur_len).is_some() {
                                let r = state.selection_range(cur_len).unwrap();
                                state.set_cursor(r.start, false, cur_len);
                            } else if state.cursor_char > 0 {
                                let prev = if cmd {
                                    prev_word_char_idx(text, state.cursor_char)
                                } else {
                                    prev_grapheme_char_idx(text, state.cursor_char)
                                };
                                state.set_cursor(prev, shift, cur_len);
                            }
                        }
                        Key::ArrowRight => {
                            let is_rtl = detect_paragraph_direction(text) == TextDirection::Rtl;
                            if is_rtl {
                                if !shift && state.selection_range(cur_len).is_some() {
                                    let r = state.selection_range(cur_len).unwrap();
                                    state.set_cursor(r.start, false, cur_len);
                                } else if state.cursor_char > 0 {
                                    let prev = if cmd {
                                        prev_word_char_idx(text, state.cursor_char)
                                    } else {
                                        prev_grapheme_char_idx(text, state.cursor_char)
                                    };
                                    state.set_cursor(prev, shift, cur_len);
                                }
                            } else if !shift && state.selection_range(cur_len).is_some() {
                                let r = state.selection_range(cur_len).unwrap();
                                state.set_cursor(r.end, false, cur_len);
                            } else if state.cursor_char < cur_len {
                                let next = if cmd {
                                    next_word_char_idx(text, state.cursor_char)
                                } else {
                                    next_grapheme_char_idx(text, state.cursor_char)
                                };
                                state.set_cursor(next, shift, cur_len);
                            }
                        }
                        Key::Home => state.set_cursor(0, shift, cur_len),
                        Key::End => state.set_cursor(cur_len, shift, cur_len),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    let font_id = FontId::proportional(13.0);
    let inner_rect = rect.shrink2(Vec2::new(7.0, 3.0));
    let has_preedit = focused && !state.ime_preedit.is_empty();
    let cursor_when_preedit = if has_preedit { state.cursor_char } else { 0 };
    let mut text_hasher = FxHasher::default();
    text.hash(&mut text_hasher);
    let text_hash = text_hasher.finish();
    let cur_atlas = ui.fonts(|f| f.texture_atlas());
    let font_bits = font_id.size.to_bits();
    let ppp_bits = ui.ctx().pixels_per_point().to_bits();

    let cache_hit = state.layout_cache.as_ref().is_some_and(|c| {
        let preedit_match = if has_preedit {
            c.preedit == state.ime_preedit
        } else {
            c.preedit.is_empty() && c.display_logical == *text
        };
        c.text_hash == text_hash
            && c.text_len == text.len()
            && c.cursor_when_preedit == cursor_when_preedit
            && c.font_size_bits == font_bits
            && c.ppp_bits == ppp_bits
            && c.theme_mode == theme.mode
            && c.atlas.as_ptr() == Arc::as_ptr(&cur_atlas)
            && preedit_match
    });

    if !cache_hit {
        let char_len = text.chars().count();
        state.clamp_cursor(char_len);
        let preedit_owned = if has_preedit {
            state.ime_preedit.clone()
        } else {
            String::new()
        };
        let display_logical = if !preedit_owned.is_empty() {
            let mut s = text.clone();
            let b = char_to_byte_idx(&s, state.cursor_char);
            s.insert_str(b, &preedit_owned);
            s
        } else {
            text.clone()
        };
        let bidi_map = Arc::new(build_visual_bidi_line_map(&display_logical));
        let galley = if !display_logical.is_empty() {
            Some(ui.fonts(|f| {
                f.layout_no_wrap(
                    bidi_map.visual_text.clone(),
                    font_id.clone(),
                    theme.text_primary,
                )
            }))
        } else {
            None
        };
        state.layout_cache = Some(BidiInputLayoutCache {
            text_hash,
            text_len: text.len(),
            preedit: preedit_owned,
            cursor_when_preedit,
            font_size_bits: font_bits,
            ppp_bits,
            theme_mode: theme.mode,
            atlas: Arc::downgrade(&cur_atlas),
            display_logical,
            char_len,
            bidi_map,
            galley,
        });
    }

    let (char_len, display_logical, bidi_map, galley) = {
        let cached = state.layout_cache.as_ref().expect("layout cache populated");
        (
            cached.char_len,
            cached.display_logical.clone(),
            Arc::clone(&cached.bidi_map),
            cached.galley.clone(),
        )
    };
    state.clamp_cursor(char_len);

    let galley_w = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
    let base_origin_x = match bidi_map.base_dir {
        TextDirection::Rtl if galley_w < inner_rect.width() => inner_rect.right() - galley_w,
        _ => inner_rect.left(),
    };

    let logical_cursor_in_display = if focused && !state.ime_preedit.is_empty() {
        state.cursor_char + state.ime_preedit.chars().count()
    } else {
        state.cursor_char
    };
    let vis_cursor = bidi_map.visual_col_for_logical(logical_cursor_in_display);
    let cursor_local_x = galley
        .as_ref()
        .map(|g| g.pos_from_ccursor(egui::text::CCursor::new(vis_cursor)).min.x)
        .unwrap_or(0.0);

    if galley_w <= inner_rect.width() {
        state.horiz_offset = 0.0;
    } else {
        let max_off = (galley_w - inner_rect.width()).max(0.0);
        if cursor_local_x - state.horiz_offset < 0.0 {
            state.horiz_offset = cursor_local_x.max(0.0);
        } else if cursor_local_x - state.horiz_offset > inner_rect.width() - 2.0 {
            state.horiz_offset = (cursor_local_x - (inner_rect.width() - 2.0)).clamp(0.0, max_off);
        } else {
            state.horiz_offset = state.horiz_offset.clamp(0.0, max_off);
        }
    }
    let text_origin_x = base_origin_x - state.horiz_offset;

    // Pointer click / double-click / drag selection
    if let Some(pos) = response.interact_pointer_pos() {
        let clicked_col = if let Some(ref g) = galley {
            let rel_x = (pos.x - text_origin_x).max(0.0);
            let vis_col = g
                .cursor_from_pos(Vec2::new(rel_x, g.size().y * 0.5))
                .ccursor
                .index;
            let raw_log = bidi_map.logical_col_for_visual(&display_logical, vis_col);
            snap_to_grapheme_start(text, raw_log.min(char_len))
        } else {
            0
        };

        if response.triple_clicked() {
            state.selection_anchor = Some(0);
            state.cursor_char = char_len;
            state.dragging = false;
        } else if response.double_clicked() {
            if let Some(wr) = word_range_in_line(text, clicked_col) {
                state.selection_anchor = Some(snap_to_grapheme_start(text, wr.start));
                state.cursor_char = snap_to_grapheme_end(text, wr.end);
            } else {
                state.selection_anchor = Some(0);
                state.cursor_char = char_len;
            }
            state.dragging = false;
        } else if response.drag_started() {
            let shift = ui.input(|i| i.modifiers.shift);
            state.set_cursor(clicked_col, shift, char_len);
            state.dragging = true;
        } else if response.dragged() && state.dragging {
            state.set_cursor(clicked_col, true, char_len);
        } else if response.clicked() {
            let shift = ui.input(|i| i.modifiers.shift);
            state.set_cursor(clicked_col, shift, char_len);
        }
    }
    if response.drag_stopped() {
        state.dragging = false;
    }

    // Paint frame, text, selection, and caret
    let bg = theme.paper_bg;
    let stroke = if focused {
        Stroke::new(1.5_f32, theme.accent)
    } else {
        Stroke::new(1.0_f32, theme.border)
    };
    ui.painter()
        .rect(rect, CornerRadius::same(4), bg, stroke, egui::StrokeKind::Inside);

    let clip = ui.painter_at(inner_rect.expand2(Vec2::new(2.0, 0.0)));
    let text_y = if let Some(ref g) = galley {
        inner_rect.center().y - g.size().y * 0.5
    } else {
        inner_rect.top() + 1.0
    };

    if let Some(ref g) = galley {
        if focused
            && let Some(sel) = state.selection_range(char_len) {
                for vr in bidi_map.visual_ranges_for_logical_span(sel.start, sel.end) {
                    let x1 = g.pos_from_ccursor(egui::text::CCursor::new(vr.start)).min.x;
                    let x2 = g.pos_from_ccursor(egui::text::CCursor::new(vr.end)).min.x;
                    let lo = text_origin_x + x1.min(x2);
                    let hi = text_origin_x + x1.max(x2);
                    if hi > lo {
                        clip.rect_filled(
                            egui::Rect::from_min_max(
                                egui::pos2(lo, inner_rect.top()),
                                egui::pos2(hi, inner_rect.bottom()),
                            ),
                            2.0,
                            theme.selection_bg,
                        );
                    }
                }
            }
        clip.galley(egui::pos2(text_origin_x, text_y), g.clone(), theme.text_primary);
    } else {
        let hint_galley = ui.fonts(|f| {
            f.layout_no_wrap(hint_text.to_string(), font_id, theme.text_muted)
        });
        let hint_y = inner_rect.center().y - hint_galley.size().y * 0.5;
        clip.galley(
            egui::pos2(inner_rect.left(), hint_y),
            hint_galley,
            theme.text_muted,
        );
    }

    if focused {
        let cx = (text_origin_x + cursor_local_x).clamp(inner_rect.left(), inner_rect.right());
        clip.line_segment(
            [
                egui::pos2(cx, inner_rect.top() + 1.0),
                egui::pos2(cx, inner_rect.bottom() - 1.0),
            ],
            Stroke::new(1.5_f32, theme.accent),
        );
    }

    (response, changed)
}

const ASYNC_SEARCH_BYTES_THRESHOLD: usize = 256_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchKey {
    buf_id: u64,
    buf_version: u64,
    query: String,
    opts: SearchOptions,
}

struct SearchJob {
    key: SearchKey,
    rope: Rope,
    repaint_ctx: Option<egui::Context>,
    cancel: Arc<AtomicBool>,
}

struct SearchJobResult {
    key: SearchKey,
    res: SearchResult,
}

#[derive(Debug)]
struct BackgroundSearcher {
    tx: Option<Sender<SearchJob>>,
    rx: Option<Receiver<SearchJobResult>>,
    in_flight: Option<SearchKey>,
    active_cancel: Arc<AtomicBool>,
}

impl Default for BackgroundSearcher {
    fn default() -> Self {
        Self {
            tx: None,
            rx: None,
            in_flight: None,
            active_cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl BackgroundSearcher {
    fn cancel_in_flight(&mut self) {
        self.active_cancel.store(true, Ordering::Release);
        self.in_flight = None;
    }

    fn request(&mut self, key: SearchKey, rope: Rope, repaint_ctx: Option<egui::Context>) {
        if self.in_flight.as_ref() == Some(&key) {
            return;
        }
        self.active_cancel.store(true, Ordering::Release);
        let cancel = Arc::new(AtomicBool::new(false));
        self.active_cancel = Arc::clone(&cancel);

        if let Some(ref tx) = self.tx
            && tx
                .send(SearchJob {
                    key: key.clone(),
                    rope: rope.clone(),
                    repaint_ctx: repaint_ctx.clone(),
                    cancel: Arc::clone(&cancel),
                })
                .is_ok()
        {
            self.in_flight = Some(key);
            return;
        }
        let (job_tx, job_rx) = mpsc::channel::<SearchJob>();
        let (res_tx, res_rx) = mpsc::channel::<SearchJobResult>();
        if job_tx
            .send(SearchJob {
                key: key.clone(),
                rope,
                repaint_ctx,
                cancel,
            })
            .is_err()
        {
            return;
        }
        if thread::Builder::new()
            .name("md-bg-search".to_string())
            .spawn(move || {
                while let Ok(mut job) = job_rx.recv() {
                    while let Ok(newer) = job_rx.try_recv() {
                        job = newer;
                    }
                    if job.cancel.load(Ordering::Relaxed) {
                        continue;
                    }
                    let res = find_all_matches_in_rope_cancellable(
                        &job.rope,
                        &job.key.query,
                        &job.key.opts,
                        Some(job.cancel.as_ref()),
                    );
                    if job.cancel.load(Ordering::Relaxed) {
                        continue;
                    }
                    let repaint = job.repaint_ctx.clone();
                    if res_tx
                        .send(SearchJobResult {
                            key: job.key,
                            res,
                        })
                        .is_err()
                    {
                        break;
                    }
                    if let Some(c) = repaint {
                        c.request_repaint();
                    }
                }
            })
            .is_ok()
        {
            self.tx = Some(job_tx);
            self.rx = Some(res_rx);
            self.in_flight = Some(key);
        }
    }

    fn poll_latest(&mut self, expected: &SearchKey) -> Option<SearchResult> {
        let rx = self.rx.as_ref()?;
        let mut matched: Option<SearchResult> = None;
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(item) => {
                    if self.in_flight.as_ref() == Some(&item.key) {
                        self.in_flight = None;
                    }
                    if &item.key == expected {
                        matched = Some(item.res);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            self.tx = None;
            self.rx = None;
            self.in_flight = None;
        }
        matched
    }

    fn is_pending(&self, key: &SearchKey) -> bool {
        self.in_flight.as_ref() == Some(key)
    }
}

impl Drop for BackgroundSearcher {
    fn drop(&mut self) {
        self.active_cancel.store(true, Ordering::Release);
        drop(self.tx.take());
        drop(self.rx.take());
    }
}

/// Unified Find & Replace bar state across WYSIWYG, Source, and Split modes.
#[derive(Debug, Default)]
pub struct FindReplaceBar {
    pub open: bool,
    pub show_replace: bool,
    pub focus_query: bool,
    pub focus_replace: bool,
    pub last_focused_replace: bool,
    pub query: String,
    pub replacement: String,
    pub query_input_state: BidiInputState,
    pub replace_input_state: BidiInputState,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
    pub in_selection: bool,
    pub selection_scope: Option<Range<usize>>,
    pub candidate_selection: Option<Range<usize>>,
    pub matches: Arc<Vec<Range<usize>>>,
    pub active_match_idx: Option<usize>,
    pub regex_error: Option<String>,
    pub status: String,
    pub last_buf_version: u64,
    last_search_key: Option<SearchKey>,
    bg_searcher: BackgroundSearcher,
}

impl FindReplaceBar {
    pub fn has_input_focus(&self, ctx: &egui::Context) -> bool {
        if !self.open {
            return false;
        }
        let find_id = egui::Id::new("unified_find_input");
        let rep_id = egui::Id::new("unified_replace_input");
        ctx.memory(|m| m.has_focus(find_id) || m.has_focus(rep_id))
    }

    pub fn search_options(&self) -> SearchOptions {
        SearchOptions {
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            use_regex: self.use_regex,
            selection_scope: if self.in_selection {
                self.selection_scope.clone()
            } else {
                None
            },
        }
    }

    fn sync_selection_context(&mut self, buf: &DocumentBuffer) {
        self.status.clear();
        if let Some(sel) = buf.selection_range() {
            self.candidate_selection = Some(sel.clone());
            if self.in_selection && self.selection_scope.is_none() {
                self.selection_scope = Some(sel.clone());
            }
            if !self.in_selection
                && sel.end.saturating_sub(sel.start) <= 160
                && !self.matches.contains(&sel)
            {
                let selected = buf.rope().slice(sel).to_string();
                if !selected.is_empty()
                    && !selected.contains('\n')
                    && !selected.contains('\r')
                    && selected.chars().count() <= 160
                {
                    self.query = selected;
                }
            }
        }
        let q_len = self.query.chars().count();
        if q_len > 0 {
            self.query_input_state.selection_anchor = Some(0);
            self.query_input_state.cursor_char = q_len;
        }
    }

    pub fn open_find(&mut self, buf: &DocumentBuffer) {
        self.open = true;
        self.show_replace = false;
        self.focus_query = true;
        self.sync_selection_context(buf);
        self.ensure_matches_fresh_with_ctx(buf, None);
    }

    pub fn open_find_with_query(&mut self, buf: &DocumentBuffer, query: &str) {
        self.open = true;
        self.show_replace = false;
        self.focus_query = true;
        self.in_selection = false;
        self.selection_scope = None;
        let first_line = query
            .lines()
            .next()
            .unwrap_or(query)
            .trim()
            .chars()
            .take(160)
            .collect::<String>();
        self.query = first_line;
        let q_len = self.query.chars().count();
        self.query_input_state.selection_anchor = if q_len > 0 { Some(0) } else { None };
        self.query_input_state.cursor_char = q_len;
        self.status.clear();
        self.ensure_matches_fresh_with_ctx(buf, None);
    }

    pub fn focus_or_open_find(&mut self, buf: &DocumentBuffer) {
        if !self.open {
            self.open_find(buf);
        } else {
            self.focus_query = true;
            self.sync_selection_context(buf);
            self.ensure_matches_fresh_with_ctx(buf, None);
        }
    }

    pub fn open_replace(&mut self, buf: &DocumentBuffer) {
        self.open = true;
        self.show_replace = true;
        self.focus_query = true;
        self.sync_selection_context(buf);
        self.ensure_matches_fresh_with_ctx(buf, None);
    }

    pub fn reset_for_new_document(&mut self) {
        self.matches = Arc::default();
        self.active_match_idx = None;
        self.selection_scope = None;
        self.candidate_selection = None;
        self.in_selection = false;
        self.regex_error = None;
        self.status.clear();
        self.last_buf_version = 0;
        self.last_search_key = None;
        self.bg_searcher.cancel_in_flight();
    }

    pub fn active_match_range(&self) -> Option<Range<usize>> {
        self.active_match_idx
            .and_then(|i| self.matches.get(i).cloned())
    }

    fn make_search_key(&self, buf: &DocumentBuffer) -> SearchKey {
        SearchKey {
            buf_id: buf.buf_id(),
            buf_version: buf.version,
            query: self.query.clone(),
            opts: self.search_options(),
        }
    }

    fn ensure_matches_fresh(&mut self, buf: &DocumentBuffer) {
        let key = self.make_search_key(buf);
        if self.last_search_key.as_ref() != Some(&key) {
            self.refresh_matches(buf);
        }
    }

    pub fn ensure_matches_fresh_with_ctx(
        &mut self,
        buf: &DocumentBuffer,
        ctx: Option<&egui::Context>,
    ) {
        let key = self.make_search_key(buf);
        if let Some(res) = self.bg_searcher.poll_latest(&key) {
            self.apply_search_result(buf, key, res);
            return;
        }
        if self.last_search_key.as_ref() == Some(&key) {
            return;
        }
        if buf.len_bytes() < ASYNC_SEARCH_BYTES_THRESHOLD || self.query.is_empty() {
            self.refresh_matches(buf);
        } else {
            self.bg_searcher
                .request(key.clone(), buf.rope().clone(), ctx.cloned());
            if self.bg_searcher.is_pending(&key)
                && let Some(c) = ctx {
                    c.request_repaint_after(Duration::from_millis(30));
                }
        }
    }

    fn apply_search_result(
        &mut self,
        buf: &DocumentBuffer,
        key: SearchKey,
        res: SearchResult,
    ) {
        self.matches = Arc::new(res.matches);
        self.regex_error = res.error;
        self.last_buf_version = key.buf_version;
        self.last_search_key = Some(key);

        if self.matches.is_empty() {
            self.active_match_idx = None;
            return;
        }

        if let Some(sel) = buf.selection_range() {
            let p = self.matches.partition_point(|m| m.start < sel.start);
            if p < self.matches.len() && self.matches[p] == sel {
                self.active_match_idx = Some(p);
                return;
            }
        }

        let cur = buf.cursor_char;
        let idx = self.matches.partition_point(|m| m.start < cur);
        self.active_match_idx = Some(if idx < self.matches.len() { idx } else { 0 });
    }

    pub fn refresh_matches(&mut self, buf: &DocumentBuffer) {
        let key = self.make_search_key(buf);
        self.bg_searcher.cancel_in_flight();
        let res = find_all_matches(buf, &key.query, &key.opts);
        self.apply_search_result(buf, key, res);
    }

    fn select_match(&mut self, buf: &mut DocumentBuffer, idx: usize) -> bool {
        let Some(m) = self.matches.get(idx).cloned() else {
            return false;
        };
        self.active_match_idx = Some(idx);
        buf.selection_anchor = Some(m.start);
        buf.cursor_char = m.end;
        buf.cache_current_selection();
        self.status.clear();
        true
    }

    pub fn find_next(&mut self, buf: &mut DocumentBuffer) -> bool {
        self.ensure_matches_fresh(buf);
        if self.matches.is_empty() {
            return false;
        }
        let next_idx = if let Some(sel) = buf.selection_range() {
            let p = self.matches.partition_point(|m| m.start < sel.start);
            if p < self.matches.len() && self.matches[p] == sel {
                (p + 1) % self.matches.len()
            } else {
                let q = self.matches.partition_point(|m| m.start < sel.end);
                if q < self.matches.len() { q } else { 0 }
            }
        } else {
            let p = self.matches.partition_point(|m| m.start < buf.cursor_char);
            if p < self.matches.len() { p } else { 0 }
        };
        self.select_match(buf, next_idx)
    }

    pub fn find_prev(&mut self, buf: &mut DocumentBuffer) -> bool {
        self.ensure_matches_fresh(buf);
        if self.matches.is_empty() {
            return false;
        }
        let prev_idx = if let Some(sel) = buf.selection_range() {
            let p = self.matches.partition_point(|m| m.start < sel.start);
            if p < self.matches.len() && self.matches[p] == sel {
                if p == 0 {
                    self.matches.len() - 1
                } else {
                    p - 1
                }
            } else if p > 0 {
                p - 1
            } else {
                self.matches.len() - 1
            }
        } else {
            let p = self.matches.partition_point(|m| m.start < buf.cursor_char);
            if p > 0 {
                p - 1
            } else {
                self.matches.len() - 1
            }
        };
        self.select_match(buf, prev_idx)
    }

    pub fn replace_current(&mut self, buf: &mut DocumentBuffer) -> bool {
        self.ensure_matches_fresh(buf);
        if self.matches.is_empty() {
            return false;
        }
        let target = if let Some(sel) = buf.selection_range() {
            let p = self.matches.partition_point(|m| m.start < sel.start);
            if p < self.matches.len() && self.matches[p] == sel {
                Some(sel)
            } else {
                self.active_match_idx.and_then(|i| self.matches.get(i).cloned())
            }
        } else {
            self.active_match_idx.and_then(|i| self.matches.get(i).cloned())
        };
        let Some(m) = target else {
            return false;
        };
        let old_len = m.end.saturating_sub(m.start);
        let opts = self.search_options();
        let inserted_range =
            match replace_single_match_with_opts(buf, &self.query, m, &self.replacement, &opts) {
                Ok(r) => r,
                Err(err) => {
                    self.regex_error = Some(err);
                    return false;
                }
            };
        let new_len = inserted_range.end.saturating_sub(inserted_range.start);
        if let Some(ref mut scope) = self.selection_scope {
            let delta = new_len as isize - old_len as isize;
            let adjusted = (scope.end as isize + delta).max(scope.start as isize) as usize;
            scope.end = adjusted.min(buf.len_chars());
        }
        buf.selection_anchor = Some(inserted_range.start);
        buf.cursor_char = inserted_range.end;
        buf.cache_current_selection();
        self.refresh_matches(buf);
        self.status = "Replaced 1".to_string();
        true
    }

    pub fn replace_and_next(&mut self, buf: &mut DocumentBuffer) -> bool {
        if !self.replace_current(buf) {
            return false;
        }
        self.find_next(buf);
        true
    }

    pub fn replace_all(&mut self, buf: &mut DocumentBuffer) -> usize {
        self.ensure_matches_fresh(buf);
        let old_len_sum: usize = self.matches.iter().map(|m| m.end - m.start).sum();
        match replace_all_matches(buf, &self.query, &self.replacement, &self.search_options()) {
            Ok((count, new_ranges)) => {
                if count > 0 {
                    if let Some(ref mut scope) = self.selection_scope {
                        let new_len_sum: usize =
                            new_ranges.iter().map(|r| r.end.saturating_sub(r.start)).sum();
                        let delta = new_len_sum as isize - old_len_sum as isize;
                        let adjusted =
                            (scope.end as isize + delta).max(scope.start as isize) as usize;
                        scope.end = adjusted.min(buf.len_chars());
                    }
                    self.status = format!(
                        "Replaced {count} occurrence{}",
                        if count == 1 { "" } else { "s" }
                    );
                } else {
                    self.status = "0 replacements".to_string();
                }
                self.refresh_matches(buf);
                count
            }
            Err(err) => {
                self.regex_error = Some(err);
                self.status.clear();
                0
            }
        }
    }

    /// Renders the unified Find & Replace bar and returns `true` if cursor jumped or text changed.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        buf: &mut DocumentBuffer,
        theme: &WordTheme,
        read_only: bool,
    ) -> bool {
        if !self.open {
            return false;
        }
        let mut jumped = false;

        self.ensure_matches_fresh_with_ctx(buf, Some(ui.ctx()));

        let toggle_pill = |ui: &mut egui::Ui, label: &str, active: &mut bool, tip: &str| -> bool {
            let btn = egui::Button::new(
                RichText::new(label)
                    .size(11.5)
                    .strong()
                    .color(if *active {
                        theme.accent
                    } else {
                        theme.text_muted
                    }),
            )
            .fill(if *active {
                theme.accent_soft
            } else {
                Color32::TRANSPARENT
            })
            .stroke(Stroke::new(
                1.0_f32,
                if *active { theme.accent } else { theme.border },
            ))
            .corner_radius(CornerRadius::same(4))
            .wrap_mode(egui::TextWrapMode::Extend);
            let resp = ui.add(btn).on_hover_text(tip);
            if resp.clicked() {
                *active = !*active;
                true
            } else {
                false
            }
        };

        egui::Frame::new()
            .fill(theme.ribbon_bg)
            .stroke(Stroke::new(1.0_f32, theme.border))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let find_id = egui::Id::new("unified_find_input");
                let replace_id = egui::Id::new("unified_replace_input");
                if ui.memory(|m| m.has_focus(find_id)) {
                    self.last_focused_replace = false;
                } else if ui.memory(|m| m.has_focus(replace_id)) {
                    self.last_focused_replace = true;
                }
                let mut keep_focus_id: Option<egui::Id> = None;

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                    ui.label(RichText::new("Find:").size(12.5).strong());
                    let focus_q = self.focus_query;
                    self.focus_query = false;
                    let (find_resp, find_changed) = render_bidi_singleline_input(
                        ui,
                        "unified_find_input",
                        &mut self.query,
                        &mut self.query_input_state,
                        210.0,
                        "Search text or pattern...",
                        theme,
                        focus_q,
                    );
                    if find_changed {
                        self.status.clear();
                        self.last_search_key = None;
                        self.ensure_matches_fresh_with_ctx(buf, Some(ui.ctx()));
                    }

                    // Enter = Find Next, Shift+Enter = Find Previous
                    if ui.memory(|m| m.has_focus(find_resp.id))
                        && ui.input(|i| i.key_pressed(Key::Enter))
                    {
                        let shift = ui.input(|i| i.modifiers.shift);
                        jumped = if shift {
                            self.find_prev(buf)
                        } else {
                            self.find_next(buf)
                        };
                    }

                    if toggle_pill(ui, "Aa", &mut self.case_sensitive, "Match Case") {
                        self.status.clear();
                        self.last_search_key = None;
                        self.ensure_matches_fresh_with_ctx(buf, Some(ui.ctx()));
                        keep_focus_id = Some(find_id);
                    }
                    if toggle_pill(
                        ui,
                        "Word",
                        &mut self.whole_word,
                        "Exact / Whole Word (Unicode word boundaries)",
                    ) {
                        self.status.clear();
                        self.last_search_key = None;
                        self.ensure_matches_fresh_with_ctx(buf, Some(ui.ctx()));
                        keep_focus_id = Some(find_id);
                    }
                    if toggle_pill(ui, ".*", &mut self.use_regex, "Regular Expression") {
                        self.status.clear();
                        self.last_search_key = None;
                        self.ensure_matches_fresh_with_ctx(buf, Some(ui.ctx()));
                        keep_focus_id = Some(find_id);
                    }
                    if toggle_pill(
                        ui,
                        "Selection",
                        &mut self.in_selection,
                        "Search within selected text only",
                    ) {
                        if self.in_selection {
                            self.selection_scope = buf
                                .selection_range()
                                .or_else(|| self.candidate_selection.clone());
                        }
                        self.status.clear();
                        self.last_search_key = None;
                        self.ensure_matches_fresh_with_ctx(buf, Some(ui.ctx()));
                        keep_focus_id = Some(find_id);
                    }

                    if ui.button("◀ Prev").clicked() {
                        if self.find_prev(buf) {
                            jumped = true;
                        }
                        keep_focus_id = Some(find_id);
                    }
                    if ui.button("Next ▶").clicked() {
                        if self.find_next(buf) {
                            jumped = true;
                        }
                        keep_focus_id = Some(find_id);
                    }

                    // Match counter badge or inline regex error
                    if let Some(ref err) = self.regex_error {
                        ui.label(
                            RichText::new(err)
                                .size(11.5)
                                .color(Color32::from_rgb(200, 48, 48)),
                        );
                    } else if !self.query.is_empty() {
                        let pending = self.bg_searcher.is_pending(&self.make_search_key(buf));
                        let count_text = if pending {
                            "Searching...".to_string()
                        } else if self.matches.is_empty() {
                            "0 of 0".to_string()
                        } else {
                            let cur = self.active_match_idx.unwrap_or(0) + 1;
                            format!("{cur} of {}", self.matches.len())
                        };
                        ui.label(RichText::new(count_text).size(12.0).color(theme.text_muted));
                    }

                    if !read_only {
                        let rep_lbl = if self.show_replace {
                            "Replace ▴"
                        } else {
                            "Replace ▾"
                        };
                        if toggle_pill(ui, rep_lbl, &mut self.show_replace, "Toggle Replace (Ctrl+H)")
                        {
                            keep_focus_id = Some(if self.show_replace {
                                replace_id
                            } else {
                                find_id
                            });
                        }
                    }

                    if !self.show_replace && !self.status.is_empty() {
                        ui.label(RichText::new(&self.status).size(11.5).color(theme.accent));
                    }

                    if ui.button("×").on_hover_text("Close (Esc)").clicked() {
                        self.open = false;
                    }
                });

                if !read_only && self.show_replace {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                        ui.label(RichText::new("Replace:").size(12.5).strong());
                        let focus_r = self.focus_replace;
                        self.focus_replace = false;
                        let (rep_resp, _) = render_bidi_singleline_input(
                            ui,
                            "unified_replace_input",
                            &mut self.replacement,
                            &mut self.replace_input_state,
                            210.0,
                            "Replace with...",
                            theme,
                            focus_r,
                        );
                        if ui.memory(|m| m.has_focus(rep_resp.id))
                            && ui.input(|i| i.key_pressed(Key::Enter))
                            && self.replace_and_next(buf)
                        {
                            jumped = true;
                        }
                        if ui.button("Replace").clicked() {
                            if self.replace_current(buf) {
                                jumped = true;
                            }
                            keep_focus_id = Some(if self.last_focused_replace {
                                replace_id
                            } else {
                                find_id
                            });
                        }
                        if ui.button("Replace & Next").clicked() {
                            if self.replace_and_next(buf) {
                                jumped = true;
                            }
                            keep_focus_id = Some(if self.last_focused_replace {
                                replace_id
                            } else {
                                find_id
                            });
                        }
                        if ui.button("Replace All").clicked() {
                            if self.replace_all(buf) > 0 {
                                jumped = true;
                            }
                            keep_focus_id = Some(if self.last_focused_replace {
                                replace_id
                            } else {
                                find_id
                            });
                        }
                        if !self.status.is_empty() {
                            ui.label(RichText::new(&self.status).size(11.5).color(theme.accent));
                        }
                    });
                }

                if let Some(id) = keep_focus_id {
                    ui.memory_mut(|m| m.request_focus(id));
                }
            });

        jumped
    }
}

/// Persistent UI state for the interactive status-bar writing goal popover.
#[derive(Debug, Default)]
pub struct StatusBarGoalState {
    pub open: bool,
    pub word_draft: String,
    pub char_draft: String,
    anchor_rect: Option<egui::Rect>,
}

/// Formats an estimated reading duration at 200 WPM.
pub fn format_reading_time_label(word_count: usize) -> String {
    if word_count == 0 {
        "0 min read".to_string()
    } else if word_count < 100 {
        "< 1 min read".to_string()
    } else {
        format!("~{} min read", word_count.div_ceil(200))
    }
}

/// Renders the bottom status bar with cursor stats, reading progress, writing goals, and background export/print progress.
/// Returns `true` when `word_goal` or `char_goal` was updated by the user.
#[allow(clippy::too_many_arguments)]
pub fn render_status_bar(
    ui: &mut egui::Ui,
    buf: &DocumentBuffer,
    view_mode: ViewMode,
    word_count: usize,
    zoom_factor: f32,
    reading_progress: Option<f32>,
    word_goal: &mut usize,
    char_goal: &mut usize,
    goal_state: &mut StatusBarGoalState,
    exporter: &BackgroundExporter,
    theme: &WordTheme,
) -> bool {
    let mut goals_changed = false;
    let char_count = buf.len_chars();
    let mut clicked_goal_rect: Option<egui::Rect> = None;

    egui::Frame::new()
        .fill(theme.ribbon_bg)
        .stroke(Stroke::new(1.0_f32, theme.border))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            let avail_w = ui.available_width();
            ui.set_min_width(avail_w);
            let mut draw_stats = |ui: &mut egui::Ui| {
                let (line, col) = buf.char_to_line_col(buf.cursor_char);
                let file_label = buf
                    .file_path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Untitled.md".to_string());
                let (status_badge, status_color) = if buf.dirty {
                    ("● Unsaved", theme.warning_amber)
                } else {
                    ("✓ Saved", theme.success_green)
                };

                ui.label(
                    RichText::new(file_label)
                        .strong()
                        .size(12.0)
                        .color(theme.text_primary),
                );
                ui.label(
                    RichText::new(status_badge)
                        .strong()
                        .size(11.5)
                        .color(status_color),
                );
                ui.separator();
                ui.label(
                    RichText::new(format!("Ln {}, Col {}", line + 1, col + 1))
                        .size(12.0)
                        .color(theme.text_muted),
                );
                ui.separator();
                let read_label = format_reading_time_label(word_count);
                let eol_label = if buf.line_ending() == "\r\n" {
                    "CRLF"
                } else {
                    "LF"
                };
                let stats_text = if let Some(p) = reading_progress.filter(|_| word_count > 0) {
                    let pct = (p * 100.0).round().clamp(0.0, 100.0) as usize;
                    let rem_mins = if pct >= 99 {
                        0
                    } else {
                        ((word_count as f32 * (1.0 - p)).ceil() as usize).div_ceil(200)
                    };
                    format!(
                        "{word_count} words · {char_count} chars ({read_label} · ~{rem_mins} min left · {pct}% read) · {} lines · {} · {eol_label} · {}%",
                        buf.len_lines(),
                        format_bytes(buf.len_bytes()),
                        (zoom_factor * 100.0).round() as usize
                    )
                } else {
                    format!(
                        "{word_count} words · {char_count} chars ({read_label}) · {} lines · {} · {eol_label} · {}%",
                        buf.len_lines(),
                        format_bytes(buf.len_bytes()),
                        (zoom_factor * 100.0).round() as usize
                    )
                };
                ui.label(
                    RichText::new(stats_text)
                        .size(12.0)
                        .color(theme.text_muted),
                );

                ui.separator();
                let has_goal = *word_goal > 0 || *char_goal > 0;
                let (goal_label, goal_frac) = match (*word_goal, *char_goal) {
                    (wg, cg) if wg > 0 && cg > 0 => {
                        let wp = ((word_count as f32 / wg as f32) * 100.0).round() as usize;
                        let cp = ((char_count as f32 / cg as f32) * 100.0).round() as usize;
                        (
                            format!("{word_count}/{wg}w ({wp}%) · {char_count}/{cg}c ({cp}%)"),
                            (word_count as f32 / wg as f32).clamp(0.0, 1.0),
                        )
                    }
                    (wg, _) if wg > 0 => {
                        let wp = ((word_count as f32 / wg as f32) * 100.0).round() as usize;
                        (
                            format!("Goal {word_count}/{wg}w ({wp}%)"),
                            (word_count as f32 / wg as f32).clamp(0.0, 1.0),
                        )
                    }
                    (_, cg) if cg > 0 => {
                        let cp = ((char_count as f32 / cg as f32) * 100.0).round() as usize;
                        (
                            format!("Goal {char_count}/{cg}c ({cp}%)"),
                            (char_count as f32 / cg as f32).clamp(0.0, 1.0),
                        )
                    }
                    _ => ("🎯 Goal".to_string(), 0.0),
                };
                let goal_done = has_goal
                    && (*word_goal == 0 || word_count >= *word_goal)
                    && (*char_goal == 0 || char_count >= *char_goal);
                let goal_fg = if goal_done {
                    theme.success_green
                } else if has_goal {
                    theme.accent
                } else {
                    theme.text_muted
                };
                let goal_resp = vivid_button(
                    ui,
                    RichText::new(goal_label).size(11.5).color(goal_fg),
                    Vec2::ZERO,
                    5,
                    goal_state.open || has_goal,
                    true,
                    ToolbarButtonKind::Subtle,
                    "Set Word & Character writing goals",
                    theme,
                );
                if goal_resp.clicked() {
                    clicked_goal_rect = Some(goal_resp.rect);
                }
                if has_goal {
                    let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(36.0, 6.0), Sense::hover());
                    ui.painter().rect_filled(bar_rect, 3.0, theme.border);
                    if goal_frac > 0.0 {
                        let fill_w = (bar_rect.width() * goal_frac).max(3.0).min(bar_rect.width());
                        let fill_rect = egui::Rect::from_min_size(
                            bar_rect.min,
                            Vec2::new(fill_w, bar_rect.height()),
                        );
                        let fill_col = if goal_done {
                            theme.success_green
                        } else {
                            theme.accent
                        };
                        ui.painter().rect_filled(fill_rect, 3.0, fill_col);
                    }
                }

                ui.separator();
                let mode_str = match view_mode {
                    ViewMode::Wysiwyg => "WYSIWYG",
                    ViewMode::Split => "Split View",
                    ViewMode::Reader => "Reader",
                    ViewMode::Source => "Source",
                };
                ui.label(
                    RichText::new(mode_str)
                        .size(12.0)
                        .color(theme.text_muted),
                );
                ui.separator();
                ui.label(
                    RichText::new(format!("v{}", crate::APP_VERSION))
                        .size(11.5)
                        .color(theme.text_muted),
                );
            };

            let draw_export_status = |ui: &mut egui::Ui, rtl: bool| {
                if exporter.busy {
                    if rtl {
                        ui.add_sized(
                            Vec2::new(140.0, 14.0),
                            ProgressBar::new(exporter.progress).show_percentage(),
                        );
                        ui.label(
                            RichText::new(&exporter.status_text)
                                .size(12.0)
                                .color(theme.accent),
                        );
                    } else {
                        ui.label(
                            RichText::new(&exporter.status_text)
                                .size(12.0)
                                .color(theme.accent),
                        );
                        ui.add_sized(
                            Vec2::new(140.0, 14.0),
                            ProgressBar::new(exporter.progress).show_percentage(),
                        );
                    }
                } else if !exporter.status_text.is_empty() {
                    ui.label(
                        RichText::new(&exporter.status_text)
                            .size(12.0)
                            .color(theme.text_muted),
                    );
                }
            };

            if avail_w >= 820.0 {
                ui.horizontal(|ui| {
                    draw_stats(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        draw_export_status(ui, true);
                    });
                });
            } else {
                ui.horizontal_wrapped(|ui| {
                    draw_stats(ui);
                    if exporter.busy || !exporter.status_text.is_empty() {
                        ui.separator();
                        draw_export_status(ui, false);
                    }
                });
            }
        });

    if let Some(rect) = clicked_goal_rect {
        goal_state.open = !goal_state.open;
        if goal_state.open {
            goal_state.word_draft = if *word_goal > 0 {
                word_goal.to_string()
            } else {
                String::new()
            };
            goal_state.char_draft = if *char_goal > 0 {
                char_goal.to_string()
            } else {
                String::new()
            };
            goal_state.anchor_rect = Some(rect);
        }
    }

    if goal_state.open {
        if ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            goal_state.open = false;
            return false;
        }
        let screen = ui.ctx().screen_rect();
        let anchor = goal_state.anchor_rect.unwrap_or_else(|| {
            egui::Rect::from_min_size(
                egui::pos2(screen.center().x - 100.0, screen.bottom() - 28.0),
                Vec2::new(80.0, 22.0),
            )
        });
        let popup_x = anchor.left().clamp(screen.left() + 8.0, (screen.right() - 290.0).max(8.0));
        let popup_bottom = (anchor.top() - 6.0).max(160.0);
        egui::Area::new(egui::Id::new("writing_goal_popover"))
            .order(egui::Order::Foreground)
            .pivot(Align2::LEFT_BOTTOM)
            .fixed_pos(egui::pos2(popup_x, popup_bottom))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme.paper_bg)
                    .stroke(Stroke::new(1.0_f32, theme.border))
                    .corner_radius(CornerRadius::same(8))
                    .shadow(theme.paper_shadow())
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_width(268.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Writing Goals")
                                    .strong()
                                    .size(13.0)
                                    .color(theme.text_primary),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("×").clicked() {
                                    goal_state.open = false;
                                }
                            });
                        });
                        ui.add_space(6.0);
                        let mut apply_now = false;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Words:").size(12.0).color(theme.text_muted));
                            let r = ui.add_sized(
                                Vec2::new(72.0, 22.0),
                                egui::TextEdit::singleline(&mut goal_state.word_draft)
                                    .hint_text("0 = off"),
                            );
                            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                apply_now = true;
                            }
                            for (lbl, val) in [("500", 500usize), ("1k", 1000), ("2.5k", 2500)] {
                                if ui.small_button(lbl).clicked() {
                                    goal_state.word_draft = val.to_string();
                                    apply_now = true;
                                }
                            }
                        });
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Chars:").size(12.0).color(theme.text_muted));
                            let r = ui.add_sized(
                                Vec2::new(72.0, 22.0),
                                egui::TextEdit::singleline(&mut goal_state.char_draft)
                                    .hint_text("0 = off"),
                            );
                            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                apply_now = true;
                            }
                            for (lbl, val) in [("3k", 3000usize), ("5k", 5000), ("10k", 10000)] {
                                if ui.small_button(lbl).clicked() {
                                    goal_state.char_draft = val.to_string();
                                    apply_now = true;
                                }
                            }
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if vivid_button(
                                ui,
                                RichText::new("Apply").size(12.0).strong().color(Color32::WHITE),
                                Vec2::ZERO,
                                5,
                                true,
                                true,
                                ToolbarButtonKind::Primary,
                                "Save writing goals",
                                theme,
                            )
                            .clicked()
                            {
                                apply_now = true;
                            }
                            if vivid_button(
                                ui,
                                RichText::new("Clear").size(12.0).color(theme.text_primary),
                                Vec2::ZERO,
                                5,
                                false,
                                true,
                                ToolbarButtonKind::Secondary,
                                "Clear all writing goals",
                                theme,
                            )
                            .clicked()
                            {
                                goal_state.word_draft.clear();
                                goal_state.char_draft.clear();
                                *word_goal = 0;
                                *char_goal = 0;
                                goals_changed = true;
                                goal_state.open = false;
                            }
                        });
                        if apply_now {
                            let parse_goal = |raw: &str| -> usize {
                                let digits: String =
                                    raw.chars().filter(|c| c.is_ascii_digit()).collect();
                                digits.parse::<usize>().unwrap_or(0).min(10_000_000)
                            };
                            *word_goal = parse_goal(&goal_state.word_draft);
                            *char_goal = parse_goal(&goal_state.char_draft);
                            goals_changed = true;
                            goal_state.open = false;
                        }
                    });
            });
    }

    goals_changed
}

fn format_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Line classification for `ExternalDiffModal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    Separator,
}

/// Single rendered row in the scalable unified diff viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: DiffLineKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
}

/// User action chosen inside `ExternalDiffModal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalDiffChoice {
    ReloadFromDisk,
    KeepLocal,
}

const DIFF_DP_CELL_LIMIT: usize = 2_500_000;

fn hash_diff_line(s: &str) -> u64 {
    let mut h = FxHasher::default();
    s.hash(&mut h);
    h.finish()
}

fn find_unique_anchor_pairs(old_mid: &[String], new_mid: &[String]) -> Vec<(usize, usize)> {
    let mut old_pos: FxHashMap<u64, Option<usize>> = FxHashMap::default();
    for (i, line) in old_mid.iter().enumerate() {
        let h = hash_diff_line(line);
        old_pos
            .entry(h)
            .and_modify(|slot| *slot = None)
            .or_insert(Some(i));
    }
    let mut new_pos: FxHashMap<u64, Option<usize>> = FxHashMap::default();
    for (j, line) in new_mid.iter().enumerate() {
        let h = hash_diff_line(line);
        new_pos
            .entry(h)
            .and_modify(|slot| *slot = None)
            .or_insert(Some(j));
    }

    let mut candidates: Vec<(usize, usize)> = Vec::new();
    for (i, line) in old_mid.iter().enumerate() {
        let h = hash_diff_line(line);
        if let (Some(Some(oi)), Some(Some(nj))) = (old_pos.get(&h), new_pos.get(&h))
            && *oi == i
            && old_mid[*oi] == new_mid[*nj]
        {
            candidates.push((*oi, *nj));
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }

    // Standard patience-sort LIS on `nj` to keep anchor pairs strictly increasing.
    let mut piles: Vec<(usize, usize)> = Vec::new();
    let mut back: Vec<Option<usize>> = vec![None; candidates.len()];
    for (idx, &(_, nj)) in candidates.iter().enumerate() {
        let p = piles.partition_point(|&(tail_nj, _)| tail_nj < nj);
        if p > 0 {
            back[idx] = Some(piles[p - 1].1);
        }
        if p == piles.len() {
            piles.push((nj, idx));
        } else {
            piles[p] = (nj, idx);
        }
    }

    let mut anchors = Vec::with_capacity(piles.len());
    let mut cur = piles.last().map(|&(_, idx)| idx);
    while let Some(idx) = cur {
        anchors.push(candidates[idx]);
        cur = back[idx];
    }
    anchors.reverse();
    anchors
}

#[allow(clippy::too_many_arguments)]
fn append_middle_diff_rows(
    old_mid: &[String],
    new_mid: &[String],
    old_base: usize,
    new_base: usize,
    raw_rows: &mut Vec<DiffRow>,
    added_count: &mut usize,
    removed_count: &mut usize,
    depth: usize,
) {
    let m = old_mid.len();
    let n = new_mid.len();
    if m == 0 {
        for (j, line) in new_mid.iter().enumerate() {
            raw_rows.push(DiffRow {
                kind: DiffLineKind::Added,
                old_line: None,
                new_line: Some(new_base + j + 1),
                text: line.clone(),
            });
            *added_count += 1;
        }
        return;
    }
    if n == 0 {
        for (i, line) in old_mid.iter().enumerate() {
            raw_rows.push(DiffRow {
                kind: DiffLineKind::Removed,
                old_line: Some(old_base + i + 1),
                new_line: None,
                text: line.clone(),
            });
            *removed_count += 1;
        }
        return;
    }

    if m.saturating_mul(n) <= DIFF_DP_CELL_LIMIT {
        let cols = n + 1;
        let mut dp = vec![0u32; (m + 1) * cols];
        for i in (0..m).rev() {
            for j in (0..n).rev() {
                dp[i * cols + j] = if old_mid[i] == new_mid[j] {
                    dp[(i + 1) * cols + (j + 1)] + 1
                } else {
                    dp[(i + 1) * cols + j].max(dp[i * cols + (j + 1)])
                };
            }
        }
        let mut i = 0usize;
        let mut j = 0usize;
        while i < m || j < n {
            if i < m && j < n && old_mid[i] == new_mid[j] {
                raw_rows.push(DiffRow {
                    kind: DiffLineKind::Context,
                    old_line: Some(old_base + i + 1),
                    new_line: Some(new_base + j + 1),
                    text: old_mid[i].clone(),
                });
                i += 1;
                j += 1;
            } else if j < n
                && (i == m || dp[i * cols + (j + 1)] >= dp[(i + 1) * cols + j])
            {
                raw_rows.push(DiffRow {
                    kind: DiffLineKind::Added,
                    old_line: None,
                    new_line: Some(new_base + j + 1),
                    text: new_mid[j].clone(),
                });
                *added_count += 1;
                j += 1;
            } else {
                raw_rows.push(DiffRow {
                    kind: DiffLineKind::Removed,
                    old_line: Some(old_base + i + 1),
                    new_line: None,
                    text: old_mid[i].clone(),
                });
                *removed_count += 1;
                i += 1;
            }
        }
        return;
    }

    // Large region: anchor on unique matching lines (Patience diff) and recurse on gaps.
    if depth < 8 {
        let anchors = find_unique_anchor_pairs(old_mid, new_mid);
        if !anchors.is_empty() {
            let mut prev_i = 0usize;
            let mut prev_j = 0usize;
            for (ai, aj) in anchors {
                if ai > prev_i || aj > prev_j {
                    append_middle_diff_rows(
                        &old_mid[prev_i..ai],
                        &new_mid[prev_j..aj],
                        old_base + prev_i,
                        new_base + prev_j,
                        raw_rows,
                        added_count,
                        removed_count,
                        depth + 1,
                    );
                }
                raw_rows.push(DiffRow {
                    kind: DiffLineKind::Context,
                    old_line: Some(old_base + ai + 1),
                    new_line: Some(new_base + aj + 1),
                    text: old_mid[ai].clone(),
                });
                prev_i = ai + 1;
                prev_j = aj + 1;
            }
            if prev_i < m || prev_j < n {
                append_middle_diff_rows(
                    &old_mid[prev_i..],
                    &new_mid[prev_j..],
                    old_base + prev_i,
                    new_base + prev_j,
                    raw_rows,
                    added_count,
                    removed_count,
                    depth + 1,
                );
            }
            return;
        }
    }

    // No shared anchors left in an immense disjoint block: emit all differing lines completely.
    for (i, line) in old_mid.iter().enumerate() {
        raw_rows.push(DiffRow {
            kind: DiffLineKind::Removed,
            old_line: Some(old_base + i + 1),
            new_line: None,
            text: line.clone(),
        });
        *removed_count += 1;
    }
    for (j, line) in new_mid.iter().enumerate() {
        raw_rows.push(DiffRow {
            kind: DiffLineKind::Added,
            old_line: None,
            new_line: Some(new_base + j + 1),
            text: line.clone(),
        });
        *added_count += 1;
    }
}

/// Computes a scalable unified line diff from `local_lines` (current buffer) to `disk_lines` (on disk).
/// Returns `(rows, added_count, removed_count, truncated)`.
pub fn compute_bounded_line_diff(
    local_lines: &[String],
    disk_lines: &[String],
) -> (Vec<DiffRow>, usize, usize, bool) {
    let mut prefix = 0usize;
    let min_len = local_lines.len().min(disk_lines.len());
    while prefix < min_len && local_lines[prefix] == disk_lines[prefix] {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < (local_lines.len() - prefix).min(disk_lines.len() - prefix)
        && local_lines[local_lines.len() - 1 - suffix]
            == disk_lines[disk_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let old_mid = &local_lines[prefix..local_lines.len() - suffix];
    let new_mid = &disk_lines[prefix..disk_lines.len() - suffix];

    let mut raw_rows: Vec<DiffRow> = Vec::new();
    let mut added_count = 0usize;
    let mut removed_count = 0usize;

    for (i, line) in local_lines[..prefix].iter().enumerate() {
        raw_rows.push(DiffRow {
            kind: DiffLineKind::Context,
            old_line: Some(i + 1),
            new_line: Some(i + 1),
            text: line.clone(),
        });
    }

    append_middle_diff_rows(
        old_mid,
        new_mid,
        prefix,
        prefix,
        &mut raw_rows,
        &mut added_count,
        &mut removed_count,
        0,
    );

    let old_tail_start = local_lines.len() - suffix;
    let new_tail_start = disk_lines.len() - suffix;
    for s in 0..suffix {
        raw_rows.push(DiffRow {
            kind: DiffLineKind::Context,
            old_line: Some(old_tail_start + s + 1),
            new_line: Some(new_tail_start + s + 1),
            text: local_lines[old_tail_start + s].clone(),
        });
    }

    // Collapse long unchanged context runs (retain 2 lines before/after every change)
    if added_count == 0 && removed_count == 0 {
        return (Vec::new(), 0, 0, false);
    }

    const CONTEXT_RADIUS: usize = 2;
    let total = raw_rows.len();
    let mut keep = vec![false; total];
    for (idx, row) in raw_rows.iter().enumerate() {
        if row.kind != DiffLineKind::Context {
            let s = idx.saturating_sub(CONTEXT_RADIUS);
            let e = (idx + CONTEXT_RADIUS + 1).min(total);
            for slot in &mut keep[s..e] {
                *slot = true;
            }
        }
    }

    let mut collapsed = Vec::new();
    let mut idx = 0usize;
    while idx < total {
        if keep[idx] {
            collapsed.push(raw_rows[idx].clone());
            idx += 1;
        } else {
            let skip_start = idx;
            while idx < total && !keep[idx] {
                idx += 1;
            }
            let skipped = idx - skip_start;
            collapsed.push(DiffRow {
                kind: DiffLineKind::Separator,
                old_line: None,
                new_line: None,
                text: format!("⋯ ({skipped} unchanged line(s))"),
            });
        }
    }

    (collapsed, added_count, removed_count, false)
}

fn adaptive_diff_byte_budget() -> usize {
    (crate::document::system_total_memory_bytes() / 128)
        .clamp(16 * 1024 * 1024, 256 * 1024 * 1024) as usize
}

fn read_adaptive_lines_from_rope(rope: &Rope, byte_budget: usize) -> (Vec<String>, bool) {
    let mut out = Vec::with_capacity(rope.len_lines().min(65_536));
    let mut total_bytes = 0usize;
    let mut truncated = false;
    for line in rope.lines() {
        if total_bytes >= byte_budget && !out.is_empty() {
            truncated = true;
            break;
        }
        let mut s = line.to_string();
        if s.ends_with('\n') {
            s.pop();
        }
        if s.ends_with('\r') {
            s.pop();
        }
        total_bytes = total_bytes.saturating_add(s.len());
        out.push(s);
    }
    (out, truncated)
}

fn read_adaptive_lines_from_disk(
    path: &std::path::Path,
    byte_budget: usize,
) -> Result<(Vec<String>, bool), String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut out = Vec::new();
    let mut buf = Vec::with_capacity(256);
    let mut total_bytes = 0usize;
    let mut truncated = false;
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if total_bytes >= byte_budget && !out.is_empty() {
            truncated = true;
            break;
        }
        let mut s = String::from_utf8_lossy(&buf).into_owned();
        if out.is_empty() && s.starts_with('\u{FEFF}') {
            s.remove(0);
        }
        if s.ends_with('\n') {
            s.pop();
        }
        if s.ends_with('\r') {
            s.pop();
        }
        total_bytes = total_bytes.saturating_add(s.len());
        out.push(s);
    }
    Ok((out, truncated))
}

/// Modal showing a bounded unified diff between the local unsaved buffer and the updated file on disk.
#[derive(Debug, Default)]
pub struct ExternalDiffModal {
    pub open: bool,
    pub file_name: String,
    pub rows: Vec<DiffRow>,
    pub added_count: usize,
    pub removed_count: usize,
    pub truncated: bool,
    pub error_message: Option<String>,
}

impl ExternalDiffModal {
    pub fn open_for(&mut self, buf: &DocumentBuffer, disk_path: &std::path::Path) {
        self.open = true;
        self.file_name = disk_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| disk_path.display().to_string());
        self.error_message = None;

        let budget = adaptive_diff_byte_budget();
        let (local_lines, local_trunc) = read_adaptive_lines_from_rope(buf.rope(), budget);
        match read_adaptive_lines_from_disk(disk_path, budget) {
            Ok((disk_lines, disk_trunc)) => {
                let (rows, added, removed, mid_trunc) =
                    compute_bounded_line_diff(&local_lines, &disk_lines);
                self.rows = rows;
                self.added_count = added;
                self.removed_count = removed;
                self.truncated = local_trunc || disk_trunc || mid_trunc;
            }
            Err(err) => {
                self.rows.clear();
                self.added_count = 0;
                self.removed_count = 0;
                self.truncated = false;
                self.error_message = Some(format!("Failed to read file from disk: {err}"));
            }
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: &WordTheme,
    ) -> Option<ExternalDiffChoice> {
        if !self.open {
            return None;
        }
        let mut keep_open = true;
        let mut choice: Option<ExternalDiffChoice> = None;
        let mut close_clicked = false;

        let title = format!("External Changes Diff — {}", self.file_name);
        egui::Window::new(title)
            .id(egui::Id::new("external_diff_modal"))
            .collapsible(false)
            .resizable(true)
            .default_width(680.0)
            .default_height(460.0)
            .open(&mut keep_open)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Local buffer (-) vs Disk file (+)")
                            .size(12.5)
                            .strong()
                            .color(theme.text_primary),
                    );
                    ui.add_space(8.0);
                    let add_col = if theme.dark {
                        Color32::from_rgb(134, 239, 172)
                    } else {
                        Color32::from_rgb(22, 101, 52)
                    };
                    let rem_col = if theme.dark {
                        Color32::from_rgb(252, 165, 165)
                    } else {
                        Color32::from_rgb(153, 27, 27)
                    };
                    ui.label(
                        RichText::new(format!("+{} added", self.added_count))
                            .size(12.0)
                            .strong()
                            .color(add_col),
                    );
                    ui.label(
                        RichText::new(format!("−{} removed", self.removed_count))
                            .size(12.0)
                            .strong()
                            .color(rem_col),
                    );
                });

                if self.truncated {
                    ui.label(
                        RichText::new(
                            "Showing diff within adaptive hardware memory preview budget.",
                        )
                        .size(11.0)
                        .color(theme.warning_amber),
                    );
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                if let Some(ref err) = self.error_message {
                    ui.label(RichText::new(err).color(Color32::from_rgb(200, 48, 48)));
                } else if self.rows.is_empty() {
                    ui.label(
                        RichText::new("No text line differences detected in preview range.")
                            .color(theme.text_muted),
                    );
                } else {
                    let row_h = 19.0_f32;
                    egui::ScrollArea::horizontal()
                        .id_salt("external_diff_hscroll")
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("external_diff_scroll")
                                .max_height(330.0)
                                .auto_shrink([false, false])
                                .show_rows(ui, row_h, self.rows.len(), |ui, row_range| {
                                    let mono = FontId::monospace(12.0);
                                    let avail_w = ui.available_width().max(520.0);
                                    for row in &self.rows[row_range] {
                                        let (prefix, bg, fg) = match row.kind {
                                            DiffLineKind::Added => {
                                                let bg = if theme.dark {
                                                    Color32::from_rgba_unmultiplied(35, 115, 65, 75)
                                                } else {
                                                    Color32::from_rgb(220, 252, 231)
                                                };
                                                let fg = if theme.dark {
                                                    Color32::from_rgb(134, 239, 172)
                                                } else {
                                                    Color32::from_rgb(22, 101, 52)
                                                };
                                                ("+", bg, fg)
                                            }
                                            DiffLineKind::Removed => {
                                                let bg = if theme.dark {
                                                    Color32::from_rgba_unmultiplied(150, 45, 45, 75)
                                                } else {
                                                    Color32::from_rgb(254, 226, 226)
                                                };
                                                let fg = if theme.dark {
                                                    Color32::from_rgb(252, 165, 165)
                                                } else {
                                                    Color32::from_rgb(153, 27, 27)
                                                };
                                                ("-", bg, fg)
                                            }
                                            DiffLineKind::Separator => {
                                                (" ", theme.ribbon_bg, theme.text_muted)
                                            }
                                            DiffLineKind::Context => {
                                                (" ", Color32::TRANSPARENT, theme.text_primary)
                                            }
                                        };
                                        let old_s = row
                                            .old_line
                                            .map(|n| format!("{n:>4}"))
                                            .unwrap_or_else(|| "    ".to_string());
                                        let new_s = row
                                            .new_line
                                            .map(|n| format!("{n:>4}"))
                                            .unwrap_or_else(|| "    ".to_string());
                                        let line_text = if row.kind == DiffLineKind::Separator {
                                            format!("          {}", row.text)
                                        } else {
                                            format!("{old_s} {new_s} {prefix} {}", row.text)
                                        };
                                        egui::Frame::new().fill(bg).show(ui, |ui| {
                                            ui.set_min_width(avail_w);
                                            ui.set_min_height(row_h);
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(line_text)
                                                        .font(mono.clone())
                                                        .color(fg),
                                                )
                                                .wrap_mode(egui::TextWrapMode::Extend),
                                            );
                                        });
                                    }
                                });
                        });
                }

                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if vivid_button(
                        ui,
                        RichText::new("Reload from Disk")
                            .size(12.0)
                            .strong()
                            .color(Color32::WHITE),
                        Vec2::ZERO,
                        6,
                        true,
                        true,
                        ToolbarButtonKind::Primary,
                        "Discard unsaved local changes and load file from disk",
                        theme,
                    )
                    .clicked()
                    {
                        choice = Some(ExternalDiffChoice::ReloadFromDisk);
                    }
                    if vivid_button(
                        ui,
                        RichText::new("Keep Local Changes")
                            .size(12.0)
                            .color(theme.text_primary),
                        Vec2::ZERO,
                        6,
                        false,
                        true,
                        ToolbarButtonKind::Secondary,
                        "Dismiss warning and keep editing local buffer",
                        theme,
                    )
                    .clicked()
                    {
                        choice = Some(ExternalDiffChoice::KeepLocal);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            close_clicked = true;
                        }
                    });
                });
            });

        if choice.is_some() || !keep_open || close_clicked {
            self.open = false;
        }
        choice
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_line_diff_computes_added_removed_and_collapses_context() {
        let local: Vec<String> = vec![
            "line 1", "line 2", "line 3", "line 4", "local only", "line 6", "line 7", "line 8",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let disk: Vec<String> = vec![
            "line 1", "line 2", "line 3", "line 4", "disk updated", "disk extra", "line 6",
            "line 7", "line 8",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let (rows, added, removed, truncated) = compute_bounded_line_diff(&local, &disk);
        assert!(!truncated);
        assert_eq!(added, 2);
        assert_eq!(removed, 1);
        assert!(rows
            .iter()
            .any(|r| r.kind == DiffLineKind::Removed && r.text == "local only"));
        assert!(rows
            .iter()
            .any(|r| r.kind == DiffLineKind::Added && r.text == "disk updated"));
        assert!(rows
            .iter()
            .any(|r| r.kind == DiffLineKind::Added && r.text == "disk extra"));
        assert!(
            rows.iter().any(|r| r.kind == DiffLineKind::Separator),
            "far unchanged lines should collapse into separator"
        );
    }
}
