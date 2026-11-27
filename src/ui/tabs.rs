use std::path::Path;

use egui::{Color32, CornerRadius, Margin, Pos2, RichText, Sense, Shape, Stroke, Vec2};
use rustc_hash::FxHashSet;

use crate::document::editing::ActiveFormatState;
use crate::document::viewport::ViewportBlockIndex;
use crate::document::{DiskFileStamp, DocumentBuffer};
use crate::ui::editor::VirtualEditorState;
use crate::ui::reader::VirtualReaderState;
use crate::ui::theme::WordTheme;

#[derive(Debug, Clone)]
pub(crate) struct OutlineItem {
    pub level: u8,
    pub title: String,
    pub title_lower: String,
    pub source_line: usize,
    pub y_start: f32,
}

/// Complete per-tab document & scroll/editor state parked while another tab is active.
pub struct InactiveTabState {
    pub doc_id: u64,
    pub buffer: DocumentBuffer,
    pub block_index: ViewportBlockIndex,
    pub last_parsed_version: u64,
    pub last_parsed_doc_id: u64,
    pub last_parsed_buf_id: u64,
    pub editor_state: VirtualEditorState,
    pub reader_state: VirtualReaderState,
    pub last_split_top_row: usize,
    pub(crate) outline_items: Vec<OutlineItem>,
    pub outline_cache_key: (u64, u64),
    pub collapsed_outline_lines: FxHashSet<usize>,
    pub word_count_cache: (u64, u64, usize),
    pub active_format_cache: ((u64, u64, usize, Option<usize>), ActiveFormatState),
    pub font_scan_version: (u64, u64, u64),
    pub font_scanned_buf_key: (u64, u64),
    pub disk_stamp: Option<DiskFileStamp>,
    pub external_change_detected: bool,
}

/// Metadata slot for an open tab in `MarkdownApp::tabs`.
///
/// The currently active tab always has `state == None` because its live fields sit directly
/// on `MarkdownApp` for zero-clone O(1) frame updates and tab switching.
pub struct TabSlot {
    pub id: u64,
    pub untitled_num: usize,
    pub state: Option<Box<InactiveTabState>>,
}

/// Lightweight borrowed metadata passed to `render_tab_bar`.
pub struct TabVisualInfo<'a> {
    pub id: u64,
    pub title: String,
    pub full_path: Option<&'a Path>,
    pub dirty: bool,
}

/// User interaction returned by `render_tab_bar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBarAction {
    None,
    Select(usize),
    Close(usize),
    NewTab,
    DetachToNewWindow(usize),
    Reorder { from: usize, to: usize },
}

/// Persistent drag tracking state across frames for Chrome-style tab tear-off & reordering.
#[derive(Debug, Default)]
pub struct TabBarState {
    pub dragging_tab_idx: Option<usize>,
    pub drag_origin: Pos2,
    pub last_pointer_pos: Pos2,
    pub last_drop_pos: Pos2,
    pub in_detach_zone: bool,
    pub reorder_target_idx: Option<usize>,
}

impl TabBarState {
    pub fn clear(&mut self) {
        self.dragging_tab_idx = None;
        self.drag_origin = Pos2::ZERO;
        self.last_pointer_pos = Pos2::ZERO;
        self.in_detach_zone = false;
        self.reorder_target_idx = None;
    }
}

fn truncate_tab_title(title: &str, max_chars: usize) -> String {
    let count = title.chars().count();
    if count <= max_chars {
        title.to_string()
    } else {
        let mut out: String = title.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Renders the compact VS Code / Chrome-style multi-document tab strip.
pub fn render_tab_bar(
    ui: &mut egui::Ui,
    tabs: &[TabVisualInfo<'_>],
    active_idx: usize,
    theme: &WordTheme,
    state: &mut TabBarState,
    over_peer_window: bool,
) -> TabBarAction {
    let mut action = TabBarAction::None;
    let mut tab_rects: Vec<egui::Rect> = Vec::with_capacity(tabs.len());

    let frame_resp = egui::Frame::new()
        .fill(theme.ribbon_bg)
        .stroke(Stroke::new(1.0_f32, theme.border_subtle))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

            egui::ScrollArea::horizontal()
                .id_salt("document_tabs_scroll")
                .drag_to_scroll(false)
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

                        for (idx, tab) in tabs.iter().enumerate() {
                            let is_active = idx == active_idx;
                            let display_title = truncate_tab_title(&tab.title, 26);
                            let tooltip = match (tab.full_path, tab.dirty) {
                                (Some(p), true) => format!("{} (Unsaved changes)", p.display()),
                                (Some(p), false) => p.display().to_string(),
                                (None, true) => format!("{} (Unsaved changes)", tab.title),
                                (None, false) => tab.title.clone(),
                            };

                            let bg_shape_idx = ui.painter().add(Shape::Noop);
                            let inner_resp = ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(3.0, 0.0);
                                ui.add_space(6.0);

                                if tab.dirty {
                                    let (dot_rect, _) = ui.allocate_exact_size(
                                        Vec2::new(7.0, 18.0),
                                        Sense::hover(),
                                    );
                                    ui.painter().circle_filled(
                                        dot_rect.center(),
                                        3.2,
                                        theme.warning_amber,
                                    );
                                }

                                let mut label_rt = RichText::new(&display_title).size(12.0).color(
                                    if is_active {
                                        theme.text_primary
                                    } else {
                                        theme.text_muted
                                    },
                                );
                                if is_active {
                                    label_rt = label_rt.strong();
                                }

                                let label_btn = egui::Button::new(label_rt)
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(Stroke::NONE)
                                    .corner_radius(CornerRadius::same(4))
                                    .sense(Sense::click_and_drag());
                                let label_resp = ui
                                    .push_id(tab.id, |ui| ui.add(label_btn))
                                    .inner
                                    .on_hover_text(&tooltip);

                                if label_resp.drag_started_by(egui::PointerButton::Primary)
                                    || (state.dragging_tab_idx.is_none()
                                        && label_resp.dragged_by(egui::PointerButton::Primary))
                                {
                                    let pos = label_resp
                                        .interact_pointer_pos()
                                        .or_else(|| ui.ctx().pointer_interact_pos())
                                        .unwrap_or_else(|| label_resp.rect.center());
                                    state.dragging_tab_idx = Some(idx);
                                    state.drag_origin = ui
                                        .ctx()
                                        .input(|i| i.pointer.press_origin())
                                        .unwrap_or(pos);
                                    state.last_pointer_pos = pos;
                                    state.in_detach_zone = false;
                                }
                                if label_resp.hovered() && state.dragging_tab_idx.is_none() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if label_resp.middle_clicked() {
                                    action = TabBarAction::Close(idx);
                                } else if label_resp.clicked() && !is_active {
                                    action = TabBarAction::Select(idx);
                                }

                                let close_bg_idx = ui.painter().add(Shape::Noop);
                                let close_btn = egui::Button::new(
                                    RichText::new("×").size(12.0).color(if is_active {
                                        theme.text_primary
                                    } else {
                                        theme.text_muted
                                    }),
                                )
                                .fill(Color32::TRANSPARENT)
                                .stroke(Stroke::NONE)
                                .corner_radius(CornerRadius::same(4))
                                .min_size(Vec2::new(16.0, 18.0));
                                let close_resp = ui
                                    .push_id(("close_tab", tab.id), |ui| ui.add(close_btn))
                                    .inner
                                    .on_hover_text("Close Tab (Ctrl+W)");
                                if close_resp.hovered() || close_resp.is_pointer_button_down_on() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    ui.painter().set(
                                        close_bg_idx,
                                        Shape::rect_filled(
                                            close_resp.rect.shrink(1.0),
                                            CornerRadius::same(4),
                                            theme.accent_soft,
                                        ),
                                    );
                                }
                                if close_resp.clicked() {
                                    action = TabBarAction::Close(idx);
                                }

                                ui.add_space(4.0);
                                (label_resp.hovered(), close_resp.hovered())
                            });

                            let card_rect = inner_resp.response.rect;
                            tab_rects.push(card_rect);
                            let (lbl_hov, cls_hov) = inner_resp.inner;
                            let hovered = lbl_hov || cls_hov || inner_resp.response.hovered();

                            let fill = if is_active {
                                theme.paper_bg
                            } else if hovered {
                                theme.accent_soft
                            } else {
                                Color32::TRANSPARENT
                            };
                            let stroke = if is_active {
                                Stroke::new(1.0_f32, theme.border)
                            } else if hovered {
                                Stroke::new(1.0_f32, theme.border_subtle)
                            } else {
                                Stroke::NONE
                            };
                            ui.painter().set(
                                bg_shape_idx,
                                Shape::rect_filled(card_rect, CornerRadius::same(6), fill),
                            );
                            if stroke != Stroke::NONE {
                                ui.painter().rect_stroke(
                                    card_rect,
                                    CornerRadius::same(6),
                                    stroke,
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if is_active {
                                let top_y = card_rect.top() + 1.0;
                                ui.painter().line_segment(
                                    [
                                        Pos2::new(card_rect.left() + 6.0, top_y),
                                        Pos2::new(card_rect.right() - 6.0, top_y),
                                    ],
                                    Stroke::new(2.0_f32, theme.accent),
                                );
                            }
                        }

                        let new_bg_idx = ui.painter().add(Shape::Noop);
                        let new_btn_fill = if tabs.is_empty() {
                            theme.accent_soft
                        } else {
                            Color32::TRANSPARENT
                        };
                        let new_btn = egui::Button::new(
                            RichText::new("+").size(13.5).strong().color(theme.accent),
                        )
                        .fill(new_btn_fill)
                        .stroke(Stroke::new(1.0_f32, theme.border_subtle))
                        .corner_radius(CornerRadius::same(6))
                        .min_size(Vec2::new(22.0, 20.0));
                        let new_resp = ui.add(new_btn).on_hover_text("New Tab (Ctrl+T)");
                        if new_resp.hovered() || new_resp.is_pointer_button_down_on() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            ui.painter().set(
                                new_bg_idx,
                                Shape::rect_filled(
                                    new_resp.rect,
                                    CornerRadius::same(6),
                                    theme.accent_soft,
                                ),
                            );
                        }
                        if new_resp.clicked() {
                            action = TabBarAction::NewTab;
                        }
                    });
                });
        });

    let tab_bar_rect = frame_resp.response.rect;
    if let Some(drag_idx) = state.dragging_tab_idx {
        if drag_idx >= tabs.len() {
            state.clear();
        } else {
            if let Some(pos) = ui
                .ctx()
                .input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()))
            {
                state.last_pointer_pos = pos;
            }
            let screen_rect = ui.ctx().screen_rect();
            let drag_dist = state.last_pointer_pos.distance(state.drag_origin);
            let outside_window = !screen_rect.contains(state.last_pointer_pos);
            let torn_off_vertically = state.last_pointer_pos.y > tab_bar_rect.bottom() + 48.0
                || state.last_pointer_pos.y < tab_bar_rect.top() - 20.0;
            state.in_detach_zone = drag_dist >= 18.0
                && (over_peer_window
                    || (tabs.len() > 1 && (outside_window || torn_off_vertically)));

            let mut indicator_x: Option<f32> = None;
            if tabs.len() > 1
                && drag_dist >= 8.0
                && !state.in_detach_zone
                && tab_rects.len() == tabs.len()
            {
                let insert_slot = tab_rects
                    .iter()
                    .take_while(|r| r.center().x < state.last_pointer_pos.x)
                    .count();
                let target_idx = if insert_slot > drag_idx {
                    insert_slot - 1
                } else {
                    insert_slot
                };
                if target_idx != drag_idx {
                    state.reorder_target_idx = Some(target_idx);
                    indicator_x = Some(if insert_slot < tab_rects.len() {
                        tab_rects[insert_slot].left() - 2.0
                    } else {
                        tab_rects[tab_rects.len() - 1].right() + 2.0
                    });
                } else {
                    state.reorder_target_idx = None;
                }
            } else {
                state.reorder_target_idx = None;
            }

            if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                state.clear();
            } else if !ui.ctx().input(|i| i.pointer.primary_down()) {
                state.last_drop_pos = state.last_pointer_pos;
                if state.in_detach_zone {
                    action = TabBarAction::DetachToNewWindow(drag_idx);
                } else if let Some(to) = state.reorder_target_idx {
                    action = TabBarAction::Reorder { from: drag_idx, to };
                }
                state.clear();
            } else {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                if drag_dist >= 8.0 {
                    let drag_title = truncate_tab_title(&tabs[drag_idx].title, 26);
                    let border_color =
                        if state.in_detach_zone || state.reorder_target_idx.is_some() {
                            theme.accent
                        } else {
                            theme.border
                        };
                    let painter = ui.ctx().layer_painter(egui::LayerId::new(
                        egui::Order::Tooltip,
                        egui::Id::new("tab_drag_ghost"),
                    ));
                    if let Some(ind_x) = indicator_x {
                        painter.line_segment(
                            [
                                Pos2::new(ind_x, tab_bar_rect.top() + 3.0),
                                Pos2::new(ind_x, tab_bar_rect.bottom() - 3.0),
                            ],
                            Stroke::new(2.5_f32, theme.accent),
                        );
                    }
                    let title_galley = painter.layout_no_wrap(
                        drag_title,
                        egui::FontId::proportional(12.0),
                        theme.text_primary,
                    );
                    let badge_galley = state.in_detach_zone.then(|| {
                        let hint = if over_peer_window {
                            "↘ Release to move into other window"
                        } else {
                            "↗ Release to open in new window"
                        };
                        painter.layout_no_wrap(
                            hint.to_string(),
                            egui::FontId::proportional(11.5),
                            theme.accent,
                        )
                    });
                    let gap = if badge_galley.is_some() { 8.0 } else { 0.0 };
                    let content_w = title_galley.size().x
                        + gap
                        + badge_galley.as_ref().map_or(0.0, |g| g.size().x);
                    let content_h = title_galley
                        .size()
                        .y
                        .max(badge_galley.as_ref().map_or(0.0, |g| g.size().y));
                    let pill_size = Vec2::new(content_w + 20.0, content_h + 12.0);
                    let min_x = screen_rect.left() + 6.0;
                    let max_x = (screen_rect.right() - 6.0 - pill_size.x).max(min_x);
                    let min_y = screen_rect.top() + 6.0;
                    let max_y = (screen_rect.bottom() - 6.0 - pill_size.y).max(min_y);
                    let ghost_pos = Pos2::new(
                        (state.last_pointer_pos.x + 12.0).clamp(min_x, max_x),
                        (state.last_pointer_pos.y + 12.0).clamp(min_y, max_y),
                    );
                    let pill_rect = egui::Rect::from_min_size(ghost_pos, pill_size);
                    painter.rect_filled(pill_rect, CornerRadius::same(6), theme.paper_bg);
                    painter.rect_stroke(
                        pill_rect,
                        CornerRadius::same(6),
                        Stroke::new(1.5_f32, border_color),
                        egui::StrokeKind::Inside,
                    );
                    let title_pos = Pos2::new(
                        pill_rect.left() + 10.0,
                        pill_rect.center().y - title_galley.size().y * 0.5,
                    );
                    let title_right = title_pos.x + title_galley.size().x;
                    painter.galley(title_pos, title_galley, theme.text_primary);
                    if let Some(badge) = badge_galley {
                        let badge_pos = Pos2::new(
                            title_right + gap,
                            pill_rect.center().y - badge.size().y * 0.5,
                        );
                        painter.galley(badge_pos, badge, theme.accent);
                    }
                }
            }
        }
    }

    action
}
