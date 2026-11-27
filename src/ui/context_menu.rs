//! Context-aware right-click menu system (Editor, Reader, Tabs, and File Explorer).

use std::ops::Range;
use std::path::{Path, PathBuf};

use egui::{CornerRadius, FontId, Key, Margin, Pos2, Rect, Sense, Stroke, Vec2};

use crate::document::ast::parse_to_ir;
use crate::document::editing::{ActiveFormatState, LineBlockKind};
use crate::export::ir::{DocBlock, InlineRun, runs_to_plain_text};
use crate::ui::theme::WordTheme;
use crate::ui::toolbar::ToolbarAction;

/// Image metadata snapshot captured for a Reader/Preview image right-click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderImageContext {
    pub url: String,
    pub alt: String,
    pub link_url: Option<String>,
    pub base_dir: Option<PathBuf>,
    pub doc_path: Option<PathBuf>,
}

/// Code / Math / Mermaid block metadata captured for right-click actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderCodeContext {
    pub info: String,
    pub code: String,
    pub source_lines: Range<usize>,
    pub is_math: bool,
    pub is_mermaid: bool,
}

/// Table metadata captured for Reader/Preview table right-click actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderTableContext {
    pub source_lines: Range<usize>,
    pub markdown: String,
    pub tsv: String,
    pub has_formulas: bool,
}

/// Semantic target identified at the right-click pointer location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuTarget {
    Editor {
        from_wysiwyg: bool,
        has_selection: bool,
        selected_text: Option<String>,
        link_url: Option<String>,
        cursor_line: usize,
    },
    Reader {
        selected_text: Option<String>,
        link_url: Option<String>,
        image: Option<Box<ReaderImageContext>>,
        code_block: Option<Box<ReaderCodeContext>>,
        table: Option<Box<ReaderTableContext>>,
        read_only: bool,
    },
    Tab {
        tab_idx: usize,
        tab_count: usize,
        file_path: Option<PathBuf>,
        dirty: bool,
    },
    Explorer {
        path: PathBuf,
        is_dir: bool,
        is_root_empty_area: bool,
    },
}

/// Declarative context menu entry (clickable action, nested submenu, or separator).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuItem<A> {
    Action {
        icon: &'static str,
        label: String,
        shortcut: &'static str,
        enabled: bool,
        checked: bool,
        action: A,
    },
    Submenu {
        icon: &'static str,
        label: String,
        enabled: bool,
        items: Vec<ContextMenuItem<A>>,
    },
    Separator,
}

impl<A> ContextMenuItem<A> {
    pub fn action(
        icon: &'static str,
        label: impl Into<String>,
        shortcut: &'static str,
        enabled: bool,
        action: A,
    ) -> Self {
        Self::Action {
            icon,
            label: label.into(),
            shortcut,
            enabled,
            checked: false,
            action,
        }
    }

    pub fn checked_action(
        icon: &'static str,
        label: impl Into<String>,
        shortcut: &'static str,
        enabled: bool,
        checked: bool,
        action: A,
    ) -> Self {
        Self::Action {
            icon,
            label: label.into(),
            shortcut,
            enabled,
            checked,
            action,
        }
    }

    pub fn submenu(
        icon: &'static str,
        label: impl Into<String>,
        enabled: bool,
        items: Vec<ContextMenuItem<A>>,
    ) -> Self {
        Self::Submenu {
            icon,
            label: label.into(),
            enabled,
            items,
        }
    }

    fn is_enabled_selectable(&self) -> bool {
        match self {
            ContextMenuItem::Action { enabled, .. } | ContextMenuItem::Submenu { enabled, .. } => {
                *enabled
            }
            ContextMenuItem::Separator => false,
        }
    }
}

/// Unified action emitted when the user activates a context menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuAction {
    Undo,
    Redo,
    Cut,
    Copy,
    CopyAsPlainText,
    Paste,
    PasteAsPlainText,
    SelectAll,
    FindSelectedText(String),
    FindAndReplace,
    Dispatch(ToolbarAction),
    OpenLink(String),
    CopyText { text: String, status: String },
    ViewImage(ReaderImageContext),
    SelectCodeBlock(Range<usize>),
    EditTable(Range<usize>),
    RecalculateTable(Range<usize>),
    CloseTab(usize),
    CloseOtherTabs(usize),
    CloseTabsToRight(usize),
    SaveTab(usize),
    CopyFilePath(PathBuf),
    RevealInFileManager(PathBuf),
    ExplorerOpenFile(PathBuf),
    ExplorerOpenInNewTab(PathBuf),
    ExplorerCopyPath(PathBuf),
    ExplorerCopyRelativePath(PathBuf),
    ExplorerReveal(PathBuf),
    ExplorerStartRename(PathBuf),
    ExplorerRequestDelete(PathBuf),
    ExplorerRefresh,
}

/// Persistent open/focus/submenu/geometry state for the active floating context menu.
#[derive(Debug, Default)]
pub struct ContextMenuState {
    pub open: bool,
    pub anchor_pos: Pos2,
    pub target: Option<ContextMenuTarget>,
    pub opened_pass: u64,
    pub focus_path: Vec<usize>,
    pub open_submenu_path: Vec<usize>,
    pub panel_rects: Vec<Rect>,
    pub panel_sizes: Vec<Vec2>,
    pub submenu_anchor_ys: Vec<f32>,
}

impl ContextMenuState {
    /// Opens the context menu anchored at `pos` for `target`.
    pub fn open_at(&mut self, pos: Pos2, target: ContextMenuTarget, pass_nr: u64) {
        self.open = true;
        self.anchor_pos = pos;
        self.target = Some(target);
        self.opened_pass = pass_nr;
        self.focus_path.clear();
        self.open_submenu_path.clear();
        self.panel_rects.clear();
        self.panel_sizes.clear();
        self.submenu_anchor_ys.clear();
    }

    /// Closes the context menu and clears transient navigation state.
    pub fn close(&mut self) {
        self.open = false;
        self.target = None;
        self.focus_path.clear();
        self.open_submenu_path.clear();
        self.panel_rects.clear();
        self.panel_sizes.clear();
        self.submenu_anchor_ys.clear();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Processes outside-click dismissal and keyboard navigation for an already-open context menu.
    pub fn process_open_input(
        &mut self,
        ctx: &egui::Context,
        items: &[ContextMenuItem<ContextMenuAction>],
    ) -> Option<ContextMenuAction> {
        if !self.open {
            return None;
        }

        let current_pass = ctx.cumulative_pass_nr();
        if current_pass > self.opened_pass && !self.panel_rects.is_empty() {
            let outside_pressed = ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer
                        .interact_pos()
                        .or_else(|| i.pointer.latest_pos())
                        .is_some_and(|p| !self.panel_rects.iter().any(|r| r.contains(p)))
            });
            if outside_pressed {
                self.close();
                return None;
            }
        }

        if let Some(kb_action) = self.handle_keyboard_navigation(ctx, items) {
            self.close();
            return Some(kb_action);
        }
        None
    }

    /// Renders the floating root menu + any open submenus on `Order::Foreground`.
    pub fn render_panels(
        &mut self,
        ctx: &egui::Context,
        theme: &WordTheme,
        items: &[ContextMenuItem<ContextMenuAction>],
    ) -> Option<ContextMenuAction> {
        if !self.open {
            return None;
        }

        let screen = ctx.screen_rect();
        let mut chosen_action: Option<ContextMenuAction> = None;
        let mut new_panel_rects: Vec<Rect> = Vec::new();
        let mut new_panel_sizes: Vec<Vec2> = Vec::new();
        let mut next_submenu_ys: Vec<f32> = Vec::new();

        // Depth 0 ..= open_submenu_path.len()
        let max_depth = self.open_submenu_path.len();
        for depth in 0..=max_depth {
            let Some(slice) = items_at_submenu_path(items, &self.open_submenu_path[..depth]) else {
                break;
            };
            if slice.is_empty() {
                break;
            }
            let est_size = self
                .panel_sizes
                .get(depth)
                .copied()
                .unwrap_or_else(|| estimate_menu_size(slice));
            let panel_pos = if depth == 0 {
                compute_root_menu_pos(self.anchor_pos, est_size, screen)
            } else {
                let parent_rect = new_panel_rects
                    .get(depth - 1)
                    .copied()
                    .unwrap_or_else(|| Rect::from_min_size(self.anchor_pos, Vec2::ZERO));
                let anchor_y = next_submenu_ys
                    .get(depth - 1)
                    .copied()
                    .or_else(|| self.submenu_anchor_ys.get(depth - 1).copied())
                    .unwrap_or(parent_rect.top());
                compute_submenu_pos(parent_rect, anchor_y, est_size, screen)
            };

            let focused_idx = self.focus_path.get(depth).copied();
            let active_sub_idx = self.open_submenu_path.get(depth).copied();
            let area_id = egui::Id::new(("md_context_menu_panel", depth));

            let area_resp = egui::Area::new(area_id)
                .order(egui::Order::Foreground)
                .fixed_pos(panel_pos)
                .constrain_to(screen.shrink(4.0))
                .show(ctx, |ui| {
                    let frame = egui::Frame::new()
                        .fill(theme.paper_bg)
                        .stroke(Stroke::new(1.0_f32, theme.border))
                        .corner_radius(CornerRadius::same(8))
                        .shadow(theme.paper_shadow())
                        .inner_margin(Margin::symmetric(5, 5));
                    frame
                        .show(ui, |ui| {
                            render_menu_panel_contents(
                                ui,
                                slice,
                                depth,
                                focused_idx,
                                active_sub_idx,
                                theme,
                            )
                        })
                        .inner
                });

            let rect = area_resp.response.rect;
            new_panel_rects.push(rect);
            new_panel_sizes.push(rect.size());

            let panel_out = area_resp.inner;
            if let Some(sub_y) = panel_out.active_submenu_y {
                next_submenu_ys.push(sub_y);
            }
            if let Some(hovered_idx) = panel_out.hovered_idx {
                self.focus_path.truncate(depth);
                self.focus_path.push(hovered_idx);
                if let Some(ContextMenuItem::Submenu { enabled: true, .. }) = slice.get(hovered_idx)
                {
                    if self.open_submenu_path.get(depth).copied() != Some(hovered_idx) {
                        self.open_submenu_path.truncate(depth);
                        self.open_submenu_path.push(hovered_idx);
                        if let Some(anchor_y) = panel_out.hovered_y {
                            next_submenu_ys.truncate(depth);
                            next_submenu_ys.push(anchor_y);
                        }
                    }
                } else {
                    self.open_submenu_path.truncate(depth);
                }
            }
            if let Some(act) = panel_out.clicked_action {
                chosen_action = Some(act);
            }
        }

        self.panel_rects = new_panel_rects;
        self.panel_sizes = new_panel_sizes;
        self.submenu_anchor_ys = next_submenu_ys;

        if chosen_action.is_some() {
            self.close();
        }
        chosen_action
    }

    /// Processes open input and renders the floating context menu in one call.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: &WordTheme,
        items: &[ContextMenuItem<ContextMenuAction>],
    ) -> Option<ContextMenuAction> {
        if let Some(act) = self.process_open_input(ctx, items) {
            return Some(act);
        }
        self.render_panels(ctx, theme, items)
    }

    fn handle_keyboard_navigation(
        &mut self,
        ctx: &egui::Context,
        items: &[ContextMenuItem<ContextMenuAction>],
    ) -> Option<ContextMenuAction> {
        let (esc, down, up, right, left, enter, space) = ctx.input_mut(|i| {
            let esc = i.consume_key(egui::Modifiers::NONE, Key::Escape);
            let down = i.consume_key(egui::Modifiers::NONE, Key::ArrowDown);
            let up = i.consume_key(egui::Modifiers::NONE, Key::ArrowUp);
            let right = i.consume_key(egui::Modifiers::NONE, Key::ArrowRight);
            let left = i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft);
            let enter = i.consume_key(egui::Modifiers::NONE, Key::Enter);
            let space = i.consume_key(egui::Modifiers::NONE, Key::Space);
            (esc, down, up, right, left, enter, space)
        });

        if esc {
            if !self.open_submenu_path.is_empty() {
                self.open_submenu_path.pop();
                self.focus_path.truncate(self.open_submenu_path.len() + 1);
            } else {
                self.close();
            }
            return None;
        }

        let active_depth = self.open_submenu_path.len();
        let active_items = items_at_submenu_path(items, &self.open_submenu_path)?;

        if left && !self.open_submenu_path.is_empty() {
            let closed_idx = self.open_submenu_path.pop().unwrap_or(0);
            self.focus_path.truncate(self.open_submenu_path.len());
            self.focus_path.push(closed_idx);
            return None;
        }

        if down {
            let cur = self.focus_path.get(active_depth).copied();
            if let Some(next_idx) = step_enabled_item(active_items, cur, true) {
                self.focus_path.truncate(active_depth);
                self.focus_path.push(next_idx);
            }
            return None;
        }

        if up {
            let cur = self.focus_path.get(active_depth).copied();
            if let Some(prev_idx) = step_enabled_item(active_items, cur, false) {
                self.focus_path.truncate(active_depth);
                self.focus_path.push(prev_idx);
            }
            return None;
        }

        let focused_idx = self.focus_path.get(active_depth).copied();
        if (right || enter || space)
            && let Some(idx) = focused_idx
            && let Some(item) = active_items.get(idx)
        {
            match item {
                ContextMenuItem::Submenu {
                    enabled: true,
                    items: sub_items,
                    ..
                } => {
                    self.open_submenu_path.truncate(active_depth);
                    self.open_submenu_path.push(idx);
                    self.focus_path.truncate(active_depth + 1);
                    if let Some(first) = step_enabled_item(sub_items, None, true) {
                        self.focus_path.push(first);
                    }
                    return None;
                }
                ContextMenuItem::Action {
                    enabled: true,
                    action,
                    ..
                } if enter || space => {
                    return Some(action.clone());
                }
                _ => {}
            }
        }
        None
    }
}

struct PanelRenderOutcome {
    hovered_idx: Option<usize>,
    hovered_y: Option<f32>,
    active_submenu_y: Option<f32>,
    clicked_action: Option<ContextMenuAction>,
}

fn items_at_submenu_path<'a, A>(
    root: &'a [ContextMenuItem<A>],
    path: &[usize],
) -> Option<&'a [ContextMenuItem<A>]> {
    let mut cur = root;
    for &idx in path {
        match cur.get(idx) {
            Some(ContextMenuItem::Submenu {
                items,
                enabled: true,
                ..
            }) => cur = items,
            _ => return None,
        }
    }
    Some(cur)
}

fn step_enabled_item<A>(
    items: &[ContextMenuItem<A>],
    current: Option<usize>,
    forward: bool,
) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    let len = items.len();
    let start = match (current, forward) {
        (Some(idx), true) => (idx + 1) % len,
        (Some(idx), false) => (idx + len - 1) % len,
        (None, true) => 0,
        (None, false) => len - 1,
    };
    for step in 0..len {
        let idx = if forward {
            (start + step) % len
        } else {
            (start + len - (step % len)) % len
        };
        if items[idx].is_enabled_selectable() {
            return Some(idx);
        }
    }
    None
}

fn estimate_menu_size<A>(items: &[ContextMenuItem<A>]) -> Vec2 {
    let mut height = 10.0_f32;
    let mut max_chars = 16usize;
    for item in items {
        match item {
            ContextMenuItem::Separator => height += 9.0,
            ContextMenuItem::Action {
                label, shortcut, ..
            } => {
                height += 26.0;
                max_chars = max_chars.max(label.chars().count() + shortcut.chars().count() + 6);
            }
            ContextMenuItem::Submenu { label, .. } => {
                height += 26.0;
                max_chars = max_chars.max(label.chars().count() + 8);
            }
        }
    }
    let width = ((max_chars as f32) * 7.2 + 36.0).clamp(180.0, 320.0);
    Vec2::new(width, height)
}

/// Computes the root context menu position within `screen`, flipping left/up near viewport edges.
pub fn compute_root_menu_pos(anchor: Pos2, size: Vec2, screen: Rect) -> Pos2 {
    let margin = 6.0_f32;
    let max_x = (screen.right() - size.x - margin).max(screen.left() + margin);
    let max_y = (screen.bottom() - size.y - margin).max(screen.top() + margin);
    let x = if anchor.x + size.x > screen.right() - margin {
        (anchor.x - size.x).clamp(screen.left() + margin, max_x)
    } else {
        anchor.x.clamp(screen.left() + margin, max_x)
    };
    let y = if anchor.y + size.y > screen.bottom() - margin {
        (anchor.y - size.y).clamp(screen.top() + margin, max_y)
    } else {
        anchor.y.clamp(screen.top() + margin, max_y)
    };
    Pos2::new(x, y)
}

/// Computes a nested submenu position relative to `parent_rect`, flipping left if near right edge.
pub fn compute_submenu_pos(
    parent_rect: Rect,
    row_top_y: f32,
    sub_size: Vec2,
    screen: Rect,
) -> Pos2 {
    let margin = 6.0_f32;
    let max_x = (screen.right() - sub_size.x - margin).max(screen.left() + margin);
    let max_y = (screen.bottom() - sub_size.y - margin).max(screen.top() + margin);
    let right_x = parent_rect.right() - 2.0;
    let x = if right_x + sub_size.x > screen.right() - margin {
        (parent_rect.left() - sub_size.x + 2.0).clamp(screen.left() + margin, max_x)
    } else {
        right_x.clamp(screen.left() + margin, max_x)
    };
    let y = (row_top_y - 4.0).clamp(screen.top() + margin, max_y);
    Pos2::new(x, y)
}

fn render_menu_panel_contents(
    ui: &mut egui::Ui,
    items: &[ContextMenuItem<ContextMenuAction>],
    depth: usize,
    focused_idx: Option<usize>,
    active_sub_idx: Option<usize>,
    theme: &WordTheme,
) -> PanelRenderOutcome {
    let label_font = FontId::proportional(13.0);
    let shortcut_font = FontId::proportional(11.5);
    let icon_font = FontId::proportional(12.5);

    let mut max_label_w = 100.0_f32;
    let mut max_right_w = 18.0_f32;
    for item in items {
        match item {
            ContextMenuItem::Action {
                label, shortcut, ..
            } => {
                let lg = ui.painter().layout_no_wrap(
                    label.clone(),
                    label_font.clone(),
                    theme.text_primary,
                );
                max_label_w = max_label_w.max(lg.size().x);
                if !shortcut.is_empty() {
                    let sg = ui.painter().layout_no_wrap(
                        (*shortcut).to_string(),
                        shortcut_font.clone(),
                        theme.text_muted,
                    );
                    max_right_w = max_right_w.max(sg.size().x);
                }
            }
            ContextMenuItem::Submenu { label, .. } => {
                let lg = ui.painter().layout_no_wrap(
                    label.clone(),
                    label_font.clone(),
                    theme.text_primary,
                );
                max_label_w = max_label_w.max(lg.size().x);
            }
            ContextMenuItem::Separator => {}
        }
    }

    let icon_col_w = 20.0_f32;
    let gap_w = 20.0_f32;
    let row_w = (10.0 + icon_col_w + max_label_w + gap_w + max_right_w + 10.0).clamp(190.0, 340.0);
    let row_h = 26.0_f32;

    let mut out = PanelRenderOutcome {
        hovered_idx: None,
        hovered_y: None,
        active_submenu_y: None,
        clicked_action: None,
    };

    ui.spacing_mut().item_spacing = Vec2::new(0.0, 1.0);
    for (idx, item) in items.iter().enumerate() {
        match item {
            ContextMenuItem::Separator => {
                let (sep_rect, _) = ui.allocate_exact_size(Vec2::new(row_w, 9.0), Sense::hover());
                ui.painter().line_segment(
                    [
                        egui::pos2(sep_rect.left() + 8.0, sep_rect.center().y),
                        egui::pos2(sep_rect.right() - 8.0, sep_rect.center().y),
                    ],
                    Stroke::new(1.0_f32, theme.border_subtle),
                );
            }
            ContextMenuItem::Action {
                icon,
                label,
                shortcut,
                enabled,
                checked,
                action,
            } => {
                let sense = if *enabled {
                    Sense::click()
                } else {
                    Sense::hover()
                };
                let resp = ui
                    .push_id(("ctx_row", depth, idx), |ui| {
                        ui.allocate_exact_size(Vec2::new(row_w, row_h), sense)
                    })
                    .inner
                    .1;
                let hovered = *enabled && resp.hovered();
                let highlighted = *enabled && (hovered || focused_idx == Some(idx));
                if hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    out.hovered_idx = Some(idx);
                    out.hovered_y = Some(resp.rect.top());
                }

                if highlighted {
                    ui.painter()
                        .rect_filled(resp.rect, CornerRadius::same(5), theme.accent_soft);
                    ui.painter().rect_stroke(
                        resp.rect,
                        CornerRadius::same(5),
                        Stroke::new(0.8_f32, theme.accent.gamma_multiply(0.45)),
                        egui::StrokeKind::Inside,
                    );
                }

                let text_col = if *enabled {
                    theme.text_primary
                } else {
                    theme.text_muted.gamma_multiply(0.55)
                };
                let icon_col = if !*enabled {
                    theme.text_muted.gamma_multiply(0.55)
                } else if *checked || highlighted {
                    theme.accent
                } else {
                    theme.text_muted
                };
                let right_col = if *enabled {
                    theme.text_muted
                } else {
                    theme.text_muted.gamma_multiply(0.5)
                };

                let icon_glyph = if *checked { "✓" } else { *icon };
                if !icon_glyph.is_empty() {
                    let ig = ui.painter().layout_no_wrap(
                        icon_glyph.to_string(),
                        icon_font.clone(),
                        icon_col,
                    );
                    let ix = resp.rect.left() + 8.0 + (icon_col_w - ig.size().x).max(0.0) * 0.5;
                    let iy = resp.rect.center().y - ig.size().y * 0.5;
                    ui.painter().galley(egui::pos2(ix, iy), ig, icon_col);
                }

                let lg = ui
                    .painter()
                    .layout_no_wrap(label.clone(), label_font.clone(), text_col);
                let lx = resp.rect.left() + 8.0 + icon_col_w + 4.0;
                let ly = resp.rect.center().y - lg.size().y * 0.5;
                ui.painter().galley(egui::pos2(lx, ly), lg, text_col);

                if !shortcut.is_empty() {
                    let sg = ui.painter().layout_no_wrap(
                        (*shortcut).to_string(),
                        shortcut_font.clone(),
                        right_col,
                    );
                    let sx = resp.rect.right() - 10.0 - sg.size().x;
                    let sy = resp.rect.center().y - sg.size().y * 0.5;
                    ui.painter().galley(egui::pos2(sx, sy), sg, right_col);
                }

                if *enabled && resp.clicked() {
                    out.clicked_action = Some(action.clone());
                }
            }
            ContextMenuItem::Submenu {
                icon,
                label,
                enabled,
                ..
            } => {
                let resp = ui
                    .push_id(("ctx_sub", depth, idx), |ui| {
                        ui.allocate_exact_size(Vec2::new(row_w, row_h), Sense::click())
                    })
                    .inner
                    .1;
                let hovered = *enabled && resp.hovered();
                let is_open_sub = active_sub_idx == Some(idx);
                if is_open_sub {
                    out.active_submenu_y = Some(resp.rect.top());
                }
                let highlighted = *enabled && (hovered || focused_idx == Some(idx) || is_open_sub);
                if hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    out.hovered_idx = Some(idx);
                    out.hovered_y = Some(resp.rect.top());
                }

                if highlighted {
                    ui.painter()
                        .rect_filled(resp.rect, CornerRadius::same(5), theme.accent_soft);
                }

                let text_col = if *enabled {
                    theme.text_primary
                } else {
                    theme.text_muted.gamma_multiply(0.55)
                };
                let icon_col = if !*enabled {
                    theme.text_muted.gamma_multiply(0.55)
                } else if highlighted {
                    theme.accent
                } else {
                    theme.text_muted
                };

                if !icon.is_empty() {
                    let ig = ui.painter().layout_no_wrap(
                        (*icon).to_string(),
                        icon_font.clone(),
                        icon_col,
                    );
                    let ix = resp.rect.left() + 8.0 + (icon_col_w - ig.size().x).max(0.0) * 0.5;
                    let iy = resp.rect.center().y - ig.size().y * 0.5;
                    ui.painter().galley(egui::pos2(ix, iy), ig, icon_col);
                }

                let lg = ui
                    .painter()
                    .layout_no_wrap(label.clone(), label_font.clone(), text_col);
                let lx = resp.rect.left() + 8.0 + icon_col_w + 4.0;
                let ly = resp.rect.center().y - lg.size().y * 0.5;
                ui.painter().galley(egui::pos2(lx, ly), lg, text_col);

                let ag =
                    ui.painter()
                        .layout_no_wrap("▸".to_string(), shortcut_font.clone(), icon_col);
                let ax = resp.rect.right() - 10.0 - ag.size().x;
                let ay = resp.rect.center().y - ag.size().y * 0.5;
                ui.painter().galley(egui::pos2(ax, ay), ag, icon_col);
            }
        }
    }
    out
}

/// Builds the Editor context menu (Source, WYSIWYG, and Split editor panes).
#[allow(clippy::too_many_arguments)]
pub fn build_editor_menu(
    can_undo: bool,
    can_redo: bool,
    has_selection: bool,
    selected_text: Option<&str>,
    doc_non_empty: bool,
    active_fmt: &ActiveFormatState,
    link_url: Option<&str>,
) -> Vec<ContextMenuItem<ContextMenuAction>> {
    let sel_query = selected_text
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let can_find_sel = has_selection && sel_query.is_some();

    let mut items = vec![
        ContextMenuItem::action("↶", "Undo", "Ctrl+Z", can_undo, ContextMenuAction::Undo),
        ContextMenuItem::action("↷", "Redo", "Ctrl+Y", can_redo, ContextMenuAction::Redo),
        ContextMenuItem::Separator,
        ContextMenuItem::action("✂", "Cut", "Ctrl+X", has_selection, ContextMenuAction::Cut),
        ContextMenuItem::action(
            "⧉",
            "Copy",
            "Ctrl+C",
            has_selection,
            ContextMenuAction::Copy,
        ),
        ContextMenuItem::action(
            "≡",
            "Copy as Plain Text",
            "",
            has_selection,
            ContextMenuAction::CopyAsPlainText,
        ),
        ContextMenuItem::action("📋", "Paste", "Ctrl+V", true, ContextMenuAction::Paste),
        ContextMenuItem::action(
            "📄",
            "Paste as Plain Text",
            "Ctrl+Shift+V",
            true,
            ContextMenuAction::PasteAsPlainText,
        ),
        ContextMenuItem::Separator,
        ContextMenuItem::action(
            "▣",
            "Select All",
            "Ctrl+A",
            doc_non_empty,
            ContextMenuAction::SelectAll,
        ),
        ContextMenuItem::Separator,
        ContextMenuItem::action(
            "🔍",
            "Find Selected Text",
            "",
            can_find_sel,
            ContextMenuAction::FindSelectedText(sel_query.unwrap_or_default()),
        ),
        ContextMenuItem::action(
            "⇄",
            "Find and Replace...",
            "Ctrl+H",
            true,
            ContextMenuAction::FindAndReplace,
        ),
    ];

    if let Some(url) = link_url.filter(|u| !u.trim().is_empty()) {
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::action(
            "↗",
            "Open Link",
            "Ctrl+Click",
            true,
            ContextMenuAction::OpenLink(url.to_string()),
        ));
        items.push(ContextMenuItem::action(
            "🔗",
            "Copy Link Address",
            "",
            true,
            ContextMenuAction::CopyText {
                text: url.to_string(),
                status: "Copied link address".to_string(),
            },
        ));
    }

    let headings_sub = vec![
        ContextMenuItem::checked_action(
            "¶",
            "Normal Text",
            "",
            true,
            active_fmt.heading_level.is_none(),
            ContextMenuAction::Dispatch(ToolbarAction::SetHeading(None)),
        ),
        ContextMenuItem::Separator,
        ContextMenuItem::checked_action(
            "H1",
            "Heading 1",
            "",
            true,
            active_fmt.heading_level == Some(1),
            ContextMenuAction::Dispatch(ToolbarAction::SetHeading(Some(1))),
        ),
        ContextMenuItem::checked_action(
            "H2",
            "Heading 2",
            "",
            true,
            active_fmt.heading_level == Some(2),
            ContextMenuAction::Dispatch(ToolbarAction::SetHeading(Some(2))),
        ),
        ContextMenuItem::checked_action(
            "H3",
            "Heading 3",
            "",
            true,
            active_fmt.heading_level == Some(3),
            ContextMenuAction::Dispatch(ToolbarAction::SetHeading(Some(3))),
        ),
        ContextMenuItem::checked_action(
            "H4",
            "Heading 4",
            "",
            true,
            active_fmt.heading_level == Some(4),
            ContextMenuAction::Dispatch(ToolbarAction::SetHeading(Some(4))),
        ),
        ContextMenuItem::checked_action(
            "H5",
            "Heading 5",
            "",
            true,
            active_fmt.heading_level == Some(5),
            ContextMenuAction::Dispatch(ToolbarAction::SetHeading(Some(5))),
        ),
        ContextMenuItem::checked_action(
            "H6",
            "Heading 6",
            "",
            true,
            active_fmt.heading_level == Some(6),
            ContextMenuAction::Dispatch(ToolbarAction::SetHeading(Some(6))),
        ),
    ];

    let formatting_items = vec![
        ContextMenuItem::checked_action(
            "B",
            "Bold",
            "Ctrl+B",
            true,
            active_fmt.bold,
            ContextMenuAction::Dispatch(ToolbarAction::ToggleBold),
        ),
        ContextMenuItem::checked_action(
            "I",
            "Italic",
            "Ctrl+I",
            true,
            active_fmt.italic,
            ContextMenuAction::Dispatch(ToolbarAction::ToggleItalic),
        ),
        ContextMenuItem::checked_action(
            "U",
            "Underline",
            "Ctrl+U",
            true,
            active_fmt.underline,
            ContextMenuAction::Dispatch(ToolbarAction::ToggleUnderline),
        ),
        ContextMenuItem::checked_action(
            "S",
            "Strikethrough",
            "",
            true,
            active_fmt.strikethrough,
            ContextMenuAction::Dispatch(ToolbarAction::ToggleStrike),
        ),
        ContextMenuItem::Separator,
        ContextMenuItem::checked_action(
            "`",
            "Inline Code",
            "",
            true,
            active_fmt.inline_code,
            ContextMenuAction::Dispatch(ToolbarAction::ToggleInlineCode),
        ),
        ContextMenuItem::action(
            "$",
            "Inline Math",
            "",
            true,
            ContextMenuAction::Dispatch(ToolbarAction::InsertInlineMath),
        ),
        ContextMenuItem::Separator,
        ContextMenuItem::submenu("H", "Headings", true, headings_sub),
        ContextMenuItem::Separator,
        ContextMenuItem::checked_action(
            "•",
            "Bulleted List",
            "",
            true,
            active_fmt.block_kind == Some(LineBlockKind::BulletList),
            ContextMenuAction::Dispatch(ToolbarAction::ToggleBlock(LineBlockKind::BulletList)),
        ),
        ContextMenuItem::checked_action(
            "1.",
            "Numbered List",
            "",
            true,
            active_fmt.block_kind == Some(LineBlockKind::NumberedList),
            ContextMenuAction::Dispatch(ToolbarAction::ToggleBlock(LineBlockKind::NumberedList)),
        ),
        ContextMenuItem::checked_action(
            "☑",
            "Checklist",
            "",
            true,
            active_fmt.block_kind == Some(LineBlockKind::TaskList),
            ContextMenuAction::Dispatch(ToolbarAction::ToggleBlock(LineBlockKind::TaskList)),
        ),
        ContextMenuItem::checked_action(
            "❝",
            "Blockquote",
            "",
            true,
            active_fmt.block_kind == Some(LineBlockKind::BlockQuote),
            ContextMenuAction::Dispatch(ToolbarAction::ToggleBlock(LineBlockKind::BlockQuote)),
        ),
        ContextMenuItem::Separator,
        ContextMenuItem::action(
            "→",
            "Indent",
            "Tab",
            true,
            ContextMenuAction::Dispatch(ToolbarAction::Indent),
        ),
        ContextMenuItem::action(
            "←",
            "Outdent",
            "Shift+Tab",
            true,
            ContextMenuAction::Dispatch(ToolbarAction::Outdent),
        ),
        ContextMenuItem::Separator,
        ContextMenuItem::action(
            "{}",
            "Insert Code Block",
            "",
            true,
            ContextMenuAction::Dispatch(ToolbarAction::InsertCodeBlock),
        ),
        ContextMenuItem::action(
            "🔗",
            "Insert Link...",
            "Ctrl+K",
            true,
            ContextMenuAction::Dispatch(ToolbarAction::OpenLinkDialog),
        ),
        ContextMenuItem::action(
            "🖼",
            "Insert Image...",
            "",
            true,
            ContextMenuAction::Dispatch(ToolbarAction::InsertImage),
        ),
        ContextMenuItem::action(
            "⊞",
            "Insert Table (3×3)",
            "",
            true,
            ContextMenuAction::Dispatch(ToolbarAction::InsertTable { rows: 3, cols: 3 }),
        ),
        ContextMenuItem::action(
            "∫",
            "Insert Math Block",
            "",
            true,
            ContextMenuAction::Dispatch(ToolbarAction::InsertMath),
        ),
        ContextMenuItem::action(
            "◇",
            "Insert Mermaid Diagram",
            "",
            true,
            ContextMenuAction::Dispatch(ToolbarAction::InsertMermaid),
        ),
        ContextMenuItem::action(
            "—",
            "Horizontal Rule",
            "",
            true,
            ContextMenuAction::Dispatch(ToolbarAction::InsertHorizontalRule),
        ),
    ];

    items.push(ContextMenuItem::Separator);
    items.push(ContextMenuItem::submenu(
        "✎",
        "Formatting",
        true,
        formatting_items,
    ));
    items
}

/// Builds the Reader / Preview context menu. In `read_only` mode, strictly omits all editing actions.
pub fn build_reader_menu(
    target: &ContextMenuTarget,
    doc_non_empty: bool,
) -> Vec<ContextMenuItem<ContextMenuAction>> {
    let ContextMenuTarget::Reader {
        selected_text,
        link_url,
        image,
        code_block,
        table,
        read_only,
    } = target
    else {
        return Vec::new();
    };

    let sel_trimmed = selected_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_selection = selected_text.as_deref().is_some_and(|s| !s.is_empty());

    let mut items = vec![
        ContextMenuItem::action(
            "⧉",
            "Copy",
            "Ctrl+C",
            has_selection,
            ContextMenuAction::Copy,
        ),
        ContextMenuItem::action(
            "≡",
            "Copy as Plain Text",
            "",
            has_selection,
            ContextMenuAction::CopyAsPlainText,
        ),
        ContextMenuItem::action(
            "▣",
            "Select All",
            "Ctrl+A",
            doc_non_empty,
            ContextMenuAction::SelectAll,
        ),
        ContextMenuItem::action(
            "🔍",
            "Find Selected Text",
            "",
            sel_trimmed.is_some(),
            ContextMenuAction::FindSelectedText(sel_trimmed.unwrap_or_default().to_string()),
        ),
    ];

    let eff_link = link_url
        .as_deref()
        .or_else(|| image.as_ref().and_then(|im| im.link_url.as_deref()))
        .filter(|u| !u.trim().is_empty());
    if let Some(url) = eff_link {
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::action(
            "↗",
            "Open Link",
            "",
            true,
            ContextMenuAction::OpenLink(url.to_string()),
        ));
        items.push(ContextMenuItem::action(
            "🔗",
            "Copy Link Address",
            "",
            true,
            ContextMenuAction::CopyText {
                text: url.to_string(),
                status: "Copied link address".to_string(),
            },
        ));
    }

    if let Some(img) = image {
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::action(
            "🔍",
            "View Image",
            "",
            true,
            ContextMenuAction::ViewImage((**img).clone()),
        ));
        items.push(ContextMenuItem::action(
            "🖼",
            "Copy Image Address",
            "",
            !img.url.trim().is_empty(),
            ContextMenuAction::CopyText {
                text: img.url.clone(),
                status: "Copied image address".to_string(),
            },
        ));
    }

    if let Some(cb) = code_block {
        items.push(ContextMenuItem::Separator);
        let copy_label = if cb.is_math {
            "Copy Math Source"
        } else if cb.is_mermaid {
            "Copy Mermaid Source"
        } else {
            "Copy Code"
        };
        let copy_status = if cb.is_math {
            "Copied math source"
        } else if cb.is_mermaid {
            "Copied Mermaid source"
        } else {
            "Copied code"
        };
        items.push(ContextMenuItem::action(
            "⧉",
            copy_label,
            "",
            true,
            ContextMenuAction::CopyText {
                text: cb.code.clone(),
                status: copy_status.to_string(),
            },
        ));
        items.push(ContextMenuItem::action(
            "{}",
            "Copy Code with Markdown Fences",
            "",
            true,
            ContextMenuAction::CopyText {
                text: format_fenced_code(&cb.info, &cb.code),
                status: "Copied fenced code block".to_string(),
            },
        ));
        items.push(ContextMenuItem::action(
            "▣",
            "Select Code Block",
            "",
            true,
            ContextMenuAction::SelectCodeBlock(cb.source_lines.clone()),
        ));
    }

    if let Some(tbl) = table {
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::action(
            "⊞",
            "Copy Table as Markdown",
            "",
            !tbl.markdown.is_empty(),
            ContextMenuAction::CopyText {
                text: tbl.markdown.clone(),
                status: "Copied table as Markdown".to_string(),
            },
        ));
        items.push(ContextMenuItem::action(
            "▤",
            "Copy Table as TSV",
            "",
            !tbl.tsv.is_empty(),
            ContextMenuAction::CopyText {
                text: tbl.tsv.clone(),
                status: "Copied table as TSV".to_string(),
            },
        ));
        if !*read_only {
            items.push(ContextMenuItem::Separator);
            items.push(ContextMenuItem::action(
                "✎",
                "Edit Table...",
                "",
                true,
                ContextMenuAction::EditTable(tbl.source_lines.clone()),
            ));
            items.push(ContextMenuItem::action(
                "fx",
                "Recalculate Table",
                "",
                tbl.has_formulas,
                ContextMenuAction::RecalculateTable(tbl.source_lines.clone()),
            ));
        }
    }

    items
}

/// Builds the document tab right-click menu.
pub fn build_tab_menu(
    tab_idx: usize,
    tab_count: usize,
    file_path: Option<&Path>,
    _dirty: bool,
) -> Vec<ContextMenuItem<ContextMenuAction>> {
    let has_path = file_path.is_some();
    let path_owned = file_path.map(Path::to_path_buf).unwrap_or_default();
    vec![
        ContextMenuItem::action(
            "×",
            "Close Tab",
            "Ctrl+W",
            tab_count > 0,
            ContextMenuAction::CloseTab(tab_idx),
        ),
        ContextMenuItem::action(
            "✕",
            "Close Other Tabs",
            "",
            tab_count > 1,
            ContextMenuAction::CloseOtherTabs(tab_idx),
        ),
        ContextMenuItem::action(
            "⇥",
            "Close Tabs to the Right",
            "",
            tab_idx + 1 < tab_count,
            ContextMenuAction::CloseTabsToRight(tab_idx),
        ),
        ContextMenuItem::Separator,
        ContextMenuItem::action(
            "💾",
            "Save",
            "Ctrl+S",
            true,
            ContextMenuAction::SaveTab(tab_idx),
        ),
        ContextMenuItem::Separator,
        ContextMenuItem::action(
            "⧉",
            "Copy File Path",
            "",
            has_path,
            ContextMenuAction::CopyFilePath(path_owned.clone()),
        ),
        ContextMenuItem::action(
            "📂",
            "Reveal File in Explorer",
            "",
            has_path,
            ContextMenuAction::RevealInFileManager(path_owned),
        ),
    ]
}

/// Builds the File Explorer right-click menu, enforcing workspace containment for mutating actions.
pub fn build_explorer_menu(
    path: &Path,
    root_dir: Option<&Path>,
    is_dir: bool,
    is_root_empty_area: bool,
) -> Vec<ContextMenuItem<ContextMenuAction>> {
    let in_workspace = root_dir.is_some_and(|root| is_path_within_workspace(root, path));
    let has_rel_path = root_dir.is_some_and(|root| path_relative_to(root, path).is_some());
    let mut items = Vec::new();

    if !is_dir && !is_root_empty_area {
        items.push(ContextMenuItem::action(
            "📄",
            "Open File",
            "",
            true,
            ContextMenuAction::ExplorerOpenFile(path.to_path_buf()),
        ));
        items.push(ContextMenuItem::action(
            "➕",
            "Open in New Tab",
            "",
            true,
            ContextMenuAction::ExplorerOpenInNewTab(path.to_path_buf()),
        ));
        items.push(ContextMenuItem::Separator);
    }

    items.push(ContextMenuItem::action(
        "⧉",
        "Copy File Path",
        "",
        true,
        ContextMenuAction::ExplorerCopyPath(path.to_path_buf()),
    ));
    items.push(ContextMenuItem::action(
        "/",
        "Copy Relative Path",
        "",
        has_rel_path && !is_root_empty_area,
        ContextMenuAction::ExplorerCopyRelativePath(path.to_path_buf()),
    ));
    items.push(ContextMenuItem::action(
        "📂",
        "Reveal in OS File Manager",
        "",
        true,
        ContextMenuAction::ExplorerReveal(path.to_path_buf()),
    ));

    if !is_root_empty_area {
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::action(
            "✎",
            "Rename...",
            "",
            in_workspace,
            ContextMenuAction::ExplorerStartRename(path.to_path_buf()),
        ));
        items.push(ContextMenuItem::action(
            "🗑",
            "Delete...",
            "",
            in_workspace,
            ContextMenuAction::ExplorerRequestDelete(path.to_path_buf()),
        ));
    }

    items.push(ContextMenuItem::Separator);
    items.push(ContextMenuItem::action(
        "↻",
        "Refresh Folder",
        "",
        root_dir.is_some(),
        ContextMenuAction::ExplorerRefresh,
    ));

    items
}

/// Converts a Markdown fragment into clean plain text via the AST IR, preserving Unicode/BiDi text.
pub fn markdown_to_plain_text(md: &str) -> String {
    if md.trim().is_empty() {
        return md.to_string();
    }
    let blocks = parse_to_ir(md);
    if blocks.is_empty() {
        return md.to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    for block in &blocks {
        append_block_plain_text(block, &mut parts);
    }
    if parts.is_empty() {
        md.to_string()
    } else {
        parts.join("\n\n")
    }
}

fn append_block_plain_text(block: &DocBlock, out: &mut Vec<String>) {
    match block {
        DocBlock::Heading { runs, .. } | DocBlock::Paragraph { runs, .. } => {
            let text = runs_to_plain_text(runs);
            if !text.is_empty() {
                out.push(text);
            }
        }
        DocBlock::BlockQuote { blocks, .. } => {
            for inner in blocks {
                append_block_plain_text(inner, out);
            }
        }
        DocBlock::CodeBlock { code, .. } => {
            let trimmed = code.trim_end_matches(&['\r', '\n'][..]);
            out.push(trimmed.to_string());
        }
        DocBlock::List { items, .. } => {
            let mut lines = Vec::with_capacity(items.len());
            for item in items {
                let mut item_parts = Vec::new();
                for b in &item.blocks {
                    append_block_plain_text(b, &mut item_parts);
                }
                if !item_parts.is_empty() {
                    lines.push(item_parts.join(" "));
                }
            }
            if !lines.is_empty() {
                out.push(lines.join("\n"));
            }
        }
        DocBlock::Table { header, rows, .. } => {
            let tsv = table_runs_to_tsv(header, rows, None);
            if !tsv.is_empty() {
                out.push(tsv);
            }
        }
        DocBlock::ThematicBreak { .. } => {}
        DocBlock::FootnoteDefinition { name, blocks, .. } => {
            let mut inner = Vec::new();
            for b in blocks {
                append_block_plain_text(b, &mut inner);
            }
            if inner.is_empty() {
                out.push(format!("[^{name}]"));
            } else {
                out.push(format!("[^{name}]: {}", inner.join(" ")));
            }
        }
    }
}

/// Formats code with a backtick fence strictly longer than any backtick run inside `code`.
pub fn format_fenced_code(info: &str, code: &str) -> String {
    let mut max_run = 0usize;
    let mut cur_run = 0usize;
    for b in code.bytes() {
        if b == b'`' {
            cur_run += 1;
            max_run = max_run.max(cur_run);
        } else {
            cur_run = 0;
        }
    }
    let fence = "`".repeat(max_run.max(2) + 1);
    let info_trimmed = info.trim();
    let body = code.trim_end_matches(&['\r', '\n'][..]);
    if body.is_empty() {
        format!("{fence}{info_trimmed}\n{fence}")
    } else {
        format!("{fence}{info_trimmed}\n{body}\n{fence}")
    }
}

fn sanitize_tsv_cell(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if matches!(ch, '\t' | '\r' | '\n') {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Formats table header & rows as tab-separated values (`\t` columns, `\n` rows).
pub fn table_runs_to_tsv(
    header: &[Vec<InlineRun>],
    rows: &[Vec<Vec<InlineRun>>],
    evaluated: Option<&[Vec<String>]>,
) -> String {
    let mut lines = Vec::with_capacity(1 + rows.len());
    if !header.is_empty() {
        let mut cols = Vec::with_capacity(header.len());
        for (c, cell) in header.iter().enumerate() {
            let text = evaluated
                .and_then(|g| g.first())
                .and_then(|r| r.get(c))
                .cloned()
                .unwrap_or_else(|| runs_to_plain_text(cell));
            cols.push(sanitize_tsv_cell(&text));
        }
        lines.push(cols.join("\t"));
    }
    for (r_idx, row) in rows.iter().enumerate() {
        let mut cols = Vec::with_capacity(row.len());
        for (c, cell) in row.iter().enumerate() {
            let text = evaluated
                .and_then(|g| g.get(r_idx + 1))
                .and_then(|r| r.get(c))
                .cloned()
                .unwrap_or_else(|| runs_to_plain_text(cell));
            cols.push(sanitize_tsv_cell(&text));
        }
        lines.push(cols.join("\t"));
    }
    lines.join("\n")
}

/// Computes `target` relative to `root` if `target` is inside `root`.
pub fn path_relative_to(root: &Path, target: &Path) -> Option<String> {
    if let (Ok(canon_root), Ok(canon_target)) = (root.canonicalize(), target.canonicalize())
        && let Ok(rel) = canon_target.strip_prefix(&canon_root)
    {
        return Some(rel.to_string_lossy().replace('\\', "/"));
    }
    target
        .strip_prefix(root)
        .ok()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
}

/// Returns `true` iff `target` resolves strictly inside `root` (never `root` itself or outside `root`).
pub fn is_path_within_workspace(root: &Path, target: &Path) -> bool {
    let Ok(canon_root) = root.canonicalize() else {
        return false;
    };
    let Ok(canon_target) = target.canonicalize() else {
        return false;
    };
    canon_target != canon_root && canon_target.starts_with(&canon_root)
}

/// Opens the OS file manager highlighting `path` (or opening its folder).
pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let arg = if target.exists() {
            format!("/select,{}", target.display())
        } else {
            let parent = target.parent().unwrap_or(&target);
            parent.display().to_string()
        };
        std::process::Command::new("explorer.exe")
            .arg(arg)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        if path.exists() {
            cmd.arg("-R").arg(path);
        } else {
            cmd.arg(path.parent().unwrap_or(path));
        }
        cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Validates a new single-component filename/folder name before disk rename.
pub fn validate_explorer_entry_name(raw_name: &str) -> Result<String, &'static str> {
    let trimmed = raw_name.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return Err("Name cannot be empty");
    }
    let has_invalid = trimmed
        .chars()
        .any(|c| matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') || c < '\u{20}');
    if has_invalid {
        return Err("Invalid characters in name");
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains_label(items: &[ContextMenuItem<ContextMenuAction>], target: &str) -> bool {
        items.iter().any(|item| match item {
            ContextMenuItem::Action { label, .. } => label == target,
            ContextMenuItem::Submenu { label, items, .. } => {
                label == target || contains_label(items, target)
            }
            ContextMenuItem::Separator => false,
        })
    }

    #[test]
    fn markdown_to_plain_text_preserves_unicode_and_strips_syntax() {
        let md =
            "# مرحبا **بالعالم**\n\nText with `code` and [link](https://example.com) and $x^2$.";
        let plain = markdown_to_plain_text(md);
        assert!(plain.contains("مرحبا بالعالم"));
        assert!(plain.contains("Text with code and link and x^2."));
        assert!(!plain.contains("**"));
        assert!(!plain.contains("https://example.com"));
    }

    #[test]
    fn format_fenced_code_picks_longer_fence_for_nested_backticks() {
        let fenced = format_fenced_code("markdown", "Here is ```nested``` code");
        assert!(fenced.starts_with("````markdown\n"));
        assert!(fenced.ends_with("\n````"));
    }

    #[test]
    fn table_runs_to_tsv_formats_columns_and_sanitizes_tabs() {
        let header = vec![
            vec![InlineRun::plain("Col\t1")],
            vec![InlineRun::plain("Col 2")],
        ];
        let rows = vec![vec![
            vec![InlineRun::plain("A\nB")],
            vec![InlineRun::plain("42")],
        ]];
        let tsv = table_runs_to_tsv(&header, &rows, None);
        assert_eq!(tsv, "Col 1\tCol 2\nA B\t42");
    }

    #[test]
    fn reader_menu_strictly_omits_editing_actions_in_read_only_mode() {
        let target = ContextMenuTarget::Reader {
            selected_text: Some("hello".into()),
            link_url: Some("https://example.com".into()),
            image: None,
            code_block: None,
            table: Some(Box::new(ReaderTableContext {
                source_lines: 0..3,
                markdown: "| a |\n| - |\n| 1 |".into(),
                tsv: "a\n1".into(),
                has_formulas: true,
            })),
            read_only: true,
        };
        let menu = build_reader_menu(&target, true);
        assert!(contains_label(&menu, "Copy"));
        assert!(contains_label(&menu, "Copy Table as Markdown"));
        assert!(contains_label(&menu, "Copy Table as TSV"));
        assert!(!contains_label(&menu, "Edit Table..."));
        assert!(!contains_label(&menu, "Recalculate Table"));
        assert!(!contains_label(&menu, "Paste"));
        assert!(!contains_label(&menu, "Formatting"));
    }

    #[test]
    fn preview_menu_exposes_table_edit_and_recalc_when_not_read_only() {
        let target = ContextMenuTarget::Reader {
            selected_text: None,
            link_url: None,
            image: None,
            code_block: None,
            table: Some(Box::new(ReaderTableContext {
                source_lines: 0..3,
                markdown: "| a |\n| - |\n| 1 |".into(),
                tsv: "a\n1".into(),
                has_formulas: true,
            })),
            read_only: false,
        };
        let menu = build_reader_menu(&target, true);
        assert!(contains_label(&menu, "Edit Table..."));
        assert!(contains_label(&menu, "Recalculate Table"));
    }

    #[test]
    fn compute_menu_positions_flip_within_screen_bounds() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0));
        let size = Vec2::new(220.0, 300.0);
        let p = compute_root_menu_pos(Pos2::new(1250.0, 760.0), size, screen);
        assert!(p.x >= 6.0 && p.x + size.x <= screen.right() - 6.0);
        assert!(p.y >= 6.0 && p.y + size.y <= screen.bottom() - 6.0);

        let parent = Rect::from_min_size(Pos2::new(1050.0, 100.0), size);
        let sub_pos = compute_submenu_pos(parent, 120.0, Vec2::new(200.0, 240.0), screen);
        assert!(sub_pos.x < parent.left());
    }

    #[test]
    fn workspace_containment_rejects_root_and_outside_paths() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().join("workspace");
        let outside = tmp.path().join("secret.md");
        let inside = root.join("notes.md");
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(&outside, "secret").expect("write outside");
        std::fs::write(&inside, "note").expect("write inside");

        assert!(is_path_within_workspace(&root, &inside));
        assert!(!is_path_within_workspace(&root, &root));
        assert!(!is_path_within_workspace(&root, &outside));
    }
}
