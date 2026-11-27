use std::ops::Range;
use std::sync::{Arc, Weak};

use egui::text::LayoutJob;
use egui::{Color32, EventFilter, FontId, Key, Sense, Stroke, TextFormat, Vec2};
use rustc_hash::FxHashMap;

use crate::document::editing::{indent_lines, outdent_lines, word_range_in_line};
use crate::document::search::is_unicode_word_char;
use crate::document::system_total_memory_bytes;
use crate::document::unicode::{
    build_visual_bidi_line_map, detect_paragraph_direction, shape_bidi_for_display,
    snap_grapheme_end_in_rope_line, snap_grapheme_start_in_rope_line,
    snap_to_grapheme_start, TextDirection, VisualBidiLineMap,
};
use crate::document::DocumentBuffer;
use crate::ui::reader::{classify_link_url, extract_markdown_link_at_char};
use crate::ui::theme::{ThemeMode, WordTheme};

#[derive(Debug, Clone, PartialEq)]
pub enum EditorAction {
    None,
    OpenLink(String),
    CopySelection(String),
    CutSelection(String),
    OpenContextMenu {
        pos: egui::Pos2,
        link_url: Option<String>,
        cursor_line: usize,
    },
}

const MAX_EDITOR_LAYOUT_CHARS: usize = 8_192;

#[derive(Debug, Clone)]
struct CachedEditorLine {
    text: String,
    truncated: bool,
    bidi_map: Arc<VisualBidiLineMap>,
    galley: Option<Arc<egui::Galley>>,
    last_tick: u64,
    approx_bytes: usize,
}

#[derive(Debug, Default)]
struct LineLayoutCache {
    theme_mode: ThemeMode,
    font_family: crate::ui::settings::FontPreference,
    font_size_bits: u32,
    ppp_bits: u32,
    atlas: Weak<egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>>,
    tick: u64,
    total_bytes: usize,
    entries: FxHashMap<usize, CachedEditorLine>,
}

fn adaptive_line_cache_bytes() -> usize {
    (system_total_memory_bytes() / 512)
        .clamp(16 * 1024 * 1024, 128 * 1024 * 1024) as usize
}

#[inline]
fn rope_line_content_chars(slice: &ropey::RopeSlice<'_>) -> usize {
    let mut len = slice.len_chars();
    if len > 0 && slice.char(len - 1) == '\n' {
        len -= 1;
    }
    if len > 0 && slice.char(len - 1) == '\r' {
        len -= 1;
    }
    len
}

fn rope_line_equals(buf: &DocumentBuffer, line_idx: usize, cached: &str, truncated: bool) -> bool {
    if line_idx >= buf.len_lines() {
        return !truncated && cached.is_empty();
    }
    let slice = buf.rope().line(line_idx);
    if truncated && rope_line_content_chars(&slice) <= MAX_EDITOR_LAYOUT_CHARS {
        return false;
    }
    let cached_bytes = cached.as_bytes();
    if slice.len_bytes() < cached_bytes.len() {
        return false;
    }
    let mut pos = 0usize;
    for chunk in slice.chunks() {
        let cb = chunk.as_bytes();
        let rem = cached_bytes.len() - pos;
        if rem > 0 {
            let take = cb.len().min(rem);
            if cb[..take] != cached_bytes[pos..pos + take] {
                return false;
            }
            pos += take;
            if truncated && pos == cached_bytes.len() {
                return true;
            }
            if !truncated && cb.len() > take && cb[take..].iter().any(|&b| b != b'\r' && b != b'\n')
            {
                return false;
            }
        } else if !truncated && cb.iter().any(|&b| b != b'\r' && b != b'\n') {
            return false;
        }
    }
    pos == cached_bytes.len()
}

/// Persistent state for the virtualized Rope source editor.
#[derive(Debug)]
pub struct VirtualEditorState {
    pub scroll_to_cursor: bool,
    pub request_focus: bool,
    pub desired_col: Option<usize>,
    pub dragging: bool,
    pub horiz_offset: f32,
    pub last_cursor_col: usize,
    pub last_visible_rows: Range<usize>,
    pub ime_composing: bool,
    pub ime_preedit: String,
    pub search_matches: Arc<Vec<Range<usize>>>,
    pub active_search_match: Option<Range<usize>>,
    pub last_scroll_area_id: Option<egui::Id>,
    pub last_pane_rect: egui::Rect,
    pub last_scroll_offset_y: f32,
    pub last_max_scroll_y: f32,
    pub pending_scroll_delta_y: f32,
    pub autoscroll_active: bool,
    pub font_family: crate::ui::settings::FontPreference,
    pub font_size: f32,
    line_cache: LineLayoutCache,
}

impl Default for VirtualEditorState {
    fn default() -> Self {
        Self {
            scroll_to_cursor: false,
            request_focus: false,
            desired_col: None,
            dragging: false,
            horiz_offset: 0.0,
            last_cursor_col: 0,
            last_visible_rows: 0..0,
            ime_composing: false,
            ime_preedit: String::new(),
            search_matches: Arc::default(),
            active_search_match: None,
            last_scroll_area_id: None,
            last_pane_rect: egui::Rect::NOTHING,
            last_scroll_offset_y: 0.0,
            last_max_scroll_y: 0.0,
            pending_scroll_delta_y: 0.0,
            autoscroll_active: false,
            font_family: crate::ui::settings::FontPreference::Monospace,
            font_size: 14.0,
            line_cache: LineLayoutCache::default(),
        }
    }
}

impl VirtualEditorState {
    /// Clears cached galleys and transient navigation state when opening/creating a document.
    pub fn reset_for_new_document(&mut self) {
        self.scroll_to_cursor = true;
        self.desired_col = None;
        self.dragging = false;
        self.horiz_offset = 0.0;
        self.last_cursor_col = 0;
        self.last_visible_rows = 0..0;
        self.ime_composing = false;
        self.ime_preedit.clear();
        self.line_cache.entries.clear();
        self.line_cache.total_bytes = 0;
    }

    fn prepare_line_cache(
        &mut self,
        ui: &egui::Ui,
        theme_mode: ThemeMode,
        font_family: crate::ui::settings::FontPreference,
        font_size: f32,
    ) {
        let cur_atlas = ui.fonts(|f| f.texture_atlas());
        let atlas_changed = self.line_cache.atlas.as_ptr() != Arc::as_ptr(&cur_atlas);
        let font_bits = font_size.to_bits();
        let ppp_bits = ui.ctx().pixels_per_point().to_bits();
        if atlas_changed
            || self.line_cache.theme_mode != theme_mode
            || self.line_cache.font_family != font_family
            || self.line_cache.font_size_bits != font_bits
            || self.line_cache.ppp_bits != ppp_bits
        {
            self.line_cache.entries.clear();
            self.line_cache.total_bytes = 0;
            self.line_cache.theme_mode = theme_mode;
            self.line_cache.font_family = font_family;
            self.line_cache.font_size_bits = font_bits;
            self.line_cache.ppp_bits = ppp_bits;
            self.line_cache.atlas = Arc::downgrade(&cur_atlas);
        }
    }

    fn get_or_build_line(
        &mut self,
        ui: &egui::Ui,
        buf: &DocumentBuffer,
        line_idx: usize,
        font_id: &FontId,
        theme: &WordTheme,
    ) -> (&str, bool, Arc<VisualBidiLineMap>, Option<Arc<egui::Galley>>) {
        let hit = self
            .line_cache
            .entries
            .get(&line_idx)
            .is_some_and(|e| rope_line_equals(buf, line_idx, &e.text, e.truncated));

        if !hit {
            let (line_str, truncated) = if line_idx < buf.len_lines() {
                let rope_line = buf.rope().line(line_idx);
                let content_chars = rope_line_content_chars(&rope_line);
                if content_chars > MAX_EDITOR_LAYOUT_CHARS {
                    (rope_line.slice(..MAX_EDITOR_LAYOUT_CHARS).to_string(), true)
                } else {
                    (rope_line.slice(..content_chars).to_string(), false)
                }
            } else {
                (String::new(), false)
            };
            let bidi_map = Arc::new(build_visual_bidi_line_map(&line_str));
            let galley = if !line_str.is_empty() {
                let job = build_markdown_line_job_bidi(&line_str, &bidi_map, font_id, theme);
                Some(ui.fonts(|f| f.layout_job(job)))
            } else {
                None
            };
            let approx_bytes = line_str
                .len()
                .saturating_mul(8)
                .saturating_add(512);
            self.line_cache.tick = self.line_cache.tick.wrapping_add(1);
            let tick = self.line_cache.tick;
            if let Some(prev) = self.line_cache.entries.insert(
                line_idx,
                CachedEditorLine {
                    text: line_str,
                    truncated,
                    bidi_map,
                    galley,
                    last_tick: tick,
                    approx_bytes,
                },
            ) {
                self.line_cache.total_bytes =
                    self.line_cache.total_bytes.saturating_sub(prev.approx_bytes);
            }
            self.line_cache.total_bytes =
                self.line_cache.total_bytes.saturating_add(approx_bytes);
        } else {
            self.line_cache.tick = self.line_cache.tick.wrapping_add(1);
            let tick = self.line_cache.tick;
            if let Some(entry) = self.line_cache.entries.get_mut(&line_idx) {
                entry.last_tick = tick;
            }
        }

        let entry = self.line_cache.entries.get(&line_idx).unwrap();
        (
            entry.text.as_str(),
            entry.truncated,
            Arc::clone(&entry.bidi_map),
            entry.galley.clone(),
        )
    }

    fn prune_line_cache(&mut self, visible: &Range<usize>) {
        let max_bytes = adaptive_line_cache_bytes();
        let max_entries = 4_096usize;
        if self.line_cache.entries.len() <= max_entries
            && self.line_cache.total_bytes <= max_bytes
        {
            return;
        }
        let keep_start = visible.start.saturating_sub(128);
        let keep_end = visible.end.saturating_add(128);
        self.line_cache
            .entries
            .retain(|&idx, _| idx >= keep_start && idx < keep_end);
        self.line_cache.total_bytes = self
            .line_cache
            .entries
            .values()
            .map(|e| e.approx_bytes)
            .sum();
    }
}

/// Renders an O(1)-viewport virtualized Rope editor that handles multi-megabyte files at 120 FPS.
pub fn render_virtual_editor(
    ui: &mut egui::Ui,
    buf: &mut DocumentBuffer,
    theme: &WordTheme,
    state: &mut VirtualEditorState,
    block_shortcuts: bool,
) -> EditorAction {
    let mut editor_action = EditorAction::None;
    let editor_id = egui::Id::new("virtual_rope_editor");
    if state.request_focus {
        ui.memory_mut(|m| m.request_focus(editor_id));
        state.request_focus = false;
    }

    let font_id = FontId::new(
        state.font_size.clamp(11.0, 28.0),
        state.font_family.to_font_family(),
    );
    state.prepare_line_cache(ui, theme.mode, state.font_family, font_id.size);
    let row_height = (font_id.size * 1.5).round().max(18.0);
    let char_width = ui.fonts(|f| f.glyph_width(&font_id, 'm')).max(7.5);
    let total_lines = buf.len_lines().max(1);
    let gutter_digits = total_lines.to_string().len().max(3);
    let gutter_w = (gutter_digits as f32 + 1.5) * char_width + 14.0;

    let has_focus = ui.memory(|m| m.has_focus(editor_id));
    if has_focus {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                editor_id,
                EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            );
        });
    }

    // Handle keyboard & clipboard input when focused and no modal dialog blocks shortcuts.
    if has_focus && !block_shortcuts {
        handle_editor_input(ui, buf, state, &mut editor_action);
    }

    let (cursor_line, cursor_col) = buf.char_to_line_col(buf.cursor_char);
    let selection = buf.selection_range();

    let mut scroll_area = egui::ScrollArea::vertical()
        .id_salt("virtual_editor_scroll")
        .auto_shrink([false, false]);

    let did_scroll_to_cursor = state.scroll_to_cursor;

    // Only adjust vertical scroll offset when cursor leaves the safe visible row window.
    if state.scroll_to_cursor {
        if !state.last_visible_rows.is_empty() && state.last_visible_rows.len() >= 3 {
            let top_safe = state.last_visible_rows.start.saturating_add(1);
            let bot_safe = state.last_visible_rows.end.saturating_sub(2).max(top_safe);
            if cursor_line < top_safe {
                let target_y = cursor_line.saturating_sub(1) as f32 * row_height;
                scroll_area = scroll_area.vertical_scroll_offset(target_y.max(0.0));
            } else if cursor_line >= bot_safe {
                let visible_rows = state.last_visible_rows.len().max(4);
                let top_row = (cursor_line + 2).saturating_sub(visible_rows);
                let target_y = top_row as f32 * row_height;
                scroll_area = scroll_area.vertical_scroll_offset(target_y.max(0.0));
            }
        } else if cursor_line > 5 {
            let target_y = cursor_line as f32 * row_height;
            scroll_area = scroll_area.vertical_scroll_offset((target_y - 120.0).max(0.0));
        }
        state.scroll_to_cursor = false;
        state.pending_scroll_delta_y = 0.0;
    } else if state.pending_scroll_delta_y.abs() > 0.001 {
        let delta = state.pending_scroll_delta_y;
        state.pending_scroll_delta_y = 0.0;
        let next_y = (state.last_scroll_offset_y + delta).clamp(0.0, state.last_max_scroll_y);
        scroll_area = scroll_area.vertical_scroll_offset(next_y);
    } else {
        state.pending_scroll_delta_y = 0.0;
    }

    let full_rect = ui.available_rect_before_wrap();
    if full_rect.width() > 2.0 && full_rect.height() > 2.0 {
        let bg = ui.painter_at(full_rect);
        bg.rect_filled(full_rect, 0.0, theme.paper_bg);
        let full_gutter = egui::Rect::from_min_max(
            full_rect.min,
            egui::pos2((full_rect.left() + gutter_w).min(full_rect.right()), full_rect.bottom()),
        );
        bg.rect_filled(full_gutter, 0.0, theme.ribbon_bg);
        bg.line_segment(
            [
                egui::pos2(full_gutter.right() - 0.5, full_rect.top()),
                egui::pos2(full_gutter.right() - 0.5, full_rect.bottom()),
            ],
            Stroke::new(1.0_f32, theme.border),
        );
    }

    let mut horiz_metrics: Option<(f32, f32)> = None;

    let scroll_out = scroll_area.show_rows(ui, row_height, total_lines, |ui, row_range| {
        state.last_visible_rows = row_range.clone();
        let visible_count = row_range.len().max(1);
        let canvas_w = ui.available_width().max(240.0);
        let block_h = visible_count as f32 * row_height;

        let max_cols = row_range
            .clone()
            .map(|l| buf.line_len_without_newline(l).min(MAX_EDITOR_LAYOUT_CHARS + 32))
            .max()
            .unwrap_or(0);
        let max_content_w = ((max_cols as f32 + 4.0) * char_width).max(1.0);

        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(canvas_w, block_h),
            Sense::hover(),
        );
        let response = ui.interact(rect, editor_id, Sense::click_and_drag());

        let text_left = rect.left() + gutter_w + 8.0;
        let text_area_w = (canvas_w - gutter_w - 16.0).max(80.0);
        horiz_metrics = Some((max_content_w, text_area_w));

        if cursor_col != state.last_cursor_col || did_scroll_to_cursor {
            let (cur_line_empty, cur_truncated, cur_bidi, cur_galley) = {
                let (s, tr, bm, g) = state.get_or_build_line(ui, buf, cursor_line, &font_id, theme);
                (s.is_empty(), tr, bm, g)
            };
            let clamped_col = if cur_truncated {
                cursor_col.min(MAX_EDITOR_LAYOUT_CHARS)
            } else {
                cursor_col
            };
            let (cur_g_w, cur_x_in_galley) = if cur_line_empty {
                (0.0_f32, 0.0_f32)
            } else if let Some(ref g) = cur_galley {
                let vis_c = cur_bidi.visual_col_for_logical(clamped_col);
                let pos_x = g.pos_from_ccursor(egui::text::CCursor::new(vis_c)).min.x;
                (g.size().x, pos_x)
            } else {
                (0.0_f32, 0.0_f32)
            };
            let base_x_rel = match cur_bidi.base_dir {
                TextDirection::Rtl => {
                    ((rect.right() - 14.0 - cur_g_w).max(text_left) - text_left).max(0.0)
                }
                TextDirection::Ltr => 0.0,
            };
            let cursor_x_rel = base_x_rel + cur_x_in_galley;
            if cur_bidi.base_dir == TextDirection::Rtl && cur_g_w <= text_area_w {
                state.horiz_offset = 0.0;
            } else if cursor_x_rel - state.horiz_offset > text_area_w - char_width * 2.0 {
                state.horiz_offset = (cursor_x_rel - text_area_w + char_width * 4.0).max(0.0);
            } else if cursor_x_rel < state.horiz_offset {
                state.horiz_offset = (cursor_x_rel - char_width * 2.0).max(0.0);
            }
            if cur_bidi.base_dir == TextDirection::Ltr && cursor_col == 0 && state.horiz_offset > 0.0
            {
                state.horiz_offset = 0.0;
            }
            state.last_cursor_col = cursor_col;
        }
        let h_scroll_delta = ui.input(|i| i.smooth_scroll_delta.x);
        if response.hovered() && h_scroll_delta.abs() > 0.1 {
            let max_off = (max_content_w - text_area_w).max(0.0);
            state.horiz_offset = (state.horiz_offset - h_scroll_delta).clamp(0.0, max_off);
        }

        if !state.autoscroll_active {
            if response.clicked() || response.drag_started_by(egui::PointerButton::Primary) {
                ui.memory_mut(|m| m.request_focus(editor_id));
            }

            let cmd_down = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
            let mut hovered_link_url: Option<String> = None;

            if cmd_down && response.hovered() && !state.dragging
                && let Some(hover_pos) = ui.input(|i| i.pointer.hover_pos())
                    && rect.contains(hover_pos) && hover_pos.x >= rect.left() + gutter_w {
                        let rel_y = (hover_pos.y - rect.top()).max(0.0);
                        let line_offset = (rel_y / row_height).floor() as usize;
                        let h_line = (row_range.start + line_offset).min(total_lines - 1);
                        let (line_empty, bidi_map, h_galley_opt) = {
                            let (s, _tr, bm, g) =
                                state.get_or_build_line(ui, buf, h_line, &font_id, theme);
                            (s.is_empty(), bm, g)
                        };
                        if !line_empty
                            && let Some(ref h_galley) = h_galley_opt {
                                let line_str = &state.line_cache.entries[&h_line].text;
                                let galley_w = h_galley.size().x;
                                let galley_origin_x = match bidi_map.base_dir {
                                    TextDirection::Rtl => {
                                        (rect.right() - 14.0 - galley_w).max(text_left)
                                            - state.horiz_offset
                                    }
                                    TextDirection::Ltr => text_left - state.horiz_offset,
                                };
                                if hover_pos.x >= galley_origin_x - 2.0
                                    && hover_pos.x <= galley_origin_x + galley_w + 4.0
                                {
                                    let rel_x = (hover_pos.x - galley_origin_x).max(0.0);
                                    let vis_col = h_galley
                                        .cursor_from_pos(Vec2::new(rel_x, row_height * 0.5))
                                        .ccursor
                                        .index;
                                    let log_col = bidi_map.logical_col_for_visual(line_str, vis_col);
                                    if let Some(raw_url) =
                                        extract_markdown_link_at_char(line_str, log_col)
                                        && classify_link_url(&raw_url).is_some() {
                                            ui.ctx()
                                                .set_cursor_icon(egui::CursorIcon::PointingHand);
                                            hovered_link_url = Some(raw_url);
                                        }
                                }
                            }
                    }

            // Mouse click / double-click / triple-click / right-click / drag positioning mapped directly via exact Galley font metrics + BiDi/Grapheme snapping.
            let pointer_pos_opt = response.interact_pointer_pos().or_else(|| {
                response
                    .secondary_clicked()
                    .then(|| ui.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos())))
                    .flatten()
            });
            if let Some(pointer_pos) = pointer_pos_opt {
                let rel_y = (pointer_pos.y - rect.top()).max(0.0);
                let line_offset = (rel_y / row_height).floor() as usize;
                let clicked_line = (row_range.start + line_offset).min(total_lines - 1);
                let (line_empty, bidi_map, clicked_galley_opt) = {
                    let (s, _tr, bm, g) =
                        state.get_or_build_line(ui, buf, clicked_line, &font_id, theme);
                    (s.is_empty(), bm, g)
                };
                let in_gutter = pointer_pos.x < rect.left() + gutter_w;
                let (clicked_col, in_text_span) = if line_empty {
                    (0, false)
                } else if let Some(ref clicked_galley) = clicked_galley_opt {
                    let line_str = &state.line_cache.entries[&clicked_line].text;
                    let galley_w = clicked_galley.size().x;
                    let galley_origin_x = match bidi_map.base_dir {
                        TextDirection::Rtl => {
                            (rect.right() - 14.0 - galley_w).max(text_left) - state.horiz_offset
                        }
                        TextDirection::Ltr => text_left - state.horiz_offset,
                    };
                    let inside_text = !in_gutter
                        && pointer_pos.x >= galley_origin_x - 2.0
                        && pointer_pos.x <= galley_origin_x + galley_w + 4.0;
                    let rel_x_in_galley = (pointer_pos.x - galley_origin_x).max(0.0);
                    let vis_col = clicked_galley
                        .cursor_from_pos(Vec2::new(rel_x_in_galley, row_height * 0.5))
                        .ccursor
                        .index;
                    (
                        bidi_map.logical_col_for_visual(line_str, vis_col),
                        inside_text,
                    )
                } else {
                    (0, false)
                };
                let line_start_char = buf.line_to_char(clicked_line);
                let line_end_char = line_start_char + buf.line_len_without_newline(clicked_line);
                let target_char = buf.line_col_to_char(clicked_line, clicked_col);

                let clicked_ctrl_link = if cmd_down && response.clicked() && in_text_span {
                    let line_str = &state.line_cache.entries[&clicked_line].text;
                    extract_markdown_link_at_char(line_str, clicked_col)
                        .filter(|u| classify_link_url(u).is_some())
                } else {
                    None
                };

                if let Some(raw_url) = clicked_ctrl_link {
                    editor_action = EditorAction::OpenLink(raw_url);
                    state.dragging = false;
                } else if response.secondary_clicked() {
                    ui.memory_mut(|m| m.request_focus(editor_id));
                    let preserve_selection = buf.selection_range().is_some_and(|sel| {
                        sel.start < sel.end
                            && if in_gutter {
                                sel.start <= line_end_char && sel.end >= line_start_char
                            } else if in_text_span {
                                target_char >= sel.start && target_char <= sel.end
                            } else {
                                sel.start < line_start_char && sel.end > line_end_char
                            }
                    });
                    if !preserve_selection {
                        let new_pos = if in_gutter {
                            line_start_char
                        } else {
                            target_char
                        };
                        buf.set_cursor(new_pos, false);
                        state.desired_col = Some(if in_gutter { 0 } else { clicked_col });
                    }
                    let link_url = if in_text_span {
                        let line_str = &state.line_cache.entries[&clicked_line].text;
                        extract_markdown_link_at_char(line_str, clicked_col)
                            .filter(|u| classify_link_url(u).is_some())
                    } else {
                        None
                    };
                    editor_action = EditorAction::OpenContextMenu {
                        pos: pointer_pos,
                        link_url,
                        cursor_line: clicked_line,
                    };
                    state.dragging = false;
                } else if response.triple_clicked() {
                    if line_end_char > line_start_char {
                        buf.selection_anchor = Some(line_start_char);
                        buf.cursor_char = line_end_char;
                        buf.cache_current_selection();
                    } else {
                        buf.set_cursor(line_start_char, false);
                    }
                    state.dragging = false;
                    state.desired_col = None;
                } else if response.double_clicked() {
                    let (sel_start, sel_end) = if in_text_span {
                        let line_str = &state.line_cache.entries[&clicked_line].text;
                        if let Some(w_range) = word_range_in_line(line_str, clicked_col) {
                            (line_start_char + w_range.start, line_start_char + w_range.end)
                        } else {
                            (line_start_char, line_end_char)
                        }
                    } else {
                        (line_start_char, line_end_char)
                    };
                    if sel_end > sel_start {
                        buf.selection_anchor = Some(sel_start);
                        buf.cursor_char = sel_end;
                        buf.cache_current_selection();
                    } else {
                        buf.set_cursor(line_start_char, false);
                    }
                    state.dragging = false;
                    state.desired_col = None;
                } else if response.drag_started_by(egui::PointerButton::Primary) {
                    let shift = ui.input(|i| i.modifiers.shift);
                    buf.set_cursor(target_char, shift);
                    state.dragging = true;
                    state.desired_col = Some(clicked_col);
                } else if response.dragged() && state.dragging {
                    buf.set_cursor(target_char, true);
                    state.desired_col = Some(clicked_col);
                } else if response.clicked() {
                    if in_gutter {
                        if line_end_char > line_start_char {
                            buf.selection_anchor = Some(line_start_char);
                            buf.cursor_char = line_end_char;
                            buf.cache_current_selection();
                        } else {
                            buf.set_cursor(line_start_char, false);
                        }
                        state.desired_col = None;
                    } else {
                        let shift = ui.input(|i| i.modifiers.shift);
                        buf.set_cursor(target_char, shift);
                        state.desired_col = Some(clicked_col);
                    }
                }
            }

            if let Some(url) = hovered_link_url {
                crate::ui::reader::show_link_hover_tooltip(
                    ui,
                    editor_id.with("link_tip"),
                    &format!("Ctrl+Click to open: {url}"),
                );
            }
        }
        if response.drag_stopped() {
            state.dragging = false;
        }
        if buf.selection_range().is_some() {
            buf.cache_current_selection();
        }

        let bg_painter = ui.painter_at(rect);
        bg_painter.rect_filled(rect, 0.0, theme.paper_bg);

        // Strictly clipped text region so long lines never bleed into the gutter or splitter.
        let text_clip = egui::Rect::from_min_max(
            egui::pos2(rect.left() + gutter_w + 1.0, rect.top()),
            rect.max,
        );
        let text_painter = ui.painter_at(text_clip);

        // Pinned line-number gutter
        let gutter_rect = egui::Rect::from_min_max(
            rect.min,
            egui::pos2((rect.left() + gutter_w).min(rect.right()), rect.bottom()),
        );
        let gutter_painter = ui.painter_at(gutter_rect);
        gutter_painter.rect_filled(gutter_rect, 0.0, theme.ribbon_bg);
        gutter_painter.line_segment(
            [
                egui::pos2(gutter_rect.right() - 0.5, rect.top()),
                egui::pos2(gutter_rect.right() - 0.5, rect.bottom()),
            ],
            Stroke::new(1.0_f32, theme.border),
        );

        let match_bg = Color32::from_rgba_unmultiplied(245, 185, 35, 76);
        let active_match_bg = Color32::from_rgba_unmultiplied(255, 150, 20, 130);
        let active_match_stroke = Stroke::new(1.5_f32, theme.warning_amber);

        // Paint ONLY the visible rows in `row_range` using cached Galleys!
        for (i, line_idx) in row_range.clone().enumerate() {
            let y = rect.top() + i as f32 * row_height;
            let line_rect = egui::Rect::from_min_size(
                egui::pos2(rect.left() + gutter_w + 1.0, y),
                Vec2::new((canvas_w - gutter_w - 1.0).max(0.0), row_height),
            );

            if line_idx == cursor_line {
                text_painter.rect_filled(line_rect, 0.0, theme.active_line_bg);
            }

            // Cached syntax-highlighted line layout with BiDi/Arabic shaping + Galley metrics
            let (line_truncated, bidi_map, line_galley) = {
                let (_s, tr, bm, g) = state.get_or_build_line(ui, buf, line_idx, &font_id, theme);
                (tr, bm, g)
            };
            let galley_w = line_galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
            let galley_origin_x = match bidi_map.base_dir {
                TextDirection::Rtl => {
                    (rect.right() - 14.0 - galley_w).max(text_left) - state.horiz_offset
                }
                TextDirection::Ltr => text_left - state.horiz_offset,
            };

            let x_for_visual_col = |vis_col: usize| -> f32 {
                if let Some(ref g) = line_galley {
                    let pos = g.pos_from_ccursor(egui::text::CCursor::new(vis_col));
                    galley_origin_x + pos.min.x
                } else {
                    galley_origin_x + vis_col as f32 * char_width
                }
            };
            let x_for_logical_col = |log_col: usize| -> f32 {
                let clamped = if line_truncated {
                    log_col.min(MAX_EDITOR_LAYOUT_CHARS)
                } else {
                    log_col
                };
                x_for_visual_col(bidi_map.visual_col_for_logical(clamped))
            };

            let line_start_char = buf.line_to_char(line_idx);
            let line_len = buf.line_len_without_newline(line_idx);
            let layout_line_len = if line_truncated {
                line_len.min(MAX_EDITOR_LAYOUT_CHARS)
            } else {
                line_len
            };
            let line_end_char = line_start_char + line_len;

            // Search match highlights on this line in O(log M)
            let first_match = state
                .search_matches
                .partition_point(|m| m.end <= line_start_char);
            for m in &state.search_matches[first_match..] {
                if m.start >= line_end_char {
                    break;
                }
                if m.end > line_start_char && m.start < line_end_char {
                    let s_col = m.start.saturating_sub(line_start_char).min(layout_line_len);
                    let e_col = m.end.saturating_sub(line_start_char).min(layout_line_len);
                    let is_active_m = state.active_search_match.as_ref() == Some(m);
                    for vr in bidi_map.visual_ranges_for_logical_span(s_col, e_col) {
                        let x1 = x_for_visual_col(vr.start);
                        let x2 = x_for_visual_col(vr.end);
                        let hi_rect = egui::Rect::from_min_max(
                            egui::pos2(x1.min(x2), y + 2.0),
                            egui::pos2(x1.max(x2), y + row_height - 2.0),
                        );
                        text_painter.rect_filled(
                            hi_rect,
                            2.0,
                            if is_active_m {
                                active_match_bg
                            } else {
                                match_bg
                            },
                        );
                        if is_active_m {
                            text_painter.rect_stroke(
                                hi_rect,
                                2.0,
                                active_match_stroke,
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                }
            }

            // Selection highlight on this line
            if let Some(sel) = &selection
                && sel.end > line_start_char && sel.start <= line_end_char {
                    let s_col = sel.start.saturating_sub(line_start_char).min(layout_line_len);
                    let e_col = sel.end.saturating_sub(line_start_char).min(layout_line_len);
                    for vr in bidi_map.visual_ranges_for_logical_span(s_col, e_col) {
                        let x1 = x_for_visual_col(vr.start);
                        let x2 = x_for_visual_col(vr.end);
                        let lo = x1.min(x2);
                        let hi = x1.max(x2)
                            + if sel.end > line_end_char && vr.end == layout_line_len {
                                char_width * 0.6
                            } else {
                                0.0
                            };
                        if hi > lo {
                            text_painter.rect_filled(
                                egui::Rect::from_min_max(
                                    egui::pos2(lo, y + 1.0),
                                    egui::pos2(hi, y + row_height - 1.0),
                                ),
                                2.0,
                                theme.selection_bg,
                            );
                        }
                    }
                }

            // Line number
            let num_str = format!("{:>width$}", line_idx + 1, width = gutter_digits);
            let num_color = if line_idx == cursor_line {
                theme.accent
            } else {
                theme.text_muted
            };
            gutter_painter.text(
                egui::pos2(rect.left() + 6.0, y + 3.0),
                egui::Align2::LEFT_TOP,
                num_str,
                font_id.clone(),
                num_color,
            );

            let caret_x_opt = if line_idx == cursor_line {
                Some(x_for_logical_col(cursor_col))
            } else {
                None
            };

            if let Some(galley) = line_galley {
                text_painter.galley(
                    egui::pos2(galley_origin_x, y + 3.0),
                    galley,
                    theme.text_primary,
                );
            }

            if line_truncated {
                let badge_text = if line_idx == cursor_line && cursor_col > MAX_EDITOR_LAYOUT_CHARS {
                    format!(
                        "… [line truncated for display • col {}]",
                        cursor_col + 1
                    )
                } else {
                    "… [line truncated for display]".to_string()
                };
                let badge_font = FontId::proportional(11.0);
                let badge_galley = ui.fonts(|f| {
                    f.layout_no_wrap(badge_text, badge_font, theme.warning_amber)
                });
                let pill_w = badge_galley.size().x + 12.0;
                let pill_x = (rect.right() - pill_w - 10.0)
                    .max(text_clip.left() + 8.0)
                    .min((galley_origin_x + galley_w + 8.0).max(rect.right() - pill_w - 10.0));
                let pill_rect = egui::Rect::from_min_size(
                    egui::pos2(pill_x, y + 2.0),
                    Vec2::new(pill_w, row_height - 4.0),
                );
                text_painter.rect_filled(pill_rect, 4.0, theme.ribbon_bg);
                text_painter.rect_stroke(
                    pill_rect,
                    4.0,
                    Stroke::new(1.0_f32, theme.warning_amber.gamma_multiply(0.7)),
                    egui::StrokeKind::Inside,
                );
                text_painter.galley(
                    egui::pos2(
                        pill_rect.left() + 6.0,
                        pill_rect.center().y - badge_galley.size().y * 0.5,
                    ),
                    badge_galley,
                    theme.warning_amber,
                );
            }

            // Caret (accent when focused, muted when unfocused so cursor position stays clear)
            if let Some(caret_x) = caret_x_opt {
                if has_focus && !state.ime_preedit.is_empty() {
                    let pre_dir = detect_paragraph_direction(&state.ime_preedit);
                    let shaped_pre = shape_bidi_for_display(&state.ime_preedit, pre_dir);
                    let mut pre_job = LayoutJob::default();
                    pre_job.append(
                        &shaped_pre,
                        0.0,
                        TextFormat {
                            font_id: font_id.clone(),
                            color: theme.accent,
                            underline: Stroke::new(1.5_f32, theme.accent),
                            background: theme.accent_soft,
                            ..Default::default()
                        },
                    );
                    let pre_galley = ui.fonts(|f| f.layout_job(pre_job));
                    text_painter.galley(
                        egui::pos2(caret_x, y + 3.0),
                        pre_galley,
                        theme.accent,
                    );
                }
                let (caret_w, caret_col) = if has_focus {
                    (2.0_f32, theme.accent)
                } else {
                    (1.5_f32, theme.text_muted)
                };
                text_painter.line_segment(
                    [
                        egui::pos2(caret_x, y + 2.0),
                        egui::pos2(caret_x, y + row_height - 2.0),
                    ],
                    Stroke::new(caret_w, caret_col),
                );
            }
        }

        state.prune_line_cache(&row_range);
    });

    state.last_scroll_area_id = Some(scroll_out.id);
    state.last_pane_rect = scroll_out.inner_rect;
    state.last_scroll_offset_y = scroll_out.state.offset.y;
    state.last_max_scroll_y = (scroll_out.content_size.y - scroll_out.inner_rect.height()).max(0.0);

    let mut bottom_reserve = 4.0_f32;
    if let Some((max_content_w, text_area_w)) = horiz_metrics
        && max_content_w > text_area_w + 8.0 && full_rect.width() > gutter_w + 48.0 {
            bottom_reserve = 14.0;
            let track_rect = egui::Rect::from_min_max(
                egui::pos2(full_rect.left() + gutter_w + 6.0, full_rect.bottom() - 10.0),
                egui::pos2(full_rect.right() - 12.0, full_rect.bottom() - 3.0),
            );
            if track_rect.width() > 24.0 {
                let max_off = (max_content_w - text_area_w).max(1.0);
                state.horiz_offset = state.horiz_offset.clamp(0.0, max_off);
                let ratio_vis = (text_area_w / max_content_w).clamp(0.12, 1.0);
                let thumb_w = (track_rect.width() * ratio_vis).max(28.0);
                let travel = (track_rect.width() - thumb_w).max(1.0);
                let thumb_x = track_rect.left() + (state.horiz_offset / max_off) * travel;
                let thumb_rect = egui::Rect::from_min_size(
                    egui::pos2(thumb_x, track_rect.top()),
                    Vec2::new(thumb_w, track_rect.height()),
                );

                let sb_resp = ui.interact(
                    track_rect,
                    editor_id.with("h_scrollbar"),
                    Sense::click_and_drag(),
                );
                if sb_resp.dragged() {
                    let dx = sb_resp.drag_delta().x;
                    state.horiz_offset =
                        (state.horiz_offset + (dx / travel) * max_off).clamp(0.0, max_off);
                } else if sb_resp.clicked()
                    && let Some(pos) = sb_resp.interact_pointer_pos() {
                        let target_ratio = ((pos.x - track_rect.left() - thumb_w * 0.5) / travel)
                            .clamp(0.0, 1.0);
                        state.horiz_offset = target_ratio * max_off;
                    }

                let p = ui.painter_at(full_rect);
                p.rect_filled(track_rect, 3.0, theme.pill_bg);
                let thumb_col = if sb_resp.hovered() || sb_resp.dragged() {
                    theme.accent
                } else {
                    theme.border
                };
                p.rect_filled(thumb_rect, 3.0, thumb_col);
            }
        }

    // Clicking empty canvas below the last line on short documents focuses and moves cursor to end.
    let content_bottom = full_rect.top() + total_lines as f32 * row_height;
    if content_bottom < full_rect.bottom() - bottom_reserve && full_rect.width() > 20.0 {
        let tail_rect = egui::Rect::from_min_max(
            egui::pos2(full_rect.left(), content_bottom),
            egui::pos2(full_rect.right() - 12.0, full_rect.bottom() - bottom_reserve),
        );
        if tail_rect.width() > 4.0 && tail_rect.height() > 4.0 {
            let tail_resp = ui.interact(tail_rect, editor_id.with("tail_click"), Sense::click());
            if tail_resp.clicked() {
                ui.memory_mut(|m| m.request_focus(editor_id));
                let end = buf.len_chars();
                let shift = ui.input(|i| i.modifiers.shift);
                buf.set_cursor(end, shift);
            }
        }
    }

    editor_action
}

fn prev_word_boundary_char(buf: &DocumentBuffer, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let (line_idx, col) = buf.char_to_line_col(pos);
    if col == 0 {
        let prev_start = buf.line_to_char(line_idx - 1);
        return prev_start + buf.line_len_without_newline(line_idx - 1);
    }
    let line_slice = buf.rope().line(line_idx);
    let line_len = buf.line_len_without_newline(line_idx);
    let mut c = col.min(line_len);
    while c > 0 && line_slice.char(c - 1).is_whitespace() {
        c -= 1;
    }
    if c > 0 {
        let target_word = is_unicode_word_char(line_slice.char(c - 1));
        while c > 0 {
            let prev_ch = line_slice.char(c - 1);
            if prev_ch.is_whitespace() || is_unicode_word_char(prev_ch) != target_word {
                break;
            }
            c -= 1;
        }
    }
    buf.line_to_char(line_idx) + snap_grapheme_start_in_rope_line(line_slice, c, line_len)
}

fn next_word_boundary_char(buf: &DocumentBuffer, pos: usize) -> usize {
    let total = buf.len_chars();
    if pos >= total {
        return total;
    }
    let (line_idx, col) = buf.char_to_line_col(pos);
    let line_slice = buf.rope().line(line_idx);
    let line_len = buf.line_len_without_newline(line_idx);
    let mut c = col.min(line_len);
    if c >= line_len {
        if line_idx + 1 < buf.len_lines() {
            return buf.line_to_char(line_idx + 1);
        }
        return total;
    }
    if line_slice.char(c).is_whitespace() {
        while c < line_len && line_slice.char(c).is_whitespace() {
            c += 1;
        }
    } else {
        let target_word = is_unicode_word_char(line_slice.char(c));
        while c < line_len {
            let cur_ch = line_slice.char(c);
            if cur_ch.is_whitespace() || is_unicode_word_char(cur_ch) != target_word {
                break;
            }
            c += 1;
        }
        while c < line_len && line_slice.char(c).is_whitespace() {
            c += 1;
        }
    }
    let snapped = snap_grapheme_end_in_rope_line(line_slice, c, line_len);
    buf.line_to_char(line_idx) + snapped
}

fn handle_editor_input(
    ui: &mut egui::Ui,
    buf: &mut DocumentBuffer,
    state: &mut VirtualEditorState,
    editor_action: &mut EditorAction,
) {
    let shift_held_global = ui.input(|i| i.modifiers.shift);
    let mut pasted_this_frame = false;
    let events = ui.input(|i| i.events.clone());
    for event in events {
        match event {
            egui::Event::Ime(ime) => match ime {
                egui::ImeEvent::Enabled => {
                    state.ime_composing = true;
                }
                egui::ImeEvent::Preedit(text) => {
                    state.ime_composing = !text.is_empty();
                    state.ime_preedit = text;
                }
                egui::ImeEvent::Commit(text) => {
                    state.ime_preedit.clear();
                    state.ime_composing = false;
                    if !text.is_empty() {
                        buf.insert_at_cursor(&text);
                        state.desired_col = None;
                        state.scroll_to_cursor = true;
                    }
                }
                egui::ImeEvent::Disabled => {
                    state.ime_preedit.clear();
                    state.ime_composing = false;
                }
            },
            egui::Event::Text(txt) => {
                // Skip raw Text events while IME preedit is uncommitted unless committed cleanly.
                if state.ime_composing {
                    state.ime_preedit.clear();
                    state.ime_composing = false;
                }
                // Filter control characters handled by Key events
                if !txt.chars().all(|c| c.is_control()) {
                    buf.insert_at_cursor(&txt);
                    state.desired_col = None;
                    state.scroll_to_cursor = true;
                }
            }
            egui::Event::Paste(pasted) => {
                pasted_this_frame = true;
                if !pasted.is_empty() {
                    let text_to_insert = if shift_held_global {
                        pasted
                    } else {
                        let html =
                            crate::document::clipboard::read_os_clipboard_html_matching(&pasted);
                        crate::document::clipboard::smart_paste_markdown(&pasted, html.as_deref())
                    };
                    buf.insert_at_cursor(&text_to_insert);
                    state.desired_col = None;
                    state.scroll_to_cursor = true;
                }
            }
            egui::Event::Copy => {
                let selected = buf.selected_text();
                if !selected.is_empty() {
                    *editor_action = EditorAction::CopySelection(selected);
                }
            }
            egui::Event::Cut => {
                if let Some(sel) = buf.selection_range() {
                    let selected = buf.selected_text();
                    buf.remove(sel);
                    state.desired_col = None;
                    state.scroll_to_cursor = true;
                    if !selected.is_empty() {
                        *editor_action = EditorAction::CutSelection(selected);
                    }
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

                // Let app-level shortcuts (Ctrl+B, I, U, K, S, O, N, P, Z, Y, F, H, =/+/-/0, 1..4) pass cleanly.
                if cmd
                    && matches!(
                        key,
                        Key::B
                            | Key::I
                            | Key::U
                            | Key::K
                            | Key::S
                            | Key::O
                            | Key::N
                            | Key::P
                            | Key::Z
                            | Key::Y
                            | Key::F
                            | Key::H
                            | Key::Equals
                            | Key::Plus
                            | Key::Minus
                            | Key::Num0
                            | Key::Num1
                            | Key::Num2
                            | Key::Num3
                            | Key::Num4
                    )
                {
                    continue;
                }

                match key {
                    Key::V if cmd && shift => {
                        if !pasted_this_frame
                            && let Some(plain) =
                                crate::document::clipboard::read_os_clipboard_plain_text()
                                && !plain.is_empty() {
                                    buf.insert_at_cursor(&plain);
                                    state.desired_col = None;
                                    state.scroll_to_cursor = true;
                                    pasted_this_frame = true;
                                }
                    }
                    Key::A if cmd => {
                        buf.selection_anchor = Some(0);
                        buf.cursor_char = buf.len_chars();
                        buf.cache_current_selection();
                    }
                    Key::Backspace => {
                        if cmd && buf.selection_range().is_none() {
                            let cur = buf.cursor_char;
                            let prev = prev_word_boundary_char(buf, cur);
                            if prev < cur {
                                buf.remove(prev..cur);
                            }
                        } else {
                            buf.backspace_grapheme();
                        }
                        state.desired_col = None;
                        state.scroll_to_cursor = true;
                    }
                    Key::Delete => {
                        if cmd && buf.selection_range().is_none() {
                            let cur = buf.cursor_char;
                            let next = next_word_boundary_char(buf, cur);
                            if next > cur {
                                buf.remove(cur..next);
                            }
                        } else {
                            buf.delete_grapheme();
                        }
                        state.desired_col = None;
                        state.scroll_to_cursor = true;
                    }
                    Key::Enter => {
                        let le = buf.line_ending();
                        buf.insert_at_cursor(le);
                        state.desired_col = None;
                        state.scroll_to_cursor = true;
                    }
                    Key::Tab => {
                        if shift {
                            outdent_lines(buf);
                        } else {
                            indent_lines(buf);
                        }
                        state.desired_col = None;
                        state.scroll_to_cursor = true;
                    }
                    Key::ArrowLeft => {
                        let line_is_rtl = {
                            let line_idx = buf.char_to_line(buf.cursor_char);
                            detect_paragraph_direction(&buf.line_text(line_idx))
                                == TextDirection::Rtl
                        };
                        if cmd {
                            let target = if line_is_rtl {
                                next_word_boundary_char(buf, buf.cursor_char)
                            } else {
                                prev_word_boundary_char(buf, buf.cursor_char)
                            };
                            buf.set_cursor(target, shift);
                        } else if !shift {
                            if let Some(sel) = buf.selection_range() {
                                let target = if line_is_rtl { sel.end } else { sel.start };
                                buf.set_cursor(target, false);
                            } else if line_is_rtl {
                                buf.move_right_grapheme(false);
                            } else {
                                buf.move_left_grapheme(false);
                            }
                        } else if line_is_rtl {
                            buf.move_right_grapheme(true);
                        } else {
                            buf.move_left_grapheme(true);
                        }
                        state.desired_col = None;
                        state.scroll_to_cursor = true;
                    }
                    Key::ArrowRight => {
                        let line_is_rtl = {
                            let line_idx = buf.char_to_line(buf.cursor_char);
                            detect_paragraph_direction(&buf.line_text(line_idx))
                                == TextDirection::Rtl
                        };
                        if cmd {
                            let target = if line_is_rtl {
                                prev_word_boundary_char(buf, buf.cursor_char)
                            } else {
                                next_word_boundary_char(buf, buf.cursor_char)
                            };
                            buf.set_cursor(target, shift);
                        } else if !shift {
                            if let Some(sel) = buf.selection_range() {
                                let target = if line_is_rtl { sel.start } else { sel.end };
                                buf.set_cursor(target, false);
                            } else if line_is_rtl {
                                buf.move_left_grapheme(false);
                            } else {
                                buf.move_right_grapheme(false);
                            }
                        } else if line_is_rtl {
                            buf.move_left_grapheme(true);
                        } else {
                            buf.move_right_grapheme(true);
                        }
                        state.desired_col = None;
                        state.scroll_to_cursor = true;
                    }
                    Key::ArrowUp => {
                        let (line, col) = buf.char_to_line_col(buf.cursor_char);
                        let target_col = *state.desired_col.get_or_insert(col);
                        if line > 0 {
                            let line_str = buf.line_text(line - 1);
                            let snapped = snap_to_grapheme_start(&line_str, target_col);
                            let next = buf.line_col_to_char(line - 1, snapped);
                            buf.set_cursor(next, shift);
                        } else {
                            buf.set_cursor(0, shift);
                        }
                        state.scroll_to_cursor = true;
                    }
                    Key::ArrowDown => {
                        let (line, col) = buf.char_to_line_col(buf.cursor_char);
                        let target_col = *state.desired_col.get_or_insert(col);
                        if line + 1 < buf.len_lines() {
                            let line_str = buf.line_text(line + 1);
                            let snapped = snap_to_grapheme_start(&line_str, target_col);
                            let next = buf.line_col_to_char(line + 1, snapped);
                            buf.set_cursor(next, shift);
                        } else {
                            buf.set_cursor(buf.len_chars(), shift);
                        }
                        state.scroll_to_cursor = true;
                    }
                    Key::Home => {
                        let (line, _) = buf.char_to_line_col(buf.cursor_char);
                        let next = if cmd { 0 } else { buf.line_to_char(line) };
                        buf.set_cursor(next, shift);
                        state.desired_col = None;
                        state.scroll_to_cursor = true;
                    }
                    Key::End => {
                        let (line, _) = buf.char_to_line_col(buf.cursor_char);
                        let next = if cmd {
                            buf.len_chars()
                        } else {
                            buf.line_to_char(line) + buf.line_len_without_newline(line)
                        };
                        buf.set_cursor(next, shift);
                        state.desired_col = None;
                        state.scroll_to_cursor = true;
                    }
                    Key::PageUp => {
                        let (line, col) = buf.char_to_line_col(buf.cursor_char);
                        let target_col = *state.desired_col.get_or_insert(col);
                        let next_line = line.saturating_sub(25);
                        let line_str = buf.line_text(next_line);
                        let snapped = snap_to_grapheme_start(&line_str, target_col);
                        buf.set_cursor(buf.line_col_to_char(next_line, snapped), shift);
                        state.scroll_to_cursor = true;
                    }
                    Key::PageDown => {
                        let (line, col) = buf.char_to_line_col(buf.cursor_char);
                        let target_col = *state.desired_col.get_or_insert(col);
                        let next_line = (line + 25).min(buf.len_lines().saturating_sub(1));
                        let line_str = buf.line_text(next_line);
                        let snapped = snap_to_grapheme_start(&line_str, target_col);
                        buf.set_cursor(buf.line_col_to_char(next_line, snapped), shift);
                        state.scroll_to_cursor = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Maps syntax highlighting from the logical Markdown line onto its visual BiDi/Arabic-shaped string.
fn build_markdown_line_job_bidi(
    logical_line: &str,
    bidi_map: &VisualBidiLineMap,
    font_id: &FontId,
    theme: &WordTheme,
) -> LayoutJob {
    let logical_job = build_markdown_line_job(logical_line, font_id, theme);
    if bidi_map.is_identity {
        return logical_job;
    }

    // Map byte offsets in `logical_job.text` to section index per logical char
    let n = bidi_map.logical_char_len;
    let mut char_to_sec = vec![0usize; n];
    let mut byte_to_char = vec![0usize; logical_line.len() + 1];
    for (c_idx, (b_idx, ch)) in logical_line.char_indices().enumerate() {
        let b_end = b_idx + ch.len_utf8();
        for slot in &mut byte_to_char[b_idx..b_end] {
            *slot = c_idx;
        }
    }
    byte_to_char[logical_line.len()] = n;

    for (sec_idx, sec) in logical_job.sections.iter().enumerate() {
        let cs = byte_to_char[sec.byte_range.start.min(logical_line.len())];
        let ce = byte_to_char[sec.byte_range.end.min(logical_line.len())];
        for slot in &mut char_to_sec[cs..ce] {
            *slot = sec_idx;
        }
    }

    let mut vis_job = LayoutJob::default();
    let vis_chars: Vec<char> = bidi_map.visual_text.chars().collect();
    if vis_chars.is_empty() {
        return vis_job;
    }

    let fallback_fmt = TextFormat {
        font_id: font_id.clone(),
        color: theme.text_primary,
        ..Default::default()
    };

    let mut run = String::new();
    let mut cur_sec = bidi_map
        .vis_to_log
        .first()
        .and_then(|&l| char_to_sec.get(l).copied())
        .unwrap_or(0);

    for (v_idx, &ch) in vis_chars.iter().enumerate() {
        let log_idx = bidi_map.vis_to_log.get(v_idx).copied().unwrap_or(v_idx);
        let sec_idx = char_to_sec.get(log_idx).copied().unwrap_or(0);
        if sec_idx != cur_sec && !run.is_empty() {
            let fmt = logical_job
                .sections
                .get(cur_sec)
                .map(|s| s.format.clone())
                .unwrap_or_else(|| fallback_fmt.clone());
            vis_job.append(&run, 0.0, fmt);
            run.clear();
            cur_sec = sec_idx;
        }
        run.push(ch);
    }
    if !run.is_empty() {
        let fmt = logical_job
            .sections
            .get(cur_sec)
            .map(|s| s.format.clone())
            .unwrap_or(fallback_fmt);
        vis_job.append(&run, 0.0, fmt);
    }
    vis_job
}

/// Fast single-line Markdown token highlighter applied ONLY to visible viewport lines.
fn build_markdown_line_job(line: &str, font_id: &FontId, theme: &WordTheme) -> LayoutJob {
    let mut job = LayoutJob::default();
    let trimmed = line.trim_start();
    let base_fmt = TextFormat {
        font_id: font_id.clone(),
        color: theme.text_primary,
        ..Default::default()
    };

    if trimmed.starts_with('#') {
        let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes)
            && (trimmed.len() == hashes || trimmed.as_bytes()[hashes] == b' ')
        {
            job.append(
                line,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: theme.heading_color,
                    ..Default::default()
                },
            );
            return job;
        }
    }

    if trimmed.starts_with('>') {
        job.append(
            line,
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: theme.text_muted,
                italics: true,
                ..Default::default()
            },
        );
        return job;
    }

    if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
        job.append(
            line,
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: theme.accent,
                ..Default::default()
            },
        );
        return job;
    }

    if trimmed.starts_with("- [ ] ")
        || trimmed.starts_with("- [x] ")
        || trimmed.starts_with("- [X] ")
        || trimmed.starts_with("* [ ] ")
        || trimmed.starts_with("* [x] ")
        || trimmed.starts_with("* [X] ")
        || trimmed.starts_with("+ [ ] ")
        || trimmed.starts_with("+ [x] ")
        || trimmed.starts_with("+ [X] ")
    {
        let marker_end = line.find(']').map(|p| p + 1).unwrap_or(0);
        if marker_end > 0 {
            job.append(
                &line[..marker_end],
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: theme.accent,
                    ..Default::default()
                },
            );
            append_inline_syntax_highlights(&line[marker_end..], font_id, theme, &mut job);
            return job;
        }
    }

    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        let marker_pos = line.find(['-', '*', '+']).map(|p| p + 1).unwrap_or(0);
        if marker_pos > 0 {
            job.append(
                &line[..marker_pos],
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: theme.accent,
                    ..Default::default()
                },
            );
            append_inline_syntax_highlights(&line[marker_pos..], font_id, theme, &mut job);
            return job;
        }
    }

    if trimmed.starts_with('|') {
        let mut cur = String::new();
        for ch in line.chars() {
            if ch == '|' {
                if !cur.is_empty() {
                    job.append(&cur, 0.0, base_fmt.clone());
                    cur.clear();
                }
                job.append(
                    "|",
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: theme.accent,
                        ..Default::default()
                    },
                );
            } else {
                cur.push(ch);
            }
        }
        if !cur.is_empty() {
            job.append(&cur, 0.0, base_fmt);
        }
        return job;
    }

    append_inline_syntax_highlights(line, font_id, theme, &mut job);
    job
}

fn append_inline_syntax_highlights(
    slice: &str,
    font_id: &FontId,
    theme: &WordTheme,
    job: &mut LayoutJob,
) {
    let base_fmt = TextFormat {
        font_id: font_id.clone(),
        color: theme.text_primary,
        ..Default::default()
    };
    let bytes = slice.as_bytes();
    let mut i = 0usize;
    let mut plain_start = 0usize;
    let mut no_more_tick = false;
    let mut no_more_bold = false;
    let mut no_more_italic = false;
    let mut no_more_strike = false;
    let mut no_more_link = false;

    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i < bytes.len() {
                i += slice[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            continue;
        }
        if bytes[i] == b'`' && !no_more_tick {
            if let Some(rel_end) = memchr::memchr(b'`', &bytes[i + 1..]) {
                if i > plain_start {
                    job.append(&slice[plain_start..i], 0.0, base_fmt.clone());
                }
                let end = i + 1 + rel_end + 1;
                job.append(
                    &slice[i..end],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: theme.accent,
                        background: theme.code_bg,
                        ..Default::default()
                    },
                );
                i = end;
                plain_start = end;
                continue;
            }
            no_more_tick = true;
        } else if i + 1 < bytes.len()
            && bytes[i] == b'*'
            && bytes[i + 1] == b'*'
            && !no_more_bold
        {
            if let Some(rel_end) = slice[i + 2..].find("**") {
                if i > plain_start {
                    job.append(&slice[plain_start..i], 0.0, base_fmt.clone());
                }
                let end = i + 2 + rel_end + 2;
                job.append(
                    &slice[i..end],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: theme.heading_color,
                        ..Default::default()
                    },
                );
                i = end;
                plain_start = end;
                continue;
            }
            no_more_bold = true;
        } else if bytes[i] == b'*' && !no_more_italic {
            if let Some(rel_end) = memchr::memchr(b'*', &bytes[i + 1..]) {
                if rel_end > 0 {
                    if i > plain_start {
                        job.append(&slice[plain_start..i], 0.0, base_fmt.clone());
                    }
                    let end = i + 1 + rel_end + 1;
                    job.append(
                        &slice[i..end],
                        0.0,
                        TextFormat {
                            font_id: font_id.clone(),
                            color: theme.text_primary,
                            italics: true,
                            ..Default::default()
                        },
                    );
                    i = end;
                    plain_start = end;
                    continue;
                }
            } else {
                no_more_italic = true;
            }
        } else if i + 1 < bytes.len()
            && bytes[i] == b'~'
            && bytes[i + 1] == b'~'
            && !no_more_strike
        {
            if let Some(rel_end) = slice[i + 2..].find("~~") {
                if i > plain_start {
                    job.append(&slice[plain_start..i], 0.0, base_fmt.clone());
                }
                let end = i + 2 + rel_end + 2;
                job.append(
                    &slice[i..end],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: theme.text_muted,
                        strikethrough: Stroke::new(1.0_f32, theme.text_muted),
                        ..Default::default()
                    },
                );
                i = end;
                plain_start = end;
                continue;
            }
            no_more_strike = true;
        } else if bytes[i] == b'[' && !no_more_link {
            if let Some(close_br) = slice[i + 1..].find("](") {
                let url_start = i + 1 + close_br + 2;
                if let Some(close_par) = memchr::memchr(b')', &bytes[url_start..]) {
                    if i > plain_start {
                        job.append(&slice[plain_start..i], 0.0, base_fmt.clone());
                    }
                    let end = url_start + close_par + 1;
                    job.append(
                        &slice[i..end],
                        0.0,
                        TextFormat {
                            font_id: font_id.clone(),
                            color: theme.link_color,
                            underline: Stroke::new(1.0_f32, theme.link_color),
                            ..Default::default()
                        },
                    );
                    i = end;
                    plain_start = end;
                    continue;
                }
                no_more_link = true;
            } else {
                no_more_link = true;
            }
        }
        let ch_len = slice[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        i += ch_len;
    }
    if plain_start < slice.len() {
        job.append(&slice[plain_start..], 0.0, base_fmt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_line_layout_caps_at_max_editor_layout_chars_and_hits_cache() {
        let ctx = egui::Context::default();
        let long_line = "x".repeat(20_000);
        let buf = DocumentBuffer::from_str(&long_line);
        let mut state = VirtualEditorState::default();
        let font_id = FontId::new(14.0, egui::FontFamily::Monospace);
        let theme = WordTheme::from_mode(ThemeMode::Light);

        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                state.prepare_line_cache(
                    ui,
                    theme.mode,
                    crate::ui::settings::FontPreference::Monospace,
                    font_id.size,
                );
                let (s1, tr1, _bidi1, g1) =
                    state.get_or_build_line(ui, &buf, 0, &font_id, &theme);
                assert!(tr1);
                assert_eq!(s1.chars().count(), MAX_EDITOR_LAYOUT_CHARS);
                let g1 = g1.expect("non-empty capped line produces galley");
                assert!(state.line_cache.entries[&0].truncated);

                // Second lookup hits the cache without rebuilding the Galley
                let (_, tr2, _, g2) = state.get_or_build_line(ui, &buf, 0, &font_id, &theme);
                assert!(tr2);
                assert!(Arc::ptr_eq(&g1, g2.as_ref().unwrap()));
            });
        });
    }
}
