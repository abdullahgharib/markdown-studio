mod caches;
mod images;

use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::text::LayoutJob;
use egui::{Color32, CornerRadius, FontFamily, FontId, RichText, Sense, Stroke, TextFormat, Vec2};
use rustc_hash::{FxHashMap, FxHasher};

use caches::{
    evict_oldest_half, hash_inline_runs, CachedMathPreview, EvaluatedTableGrid, GalleyLinkSpan,
    NavTarget, ReaderGalleyCache, TableEvalCache,
};
use images::{load_cached_image_state, ImageLoadState};

use crate::document::ast::{is_math_code_info, is_mermaid_code_info};
use crate::document::calc::{
    evaluate_math_block_text, evaluate_table_grid, extract_table_cell_formula, preview_math_result,
};
use crate::document::editing::{
    delete_block_lines, indent_lines, outdent_lines, parse_code_fence_line,
    set_code_block_language, table_at_line, toggle_task_checkbox_at_line, update_image_alt_text,
    word_range_in_line,
};
use crate::document::math::{render_math_in_ui, MATH_TEXTBOOK_TEMPLATES};
use crate::document::mermaid::{render_mermaid_in_ui, MERMAID_TEMPLATES};
use crate::document::search::is_whole_word_match;
use crate::document::unicode::{
    build_visual_bidi_line_map, detect_paragraph_direction, is_rtl_char, parse_line_block_meta,
    shape_bidi_for_display, BlockAlignment, MultilineBidiMap, TextDirection,
};
use crate::document::viewport::ViewportBlockIndex;
use crate::document::DocumentBuffer;
use crate::export::ir::{DocBlock, InlineRun, TableAlign};
use crate::ui::settings::{LineSpacing, PageWidth};
use crate::ui::theme::{ThemeMode, WordTheme};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReaderAction {
    None,
    EditTable(Range<usize>),
    RecalculateTable(Range<usize>),
    JumpToSourceLine(usize),
    JumpToAnchor(String),
    JumpToFootnote(String),
    OpenLink(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    External(String),
    Anchor(String),
    LocalFile {
        path: String,
        fragment: Option<String>,
    },
}

const COMMON_CODE_LANGUAGES: &[(&str, &str)] = &[
    ("", "Plain Text"),
    ("mermaid", "Mermaid Diagram"),
    ("math", "Math (LaTeX)"),
    ("rust", "Rust"),
    ("python", "Python"),
    ("javascript", "JavaScript"),
    ("typescript", "TypeScript"),
    ("bash", "Bash / Shell"),
    ("powershell", "PowerShell"),
    ("c", "C"),
    ("cpp", "C++"),
    ("csharp", "C#"),
    ("go", "Go"),
    ("java", "Java"),
    ("json", "JSON"),
    ("toml", "TOML"),
    ("yaml", "YAML"),
    ("sql", "SQL"),
    ("html", "HTML"),
    ("css", "CSS"),
    ("markdown", "Markdown"),
];

fn code_language_display_label(info: &str) -> String {
    let trimmed = info.trim();
    if trimmed.is_empty() {
        return "Plain Text".to_string();
    }
    let key = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    for &(id, label) in COMMON_CODE_LANGUAGES {
        if !id.is_empty() && id == key {
            return label.to_string();
        }
    }
    trimmed.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveBlockKind {
    Heading(u8),
    Paragraph,
    CodeBlock,
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageLabelEditorState {
    pub source_lines: Range<usize>,
    pub image_url: String,
    pub run_idx: usize,
    pub old_alt: String,
    pub draft: String,
    pub focus_requested: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageLightboxState {
    pub url: String,
    pub alt: String,
    pub link_url: Option<String>,
    pub base_dir: Option<PathBuf>,
    pub doc_path: Option<PathBuf>,
    pub zoom: f32,
}

#[derive(Debug)]
pub struct VirtualReaderState {
    pub active_block_lines: Option<Range<usize>>,
    pub active_block_text: String,
    pub active_block_prefix: String,
    pub active_block_suffix: String,
    active_block_kind: Option<ActiveBlockKind>,
    pub active_code_lang_draft: String,
    active_code_lang_line: Option<usize>,
    pub last_synced_version: u64,
    pub last_synced_show_markup: bool,
    pub scroll_to_y: Option<f32>,
    pub smooth_scroll_target_y: Option<f32>,
    pub highlight_block_lines: Option<(Range<usize>, f64)>,
    pub page_width: PageWidth,
    pub line_spacing: LineSpacing,
    pub focus_inline_editor: bool,
    pub sync_cursor_from_buf: bool,
    pub last_cursor_range: Option<egui::text::CCursorRange>,
    pub search_matches: Arc<Vec<Range<usize>>>,
    pub active_search_match: Option<Range<usize>>,
    pub search_whole_word: bool,
    pub modal_open: bool,
    pub base_dir: Option<PathBuf>,
    copied_code_block_until: Option<(usize, f64)>,
    pub last_scroll_area_id: Option<egui::Id>,
    pub last_pane_rect: egui::Rect,
    pub last_scroll_offset_y: f32,
    pub last_max_scroll_y: f32,
    pub pending_scroll_delta_y: f32,
    pub autoscroll_active: bool,
    pub auto_calc_math: bool,
    pub auto_calc_tables: bool,
    pub image_label_editor: Option<ImageLabelEditorState>,
    pub image_lightbox: Option<ImageLightboxState>,
    galley_cache: ReaderGalleyCache,
    table_eval_epoch: (u64, u64),
    table_eval_cache: TableEvalCache,
    math_preview_tick: u64,
    math_preview_cache: FxHashMap<u64, CachedMathPreview>,
}

impl Default for VirtualReaderState {
    fn default() -> Self {
        Self {
            active_block_lines: None,
            active_block_text: String::new(),
            active_block_prefix: String::new(),
            active_block_suffix: String::new(),
            active_block_kind: None,
            active_code_lang_draft: String::new(),
            active_code_lang_line: None,
            last_synced_version: 0,
            last_synced_show_markup: false,
            scroll_to_y: None,
            smooth_scroll_target_y: None,
            highlight_block_lines: None,
            page_width: PageWidth::Standard,
            line_spacing: LineSpacing::Comfortable,
            focus_inline_editor: false,
            sync_cursor_from_buf: false,
            last_cursor_range: None,
            search_matches: Arc::default(),
            active_search_match: None,
            search_whole_word: false,
            modal_open: false,
            base_dir: None,
            copied_code_block_until: None,
            last_scroll_area_id: None,
            last_pane_rect: egui::Rect::NOTHING,
            last_scroll_offset_y: 0.0,
            last_max_scroll_y: 0.0,
            pending_scroll_delta_y: 0.0,
            autoscroll_active: false,
            auto_calc_math: false,
            auto_calc_tables: true,
            image_label_editor: None,
            image_lightbox: None,
            galley_cache: ReaderGalleyCache::default(),
            table_eval_epoch: (0, 0),
            table_eval_cache: TableEvalCache::default(),
            math_preview_tick: 0,
            math_preview_cache: FxHashMap::default(),
        }
    }
}

impl VirtualReaderState {
    pub fn with_base_dir(base_dir: Option<PathBuf>) -> Self {
        Self {
            base_dir,
            ..Self::default()
        }
    }

    /// Requests fluid eased scrolling toward vertical offset `y` inside the active reader pane.
    pub fn smooth_scroll_to(&mut self, y: f32) {
        self.scroll_to_y = None;
        self.pending_scroll_delta_y = 0.0;
        self.smooth_scroll_target_y = Some(y.max(0.0));
    }

    fn compute_search_sig(&self) -> u64 {
        let mut h = FxHasher::default();
        (Arc::as_ptr(&self.search_matches) as usize).hash(&mut h);
        self.search_matches.len().hash(&mut h);
        self.active_search_match.hash(&mut h);
        self.search_whole_word.hash(&mut h);
        h.finish()
    }

    fn sync_table_eval_cache(&mut self, doc_id: u64, buf_version: u64) {
        let eval_epoch = (doc_id, buf_version);
        if self.table_eval_epoch != eval_epoch {
            self.table_eval_cache.clear();
            self.table_eval_epoch = eval_epoch;
        }
    }

    fn prepare_galley_cache(
        &mut self,
        ui: &egui::Ui,
        doc_id: u64,
        theme_mode: ThemeMode,
        line_spacing: LineSpacing,
        show_inline_markup: bool,
    ) {
        let cur_atlas = ui.fonts(|f| f.texture_atlas());
        let atlas_changed = self.galley_cache.atlas.as_ptr() != Arc::as_ptr(&cur_atlas);
        let ppp = ui.ctx().pixels_per_point();
        let epoch = (
            doc_id,
            theme_mode,
            line_spacing,
            show_inline_markup,
            ppp.to_bits(),
            self.compute_search_sig(),
        );
        if atlas_changed || self.galley_cache.epoch != epoch {
            self.galley_cache.entries.clear();
            self.galley_cache.epoch = epoch;
            self.galley_cache.atlas = Arc::downgrade(&cur_atlas);
        }
    }

    fn cached_table_evaluation(
        &mut self,
        buf: &DocumentBuffer,
        start_line: usize,
    ) -> Option<Arc<EvaluatedTableGrid>> {
        if let Some(cached) = self.table_eval_cache.get(start_line) {
            return cached;
        }
        let computed = table_at_line(buf, start_line).and_then(|tbl| {
            let formula_mask: Vec<Vec<bool>> = tbl
                .rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|c| extract_table_cell_formula(c).0.is_some())
                        .collect()
                })
                .collect();
            if formula_mask.iter().any(|r| r.iter().any(|&f| f)) {
                Some(Arc::new((evaluate_table_grid(&tbl.rows), formula_mask)))
            } else {
                None
            }
        });
        self.table_eval_cache
            .insert_bounded(start_line, computed.clone());
        computed
    }

    fn cached_math_preview(&mut self, expr: &str) -> Option<String> {
        const MAX_PREVIEW_ENTRIES: usize = 128;
        let mut h = FxHasher::default();
        expr.hash(&mut h);
        let key = h.finish();
        self.math_preview_tick = self.math_preview_tick.wrapping_add(1);
        let tick = self.math_preview_tick;
        if let Some(entry) = self.math_preview_cache.get_mut(&key)
            && entry.text == expr {
                entry.last_tick = tick;
                return entry.result.clone();
            }
        let res = preview_math_result(expr);
        if !self.math_preview_cache.contains_key(&key) {
            evict_oldest_half(&mut self.math_preview_cache, MAX_PREVIEW_ENTRIES, |e| {
                e.last_tick
            });
        }
        self.math_preview_cache.insert(
            key,
            CachedMathPreview {
                text: expr.to_string(),
                result: res.clone(),
                last_tick: tick,
            },
        );
        res
    }
}

fn find_ascii_case_insensitive(haystack: &str, needle_lower: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle_lower.as_bytes();
    if n.is_empty() {
        return Some(0);
    }
    if n.len() > h.len() {
        return None;
    }
    h.windows(n.len()).position(|w| {
        w.iter()
            .zip(n.iter())
            .all(|(&a, &b)| a.to_ascii_lowercase() == b)
    })
}

pub use crate::export::ir::heading_slug;

/// Classifies a Markdown link destination into an internal heading anchor, safe web/email URL,
/// or local/relative file path, rejecting dangerous URI schemes (`javascript:`, `data:`, etc.).
pub fn classify_link_url(url: &str) -> Option<LinkTarget> {
    let trimmed = url.trim().trim_start_matches('<').trim_end_matches('>').trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(slug) = trimmed.strip_prefix('#') {
        let decoded = slug.replace("%20", " ");
        if !decoded.trim().is_empty() {
            return Some(LinkTarget::Anchor(decoded));
        }
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
    {
        return Some(LinkTarget::External(trimmed.to_string()));
    }
    if lower.starts_with("www.") {
        return Some(LinkTarget::External(format!("https://{trimmed}")));
    }

    if lower.starts_with("file://") {
        let mut rest = &trimmed["file://".len()..];
        if rest.starts_with('/') && rest.as_bytes().get(2) == Some(&b':') {
            // file:///C:/path -> C:/path on Windows
            rest = &rest[1..];
        } else if let Some(after_local) = rest.strip_prefix("localhost/") {
            rest = after_local;
        }
        let (path_part, frag_part) = match rest.split_once('#') {
            Some((p, f)) if !f.trim().is_empty() => (p, Some(f.replace("%20", " "))),
            Some((p, _)) => (p, None),
            None => (rest, None),
        };
        let path_decoded = path_part.replace("%20", " ");
        if path_decoded.trim().is_empty() {
            return None;
        }
        return Some(LinkTarget::LocalFile {
            path: path_decoded,
            fragment: frag_part,
        });
    }

    // Reject explicit URI schemes (e.g. javascript:, data:, vbscript:, ms-msdt:) while allowing
    // single-letter Windows drive paths (e.g. C:\foo or D:/bar).
    if let Some(colon) = trimmed.find(':') {
        let prefix = &trimmed[..colon];
        if !prefix.contains('/') && !prefix.contains('\\') && !prefix.contains('?') {
            if prefix.len() >= 2
                && prefix.as_bytes()[0].is_ascii_alphabetic()
                && prefix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
            {
                return None;
            }
            if prefix.is_empty() {
                return None;
            }
        }
    }

    let (path_part, frag_part) = match trimmed.split_once('#') {
        Some((p, f)) if !f.trim().is_empty() => (p, Some(f.replace("%20", " "))),
        Some((p, _)) => (p, None),
        None => (trimmed, None),
    };
    let path_decoded = path_part.trim().replace("%20", " ");
    if path_decoded.is_empty() {
        return None;
    }
    Some(LinkTarget::LocalFile {
        path: path_decoded,
        fragment: frag_part,
    })
}

fn clean_bracket_link_destination(raw_inner: &str) -> String {
    let trimmed = raw_inner.trim();
    if let Some(after_lt) = trimmed.strip_prefix('<')
        && let Some(gt) = after_lt.find('>') {
            return after_lt[..gt].trim().to_string();
        }
    trimmed
        .split_ascii_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

/// Extracts the raw URL/target of any inline Markdown link `[label](url)`, `<url>` autolink,
/// or bare `http(s)://`/`www.` URL covering logical character offset `char_idx` inside `text`.
pub fn extract_markdown_link_at_char(text: &str, char_idx: usize) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let chars_total = text.chars().count();
    let c = char_idx.min(chars_total);
    let mut cur_c = 0usize;
    let mut target_line = "";
    let mut target_col = 0usize;

    for seg in text.split_inclusive('\n') {
        let clean = seg.trim_end_matches(['\r', '\n']);
        let seg_chars = seg.chars().count();
        let clean_chars = clean.chars().count();
        if c <= cur_c + seg_chars {
            target_line = clean;
            target_col = c.saturating_sub(cur_c).min(clean_chars);
            break;
        }
        cur_c += seg_chars;
    }

    let bytes = target_line.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;

    while i < len {
        if bytes[i] == b'\\' {
            i += 1;
            if i < len {
                i += target_line[i..]
                    .chars()
                    .next()
                    .map(|ch| ch.len_utf8())
                    .unwrap_or(1);
            }
            continue;
        }

        // Skip images `![alt](src)` so they are not treated as clickable text hyperlinks.
        if bytes[i] == b'!' && target_line[i..].starts_with("![")
            && let Some((_lbl_end, url_close)) = find_bracket_link_end(&target_line[i + 2..]) {
                i = i + 2 + url_close + 1;
                continue;
            }

        // Inline Markdown link `[label](url)`
        if bytes[i] == b'['
            && let Some((lbl_end_rel, url_close_rel)) = find_bracket_link_end(&target_line[i + 1..])
            {
                let span_end_b = i + 1 + url_close_rel + 1;
                let char_s = target_line[..i].chars().count();
                let char_e = target_line[..span_end_b].chars().count();
                if target_col >= char_s && target_col < char_e {
                    let inner = &target_line[(i + 1 + lbl_end_rel + 2)..(i + 1 + url_close_rel)];
                    let dest = clean_bracket_link_destination(inner);
                    if !dest.is_empty() {
                        return Some(dest);
                    }
                }
                i = span_end_b;
                continue;
            }

        // Autolink `<https://...>`, `<http://...>`, `<mailto:...>`
        if bytes[i] == b'<'
            && let Some(rel_gt) = target_line[i + 1..].find('>') {
                let inner = &target_line[i + 1..i + 1 + rel_gt];
                let inner_low = inner.to_ascii_lowercase();
                if !inner.chars().any(char::is_whitespace)
                    && (inner_low.starts_with("https://")
                        || inner_low.starts_with("http://")
                        || inner_low.starts_with("mailto:"))
                {
                    let span_end_b = i + 1 + rel_gt + 1;
                    let char_s = target_line[..i].chars().count();
                    let char_e = target_line[..span_end_b].chars().count();
                    if target_col >= char_s && target_col < char_e {
                        return Some(inner.to_string());
                    }
                    i = span_end_b;
                    continue;
                }
            }

        // Bare URL (`https://...`, `http://...`, `www....`)
        let rest_low_match = target_line
            .get(i..i + 8)
            .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
            || target_line
                .get(i..i + 7)
                .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
            || (target_line
                .get(i..i + 4)
                .is_some_and(|s| s.eq_ignore_ascii_case("www."))
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()));
        if rest_low_match {
            let mut j = i;
            while j < len
                && !bytes[j].is_ascii_whitespace()
                && !matches!(bytes[j], b'<' | b'>' | b'"' | b'\'')
            {
                j += 1;
            }
            while j > i {
                let last = bytes[j - 1];
                if matches!(last, b'.' | b',' | b';' | b':' | b'!' | b'?' | b']') {
                    j -= 1;
                } else if last == b')' {
                    let candidate = &target_line[i..j];
                    let opens = candidate.bytes().filter(|&b| b == b'(').count();
                    let closes = candidate.bytes().filter(|&b| b == b')').count();
                    if closes > opens {
                        j -= 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            if j > i + 4 {
                let char_s = target_line[..i].chars().count();
                let char_e = target_line[..j].chars().count();
                if target_col >= char_s && target_col < char_e {
                    return Some(target_line[i..j].to_string());
                }
                i = j;
                continue;
            }
        }

        i += target_line[i..]
            .chars()
            .next()
            .map(|ch| ch.len_utf8())
            .unwrap_or(1);
    }

    None
}

fn clean_footnote_name(raw: &str) -> String {
    let t = raw.trim();
    let t = t
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(t);
    t.strip_prefix('^').unwrap_or(t).trim().to_string()
}

fn nav_target_tooltip(target: &NavTarget, read_only: bool) -> String {
    let prefix = if read_only { "Click" } else { "Ctrl+Click" };
    match target {
        NavTarget::Link(raw, LinkTarget::External(_)) => {
            format!("{prefix} to open: {raw}")
        }
        NavTarget::Link(_, LinkTarget::Anchor(slug)) => {
            if read_only {
                format!("Jump to section: #{slug}")
            } else {
                format!("Ctrl+Click to jump to section: #{slug}")
            }
        }
        NavTarget::Link(raw, LinkTarget::LocalFile { .. }) => {
            format!("{prefix} to open file: {raw}")
        }
        NavTarget::Footnote(name) => {
            if read_only {
                format!("Jump to footnote: [^{name}]")
            } else {
                format!("Ctrl+Click to jump to footnote: [^{name}]")
            }
        }
    }
}

pub(crate) fn show_link_hover_tooltip(ui: &egui::Ui, tip_id: egui::Id, text: &str) {
    if let Some(pos) = ui.ctx().input(|i| i.pointer.hover_pos()) {
        let style = ui.style();
        let visuals = &style.visuals;
        let font_id = egui::TextStyle::Body.resolve(style);
        let text_color = visuals.text_color();
        let galley = ui.fonts(|f| {
            f.layout(
                text.to_string(),
                font_id,
                text_color,
                340.0,
            )
        });
        let pad = Vec2::new(8.0, 5.0);
        let size = galley.size() + pad * 2.0;
        let screen = ui.ctx().screen_rect();
        let raw_min = pos + Vec2::new(12.0, 18.0);
        let clamped_x = raw_min
            .x
            .min((screen.right() - size.x - 4.0).max(screen.left() + 4.0))
            .max(screen.left() + 4.0);
        let mut clamped_y = raw_min.y;
        if clamped_y + size.y > screen.bottom() - 4.0 {
            clamped_y = (pos.y - size.y - 8.0).max(screen.top() + 4.0);
        }
        let popup_rect = egui::Rect::from_min_size(egui::pos2(clamped_x, clamped_y), size);
        let painter = ui
            .ctx()
            .layer_painter(egui::LayerId::new(egui::Order::Tooltip, tip_id));
        painter.add(
            visuals
                .popup_shadow
                .as_shape(popup_rect, visuals.window_corner_radius),
        );
        painter.rect(
            popup_rect,
            visuals.window_corner_radius,
            visuals.window_fill(),
            visuals.window_stroke(),
            egui::StrokeKind::Inside,
        );
        painter.galley(popup_rect.min + pad, galley, text_color);
    }
}

fn nav_target_action(target: &NavTarget) -> ReaderAction {
    match target {
        NavTarget::Link(_, LinkTarget::Anchor(slug)) => ReaderAction::JumpToAnchor(slug.clone()),
        NavTarget::Link(raw, _) => ReaderAction::OpenLink(raw.clone()),
        NavTarget::Footnote(name) => ReaderAction::JumpToFootnote(name.clone()),
    }
}

fn heading_font_size(level: u8) -> f32 {
    match level {
        1 => 30.0,
        2 => 23.0,
        3 => 18.5,
        4 => 15.8,
        5 => 14.8,
        _ => 13.8,
    }
}

fn resolve_egui_align(alignment: BlockAlignment, direction: TextDirection) -> egui::Align {
    match alignment {
        BlockAlignment::Left => egui::Align::Min,
        BlockAlignment::Right => egui::Align::Max,
        BlockAlignment::Center => egui::Align::Center,
        BlockAlignment::Justify | BlockAlignment::Start => match direction {
            TextDirection::Rtl => egui::Align::Max,
            TextDirection::Ltr => egui::Align::Min,
        },
        BlockAlignment::End => match direction {
            TextDirection::Rtl => egui::Align::Min,
            TextDirection::Ltr => egui::Align::Max,
        },
    }
}

fn split_wysiwyg_block_text(
    raw: &str,
    kind: ActiveBlockKind,
    show_markup: bool,
) -> (String, String, String) {
    let normalize_lf = |s: &str| -> String {
        if s.contains('\r') {
            s.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            s.to_string()
        }
    };

    if show_markup {
        return (String::new(), normalize_lf(raw), String::new());
    }

    if matches!(kind, ActiveBlockKind::CodeBlock) {
        if let Some(first_nl) = raw.find('\n') {
            let first_line = raw[..first_nl].trim_end_matches('\r').trim_start();
            let is_dollar_fence = first_line == "$$";
            if first_line.starts_with("```") || first_line.starts_with("~~~") || is_dollar_fence {
                let prefix = raw[..=first_nl].to_string();
                let eol = if prefix.ends_with("\r\n") { "\r\n" } else { "\n" };
                let rest = &raw[first_nl + 1..];
                if let Some(last_nl) = rest.rfind('\n') {
                    let last_line = rest[last_nl + 1..].trim();
                    let is_close = if is_dollar_fence {
                        last_line == "$$"
                    } else {
                        last_line.starts_with("```") || last_line.starts_with("~~~")
                    };
                    if is_close {
                        let eol_start = if last_nl > 0 && rest.as_bytes()[last_nl - 1] == b'\r' {
                            last_nl - 1
                        } else {
                            last_nl
                        };
                        return (
                            prefix,
                            normalize_lf(&rest[..eol_start]),
                            rest[eol_start..].to_string(),
                        );
                    }
                } else {
                    let t_rest = rest.trim();
                    let is_close = if is_dollar_fence {
                        t_rest == "$$"
                    } else {
                        t_rest.starts_with("```") || t_rest.starts_with("~~~")
                    };
                    if is_close {
                        return (prefix, String::new(), format!("{eol}{rest}"));
                    }
                }
                return (prefix, normalize_lf(rest), String::new());
            }
        }
        return (String::new(), normalize_lf(raw), String::new());
    }

    let (_, _, without_meta) = parse_line_block_meta(raw);
    let meta_len = raw.len() - without_meta.len();

    match kind {
        ActiveBlockKind::Heading(_) => {
            let trimmed_start = without_meta.trim_start_matches(' ');
            let lead_spaces = without_meta.len() - trimmed_start.len();
            let hash_count = trimmed_start.bytes().take_while(|&b| b == b'#').count();
            if lead_spaces <= 3 && (1..=6).contains(&hash_count) {
                let after_hashes = &trimmed_start[hash_count..];
                if after_hashes.starts_with(' ') || after_hashes.starts_with('\t') {
                    let split_at = meta_len + lead_spaces + hash_count + 1;
                    return (
                        raw[..split_at].to_string(),
                        normalize_lf(&raw[split_at..]),
                        String::new(),
                    );
                }
            }
            (
                raw[..meta_len].to_string(),
                normalize_lf(without_meta),
                String::new(),
            )
        }
        ActiveBlockKind::Paragraph => {
            let bytes = without_meta.as_bytes();
            let mut idx = 0usize;
            while idx < bytes.len() && bytes[idx] == b' ' {
                idx += 1;
            }
            if without_meta[idx..].starts_with("> ") {
                let split_at = meta_len + idx + 2;
                return (
                    raw[..split_at].to_string(),
                    normalize_lf(&raw[split_at..]),
                    String::new(),
                );
            }
            if idx + 1 < bytes.len()
                && matches!(bytes[idx], b'-' | b'*' | b'+')
                && bytes[idx + 1] == b' '
            {
                idx += 2;
                if without_meta[idx..].starts_with("[ ] ")
                    || without_meta[idx..].starts_with("[x] ")
                    || without_meta[idx..].starts_with("[X] ")
                {
                    idx += 4;
                }
                let split_at = meta_len + idx;
                return (
                    raw[..split_at].to_string(),
                    normalize_lf(&raw[split_at..]),
                    String::new(),
                );
            }
            let digits_start = idx;
            while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                idx += 1;
            }
            if idx > digits_start
                && idx + 1 < bytes.len()
                && matches!(bytes[idx], b'.' | b')')
                && bytes[idx + 1] == b' '
            {
                let split_at = meta_len + idx + 2;
                return (
                    raw[..split_at].to_string(),
                    normalize_lf(&raw[split_at..]),
                    String::new(),
                );
            }
            (
                raw[..meta_len].to_string(),
                normalize_lf(without_meta),
                String::new(),
            )
        }
        ActiveBlockKind::CodeBlock => (String::new(), normalize_lf(raw), String::new()),
    }
}

fn next_list_continuation_marker(prefix: &str) -> Option<String> {
    let (_, _, after_meta) = parse_line_block_meta(prefix);
    let ws: String = after_meta.chars().take_while(|&c| c == ' ').collect();
    let body = &after_meta[ws.len()..];
    if (body.starts_with("- [") || body.starts_with("* [") || body.starts_with("+ ["))
        && body.ends_with("] ")
    {
        let bullet = &body[..1];
        return Some(format!("{ws}{bullet} [ ] "));
    }
    if body.starts_with("- ") || body.starts_with("* ") || body.starts_with("+ ") {
        return Some(after_meta.to_string());
    }
    let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let rest = &body[digits.len()..];
        if let Some(delim) = rest.chars().next()
            && (delim == '.' || delim == ')') && rest.get(1..2) == Some(" ")
                && let Ok(n) = digits.parse::<usize>() {
                    return Some(format!("{ws}{}{delim} ", n + 1));
                }
    }
    None
}

fn infer_heading_level_from_raw(raw: &str) -> Option<u8> {
    let (_, _, without_meta) = parse_line_block_meta(raw);
    let trimmed = without_meta.trim_start_matches(' ');
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && trimmed.as_bytes().get(hashes) == Some(&b' ') {
        Some(hashes as u8)
    } else {
        None
    }
}

fn infer_code_block_info_at_line(line: &str) -> String {
    if let Some((_, _, _, info)) = parse_code_fence_line(line) {
        return info;
    }
    let t = line.trim();
    if (t.starts_with("$$") && t.ends_with("$$") && t.len() > 4)
        || (t.starts_with("\\[") && t.ends_with("\\]") && t.len() > 4)
        || t.starts_with("$$")
        || t.starts_with("\\[")
    {
        return "math".to_string();
    }
    String::new()
}

fn extract_math_preview_source(raw: &str) -> &str {
    let t = raw.trim();
    if let Some(inner) = t.strip_prefix("$$").and_then(|s| s.strip_suffix("$$")) {
        return inner.trim();
    }
    if let Some(inner) = t.strip_prefix("\\[").and_then(|s| s.strip_suffix("\\]")) {
        return inner.trim();
    }
    t
}

fn replace_fenced_block_body(
    buf: &mut DocumentBuffer,
    src_lines: &Range<usize>,
    new_body: &str,
) -> Option<Range<usize>> {
    if src_lines.is_empty() || src_lines.end > buf.len_lines() {
        return None;
    }
    let le = buf.line_ending().to_string();
    let open_line = buf.line_text(src_lines.start);
    let close_line = if src_lines.len() >= 2 {
        buf.line_text(src_lines.end - 1)
    } else {
        "```".to_string()
    };
    let body_norm = new_body.replace("\r\n", "\n").replace('\n', &le);
    let replacement = format!("{open_line}{le}{body_norm}{le}{close_line}");
    let range = buf.line_range_chars(src_lines.start, src_lines.end - 1);
    let new_line_count = replacement.split('\n').count().max(1);
    let start_char = range.start;
    buf.replace_range(range, &replacement);
    buf.set_cursor(
        start_char + open_line.chars().count() + le.chars().count(),
        false,
    );
    Some(src_lines.start..(src_lines.start + new_line_count))
}

fn delete_block_small_button(ui: &mut egui::Ui, theme: &WordTheme, tooltip: &str) -> egui::Response {
    let del_color = if theme.dark {
        Color32::from_rgb(248, 113, 113)
    } else {
        Color32::from_rgb(200, 38, 38)
    };
    let btn = egui::Button::new(RichText::new("Delete").size(11.5).color(del_color)).small();
    let resp = ui.add(btn).on_hover_text(tooltip);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

#[allow(clippy::too_many_arguments)]
fn render_fenced_template_picker(
    ui: &mut egui::Ui,
    button_label: &str,
    popup_id_tag: (&str, usize),
    tooltip: &str,
    min_width: f32,
    templates: &[(&str, &str)],
    on_pick: impl FnOnce(&mut egui::Ui, &str),
) -> bool {
    let popup_id = ui.make_persistent_id(popup_id_tag);
    let btn = ui.small_button(button_label).on_hover_text(tooltip);
    let mut interacted = btn.clicked();
    if btn.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }
    let mut picked: Option<&str> = None;
    egui::popup_below_widget(
        ui,
        popup_id,
        &btn,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(min_width);
            for &(name, body) in templates {
                if ui.button(name).clicked() {
                    picked = Some(body);
                    ui.memory_mut(|m| m.close_popup());
                }
            }
        },
    );
    if let Some(body) = picked {
        on_pick(ui, body);
        interacted = true;
    }
    interacted
}

fn render_calc_preview_pill(ui: &mut egui::Ui, theme: &WordTheme, res_str: &str) {
    egui::Frame::new()
        .fill(theme.accent_soft)
        .stroke(Stroke::new(1.0_f32, theme.accent))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!("= {res_str}"))
                    .size(12.5)
                    .strong()
                    .color(theme.accent),
            );
        });
}

fn sync_active_block_from_buf(
    buf: &DocumentBuffer,
    show_markup: bool,
    state: &mut VirtualReaderState,
) {
    if let Some(ref lines) = state.active_block_lines {
        if !lines.is_empty() && lines.end <= buf.len_lines() {
            let range = buf.line_range_chars(lines.start, lines.end - 1);
            let raw = buf.rope().slice(range).to_string();
            let mut kind = state.active_block_kind.unwrap_or(ActiveBlockKind::Paragraph);
            if matches!(kind, ActiveBlockKind::Paragraph | ActiveBlockKind::Heading(_)) {
                if let Some(lvl) = infer_heading_level_from_raw(&raw) {
                    kind = ActiveBlockKind::Heading(lvl);
                } else if matches!(kind, ActiveBlockKind::Heading(_)) {
                    kind = ActiveBlockKind::Paragraph;
                }
            }
            state.active_block_kind = Some(kind);
            if matches!(kind, ActiveBlockKind::CodeBlock) {
                let info = infer_code_block_info_at_line(&buf.line_text(lines.start));
                state.active_code_lang_draft = info;
                state.active_code_lang_line = Some(lines.start);
            } else {
                state.active_code_lang_draft.clear();
                state.active_code_lang_line = None;
            }
            let (prefix, text, suffix) = split_wysiwyg_block_text(&raw, kind, show_markup);
            state.active_block_prefix = prefix;
            state.active_block_text = text;
            state.active_block_suffix = suffix;
            state.sync_cursor_from_buf = true;
        } else {
            state.active_block_lines = None;
            state.active_block_prefix.clear();
            state.active_block_suffix.clear();
            state.active_block_kind = None;
            state.active_code_lang_draft.clear();
            state.active_code_lang_line = None;
        }
    }
    state.last_synced_version = buf.version;
    state.last_synced_show_markup = show_markup;
}

/// Renders the virtualized rich Markdown Reader / WYSIWYG document page in O(log B) + O(visible).
#[allow(clippy::too_many_arguments)]
pub fn render_virtual_reader(
    ui: &mut egui::Ui,
    buf: &mut DocumentBuffer,
    index: &mut ViewportBlockIndex,
    theme: &WordTheme,
    wysiwyg: bool,
    show_markup: bool,
    read_only: bool,
    state: &mut VirtualReaderState,
) -> ReaderAction {
    let mut action = ReaderAction::None;

    if read_only || !wysiwyg {
        state.active_block_lines = None;
    }
    if !read_only {
        state.image_lightbox = None;
    }

    let initial_active_lines = state.active_block_lines.clone();

    // Keep inline WYSIWYG buffer synchronized if external edit (toolbar/undo) or Show Markup changed.
    if wysiwyg
        && !read_only
        && (state.last_synced_version != buf.version
            || state.last_synced_show_markup != show_markup)
    {
        sync_active_block_from_buf(buf, show_markup, state);
    } else {
        state.last_synced_version = buf.version;
        state.last_synced_show_markup = show_markup;
    }

    let workspace_top = ui.max_rect().top();
    let line_spacing = state.line_spacing;
    let block_gap = line_spacing.block_gap();
    state.sync_table_eval_cache(index.doc_id, buf.version);
    state.prepare_galley_cache(
        ui,
        index.doc_id,
        theme.mode,
        line_spacing,
        wysiwyg && show_markup,
    );

    let now_frame_time = ui.input(|i| i.time);
    if state
        .highlight_block_lines
        .as_ref()
        .is_some_and(|(_, until)| now_frame_time >= *until)
    {
        state.highlight_block_lines = None;
    }

    // Cancel active eased animation when manual wheel, middle-click autoscroll, or instant jump occurs.
    let dt = ui.input(|i| i.stable_dt).clamp(0.001, 0.05);
    let user_wheel_y = ui.input(|i| i.raw_scroll_delta.y.abs());
    if user_wheel_y > 0.1 || state.autoscroll_active || state.scroll_to_y.is_some() {
        state.smooth_scroll_target_y = None;
    }

    // Keyboard page/line smooth navigation in Reader mode (disabled while modals, lightbox, or text inputs hold focus).
    if read_only
        && !state.modal_open
        && state.image_lightbox.is_none()
        && !state.autoscroll_active
        && !ui.ctx().wants_keyboard_input()
    {
        let page_step = (ui.available_height() * 0.85).max(160.0);
        let line_step = 96.0_f32;
        let max_y = state.last_max_scroll_y;
        let base_target = state
            .smooth_scroll_target_y
            .unwrap_or(state.last_scroll_offset_y);
        let mut next_target: Option<f32> = None;

        ui.ctx().input_mut(|i| {
            let no_mod = egui::Modifiers::NONE;
            if i.consume_key(no_mod, egui::Key::PageDown) || i.consume_key(no_mod, egui::Key::Space)
            {
                next_target = Some((base_target + page_step).clamp(0.0, max_y));
            } else if i.consume_key(no_mod, egui::Key::PageUp)
                || i.consume_key(egui::Modifiers::SHIFT, egui::Key::Space)
            {
                next_target = Some((base_target - page_step).clamp(0.0, max_y));
            } else if i.consume_key(no_mod, egui::Key::ArrowDown)
                || i.consume_key(no_mod, egui::Key::J)
            {
                next_target = Some((base_target + line_step).clamp(0.0, max_y));
            } else if i.consume_key(no_mod, egui::Key::ArrowUp)
                || i.consume_key(no_mod, egui::Key::K)
            {
                next_target = Some((base_target - line_step).clamp(0.0, max_y));
            } else if i.consume_key(no_mod, egui::Key::Home) {
                next_target = Some(0.0);
            } else if i.consume_key(no_mod, egui::Key::End) {
                next_target = Some(max_y);
            }
        });
        if let Some(target) = next_target {
            state.smooth_scroll_target_y = Some(target);
        }
    }

    let mut applied_smooth_y: Option<f32> = None;
    if let Some(target_y) = state.smooth_scroll_target_y {
        let clamped = if state.last_max_scroll_y > 0.0 {
            target_y.clamp(0.0, state.last_max_scroll_y)
        } else {
            target_y.max(0.0)
        };
        let diff = clamped - state.last_scroll_offset_y;
        if diff.abs() <= 0.75 {
            applied_smooth_y = Some(clamped);
            state.smooth_scroll_target_y = None;
        } else {
            let alpha = 1.0 - (-18.0 * dt).exp();
            applied_smooth_y = Some((state.last_scroll_offset_y + diff * alpha).max(0.0));
            ui.ctx().request_repaint();
        }
    }

    let mut scroll_out_id: Option<egui::Id> = None;
    let mut scroll_inner_rect = egui::Rect::NOTHING;
    let mut scroll_offset_y = state.last_scroll_offset_y;
    let mut scroll_max_y = state.last_max_scroll_y;

    egui::Frame::new()
        .fill(theme.desk_bg)
        .show(ui, |ui| {
            let mut scroll_area = egui::ScrollArea::vertical()
                .id_salt(if wysiwyg {
                    "wysiwyg_reader_scroll"
                } else if read_only {
                    "readonly_reader_scroll"
                } else {
                    "split_reader_scroll"
                })
                .auto_shrink([false, false]);

            if let Some(target_y) = state.scroll_to_y.take() {
                scroll_area = scroll_area.vertical_scroll_offset(target_y.max(0.0));
                state.pending_scroll_delta_y = 0.0;
                state.smooth_scroll_target_y = None;
            } else if state.pending_scroll_delta_y.abs() > 0.001 {
                let delta = state.pending_scroll_delta_y;
                state.pending_scroll_delta_y = 0.0;
                let next_y =
                    (state.last_scroll_offset_y + delta).clamp(0.0, state.last_max_scroll_y);
                scroll_area = scroll_area.vertical_scroll_offset(next_y);
            } else if let Some(smooth_y) = applied_smooth_y {
                scroll_area = scroll_area.vertical_scroll_offset(smooth_y.max(0.0));
            } else {
                state.pending_scroll_delta_y = 0.0;
            }

            let out = scroll_area.show_viewport(ui, |ui, viewport| {
                let inner_avail_w = ui.available_width().max(260.0);
                let paper_max = if read_only {
                    state.page_width.max_px()
                } else {
                    820.0
                };
                let paper_w = (inner_avail_w - 28.0).clamp(240.0, paper_max);
                let h_margin: i8 = if paper_w < 500.0 { 24 } else { 48 };
                let side_pad = ((inner_avail_w - paper_w) * 0.5).max(8.0);
                let inner_content_w = (paper_w - (h_margin as f32 * 2.0)).max(180.0);

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.add_space(side_pad);

                    egui::Frame::new()
                        .fill(theme.paper_bg)
                        .stroke(Stroke::new(1.0_f32, theme.border_subtle))
                        .shadow(theme.paper_shadow())
                        .inner_margin(egui::Margin::symmetric(h_margin, 32))
                        .corner_radius(8.0)
                        .show(ui, |ui| {
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                ui.set_width(inner_content_w);
                                ui.set_max_width(inner_content_w);
                                ui.set_min_height((viewport.height() - 56.0).max(320.0));
                                let content_w = inner_content_w;

                                if index.blocks.is_empty() {
                                    if wysiwyg && !read_only {
                                        if state.active_block_lines.is_none() {
                                            state.active_block_lines = Some(0..1);
                                            state.active_block_kind =
                                                Some(ActiveBlockKind::Paragraph);
                                            state.active_block_text.clear();
                                            state.active_block_prefix.clear();
                                            state.active_block_suffix.clear();
                                            state.last_synced_version = buf.version;
                                            state.focus_inline_editor = true;
                                        }
                                        let active_lines =
                                            state.active_block_lines.clone().unwrap_or(0..1);
                                        let kind = state
                                            .active_block_kind
                                            .unwrap_or(ActiveBlockKind::Paragraph);
                                        render_inline_wysiwyg_block(
                                            ui,
                                            buf,
                                            &active_lines,
                                            kind,
                                            content_w,
                                            theme,
                                            show_markup,
                                            workspace_top,
                                            state,
                                        );
                                    } else {
                                        ui.add_space(40.0);
                                        ui.label(
                                            RichText::new(if read_only {
                                                "Empty document."
                                            } else {
                                                "Start typing in the editor or use the formatting toolbar."
                                            })
                                            .color(theme.text_muted)
                                            .italics(),
                                        );
                                        ui.add_space(200.0);
                                    }
                                    return;
                                }

                                let use_virtual_window = index.blocks.len() > 48;
                                let vis = if use_virtual_window {
                                    let local_top = (viewport.top() - 48.0).max(0.0);
                                    index.visible_range(local_top, viewport.height(), 240.0)
                                } else {
                                    0..index.blocks.len()
                                };

                                if use_virtual_window && vis.start > 0 {
                                    let top_h = (index.block_y_start(vis.start) - 16.0).max(0.0);
                                    if top_h > 0.0 {
                                        ui.add_space(top_h);
                                    }
                                }

                                let paper_content_top = ui.cursor().top();
                                let mut height_updates: Vec<(usize, f32)> = Vec::new();
                                let mut non_virt_updates: Vec<(usize, f32, f32)> = Vec::new();
                                let mut rendered_active = false;
                                let active_has_leaf = if wysiwyg && !read_only {
                                    state
                                        .active_block_lines
                                        .as_ref()
                                        .is_some_and(|r| index_has_leaf_block_for_lines(index, r))
                                } else {
                                    false
                                };

                                for idx in vis.clone() {
                                    let block = &index.blocks[idx].block;
                                    let src_lines = block.source_lines();

                                    if wysiwyg
                                        && !read_only
                                        && !rendered_active
                                        && !active_has_leaf
                                        && state.active_block_lines == initial_active_lines
                                        && let Some(active_lines) = state.active_block_lines.clone()
                                            && !active_lines.is_empty()
                                                && active_lines.end <= buf.len_lines()
                                                && active_lines.start < src_lines.start
                                            {
                                                let kind = state
                                                    .active_block_kind
                                                    .unwrap_or(ActiveBlockKind::Paragraph);
                                                let a = render_inline_wysiwyg_block(
                                                    ui,
                                                    buf,
                                                    &active_lines,
                                                    kind,
                                                    content_w,
                                                    theme,
                                                    show_markup,
                                                    workspace_top,
                                                    state,
                                                );
                                                if a != ReaderAction::None {
                                                    action = a;
                                                }
                                                rendered_active = true;
                                                ui.add_space(block_gap);
                                            }

                                    let heading_top_pad = if idx > 0 {
                                        match block {
                                            DocBlock::Heading { level: 1, .. } => 14.0,
                                            DocBlock::Heading { level: 2, .. } => 10.0,
                                            DocBlock::Heading { level: 3, .. } => 6.0,
                                            _ => 0.0,
                                        }
                                    } else {
                                        0.0
                                    };
                                    let block_y = ui.cursor().top() - paper_content_top;
                                    if heading_top_pad > 0.0 {
                                        ui.add_space(heading_top_pad);
                                    }

                                    let block_resp = ui.scope(|ui| {
                                        let a = render_doc_block(
                                            ui,
                                            buf,
                                            block,
                                            content_w,
                                            theme,
                                            wysiwyg,
                                            show_markup,
                                            read_only,
                                            workspace_top,
                                            state,
                                            0,
                                            &mut rendered_active,
                                        );
                                        if a != ReaderAction::None {
                                            action = a;
                                        }
                                    });

                                    let rect = block_resp.response.rect;
                                    if let Some((ref hl_range, until)) = state.highlight_block_lines
                                        && now_frame_time < until && *hl_range == src_lines {
                                            let frac =
                                                ((until - now_frame_time) / 1.4).clamp(0.0, 1.0)
                                                    as f32;
                                            let alpha = (frac * 44.0).round() as u8;
                                            let halo_rect = rect.expand2(Vec2::new(8.0, 4.0));
                                            let halo_col = Color32::from_rgba_unmultiplied(
                                                theme.accent.r(),
                                                theme.accent.g(),
                                                theme.accent.b(),
                                                alpha,
                                            );
                                            ui.painter().rect_filled(halo_rect, 6.0, halo_col);
                                            ui.ctx().request_repaint();
                                        }
                                    if !state.search_matches.is_empty()
                                        && !src_lines.is_empty()
                                        && src_lines.end <= buf.len_lines()
                                    {
                                        let blk_range = buf.line_range_chars(
                                            src_lines.start,
                                            src_lines.end.saturating_sub(1).max(src_lines.start),
                                        );
                                        let first_hit = state
                                            .search_matches
                                            .partition_point(|m| m.end <= blk_range.start);
                                        let has_search_hit = state
                                            .search_matches
                                            .get(first_hit)
                                            .is_some_and(|m| m.start <= blk_range.end);
                                        if has_search_hit {
                                            let is_active_hit =
                                                state.active_search_match.as_ref().is_some_and(
                                                    |m| {
                                                        m.end > blk_range.start
                                                            && m.start <= blk_range.end
                                                    },
                                                );
                                            let hi_rect = rect.expand2(Vec2::new(6.0, 2.0));
                                            let accent = if is_active_hit {
                                                Color32::from_rgba_unmultiplied(255, 150, 20, 42)
                                            } else {
                                                Color32::from_rgba_unmultiplied(245, 185, 35, 24)
                                            };
                                            ui.painter().rect_filled(hi_rect, 4.0, accent);
                                            let bar_color = if is_active_hit {
                                                theme.warning_amber
                                            } else {
                                                Color32::from_rgb(230, 175, 45)
                                            };
                                            ui.painter().rect_filled(
                                                egui::Rect::from_min_max(
                                                    egui::pos2(hi_rect.left(), hi_rect.top()),
                                                    egui::pos2(
                                                        hi_rect.left() + 3.0,
                                                        hi_rect.bottom(),
                                                    ),
                                                ),
                                                1.5,
                                                bar_color,
                                            );
                                        }
                                    }
                                    let measured = rect.height() + block_gap + heading_top_pad;
                                    if use_virtual_window {
                                        if (measured - index.blocks[idx].height).abs() > 6.0 {
                                            height_updates.push((idx, measured));
                                        }
                                    } else {
                                        non_virt_updates.push((idx, block_y + 16.0, measured));
                                    }

                                    // Split preview mode click-to-jump-to-source-line on non-text blocks (Heading/Paragraph/CodeBlock handle their own clicks)
                                    if !read_only
                                        && !wysiwyg
                                        && !state.autoscroll_active
                                        && matches!(block, DocBlock::ThematicBreak { .. })
                                    {
                                        let click_resp = ui.interact(
                                            rect,
                                            ui.id().with(("blk_click", idx)),
                                            Sense::click(),
                                        );
                                        if click_resp.clicked() && !src_lines.is_empty() {
                                            action = ReaderAction::JumpToSourceLine(src_lines.start);
                                        }
                                    }

                                    ui.add_space(block_gap);
                                }

                                if wysiwyg
                                    && !read_only
                                    && !rendered_active
                                    && !active_has_leaf
                                    && state.active_block_lines == initial_active_lines
                                    && let Some(active_lines) = state.active_block_lines.clone()
                                        && !active_lines.is_empty()
                                            && active_lines.end <= buf.len_lines()
                                        {
                                            let at_end = !use_virtual_window
                                                || vis.end == index.blocks.len()
                                                || index.blocks.get(vis.end.saturating_sub(1)).is_some_and(|b| {
                                                    b.block.source_lines().end <= active_lines.start
                                                });
                                            if at_end {
                                                let kind = state
                                                    .active_block_kind
                                                    .unwrap_or(ActiveBlockKind::Paragraph);
                                                let a = render_inline_wysiwyg_block(
                                                    ui,
                                                    buf,
                                                    &active_lines,
                                                    kind,
                                                    content_w,
                                                    theme,
                                                    show_markup,
                                                    workspace_top,
                                                    state,
                                                );
                                                if a != ReaderAction::None {
                                                    action = a;
                                                }
                                            }
                                        }

                                if wysiwyg
                                    && !read_only
                                    && !state.autoscroll_active
                                    && vis.end == index.blocks.len()
                                {
                                    let (_tail_rect, tail_resp) = ui.allocate_exact_size(
                                        Vec2::new(content_w, 72.0),
                                        Sense::click(),
                                    );
                                    if tail_resp.clicked() {
                                        let last_idx = buf.len_lines().saturating_sub(1);
                                        let last_empty =
                                            buf.line_text(last_idx).trim().is_empty();
                                        let last_in_block = index
                                            .blocks
                                            .last()
                                            .is_some_and(|b| b.block.source_lines().contains(&last_idx));
                                        let already_active_here = state
                                            .active_block_lines
                                            .as_ref()
                                            .is_some_and(|r| r.contains(&last_idx));
                                        if already_active_here {
                                            state.focus_inline_editor = true;
                                        } else {
                                            let target_line = if last_empty && !last_in_block {
                                                last_idx
                                            } else {
                                                let le = buf.line_ending();
                                                let end_char = buf.len_chars();
                                                let ins = if last_empty {
                                                    le.to_string()
                                                } else {
                                                    format!("{le}{le}")
                                                };
                                                buf.replace_range(end_char..end_char, &ins);
                                                buf.len_lines().saturating_sub(1)
                                            };
                                            let rng =
                                                target_line..(target_line + 1).min(buf.len_lines());
                                            activate_wysiwyg_block(
                                                Some(ui),
                                                buf,
                                                &rng,
                                                ActiveBlockKind::Paragraph,
                                                content_w,
                                                theme,
                                                show_markup,
                                                None,
                                                None,
                                                1,
                                                state,
                                            );
                                        }
                                    }
                                }

                                if use_virtual_window && vis.end < index.blocks.len() {
                                    let rendered_end = if vis.end > 0 {
                                        let last_idx = vis.end - 1;
                                        index.block_y_start(last_idx) + index.blocks[last_idx].height
                                    } else {
                                        0.0
                                    };
                                    let bot_h = (index.total_height - rendered_end).max(0.0);
                                    if bot_h > 0.0 {
                                        ui.add_space(bot_h);
                                    }
                                } else if !use_virtual_window {
                                    index.sync_non_virtual_positions(&non_virt_updates);
                                }

                                index.set_measured_heights_batch(&height_updates);
                            });
                        });
                });
                ui.add_space(24.0);
            });
            scroll_out_id = Some(out.id);
            scroll_inner_rect = out.inner_rect;
            scroll_offset_y = out.state.offset.y;
            scroll_max_y = (out.content_size.y - out.inner_rect.height()).max(0.0);
        });

    if let Some(sy) = applied_smooth_y
        && (scroll_offset_y - sy).abs() > 2.0 {
            state.smooth_scroll_target_y = None;
        }

    if scroll_max_y > 20.0 && scroll_inner_rect.is_positive() {
        let frac = (scroll_offset_y / scroll_max_y).clamp(0.0, 1.0);
        let painter = ui.painter_at(scroll_inner_rect);
        let top_bar_bg = egui::Rect::from_min_max(
            scroll_inner_rect.left_top(),
            egui::pos2(scroll_inner_rect.right(), scroll_inner_rect.top() + 2.5),
        );
        painter.rect_filled(top_bar_bg, 0.0, theme.border_subtle);
        let top_bar_fill = egui::Rect::from_min_max(
            scroll_inner_rect.left_top(),
            egui::pos2(
                scroll_inner_rect.left() + scroll_inner_rect.width() * frac,
                scroll_inner_rect.top() + 2.5,
            ),
        );
        painter.rect_filled(top_bar_fill, 0.0, theme.accent);
    }

    if read_only
        && scroll_offset_y > 360.0
        && !state.autoscroll_active
        && scroll_inner_rect.width() > 240.0
        && scroll_inner_rect.height() > 140.0
    {
        egui::Area::new(ui.id().with("reader_back_to_top_pill"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(
                (scroll_inner_rect.right() - 88.0).max(scroll_inner_rect.left() + 8.0),
                (scroll_inner_rect.bottom() - 42.0).max(scroll_inner_rect.top() + 8.0),
            ))
            .show(ui.ctx(), |ui| {
                let top_btn = egui::Button::new(
                    RichText::new("↑ Top")
                        .size(12.0)
                        .strong()
                        .color(theme.accent),
                )
                .fill(theme.paper_bg)
                .stroke(Stroke::new(1.0_f32, theme.border))
                .corner_radius(CornerRadius::same(16))
                .min_size(Vec2::new(68.0, 28.0));
                if ui
                    .add(top_btn)
                    .on_hover_text("Back to top (Home)")
                    .clicked()
                {
                    state.smooth_scroll_to(0.0);
                }
            });
    }

    state.last_scroll_area_id = scroll_out_id;
    state.last_pane_rect = scroll_inner_rect;
    state.last_scroll_offset_y = scroll_offset_y;
    state.last_max_scroll_y = scroll_max_y;

    if read_only && state.image_lightbox.is_some()
        && let Some(lb_action) = render_image_lightbox(ui.ctx(), theme, state) {
            action = lb_action;
        }

    action
}

fn render_image_lightbox(
    ctx: &egui::Context,
    theme: &WordTheme,
    state: &mut VirtualReaderState,
) -> Option<ReaderAction> {
    let mut close_lightbox = false;
    let mut action = None;

    let screen = ctx.screen_rect();
    let backdrop_resp = egui::Area::new(egui::Id::new("md_image_lightbox_backdrop"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_black_alpha(205));
            resp
        })
        .inner;
    if backdrop_resp.clicked() {
        close_lightbox = true;
    }

    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        close_lightbox = true;
    }

    if let Some(lb) = state.image_lightbox.as_mut() {
        ctx.input_mut(|i| {
            let no_mod = egui::Modifiers::NONE;
            if i.consume_key(no_mod, egui::Key::Plus) || i.consume_key(no_mod, egui::Key::Equals) {
                lb.zoom = (lb.zoom + 0.25).clamp(0.5, 4.0);
            } else if i.consume_key(no_mod, egui::Key::Minus) {
                lb.zoom = (lb.zoom - 0.25).clamp(0.5, 4.0);
            } else if i.consume_key(no_mod, egui::Key::Num0) {
                lb.zoom = 1.0;
            }
        });

        let load_state = load_cached_image_state(
            ctx,
            &lb.url,
            lb.base_dir.as_deref(),
            lb.doc_path.as_deref(),
        );

        let max_card_w = (screen.width() - 64.0).max(300.0);
        let max_card_h = (screen.height() - 72.0).max(240.0);
        let max_view_w = (max_card_w - 32.0).max(240.0);
        let max_view_h = (max_card_h - 84.0).max(160.0);

        let title_text = {
            let trimmed_alt = lb.alt.trim();
            if !trimmed_alt.is_empty() && trimmed_alt != lb.url.trim() {
                trimmed_alt.to_string()
            } else {
                lb.url
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(&lb.url)
                    .to_string()
            }
        };

        egui::Area::new(egui::Id::new("md_image_lightbox_card"))
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme.paper_bg)
                    .stroke(Stroke::new(1.5_f32, theme.border))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_max_width(max_card_w);
                        ui.set_max_height(max_card_h);

                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(
                                RichText::new(&title_text)
                                    .size(13.5)
                                    .strong()
                                    .color(theme.heading_color),
                            );
                            if let ImageLoadState::Loaded(ref tex) = load_state {
                                let [tw, th] = tex.size();
                                ui.label(
                                    RichText::new(format!("{tw}×{th}"))
                                        .size(11.5)
                                        .color(theme.text_muted),
                                );
                            }
                            ui.separator();
                            if ui.small_button("−").on_hover_text("Zoom Out (-)").clicked() {
                                lb.zoom = (lb.zoom - 0.25).clamp(0.5, 4.0);
                            }
                            ui.label(
                                RichText::new(format!("{}%", (lb.zoom * 100.0).round() as i32))
                                    .size(12.0)
                                    .color(theme.text_primary),
                            );
                            if ui.small_button("+").on_hover_text("Zoom In (+)").clicked() {
                                lb.zoom = (lb.zoom + 0.25).clamp(0.5, 4.0);
                            }
                            if ui.small_button("Fit").on_hover_text("Fit to Window (0)").clicked() {
                                lb.zoom = 1.0;
                            }
                            if let Some(ref raw_link) = lb.link_url
                                && let Some(classified) = classify_link_url(raw_link)
                                    && ui
                                        .small_button("Open Link ↗")
                                        .on_hover_text(raw_link)
                                        .clicked()
                                    {
                                        let target = NavTarget::Link(raw_link.clone(), classified);
                                        action = Some(nav_target_action(&target));
                                        close_lightbox = true;
                                    }
                            if ui
                                .small_button("✕ Close")
                                .on_hover_text("Close Lightbox (Esc)")
                                .clicked()
                            {
                                close_lightbox = true;
                            }
                        });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(8.0);

                        egui::ScrollArea::both()
                            .id_salt("md_image_lightbox_scroll")
                            .max_width(max_view_w)
                            .max_height(max_view_h)
                            .show(ui, |ui| match &load_state {
                                ImageLoadState::Loaded(tex) => {
                                    let [tw, th] = tex.size();
                                    let fw = tw.max(1) as f32;
                                    let fh = th.max(1) as f32;
                                    let fit_scale =
                                        (max_view_w / fw).min(max_view_h / fh).clamp(0.1, 3.0);
                                    let draw_w = (fw * fit_scale * lb.zoom).max(24.0);
                                    let draw_h = (fh * fit_scale * lb.zoom).max(24.0);
                                    ui.vertical_centered(|ui| {
                                        ui.image((tex.id(), Vec2::new(draw_w, draw_h)));
                                    });
                                }
                                ImageLoadState::Loading => {
                                    ui.label(
                                        RichText::new("Loading image...")
                                            .size(13.0)
                                            .color(theme.text_muted),
                                    );
                                }
                                ImageLoadState::Unavailable => {
                                    ui.label(
                                        RichText::new(format!("Unable to load image: {}", lb.url))
                                            .size(13.0)
                                            .color(theme.text_muted),
                                    );
                                }
                            });
                    });
            });
    }

    if close_lightbox {
        state.image_lightbox = None;
    }
    action
}

fn line_bounds_at_col(text: &str, col: usize) -> Range<usize> {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let c = col.min(len);
    let mut start = c;
    while start > 0 && chars[start - 1] != '\n' {
        start -= 1;
    }
    let mut end = c;
    while end < len && chars[end] != '\n' {
        end += 1;
    }
    start..end
}

fn wysiwyg_word_at_col(text: &str, col: usize, show_markup: bool) -> Option<Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut probe = col.min(len);

    if !show_markup && len >= 3 {
        // Snap off hidden <u> or </u> delimiter tokens
        let open_start = probe.saturating_sub(2);
        for s in open_start..=probe.min(len.saturating_sub(3)) {
            if chars[s] == '<'
                && (chars[s + 1] == 'u' || chars[s + 1] == 'U')
                && chars[s + 2] == '>'
                && probe >= s
                && probe < s + 3
            {
                probe = s + 3;
                break;
            }
        }
        if len >= 4 {
            let close_start = probe.saturating_sub(3);
            for s in close_start..=probe.min(len.saturating_sub(4)) {
                if chars[s] == '<'
                    && chars[s + 1] == '/'
                    && (chars[s + 2] == 'u' || chars[s + 2] == 'U')
                    && chars[s + 3] == '>'
                    && probe > s
                    && probe <= s + 4
                {
                    probe = s;
                    break;
                }
            }
        }
    }

    if let Some(r) = word_range_in_line(text, probe) {
        return Some(r);
    }
    if show_markup {
        return None;
    }

    let is_inline_delim = |c: char| matches!(c, '*' | '~' | '`');
    let mut fwd = probe;
    for _ in 0..3 {
        if fwd < len && is_inline_delim(chars[fwd]) {
            fwd += 1;
        } else {
            break;
        }
    }
    if fwd != probe && fwd < len && (chars[fwd].is_alphanumeric() || chars[fwd] == '_')
        && let Some(r) = word_range_in_line(text, fwd) {
            return Some(r);
        }
    let mut bck = probe;
    for _ in 0..3 {
        if bck > 0 && is_inline_delim(chars[bck - 1]) {
            bck -= 1;
        } else {
            break;
        }
    }
    if bck != probe && bck > 0 && (chars[bck - 1].is_alphanumeric() || chars[bck - 1] == '_') {
        return word_range_in_line(text, bck);
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn activate_wysiwyg_block(
    ui: Option<&egui::Ui>,
    buf: &mut DocumentBuffer,
    src_lines: &Range<usize>,
    kind: ActiveBlockKind,
    content_w: f32,
    theme: &WordTheme,
    show_markup: bool,
    block_rect: Option<egui::Rect>,
    pointer_pos: Option<egui::Pos2>,
    click_count: u8,
    state: &mut VirtualReaderState,
) {
    if src_lines.is_empty() || src_lines.end > buf.len_lines() {
        return;
    }
    let range = buf.line_range_chars(src_lines.start, src_lines.end - 1);
    let raw = buf.rope().slice(range).to_string();
    let (prefix, text, suffix) = split_wysiwyg_block_text(&raw, kind, show_markup);
    state.active_block_prefix = prefix;
    state.active_block_text = text;
    state.active_block_suffix = suffix;
    state.active_block_kind = Some(kind);
    state.active_block_lines = Some(src_lines.clone());
    if matches!(kind, ActiveBlockKind::CodeBlock) {
        let info = infer_code_block_info_at_line(&buf.line_text(src_lines.start));
        state.active_code_lang_draft = info;
        state.active_code_lang_line = Some(src_lines.start);
    } else {
        state.active_code_lang_draft.clear();
        state.active_code_lang_line = None;
    }
    state.last_synced_version = buf.version;
    state.last_synced_show_markup = show_markup;
    state.focus_inline_editor = true;
    state.sync_cursor_from_buf = true;

    let block_start = buf.line_to_char(src_lines.start);
    let prefix_chars = state.active_block_prefix.chars().count();
    let text_chars = state.active_block_text.chars().count();

    if let (Some(ui), Some(b_rect), Some(pos)) = (ui, block_rect, pointer_pos) {
        let is_code = matches!(kind, ActiveBlockKind::CodeBlock);
        let line_raw = buf.line_text(src_lines.start);
        let (alignment, indent_level, rest) = parse_line_block_meta(&line_raw);
        let direction = if is_code {
            TextDirection::Ltr
        } else {
            detect_paragraph_direction(rest)
        };
        let indent_px = if is_code {
            0.0
        } else {
            (indent_level as f32 * 28.0).min((content_w * 0.5).max(0.0))
        };
        let inner_w = if is_code {
            (content_w - 32.0).max(120.0)
        } else if show_markup {
            (content_w - indent_px - 20.0).max(120.0)
        } else {
            (content_w - indent_px).max(120.0)
        };
        let code_lang = if is_code {
            state.active_code_lang_draft.as_str()
        } else {
            ""
        };

        let job = build_wysiwyg_edit_layout_job(
            &state.active_block_text,
            inner_w,
            kind,
            alignment,
            direction,
            theme,
            show_markup,
            code_lang,
        );
        let galley = ui.fonts(|f| f.layout_job(job));
        let (inner_left, inner_top) = if is_code {
            (b_rect.left() + 14.0, b_rect.top() + 12.0)
        } else {
            let pad_x = if show_markup { 8.0 } else { 0.0 };
            let left = if direction == TextDirection::Ltr {
                b_rect.left() + indent_px + pad_x
            } else {
                b_rect.left() + pad_x
            };
            let top = b_rect.top() + if show_markup { 24.0 } else { 1.0 };
            (left, top)
        };
        let egui_align = if is_code {
            egui::Align::Min
        } else {
            resolve_egui_align(alignment, direction)
        };
        let g_w = galley.size().x;
        let galley_origin_x = match egui_align {
            egui::Align::Min => inner_left,
            egui::Align::Center => inner_left + ((inner_w - g_w) * 0.5).max(0.0),
            egui::Align::Max => inner_left + (inner_w - g_w).max(0.0),
        };
        let bidi = MultilineBidiMap::new(&state.active_block_text);
        let rel = pos - egui::pos2(galley_origin_x, inner_top);
        let vis_ccursor = galley.cursor_from_pos(rel).ccursor.index.min(text_chars);
        let ccursor_idx = bidi.logical_col_for_visual(&state.active_block_text, vis_ccursor);
        let row_opt = galley
            .rows
            .iter()
            .find(|r| rel.y >= r.rect.top() && rel.y <= r.rect.bottom())
            .or_else(|| {
                if rel.y < 0.0 {
                    galley.rows.first()
                } else {
                    galley.rows.last()
                }
            });
        let in_row_text = row_opt.is_some_and(|r| {
            !r.glyphs.is_empty()
                && pos.x >= galley_origin_x + r.rect.min.x - 2.0
                && pos.x <= galley_origin_x + r.rect.max.x + 4.0
        });

        let (s, e) = if click_count >= 3 {
            (0, text_chars)
        } else if click_count == 2 {
            if in_row_text {
                if let Some(w_range) =
                    wysiwyg_word_at_col(&state.active_block_text, ccursor_idx, show_markup)
                {
                    (w_range.start, w_range.end)
                } else {
                    let lr = line_bounds_at_col(&state.active_block_text, ccursor_idx);
                    (lr.start, lr.end)
                }
            } else {
                let lr = line_bounds_at_col(&state.active_block_text, ccursor_idx);
                (lr.start, lr.end)
            }
        } else {
            (ccursor_idx, ccursor_idx)
        };

        let p_cc = egui::text::CCursor::new(e);
        let s_cc = egui::text::CCursor::new(s);
        state.last_cursor_range = Some(egui::text::CCursorRange::two(s_cc, p_cc));
        if e > s {
            buf.selection_anchor = Some((block_start + prefix_chars + s).min(buf.len_chars()));
            buf.cursor_char = (block_start + prefix_chars + e).min(buf.len_chars());
            buf.cache_current_selection();
        } else {
            buf.set_cursor((block_start + prefix_chars + e).min(buf.len_chars()), false);
        }
    } else {
        let zero = egui::text::CCursor::new(0);
        state.last_cursor_range = Some(egui::text::CCursorRange::one(zero));
        let c = block_start + prefix_chars;
        buf.set_cursor(c.min(buf.len_chars()), false);
    }
}

fn doc_to_local_chars(doc_rel: usize, local_lf_text: &str, is_crlf: bool) -> usize {
    if !is_crlf || doc_rel == 0 {
        return doc_rel.min(local_lf_text.chars().count());
    }
    let mut doc_acc = 0usize;
    let mut local_idx = 0usize;
    for ch in local_lf_text.chars() {
        let step = if ch == '\n' { 2 } else { 1 };
        if doc_acc + step > doc_rel {
            return local_idx;
        }
        doc_acc += step;
        local_idx += 1;
    }
    local_idx
}

fn local_to_doc_chars(local_idx: usize, local_lf_text: &str, is_crlf: bool) -> usize {
    if !is_crlf || local_idx == 0 {
        return local_idx;
    }
    local_lf_text
        .chars()
        .take(local_idx)
        .map(|ch| if ch == '\n' { 2 } else { 1 })
        .sum()
}

fn block_list_contains_source_lines(blocks: &[DocBlock], target: &Range<usize>) -> bool {
    blocks.iter().any(|block| match block {
        DocBlock::Heading { source_lines, .. }
        | DocBlock::Paragraph { source_lines, .. }
        | DocBlock::ThematicBreak { source_lines } => source_lines == target,
        DocBlock::CodeBlock { source_lines, .. } => source_lines.start == target.start,
        DocBlock::BlockQuote { blocks, .. } | DocBlock::FootnoteDefinition { blocks, .. } => {
            block_list_contains_source_lines(blocks, target)
        }
        DocBlock::List { items, .. } => items
            .iter()
            .any(|it| block_list_contains_source_lines(&it.blocks, target)),
        _ => false,
    })
}

fn index_has_leaf_block_for_lines(index: &ViewportBlockIndex, target: &Range<usize>) -> bool {
    let Some(pos) = index.block_for_source_line(target.start) else {
        return false;
    };
    let check_start = pos.saturating_sub(1);
    let check_end = (pos + 2).min(index.blocks.len());
    index.blocks[check_start..check_end].iter().any(|entry| match &entry.block {
        DocBlock::Heading { source_lines, .. }
        | DocBlock::Paragraph { source_lines, .. }
        | DocBlock::ThematicBreak { source_lines } => source_lines == target,
        DocBlock::CodeBlock { source_lines, .. } => source_lines.start == target.start,
        DocBlock::BlockQuote { blocks, .. } | DocBlock::FootnoteDefinition { blocks, .. } => {
            block_list_contains_source_lines(blocks, target)
        }
        DocBlock::List { items, .. } => items
            .iter()
            .any(|it| block_list_contains_source_lines(&it.blocks, target)),
        _ => false,
    })
}

fn find_leaf_block_at_line(
    block: &DocBlock,
    line_idx: usize,
) -> Option<(Range<usize>, ActiveBlockKind)> {
    match block {
        DocBlock::Heading {
            level,
            source_lines,
            ..
        } if source_lines.contains(&line_idx) => {
            Some((source_lines.clone(), ActiveBlockKind::Heading(*level)))
        }
        DocBlock::Paragraph { source_lines, .. }
        | DocBlock::ThematicBreak { source_lines }
            if source_lines.contains(&line_idx) =>
        {
            Some((source_lines.clone(), ActiveBlockKind::Paragraph))
        }
        DocBlock::CodeBlock { source_lines, .. } if source_lines.contains(&line_idx) => {
            Some((source_lines.clone(), ActiveBlockKind::CodeBlock))
        }
        DocBlock::BlockQuote { blocks, .. } | DocBlock::FootnoteDefinition { blocks, .. } => {
            blocks
                .iter()
                .find_map(|b| find_leaf_block_at_line(b, line_idx))
        }
        DocBlock::List { items, .. } => items
            .iter()
            .flat_map(|it| it.blocks.iter())
            .find_map(|b| find_leaf_block_at_line(b, line_idx)),
        _ => None,
    }
}

/// Programmatically activates inline WYSIWYG editing for the block containing `line_idx`.
pub fn activate_wysiwyg_at_line(
    buf: &mut DocumentBuffer,
    index: &ViewportBlockIndex,
    line_idx: usize,
    show_markup: bool,
    state: &mut VirtualReaderState,
) -> bool {
    let default_theme = WordTheme::light();
    let Some(pos) = index.block_for_source_line(line_idx) else {
        return false;
    };
    let check_start = pos.saturating_sub(1);
    let check_end = (pos + 2).min(index.blocks.len());
    for entry in &index.blocks[check_start..check_end] {
        if let Some((lines, kind)) = find_leaf_block_at_line(&entry.block, line_idx) {
            activate_wysiwyg_block(
                None,
                buf,
                &lines,
                kind,
                720.0,
                &default_theme,
                show_markup,
                None,
                None,
                1,
                state,
            );
            return true;
        }
    }
    false
}

/// Programmatically activates inline WYSIWYG editing for a freshly inserted fenced code block
/// spanning `src_lines` (including opening and closing fence lines) and places the cursor inside.
pub fn activate_wysiwyg_code_block_range(
    buf: &mut DocumentBuffer,
    src_lines: Range<usize>,
    show_markup: bool,
    state: &mut VirtualReaderState,
) -> bool {
    if src_lines.is_empty() || src_lines.end > buf.len_lines() {
        return false;
    }
    let saved_cursor = buf.cursor_char;
    let is_crlf = buf.line_ending() == "\r\n";
    let default_theme = WordTheme::light();
    activate_wysiwyg_block(
        None,
        buf,
        &src_lines,
        ActiveBlockKind::CodeBlock,
        720.0,
        &default_theme,
        show_markup,
        None,
        None,
        1,
        state,
    );
    let body_start = buf.line_to_char(src_lines.start) + state.active_block_prefix.chars().count();
    let doc_rel = saved_cursor.saturating_sub(body_start);
    let rel_cursor = doc_to_local_chars(doc_rel, &state.active_block_text, is_crlf);
    let cc = egui::text::CCursor::new(rel_cursor);
    state.last_cursor_range = Some(egui::text::CCursorRange::one(cc));
    buf.set_cursor(saved_cursor.min(buf.len_chars()), false);
    true
}

/// Activates the WYSIWYG block containing `line_idx` while keeping `buf`'s current selection
/// and cursor intact (used when jumping to Find/Replace matches).
pub fn activate_wysiwyg_preserving_selection(
    buf: &DocumentBuffer,
    index: &ViewportBlockIndex,
    line_idx: usize,
    show_markup: bool,
    state: &mut VirtualReaderState,
) -> bool {
    let Some(pos) = index.block_for_source_line(line_idx) else {
        return false;
    };
    let check_start = pos.saturating_sub(1);
    let check_end = (pos + 2).min(index.blocks.len());
    for entry in &index.blocks[check_start..check_end] {
        if let Some((lines, kind)) = find_leaf_block_at_line(&entry.block, line_idx) {
            if lines.is_empty() || lines.end > buf.len_lines() {
                return false;
            }
            let range = buf.line_range_chars(lines.start, lines.end - 1);
            let raw = buf.rope().slice(range).to_string();
            let (prefix, text, suffix) = split_wysiwyg_block_text(&raw, kind, show_markup);
            state.active_block_prefix = prefix;
            state.active_block_text = text;
            state.active_block_suffix = suffix;
            state.active_block_kind = Some(kind);
            state.active_block_lines = Some(lines.clone());
            if matches!(kind, ActiveBlockKind::CodeBlock) {
                let info = infer_code_block_info_at_line(&buf.line_text(lines.start));
                state.active_code_lang_draft = info;
                state.active_code_lang_line = Some(lines.start);
            } else {
                state.active_code_lang_draft.clear();
                state.active_code_lang_line = None;
            }
            state.last_synced_version = buf.version;
            state.last_synced_show_markup = show_markup;
            state.focus_inline_editor = false;
            state.sync_cursor_from_buf = true;
            return true;
        }
    }
    false
}

#[derive(Clone, Copy, Default)]
struct InlineEditStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    link: bool,
}

fn project_layout_job_to_bidi(
    logical_job: LayoutJob,
    bidi: &MultilineBidiMap,
    fallback_color: Color32,
    fallback_size: f32,
) -> LayoutJob {
    if bidi.is_identity {
        return logical_job;
    }
    let logical_text = &logical_job.text;
    let n = logical_text.chars().count();
    if n == 0 {
        return logical_job;
    }

    let mut char_to_sec = vec![0usize; n];
    let mut byte_to_char = vec![0usize; logical_text.len() + 1];
    for (c_idx, (b_idx, ch)) in logical_text.char_indices().enumerate() {
        let b_end = b_idx + ch.len_utf8();
        for slot in &mut byte_to_char[b_idx..b_end] {
            *slot = c_idx;
        }
    }
    byte_to_char[logical_text.len()] = n;

    for (sec_idx, sec) in logical_job.sections.iter().enumerate() {
        let cs = byte_to_char[sec.byte_range.start.min(logical_text.len())];
        let ce = byte_to_char[sec.byte_range.end.min(logical_text.len())];
        for slot in &mut char_to_sec[cs..ce] {
            *slot = sec_idx;
        }
    }

    let mut vis_job = LayoutJob {
        wrap: logical_job.wrap.clone(),
        halign: logical_job.halign,
        justify: logical_job.justify,
        ..Default::default()
    };
    let vis_chars: Vec<char> = bidi.visual_text.chars().collect();
    if vis_chars.is_empty() {
        return vis_job;
    }
    let fallback_fmt = TextFormat {
        font_id: FontId::new(fallback_size, FontFamily::Proportional),
        color: fallback_color,
        ..Default::default()
    };

    let mut run = String::new();
    let mut cur_sec = bidi
        .vis_to_log
        .first()
        .and_then(|&l| char_to_sec.get(l).copied())
        .unwrap_or(0);

    for (v_idx, &ch) in vis_chars.iter().enumerate() {
        let log_idx = bidi.vis_to_log.get(v_idx).copied().unwrap_or(v_idx);
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

#[allow(clippy::too_many_arguments)]
fn build_wysiwyg_edit_layout_job(
    text: &str,
    wrap_width: f32,
    kind: ActiveBlockKind,
    alignment: BlockAlignment,
    _direction: TextDirection,
    theme: &WordTheme,
    show_markup: bool,
    code_lang: &str,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width.max(40.0);
    // Keep Galley glyph coordinates in [0.0, width]; TextEdit::horizontal_align positions the Galley.
    job.halign = egui::Align::Min;

    let bidi = MultilineBidiMap::new(text);

    if matches!(kind, ActiveBlockKind::CodeBlock) {
        let line_h = Some((13.0_f32 * 1.42).round());
        let mut fmt = TextFormat {
            font_id: FontId::new(13.0, FontFamily::Monospace),
            color: theme.text_primary,
            line_height: line_h,
            ..Default::default()
        };
        if show_markup {
            // Color fence lines distinctly when markup is toggled on
            for (idx, line) in text.split_inclusive('\n').enumerate() {
                let trimmed = line.trim();
                let is_fence = trimmed.starts_with("```")
                    || trimmed.starts_with("~~~")
                    || trimmed == "$$";
                fmt.color = if (idx == 0 || is_fence) && is_fence {
                    theme.accent
                } else {
                    theme.text_primary
                };
                job.append(line, 0.0, fmt.clone());
            }
        } else if text.len() <= 32_768 {
            append_highlighted_code_block(&mut job, text, code_lang, theme, fmt);
        } else {
            job.append(text, 0.0, fmt);
        }
        return project_layout_job_to_bidi(job, &bidi, theme.text_primary, 13.0);
    }

    job.justify = alignment == BlockAlignment::Justify;

    let (base_size, base_bold, base_color) = match kind {
        ActiveBlockKind::Heading(lvl) => (heading_font_size(lvl), true, theme.heading_color),
        ActiveBlockKind::Paragraph | ActiveBlockKind::CodeBlock => (15.0, false, theme.text_primary),
    };

    let delim_fmt = if show_markup {
        TextFormat {
            font_id: FontId::new(base_size * 0.9, FontFamily::Monospace),
            color: theme.accent,
            ..Default::default()
        }
    } else {
        TextFormat {
            font_id: FontId::new(0.2, FontFamily::Proportional),
            color: Color32::TRANSPARENT,
            ..Default::default()
        }
    };

    append_inline_markdown_spans(
        &mut job,
        text,
        InlineEditStyle {
            bold: base_bold,
            ..Default::default()
        },
        base_size,
        base_color,
        theme,
        show_markup,
        &delim_fmt,
        0,
    );
    project_layout_job_to_bidi(job, &bidi, base_color, base_size)
}

fn find_closing_single_underscore(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            return None;
        }
        if bytes[i] == b'_' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'_' {
                i += 2;
                continue;
            }
            if i > 0 && !bytes[i - 1].is_ascii_whitespace() {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn append_inline_markdown_spans(
    job: &mut LayoutJob,
    s: &str,
    style: InlineEditStyle,
    base_size: f32,
    base_color: Color32,
    theme: &WordTheme,
    show_markup: bool,
    delim_fmt: &TextFormat,
    depth: u8,
) {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut plain_start = 0usize;

    let flush_plain = |job: &mut LayoutJob, slice: &str, st: InlineEditStyle| {
        if slice.is_empty() {
            return;
        }
        let family = if st.bold {
            FontFamily::Name("bold".into())
        } else {
            FontFamily::Proportional
        };
        let color = if st.link { theme.link_color } else { base_color };
        let mut fmt = TextFormat {
            font_id: FontId::new(base_size, family),
            color,
            italics: st.italic,
            ..Default::default()
        };
        if st.strike {
            fmt.strikethrough = Stroke::new(1.2_f32, color);
        }
        if st.link {
            fmt.underline = Stroke::new(1.0_f32, theme.link_color);
        } else if st.underline {
            fmt.underline = Stroke::new(1.1_f32, color);
        }
        job.append(slice, 0.0, fmt);
    };

    while i < len {
        let b = bytes[i];

        if b == b'\\' {
            i += 1;
            if i < len {
                i += s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            continue;
        }

        // Hide continuation blockquote/structural prefixes at start of lines unless show_markup
        if !show_markup && depth == 0 && (i == 0 || bytes[i - 1] == b'\n') && s[i..].starts_with("> ")
        {
            flush_plain(job, &s[plain_start..i], style);
            job.append(&s[i..i + 2], 0.0, delim_fmt.clone());
            i += 2;
            plain_start = i;
            continue;
        }
        if show_markup && depth == 0 && i == 0 {
            let trimmed = s.trim_start_matches(' ');
            let off = s.len() - trimmed.len();
            let hashes = trimmed.bytes().take_while(|&c| c == b'#').count();
            if (1..=6).contains(&hashes) && trimmed.as_bytes().get(hashes) == Some(&b' ') {
                flush_plain(job, &s[plain_start..off], style);
                job.append(&s[off..off + hashes + 1], 0.0, delim_fmt.clone());
                i = off + hashes + 1;
                plain_start = i;
                continue;
            }
        }

        // Underline HTML inline tag: <u>...</u> or <ins>...</ins>
        if depth < 4 && b == b'<' {
            let tag_match = if s.get(i..i + 3).is_some_and(|t| t.eq_ignore_ascii_case("<u>")) {
                Some((3usize, "</u>", 4usize))
            } else if s.get(i..i + 5).is_some_and(|t| t.eq_ignore_ascii_case("<ins>")) {
                Some((5usize, "</ins>", 6usize))
            } else {
                None
            };
            if let Some((open_len, close_tag, close_len)) = tag_match
                && let Some(rel_end) = find_ascii_case_insensitive(&s[i + open_len..], close_tag)
                    && !s[i + open_len..i + open_len + rel_end].contains("\n\n") {
                        flush_plain(job, &s[plain_start..i], style);
                        job.append(&s[i..i + open_len], 0.0, delim_fmt.clone());
                        let mut next_st = style;
                        next_st.underline = true;
                        append_inline_markdown_spans(
                            job,
                            &s[i + open_len..i + open_len + rel_end],
                            next_st,
                            base_size,
                            base_color,
                            theme,
                            show_markup,
                            delim_fmt,
                            depth + 1,
                        );
                        let close_start = i + open_len + rel_end;
                        job.append(
                            &s[close_start..close_start + close_len],
                            0.0,
                            delim_fmt.clone(),
                        );
                        i = close_start + close_len;
                        plain_start = i;
                        continue;
                    }
        }

        // Inline code: `...`
        if b == b'`' {
            let mut ticks = 1usize;
            while i + ticks < len && bytes[i + ticks] == b'`' {
                ticks += 1;
            }
            if let Some(rel_close) = find_closing_ticks(&s[i + ticks..], ticks) {
                let inner_start = i + ticks;
                let inner_end = inner_start + rel_close;
                let close_end = inner_end + ticks;
                flush_plain(job, &s[plain_start..i], style);
                job.append(&s[i..inner_start], 0.0, delim_fmt.clone());
                let code_fmt = TextFormat {
                    font_id: FontId::new(base_size * 0.9, FontFamily::Monospace),
                    color: theme.accent,
                    background: theme.code_bg,
                    ..Default::default()
                };
                job.append(&s[inner_start..inner_end], 0.0, code_fmt);
                job.append(&s[inner_end..close_end], 0.0, delim_fmt.clone());
                i = close_end;
                plain_start = i;
                continue;
            }
            i += ticks;
            continue;
        }

        // Bold+Italic / Bold / Italic (`***`, `**`, `*`)
        if depth < 4 && b == b'*' {
            if s[i..].starts_with("***")
                && let Some(rel_end) = s[i + 3..].find("***")
                    && rel_end > 0 {
                        flush_plain(job, &s[plain_start..i], style);
                        job.append("***", 0.0, delim_fmt.clone());
                        let mut next_st = style;
                        next_st.bold = true;
                        next_st.italic = true;
                        append_inline_markdown_spans(
                            job,
                            &s[i + 3..i + 3 + rel_end],
                            next_st,
                            base_size,
                            base_color,
                            theme,
                            show_markup,
                            delim_fmt,
                            depth + 1,
                        );
                        job.append("***", 0.0, delim_fmt.clone());
                        i = i + 6 + rel_end;
                        plain_start = i;
                        continue;
                    }
            if s[i..].starts_with("**") {
                if let Some(rel_end) = find_closing_marker(&s[i + 2..], "**")
                    && rel_end > 0 {
                        flush_plain(job, &s[plain_start..i], style);
                        job.append("**", 0.0, delim_fmt.clone());
                        let mut next_st = style;
                        next_st.bold = true;
                        append_inline_markdown_spans(
                            job,
                            &s[i + 2..i + 2 + rel_end],
                            next_st,
                            base_size,
                            base_color,
                            theme,
                            show_markup,
                            delim_fmt,
                            depth + 1,
                        );
                        job.append("**", 0.0, delim_fmt.clone());
                        i = i + 4 + rel_end;
                        plain_start = i;
                        continue;
                    }
            } else if i + 1 < len && !bytes[i + 1].is_ascii_whitespace()
                && let Some(rel_end) = find_closing_single_star(&s[i + 1..])
                    && rel_end > 0 {
                        flush_plain(job, &s[plain_start..i], style);
                        job.append("*", 0.0, delim_fmt.clone());
                        let mut next_st = style;
                        next_st.italic = true;
                        append_inline_markdown_spans(
                            job,
                            &s[i + 1..i + 1 + rel_end],
                            next_st,
                            base_size,
                            base_color,
                            theme,
                            show_markup,
                            delim_fmt,
                            depth + 1,
                        );
                        job.append("*", 0.0, delim_fmt.clone());
                        i = i + 2 + rel_end;
                        plain_start = i;
                        continue;
                    }
        }

        // Bold / Italic (`__`, `_`) respecting intra-word underscores (`snake_case`)
        if depth < 4 && b == b'_' {
            let left_ok = i == 0
                || !s[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric());
            if left_ok && s[i..].starts_with("__") {
                if let Some(rel_end) = find_closing_marker(&s[i + 2..], "__") {
                    let close_end = i + 4 + rel_end;
                    let right_ok = s[close_end..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_alphanumeric());
                    if rel_end > 0 && right_ok {
                        flush_plain(job, &s[plain_start..i], style);
                        job.append("__", 0.0, delim_fmt.clone());
                        let mut next_st = style;
                        next_st.bold = true;
                        append_inline_markdown_spans(
                            job,
                            &s[i + 2..i + 2 + rel_end],
                            next_st,
                            base_size,
                            base_color,
                            theme,
                            show_markup,
                            delim_fmt,
                            depth + 1,
                        );
                        job.append("__", 0.0, delim_fmt.clone());
                        i = close_end;
                        plain_start = i;
                        continue;
                    }
                }
            } else if left_ok
                && !s[i..].starts_with("__")
                && i + 1 < len
                && !bytes[i + 1].is_ascii_whitespace()
                && let Some(rel_end) = find_closing_single_underscore(&s[i + 1..]) {
                    let close_end = i + 2 + rel_end;
                    let right_ok = s[close_end..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_alphanumeric());
                    if rel_end > 0 && right_ok {
                        flush_plain(job, &s[plain_start..i], style);
                        job.append("_", 0.0, delim_fmt.clone());
                        let mut next_st = style;
                        next_st.italic = true;
                        append_inline_markdown_spans(
                            job,
                            &s[i + 1..i + 1 + rel_end],
                            next_st,
                            base_size,
                            base_color,
                            theme,
                            show_markup,
                            delim_fmt,
                            depth + 1,
                        );
                        job.append("_", 0.0, delim_fmt.clone());
                        i = close_end;
                        plain_start = i;
                        continue;
                    }
                }
        }

        // Strikethrough `~~...~~`
        if depth < 4 && b == b'~' && s[i..].starts_with("~~")
            && let Some(rel_end) = s[i + 2..].find("~~")
                && rel_end > 0 {
                    flush_plain(job, &s[plain_start..i], style);
                    job.append("~~", 0.0, delim_fmt.clone());
                    let mut next_st = style;
                    next_st.strike = true;
                    append_inline_markdown_spans(
                        job,
                        &s[i + 2..i + 2 + rel_end],
                        next_st,
                        base_size,
                        base_color,
                        theme,
                        show_markup,
                        delim_fmt,
                        depth + 1,
                    );
                    job.append("~~", 0.0, delim_fmt.clone());
                    i = i + 4 + rel_end;
                    plain_start = i;
                    continue;
                }

        // Image `![alt](url)` or Link `[label](url)`
        let is_img = b == b'!' && s[i..].starts_with("![");
        if depth < 4 && (is_img || b == b'[') {
            let open_len = if is_img { 2 } else { 1 };
            if let Some((label_end_rel, url_end_rel)) = find_bracket_link_end(&s[i + open_len..]) {
                let label_start = i + open_len;
                let label_end = label_start + label_end_rel;
                let url_close = label_start + url_end_rel;
                flush_plain(job, &s[plain_start..i], style);
                job.append(&s[i..label_start], 0.0, delim_fmt.clone());
                if is_img {
                    let img_label_fmt = TextFormat {
                        font_id: FontId::new(base_size * 0.92, FontFamily::Proportional),
                        color: theme.accent,
                        background: theme.accent_soft,
                        ..Default::default()
                    };
                    job.append(&s[label_start..label_end], 0.0, img_label_fmt);
                } else {
                    let mut next_st = style;
                    next_st.link = true;
                    append_inline_markdown_spans(
                        job,
                        &s[label_start..label_end],
                        next_st,
                        base_size,
                        base_color,
                        theme,
                        show_markup,
                        delim_fmt,
                        depth + 1,
                    );
                }
                job.append(&s[label_end..=url_close], 0.0, delim_fmt.clone());
                i = url_close + 1;
                plain_start = i;
                continue;
            }
        }

        // Advance cleanly to next UTF-8 character boundary
        let ch_len = s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        i += ch_len;
    }

    flush_plain(job, &s[plain_start..], style);
}

fn find_closing_ticks(s: &str, ticks: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            return None;
        }
        if bytes[i] == b'`' {
            let mut run = 1;
            while i + run < bytes.len() && bytes[i + run] == b'`' {
                run += 1;
            }
            if run == ticks {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

fn find_closing_marker(s: &str, marker: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\n' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            return None;
        }
        if bytes[i] == b'\\' {
            i += 1;
            if i < bytes.len() {
                i += s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            continue;
        }
        if s[i..].starts_with(marker) {
            return Some(i);
        }
        i += s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    None
}

fn find_closing_single_star(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            return None;
        }
        if bytes[i] == b'\\' {
            i += 1;
            if i < bytes.len() {
                i += s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            continue;
        }
        if bytes[i] == b'*' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                i += 2;
                continue;
            }
            if i > 0 && !bytes[i - 1].is_ascii_whitespace() {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn find_bracket_link_end(s: &str) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            return None;
        }
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b']' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'(' {
                let mut j = i + 2;
                let mut paren_depth = 1usize;
                while j < bytes.len() {
                    if bytes[j] == b'\n' {
                        return None;
                    }
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == b'(' {
                        paren_depth += 1;
                    } else if bytes[j] == b')' {
                        paren_depth -= 1;
                        if paren_depth == 0 {
                            return Some((i, j));
                        }
                    }
                    j += 1;
                }
            }
            return None;
        }
        i += 1;
    }
    None
}

fn render_indented_logical(
    ui: &mut egui::Ui,
    content_w: f32,
    indent_level: u8,
    direction: TextDirection,
    add_contents: impl FnOnce(&mut egui::Ui, f32),
) -> egui::Response {
    let indent_px = (indent_level as f32 * 28.0).min((content_w * 0.5).max(0.0));
    let inner_w = (content_w - indent_px).max(80.0);
    if indent_px <= 0.1 {
        return ui
            .allocate_ui_with_layout(
                Vec2::new(content_w, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(inner_w);
                    ui.set_max_width(inner_w);
                    add_contents(ui, inner_w);
                },
            )
            .response;
    }

    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        match direction {
            TextDirection::Ltr => {
                ui.add_space(indent_px);
                ui.allocate_ui_with_layout(
                    Vec2::new(inner_w, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(inner_w);
                        ui.set_max_width(inner_w);
                        add_contents(ui, inner_w);
                    },
                );
            }
            TextDirection::Rtl => {
                ui.allocate_ui_with_layout(
                    Vec2::new(inner_w, 0.0),
                    egui::Layout::top_down(egui::Align::Max),
                    |ui| {
                        ui.set_width(inner_w);
                        ui.set_max_width(inner_w);
                        add_contents(ui, inner_w);
                    },
                );
                ui.add_space(indent_px);
            }
        }
    })
    .response
}

struct InlineEditorFrameResult {
    text_resp: egui::Response,
    inline_id: egui::Id,
    galley: Arc<egui::Galley>,
    had_focus_request: bool,
    did_click_select: bool,
}

fn apply_code_block_tab(
    text: &mut String,
    cr: egui::text::CCursorRange,
    outdent: bool,
) -> egui::text::CCursorRange {
    let total_chars = text.chars().count();
    let p = cr.primary.index.min(total_chars);
    let s = cr.secondary.index.min(total_chars);
    let lo = p.min(s);
    let hi = p.max(s);

    if !outdent && lo == hi {
        let b = text
            .char_indices()
            .nth(p)
            .map(|(idx, _)| idx)
            .unwrap_or(text.len());
        text.insert_str(b, "    ");
        return egui::text::CCursorRange::one(egui::text::CCursor::new(p + 4));
    }

    let mut out = String::with_capacity(text.len() + 16);
    let mut cur_char = 0usize;
    let mut cum_delta = 0isize;
    let mut new_p = p;
    let mut new_s = s;
    let mut p_mapped = false;
    let mut s_mapped = false;

    let line_strs: Vec<&str> = text.split('\n').collect();
    let num_lines = line_strs.len();

    for (li, &line) in line_strs.iter().enumerate() {
        if li > 0 {
            out.push('\n');
        }
        let l_chars = line.chars().count();
        let l_start = cur_char;
        let l_end = cur_char + l_chars;
        let has_nl = li + 1 < num_lines;

        let touched = if lo == hi {
            lo >= l_start && lo <= l_end
        } else {
            lo <= l_end && hi > l_start
        };

        let delta_here: isize = if !touched {
            out.push_str(line);
            0
        } else if outdent {
            if let Some(stripped) = line.strip_prefix('\t') {
                out.push_str(stripped);
                -1
            } else {
                let spaces = line.bytes().take(4).take_while(|&b| b == b' ').count();
                out.push_str(&line[spaces..]);
                -(spaces as isize)
            }
        } else {
            out.push_str("    ");
            out.push_str(line);
            4
        };

        let new_l_start = (l_start as isize + cum_delta).max(0) as usize;
        let map_pos = |pos: usize| -> usize {
            let rel = pos.saturating_sub(l_start);
            if !touched || delta_here == 0 {
                new_l_start + rel
            } else if outdent {
                let removed = (-delta_here) as usize;
                new_l_start + rel.saturating_sub(removed)
            } else if rel == 0 && lo == l_start && hi > lo {
                new_l_start
            } else {
                new_l_start + rel + delta_here as usize
            }
        };

        if !p_mapped && (p <= l_end || !has_nl) {
            new_p = map_pos(p);
            p_mapped = true;
        }
        if !s_mapped && (s <= l_end || !has_nl) {
            new_s = map_pos(s);
            s_mapped = true;
        }

        cum_delta += delta_here;
        cur_char = l_end + usize::from(has_nl);
    }

    *text = out;
    if lo == hi {
        egui::text::CCursorRange::one(egui::text::CCursor::new(new_p))
    } else {
        egui::text::CCursorRange::two(
            egui::text::CCursor::new(new_s),
            egui::text::CCursor::new(new_p),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn render_inline_wysiwyg_block(
    ui: &mut egui::Ui,
    buf: &mut DocumentBuffer,
    src_lines: &Range<usize>,
    kind: ActiveBlockKind,
    content_w: f32,
    theme: &WordTheme,
    show_markup: bool,
    workspace_top: f32,
    state: &mut VirtualReaderState,
) -> ReaderAction {
    let mut action = ReaderAction::None;
    let mut done_clicked = false;
    let mut header_interacted = false;
    let mut lang_input_active = false;
    let mut lang_esc_handled = false;
    let was_any_popup_open = ui.ctx().memory(|m| m.any_popup_open());
    let is_code = matches!(kind, ActiveBlockKind::CodeBlock);
    let is_crlf = buf.line_ending() == "\r\n";

    let line_raw = if !src_lines.is_empty() && src_lines.start < buf.len_lines() {
        buf.line_text(src_lines.start)
    } else {
        String::new()
    };
    let cur_lang = if is_code {
        let info = infer_code_block_info_at_line(&line_raw);
        if state.active_code_lang_line != Some(src_lines.start) {
            state.active_code_lang_draft = info.clone();
            state.active_code_lang_line = Some(src_lines.start);
        }
        info
    } else {
        String::new()
    };
    let (alignment, indent_level, rest) = parse_line_block_meta(&line_raw);
    let dir_probe = if !state.active_block_text.is_empty() {
        state.active_block_text.as_str()
    } else {
        rest
    };
    let direction = if is_code {
        TextDirection::Ltr
    } else {
        detect_paragraph_direction(dir_probe)
    };

    let frame = if is_code {
        egui::Frame::new()
            .fill(theme.code_bg)
            .stroke(Stroke::new(1.5_f32, theme.accent))
            .inner_margin(egui::Margin::same(14))
            .corner_radius(6.0)
    } else if show_markup {
        egui::Frame::new()
            .fill(theme.active_line_bg)
            .stroke(Stroke::new(1.5_f32, theme.accent))
            .inner_margin(egui::Margin::same(8))
            .corner_radius(6.0)
    } else {
        // Pure Microsoft Word-like seamless inline editing (no popup frame or line-range chrome)
        egui::Frame::new()
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE)
            .inner_margin(egui::Margin::symmetric(0, 1))
    };

    let logical_indent = if is_code { 0 } else { indent_level };
    let logical_dir = if is_code {
        TextDirection::Ltr
    } else {
        direction
    };

    let mut frame_result: Option<InlineEditorFrameResult> = None;

    let outer_resp = render_indented_logical(ui, content_w, logical_indent, logical_dir, |ui, avail_w| {
        let editor_w = if is_code {
            (avail_w - 32.0).max(120.0)
        } else if show_markup {
            (avail_w - 20.0).max(120.0)
        } else {
            avail_w.max(120.0)
        };

        frame.show(ui, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(editor_w);
                ui.set_max_width(editor_w);

                let inline_id = egui::Id::new(("wysiwyg_inline_edit", src_lines.start));

                if is_code {
                    let now_time = ui.input(|i| i.time);
                    let recently_copied = state
                        .copied_code_block_until
                        .is_some_and(|(line, until)| {
                            line == src_lines.start && now_time <= until
                        });
                    if recently_copied {
                        ui.ctx().request_repaint_after(Duration::from_millis(200));
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            RichText::new("Language:")
                                .size(11.0)
                                .color(theme.text_muted),
                        );
                        let cb_resp = egui::ComboBox::from_id_salt((
                            "active_code_lang_cb",
                            src_lines.start,
                        ))
                        .selected_text(code_language_display_label(&cur_lang))
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            let cur_key = cur_lang
                                .split_whitespace()
                                .next()
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            for &(lang_id, label) in COMMON_CODE_LANGUAGES {
                                let selected = if lang_id.is_empty() {
                                    cur_lang.trim().is_empty()
                                } else {
                                    cur_key == lang_id
                                };
                                if ui.selectable_label(selected, label).clicked() {
                                    if set_code_block_language(buf, src_lines.start, lang_id) {
                                        state.active_code_lang_draft = lang_id.to_string();
                                        state.active_code_lang_line = Some(src_lines.start);
                                        sync_active_block_from_buf(buf, show_markup, state);
                                        state.focus_inline_editor = true;
                                        ui.ctx().request_repaint();
                                    }
                                    header_interacted = true;
                                }
                            }
                        });
                        if cb_resp.response.clicked() || cb_resp.inner.is_some() {
                            header_interacted = true;
                        }

                        let lang_input_id =
                            ui.id().with(("active_code_lang_input", src_lines.start));
                        let lang_resp = ui.add(
                            egui::TextEdit::singleline(&mut state.active_code_lang_draft)
                                .id(lang_input_id)
                                .desired_width(105.0)
                                .font(FontId::new(11.5, FontFamily::Monospace))
                                .hint_text("custom..."),
                        );
                        if lang_resp.has_focus()
                            || lang_resp.gained_focus()
                            || lang_resp.lost_focus()
                            || lang_resp.clicked()
                        {
                            lang_input_active = true;
                            header_interacted = true;
                        }
                        if lang_resp.has_focus()
                            && ui.input_mut(|i| {
                                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                            })
                        {
                            state.active_code_lang_draft = cur_lang.clone();
                            ui.memory_mut(|m| m.request_focus(inline_id));
                            lang_esc_handled = true;
                        }
                        let commit_enter = lang_resp.has_focus()
                            && ui.input_mut(|i| {
                                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                            });
                        if commit_enter || lang_resp.lost_focus() {
                            let trimmed = state.active_code_lang_draft.trim().to_string();
                            if trimmed != cur_lang.trim() {
                                if set_code_block_language(buf, src_lines.start, &trimmed) {
                                    state.active_code_lang_draft = trimmed;
                                    state.active_code_lang_line = Some(src_lines.start);
                                    sync_active_block_from_buf(buf, show_markup, state);
                                }
                                ui.ctx().request_repaint();
                            } else {
                                state.active_code_lang_draft = trimmed;
                            }
                            if commit_enter {
                                state.focus_inline_editor = true;
                                ui.memory_mut(|m| m.request_focus(inline_id));
                            }
                        }

                        if is_mermaid_code_info(&state.active_code_lang_draft) {
                            let picked = render_fenced_template_picker(
                                ui,
                                "Template ▾",
                                ("active_mermaid_tpl", src_lines.start),
                                "Load a starter Mermaid diagram template",
                                140.0,
                                MERMAID_TEMPLATES,
                                |_ui, body| {
                                    state.active_block_text = body.to_string();
                                    if let Some(new_range) =
                                        replace_fenced_block_body(buf, src_lines, body)
                                    {
                                        state.active_block_lines = Some(new_range);
                                        state.last_synced_version = buf.version;
                                    }
                                    state.focus_inline_editor = true;
                                },
                            );
                            if picked {
                                header_interacted = true;
                            }
                        } else if is_math_code_info(&state.active_code_lang_draft) {
                            let picked = render_fenced_template_picker(
                                ui,
                                "Formula ▾",
                                ("active_math_tpl", src_lines.start),
                                "Load a textbook LaTeX formula template",
                                180.0,
                                MATH_TEXTBOOK_TEMPLATES,
                                |_ui, body| {
                                    state.active_block_text = body.to_string();
                                    if let Some(new_range) =
                                        replace_fenced_block_body(buf, src_lines, body)
                                    {
                                        state.active_block_lines = Some(new_range);
                                        state.last_synced_version = buf.version;
                                    }
                                    state.focus_inline_editor = true;
                                },
                            );
                            if picked {
                                header_interacted = true;
                            }
                            if ui
                                .small_button("= Calculate")
                                .on_hover_text("Evaluate this equation numerically and append = result")
                                .clicked()
                            {
                                let (updated, _) =
                                    evaluate_math_block_text(&state.active_block_text);
                                if updated != state.active_block_text {
                                    state.active_block_text = updated.clone();
                                    if let Some(new_range) =
                                        replace_fenced_block_body(buf, src_lines, &updated)
                                    {
                                        state.active_block_lines = Some(new_range);
                                        state.last_synced_version = buf.version;
                                    }
                                }
                                state.focus_inline_editor = true;
                                header_interacted = true;
                            }
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Done (Esc)").clicked() {
                                done_clicked = true;
                                header_interacted = true;
                            }
                            let copy_lbl = if recently_copied { "✓ Copied" } else { "Copy" };
                            if ui
                                .small_button(copy_lbl)
                                .on_hover_text("Copy code block to clipboard")
                                .clicked()
                            {
                                ui.ctx().copy_text(state.active_block_text.clone());
                                state.copied_code_block_until =
                                    Some((src_lines.start, now_time + 1.5));
                                ui.ctx().request_repaint_after(Duration::from_millis(200));
                                state.focus_inline_editor = true;
                                header_interacted = true;
                            }
                            if delete_block_small_button(
                                ui,
                                theme,
                                "Delete this block (Ctrl+Z to undo)",
                            )
                            .clicked()
                            {
                                if delete_block_lines(buf, src_lines) {
                                    state.active_block_lines = None;
                                    state.active_code_lang_line = None;
                                    state.active_code_lang_draft.clear();
                                    state.last_synced_version = buf.version;
                                    ui.ctx().request_repaint();
                                }
                                header_interacted = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    let (div_rect, _) = ui.allocate_exact_size(
                        Vec2::new(editor_w.max(40.0), 4.0),
                        Sense::hover(),
                    );
                    ui.painter().line_segment(
                        [
                            egui::pos2(div_rect.left(), div_rect.center().y),
                            egui::pos2(div_rect.right(), div_rect.center().y),
                        ],
                        Stroke::new(1.0_f32, theme.border_subtle),
                    );
                    ui.add_space(4.0);
                }

                if show_markup {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Markup: Lines {}–{}",
                                src_lines.start + 1,
                                src_lines.end
                            ))
                            .size(11.0)
                            .color(theme.accent),
                        );
                        if !is_code {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Done (Esc)").clicked() {
                                        done_clicked = true;
                                    }
                                },
                            );
                        }
                    });
                }

                let had_focus_request = state.focus_inline_editor;
                if state.focus_inline_editor {
                    ui.memory_mut(|m| m.request_focus(inline_id));
                    state.focus_inline_editor = false;
                }

                if state.sync_cursor_from_buf {
                    let block_start_char = buf.line_to_char(src_lines.start);
                    let prefix_chars = state.active_block_prefix.chars().count();
                    let text_chars = state.active_block_text.chars().count();
                    let to_local = |c: usize| {
                        doc_to_local_chars(
                            c.saturating_sub(block_start_char + prefix_chars),
                            &state.active_block_text,
                            is_crlf,
                        )
                        .min(text_chars)
                    };
                    let prim = egui::text::CCursor::new(to_local(buf.cursor_char));
                    let sec = egui::text::CCursor::new(to_local(
                        buf.selection_anchor.unwrap_or(buf.cursor_char),
                    ));
                    let cr = egui::text::CCursorRange::two(sec, prim);
                    let mut edit_state =
                        egui::TextEdit::load_state(ui.ctx(), inline_id).unwrap_or_default();
                    edit_state.cursor.set_char_range(Some(cr));
                    egui::TextEdit::store_state(ui.ctx(), inline_id, edit_state);
                    state.last_cursor_range = Some(cr);
                    state.sync_cursor_from_buf = false;
                }

                let has_inline_focus = ui.memory(|m| m.has_focus(inline_id)) && !state.modal_open;
                let was_empty_text = state.active_block_text.trim().is_empty();
                let prev_block_text = state.active_block_text.clone();
                let mut backspace_pressed = false;
                let mut enter_pressed = false;
                let mut arrow_up_pressed = false;
                let mut arrow_down_pressed = false;
                if has_inline_focus && is_code {
                    ui.memory_mut(|m| {
                        m.set_focus_lock_filter(
                            inline_id,
                            egui::EventFilter {
                                tab: true,
                                horizontal_arrows: false,
                                vertical_arrows: false,
                                escape: false,
                            },
                        );
                    });
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)) {
                        done_clicked = true;
                    } else {
                        let shift_tab =
                            ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab));
                        let plain_tab = !shift_tab
                            && ui.input_mut(|i| {
                                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                            });
                        if shift_tab || plain_tab {
                            let cur_cr = egui::TextEdit::load_state(ui.ctx(), inline_id)
                                .and_then(|s| s.cursor.char_range())
                                .or(state.last_cursor_range)
                                .unwrap_or_else(|| {
                                    let end = state.active_block_text.chars().count();
                                    egui::text::CCursorRange::one(egui::text::CCursor::new(end))
                                });
                            let new_cr = apply_code_block_tab(
                                &mut state.active_block_text,
                                cur_cr,
                                shift_tab,
                            );
                            let mut edit_state =
                                egui::TextEdit::load_state(ui.ctx(), inline_id).unwrap_or_default();
                            edit_state.cursor.set_char_range(Some(new_cr));
                            egui::TextEdit::store_state(ui.ctx(), inline_id, edit_state);
                            state.last_cursor_range = Some(new_cr);
                        }
                    }
                } else if has_inline_focus && !is_code {
                    ui.memory_mut(|m| {
                        m.set_focus_lock_filter(
                            inline_id,
                            egui::EventFilter {
                                tab: true,
                                horizontal_arrows: false,
                                vertical_arrows: false,
                                escape: false,
                            },
                        );
                    });

                    if !src_lines.is_empty() && src_lines.end <= buf.len_lines() {
                        if ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab)) {
                            outdent_lines(buf);
                            sync_active_block_from_buf(buf, show_markup, state);
                            state.focus_inline_editor = true;
                        } else if ui
                            .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
                        {
                            indent_lines(buf);
                            sync_active_block_from_buf(buf, show_markup, state);
                            state.focus_inline_editor = true;
                        }

                        if !show_markup {
                            backspace_pressed = ui.input(|i| {
                                i.key_pressed(egui::Key::Backspace)
                                    && !i.modifiers.ctrl
                                    && !i.modifiers.command
                                    && !i.modifiers.alt
                            });
                            enter_pressed = src_lines.len() == 1
                                && ui.input(|i| {
                                    i.key_pressed(egui::Key::Enter)
                                        && !i.modifiers.shift
                                        && !i.modifiers.command
                                });
                            arrow_up_pressed = ui.input(|i| {
                                i.key_pressed(egui::Key::ArrowUp)
                                    && !i.modifiers.shift
                                    && !i.modifiers.ctrl
                                    && !i.modifiers.command
                                    && !i.modifiers.alt
                            });
                            arrow_down_pressed = ui.input(|i| {
                                i.key_pressed(egui::Key::ArrowDown)
                                    && !i.modifiers.shift
                                    && !i.modifiers.ctrl
                                    && !i.modifiers.command
                                    && !i.modifiers.alt
                            });
                        }
                    }
                }

                let code_lang_for_layout = cur_lang.clone();
                let mut layouter =
                    |ui: &egui::Ui, string: &str, wrap_width: f32| -> Arc<egui::Galley> {
                        let job = build_wysiwyg_edit_layout_job(
                            string,
                            wrap_width,
                            kind,
                            alignment,
                            direction,
                            theme,
                            show_markup,
                            &code_lang_for_layout,
                        );
                        ui.fonts(|f| f.layout_job(job))
                    };

                let text_align = if is_code {
                    egui::Align::Min
                } else {
                    resolve_egui_align(alignment, direction)
                };

                let prev_logical_cr = egui::TextEdit::load_state(ui.ctx(), inline_id)
                    .and_then(|s| s.cursor.char_range())
                    .or(state.last_cursor_range);

                let swap_rtl_arrows =
                    has_inline_focus && !is_code && direction == TextDirection::Rtl;
                if swap_rtl_arrows {
                    ui.input_mut(|i| {
                        for ev in &mut i.events {
                            if let egui::Event::Key { key, .. } = ev {
                                if *key == egui::Key::ArrowLeft {
                                    *key = egui::Key::ArrowRight;
                                } else if *key == egui::Key::ArrowRight {
                                    *key = egui::Key::ArrowLeft;
                                }
                            }
                        }
                    });
                }

                if has_inline_focus && !is_code {
                    ui.input_mut(|i| {
                        let shift_held = i.modifiers.shift;
                        let mut has_paste_event = false;
                        let mut has_cmd_shift_v = false;
                        for ev in &mut i.events {
                            match ev {
                                egui::Event::Paste(pasted) => {
                                    has_paste_event = true;
                                    if !shift_held && !pasted.is_empty() {
                                        let html =
                                            crate::document::clipboard::read_os_clipboard_html_matching(
                                                pasted,
                                            );
                                        *pasted =
                                            crate::document::clipboard::smart_paste_markdown(
                                                pasted,
                                                html.as_deref(),
                                            );
                                    }
                                }
                                egui::Event::Key {
                                    key: egui::Key::V,
                                    pressed: true,
                                    modifiers,
                                    ..
                                } if modifiers.command && modifiers.shift => {
                                    has_cmd_shift_v = true;
                                }
                                _ => {}
                            }
                        }
                        if has_cmd_shift_v && !has_paste_event
                            && let Some(plain) =
                                crate::document::clipboard::read_os_clipboard_plain_text()
                                && !plain.is_empty() {
                                    i.events.push(egui::Event::Paste(plain));
                                }
                    });
                }

                let is_empty_block_text = state.active_block_text.is_empty();
                let mut edit = egui::TextEdit::multiline(&mut state.active_block_text)
                    .id(inline_id)
                    .lock_focus(true)
                    .desired_width(editor_w)
                    .desired_rows(if is_code {
                        4
                    } else if buf.is_empty() {
                        12
                    } else {
                        1
                    })
                    .frame(false)
                    .margin(egui::Margin::ZERO)
                    .horizontal_align(text_align)
                    .layouter(&mut layouter);
                if is_code && is_empty_block_text {
                    edit = edit.hint_text(
                        "Type code here... (Tab indents, Ctrl+Enter / Esc finishes)",
                    );
                } else if buf.is_empty() && state.active_block_prefix.is_empty() {
                    edit = edit.hint_text("Start typing...");
                }

                let mut output = ui
                    .scope(|ui| {
                        ui.visuals_mut().text_cursor.stroke = Stroke::NONE;
                        ui.visuals_mut().selection.bg_fill = Color32::TRANSPARENT;
                        edit.show(ui)
                    })
                    .inner;
                if swap_rtl_arrows {
                    ui.input_mut(|i| {
                        for ev in &mut i.events {
                            if let egui::Event::Key { key, .. } = ev {
                                if *key == egui::Key::ArrowLeft {
                                    *key = egui::Key::ArrowRight;
                                } else if *key == egui::Key::ArrowRight {
                                    *key = egui::Key::ArrowLeft;
                                }
                            }
                        }
                    });
                }
                if had_focus_request {
                    output.response.request_focus();
                }
                let text_chars = state.active_block_text.chars().count();
                let bidi = MultilineBidiMap::new(&state.active_block_text);
                let mut did_click_select = false;
                let mut did_mutate_sync = false;

                let pointer_pos_opt = output
                    .response
                    .interact_pointer_pos()
                    .or_else(|| ui.input(|i| i.pointer.interact_pos()));
                let in_text_rect = !ui.ctx().memory(|m| m.any_popup_open())
                    && pointer_pos_opt.is_some_and(|p| output.response.rect.contains(p));
                let is_triple = output.response.triple_clicked()
                    || (in_text_rect
                        && ui.input(|i| i.pointer.button_triple_clicked(egui::PointerButton::Primary)));
                let is_double = output.response.double_clicked()
                    || (in_text_rect
                        && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)));

                if is_triple {
                    let cr = egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(text_chars),
                    );
                    output.state.cursor.set_char_range(Some(cr));
                    output.state.clone().store(ui.ctx(), inline_id);
                    state.last_cursor_range = Some(cr);
                    let block_start = buf.line_to_char(src_lines.start);
                    let prefix_chars = state.active_block_prefix.chars().count();
                    let text_doc_chars =
                        local_to_doc_chars(text_chars, &state.active_block_text, is_crlf);
                    if text_chars > 0 {
                        buf.selection_anchor = Some((block_start + prefix_chars).min(buf.len_chars()));
                        buf.cursor_char =
                            (block_start + prefix_chars + text_doc_chars).min(buf.len_chars());
                        buf.cache_current_selection();
                    } else {
                        buf.set_cursor((block_start + prefix_chars).min(buf.len_chars()), false);
                    }
                    did_click_select = true;
                } else if is_double
                    && let Some(pos) = pointer_pos_opt {
                        let rel = pos - output.galley_pos;
                        let vis_ccursor =
                            output.galley.cursor_from_pos(rel).ccursor.index.min(text_chars);
                        let ccursor_idx =
                            bidi.logical_col_for_visual(&state.active_block_text, vis_ccursor);
                        let row_opt = output
                            .galley
                            .rows
                            .iter()
                            .find(|r| rel.y >= r.rect.top() && rel.y <= r.rect.bottom())
                            .or_else(|| {
                                if rel.y < 0.0 {
                                    output.galley.rows.first()
                                } else {
                                    output.galley.rows.last()
                                }
                            });
                        let in_row_text = row_opt.is_some_and(|r| {
                            !r.glyphs.is_empty()
                                && pos.x >= output.galley_pos.x + r.rect.min.x - 2.0
                                && pos.x <= output.galley_pos.x + r.rect.max.x + 4.0
                        });
                        let (s, e) = if in_row_text {
                            if let Some(w_range) = wysiwyg_word_at_col(
                                &state.active_block_text,
                                ccursor_idx,
                                show_markup,
                            ) {
                                (w_range.start, w_range.end)
                            } else {
                                let lr = line_bounds_at_col(&state.active_block_text, ccursor_idx);
                                (lr.start, lr.end)
                            }
                        } else {
                            let lr = line_bounds_at_col(&state.active_block_text, ccursor_idx);
                            (lr.start, lr.end)
                        };
                        let cr = egui::text::CCursorRange::two(
                            egui::text::CCursor::new(s),
                            egui::text::CCursor::new(e),
                        );
                        output.state.cursor.set_char_range(Some(cr));
                        output.state.clone().store(ui.ctx(), inline_id);
                        state.last_cursor_range = Some(cr);
                        let block_start = buf.line_to_char(src_lines.start);
                        let prefix_chars = state.active_block_prefix.chars().count();
                        let s_doc = local_to_doc_chars(s, &state.active_block_text, is_crlf);
                        let e_doc = local_to_doc_chars(e, &state.active_block_text, is_crlf);
                        if e > s {
                            buf.selection_anchor =
                                Some((block_start + prefix_chars + s_doc).min(buf.len_chars()));
                            buf.cursor_char =
                                (block_start + prefix_chars + e_doc).min(buf.len_chars());
                            buf.cache_current_selection();
                        } else {
                            buf.set_cursor(
                                (block_start + prefix_chars + s_doc).min(buf.len_chars()),
                                false,
                            );
                        }
                        did_click_select = true;
                    }

                let text_mutated = state.active_block_text != prev_block_text;
                if !did_click_select && !text_mutated && !bidi.is_identity {
                    let pointer_pressed = ui.input(|i| i.pointer.any_pressed());
                    let is_drag = output.response.dragged() || output.response.drag_started();
                    if ((output.response.hovered() && pointer_pressed) || is_drag)
                        && let Some(pos) = pointer_pos_opt {
                            let rel = pos - output.galley_pos;
                            let vis_idx = output
                                .galley
                                .cursor_from_pos(rel)
                                .ccursor
                                .index
                                .min(text_chars);
                            let log_idx =
                                bidi.logical_col_for_visual(&state.active_block_text, vis_idx);
                            let shift = ui.input(|i| i.modifiers.shift);
                            let sec = if (is_drag && !pointer_pressed) || shift {
                                prev_logical_cr
                                    .map(|cr| cr.secondary.index.min(text_chars))
                                    .unwrap_or(log_idx)
                            } else {
                                log_idx
                            };
                            let cr = egui::text::CCursorRange::two(
                                egui::text::CCursor::new(sec),
                                egui::text::CCursor::new(log_idx),
                            );
                            output.state.cursor.set_char_range(Some(cr));
                            output.state.clone().store(ui.ctx(), inline_id);
                            state.last_cursor_range = Some(cr);
                        }
                }

                if text_mutated && !src_lines.is_empty() && src_lines.end <= buf.len_lines() {
                    let le = buf.line_ending();
                    if enter_pressed {
                        if let Some(next_marker) =
                            next_list_continuation_marker(&state.active_block_prefix)
                        {
                            let (_, _, struct_prefix) =
                                parse_line_block_meta(&state.active_block_prefix);
                            let meta_len = state.active_block_prefix.len() - struct_prefix.len();
                            let range = buf.line_range_chars(src_lines.start, src_lines.end - 1);
                            if was_empty_text {
                                let meta_only = state.active_block_prefix[..meta_len].to_string();
                                buf.replace_range(range, &meta_only);
                                let c =
                                    buf.line_to_char(src_lines.start) + meta_only.chars().count();
                                buf.set_cursor(c, false);
                                sync_active_block_from_buf(buf, show_markup, state);
                                state.focus_inline_editor = true;
                                did_mutate_sync = true;
                            } else if state.active_block_text.contains('\n') {
                                let mut replaced_body = state
                                    .active_block_text
                                    .replacen('\n', &format!("\n{next_marker}"), 1);
                                if is_crlf {
                                    replaced_body = replaced_body.replace('\n', "\r\n");
                                }
                                let full_text = format!(
                                    "{}{}{}",
                                    state.active_block_prefix,
                                    replaced_body,
                                    state.active_block_suffix
                                );
                                buf.replace_range(range, &full_text);
                                let next_line = src_lines.start + 1;
                                let c =
                                    buf.line_to_char(next_line) + next_marker.chars().count();
                                buf.set_cursor(c.min(buf.len_chars()), false);
                                state.active_block_lines = Some(next_line..(next_line + 1));
                                sync_active_block_from_buf(buf, show_markup, state);
                                state.focus_inline_editor = true;
                                did_mutate_sync = true;
                            }
                        } else {
                            let (before, after) = state
                                .active_block_text
                                .split_once('\n')
                                .unwrap_or((state.active_block_text.as_str(), ""));
                            let after_buf = if is_crlf {
                                std::borrow::Cow::Owned(after.replace('\n', "\r\n"))
                            } else {
                                std::borrow::Cow::Borrowed(after)
                            };
                            let full_text = format!(
                                "{}{}{le}{}{}",
                                state.active_block_prefix,
                                before,
                                after_buf,
                                state.active_block_suffix
                            );
                            let range = buf.line_range_chars(src_lines.start, src_lines.end - 1);
                            buf.replace_range(range, &full_text);
                            let next_line = src_lines.start + 1;
                            let next_cursor = buf.line_to_char(next_line);
                            buf.set_cursor(next_cursor.min(buf.len_chars()), false);
                            state.active_block_lines = Some(next_line..(next_line + 1));
                            state.active_block_kind = Some(ActiveBlockKind::Paragraph);
                            sync_active_block_from_buf(buf, show_markup, state);
                            state.focus_inline_editor = true;
                            did_mutate_sync = true;
                        }
                    } else {
                        let cursor_local = output
                            .state
                            .cursor
                            .char_range()
                            .map(|cr| cr.primary.index)
                            .unwrap_or(state.active_block_text.chars().count());
                        let doc_cursor_rel =
                            local_to_doc_chars(cursor_local, &state.active_block_text, is_crlf);
                        let mut next_cursor_char = buf.line_to_char(src_lines.start)
                            + state.active_block_prefix.chars().count()
                            + doc_cursor_rel;
                        let body_for_buf: std::borrow::Cow<'_, str> = if is_crlf {
                            std::borrow::Cow::Owned(state.active_block_text.replace('\n', "\r\n"))
                        } else {
                            std::borrow::Cow::Borrowed(state.active_block_text.as_str())
                        };
                        let full_text = format!(
                            "{}{}{}",
                            state.active_block_prefix,
                            body_for_buf,
                            state.active_block_suffix
                        );
                        let old_lines = src_lines.end - src_lines.start;
                        let range = buf.line_range_chars(src_lines.start, src_lines.end - 1);
                        let new_line_count = full_text.split('\n').count().max(1);
                        buf.replace_range(range, &full_text);
                        next_cursor_char = next_cursor_char.min(buf.len_chars());
                        buf.set_cursor(next_cursor_char, false);

                        if !show_markup && !is_code {
                            let cursor_line = buf.char_to_line(next_cursor_char);
                            if new_line_count != old_lines {
                                state.active_block_lines = Some(cursor_line..(cursor_line + 1));
                                state.active_block_kind = Some(ActiveBlockKind::Paragraph);
                                sync_active_block_from_buf(buf, show_markup, state);
                                state.focus_inline_editor = true;
                                did_mutate_sync = true;
                            } else {
                                let inferred_kind =
                                    if let Some(lvl) = infer_heading_level_from_raw(&full_text) {
                                        ActiveBlockKind::Heading(lvl)
                                    } else {
                                        ActiveBlockKind::Paragraph
                                    };
                                let (new_prefix, _, _) =
                                    split_wysiwyg_block_text(&full_text, inferred_kind, show_markup);
                                if new_prefix != state.active_block_prefix
                                    || inferred_kind != kind
                                {
                                    state.active_block_kind = Some(inferred_kind);
                                    sync_active_block_from_buf(buf, show_markup, state);
                                    state.focus_inline_editor = true;
                                    did_mutate_sync = true;
                                } else {
                                    state.last_synced_version = buf.version;
                                }
                            }
                        } else {
                            if new_line_count != old_lines {
                                state.active_block_lines =
                                    Some(src_lines.start..(src_lines.start + new_line_count));
                            }
                            state.last_synced_version = buf.version;
                        }
                    }
                    ui.ctx().request_repaint();
                } else if backspace_pressed
                    && !text_mutated
                    && !src_lines.is_empty()
                    && src_lines.end <= buf.len_lines()
                    && output
                        .state
                        .cursor
                        .char_range()
                        .is_some_and(|cr| cr.primary.index == 0 && cr.secondary.index == 0)
                {
                    let (_, _, struct_prefix) =
                        parse_line_block_meta(&state.active_block_prefix);
                    if !struct_prefix.is_empty() {
                        let meta_len = state.active_block_prefix.len() - struct_prefix.len();
                        let meta_only = state.active_block_prefix[..meta_len].to_string();
                        let block_start = buf.line_to_char(src_lines.start);
                        let old_prefix_chars = state.active_block_prefix.chars().count();
                        buf.replace_range(
                            block_start..(block_start + old_prefix_chars),
                            &meta_only,
                        );
                        buf.set_cursor(block_start + meta_only.chars().count(), false);
                        sync_active_block_from_buf(buf, show_markup, state);
                        state.focus_inline_editor = true;
                        did_mutate_sync = true;
                        ui.ctx().request_repaint();
                    } else if !is_code && src_lines.start > 0 {
                        let prev_line = src_lines.start - 1;
                        let prev_end =
                            buf.line_to_char(prev_line) + buf.line_len_without_newline(prev_line);
                        let cur_start = buf.line_to_char(src_lines.start);
                        if cur_start > prev_end {
                            buf.remove(prev_end..cur_start);
                            buf.set_cursor(prev_end, false);
                            let merged_raw = buf.line_text(prev_line);
                            let next_kind = infer_heading_level_from_raw(&merged_raw)
                                .map(ActiveBlockKind::Heading)
                                .unwrap_or(ActiveBlockKind::Paragraph);
                            state.active_block_lines = Some(prev_line..(prev_line + 1));
                            state.active_block_kind = Some(next_kind);
                            sync_active_block_from_buf(buf, show_markup, state);
                            state.focus_inline_editor = true;
                            did_mutate_sync = true;
                            ui.ctx().request_repaint();
                        }
                    }
                } else if (arrow_up_pressed || arrow_down_pressed)
                    && !text_mutated
                    && !src_lines.is_empty()
                    && src_lines.end <= buf.len_lines()
                {
                    let prev_row = prev_logical_cr.map(|cr| {
                        let vis = bidi.visual_col_for_logical(cr.primary.index);
                        output
                            .galley
                            .from_ccursor(egui::text::CCursor::new(vis))
                            .rcursor
                            .row
                    });
                    if arrow_up_pressed
                        && prev_row.is_some_and(|r| r == 0)
                        && src_lines.start > 0
                    {
                        let target_line = src_lines.start - 1;
                        let target_end = buf.line_to_char(target_line)
                            + buf.line_len_without_newline(target_line);
                        buf.set_cursor(target_end, false);
                        let raw_t = buf.line_text(target_line);
                        let next_kind = infer_heading_level_from_raw(&raw_t)
                            .map(ActiveBlockKind::Heading)
                            .unwrap_or(ActiveBlockKind::Paragraph);
                        state.active_block_lines = Some(target_line..(target_line + 1));
                        state.active_block_kind = Some(next_kind);
                        sync_active_block_from_buf(buf, show_markup, state);
                        state.focus_inline_editor = true;
                        did_mutate_sync = true;
                        ui.ctx().request_repaint();
                    } else if arrow_down_pressed
                        && prev_row.is_some_and(|r| r + 1 >= output.galley.rows.len().max(1))
                        && src_lines.end < buf.len_lines()
                    {
                        let target_line = src_lines.end;
                        let target_start = buf.line_to_char(target_line);
                        buf.set_cursor(target_start, false);
                        let raw_t = buf.line_text(target_line);
                        let next_kind = infer_heading_level_from_raw(&raw_t)
                            .map(ActiveBlockKind::Heading)
                            .unwrap_or(ActiveBlockKind::Paragraph);
                        state.active_block_lines = Some(target_line..(target_line + 1));
                        state.active_block_kind = Some(next_kind);
                        sync_active_block_from_buf(buf, show_markup, state);
                        state.focus_inline_editor = true;
                        did_mutate_sync = true;
                        ui.ctx().request_repaint();
                    }
                }

                if !did_mutate_sync && !did_click_select && !state.sync_cursor_from_buf
                    && let Some(cr) = output.state.cursor.char_range()
                        && output.response.has_focus() {
                            state.last_cursor_range = Some(cr);
                            let block_start_char = buf.line_to_char(src_lines.start);
                            let prefix_chars = state.active_block_prefix.chars().count();
                            let p_doc = local_to_doc_chars(
                                cr.primary.index,
                                &state.active_block_text,
                                is_crlf,
                            );
                            let s_doc = local_to_doc_chars(
                                cr.secondary.index,
                                &state.active_block_text,
                                is_crlf,
                            );
                            let p = (block_start_char + prefix_chars + p_doc)
                                .min(buf.len_chars());
                            let s = (block_start_char + prefix_chars + s_doc)
                                .min(buf.len_chars());
                            if p == s {
                                buf.selection_anchor = None;
                                buf.cached_selection = None;
                                buf.cursor_char = p;
                            } else {
                                buf.selection_anchor = Some(s);
                                buf.cursor_char = p;
                                buf.cache_current_selection();
                            }
                        }

                let clip = ui.painter_at(output.text_clip_rect.expand(2.0));
                if !state.search_matches.is_empty() && !src_lines.is_empty() && src_lines.end <= buf.len_lines()
                {
                    let block_start_char = buf.line_to_char(src_lines.start);
                    let prefix_chars = state.active_block_prefix.chars().count();
                    let text_start_doc = block_start_char + prefix_chars;
                    let text_end_doc =
                        text_start_doc + local_to_doc_chars(text_chars, &state.active_block_text, is_crlf);
                    let match_bg = Color32::from_rgba_unmultiplied(245, 185, 35, 82);
                    let active_match_bg = Color32::from_rgba_unmultiplied(255, 150, 20, 128);
                    let active_match_stroke = Stroke::new(1.5_f32, theme.warning_amber);

                    let first_m = state
                        .search_matches
                        .partition_point(|m| m.end <= text_start_doc);
                    for m in &state.search_matches[first_m..] {
                        if m.start >= text_end_doc {
                            break;
                        }
                        if m.end > text_start_doc && m.start < text_end_doc {
                            let ls_doc = m.start.saturating_sub(text_start_doc);
                            let le_doc = m.end.saturating_sub(text_start_doc);
                            let ls = doc_to_local_chars(ls_doc, &state.active_block_text, is_crlf)
                                .min(text_chars);
                            let le = doc_to_local_chars(le_doc, &state.active_block_text, is_crlf)
                                .min(text_chars);
                            let is_active_m = state.active_search_match.as_ref() == Some(m);
                            for vr in bidi.visual_ranges_for_logical_span(ls, le) {
                                let c_min = output
                                    .galley
                                    .from_ccursor(egui::text::CCursor::new(vr.start))
                                    .rcursor;
                                let c_max = output
                                    .galley
                                    .from_ccursor(egui::text::CCursor::new(vr.end))
                                    .rcursor;
                                for ri in c_min.row..=c_max.row {
                                    if let Some(row) = output.galley.rows.get(ri) {
                                        let x0 = if ri == c_min.row {
                                            row.x_offset(c_min.column)
                                        } else {
                                            row.rect.left()
                                        };
                                        let x1 = if ri == c_max.row {
                                            row.x_offset(c_max.column)
                                        } else {
                                            row.rect.right()
                                        };
                                        let lo = x0.min(x1);
                                        let hi = x0.max(x1);
                                        if hi > lo {
                                            let hi_rect = egui::Rect::from_min_max(
                                                output.galley_pos + Vec2::new(lo, row.min_y()),
                                                output.galley_pos + Vec2::new(hi, row.max_y()),
                                            );
                                            clip.rect_filled(
                                                hi_rect,
                                                2.0,
                                                if is_active_m {
                                                    active_match_bg
                                                } else {
                                                    match_bg
                                                },
                                            );
                                            if is_active_m {
                                                clip.rect_stroke(
                                                    hi_rect,
                                                    2.0,
                                                    active_match_stroke,
                                                    egui::StrokeKind::Inside,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                let active_cr = output.state.cursor.char_range().unwrap_or_else(|| {
                    egui::text::CCursorRange::one(egui::text::CCursor::new(text_chars))
                });
                let p_log = active_cr.primary.index.min(text_chars);
                let s_log = active_cr.secondary.index.min(text_chars);
                let (sel_min, sel_max) = if s_log <= p_log {
                    (s_log, p_log)
                } else {
                    (p_log, s_log)
                };
                let show_unfocused_sel =
                    !output.response.has_focus() && !src_lines.is_empty() && buf.selection_range().is_some();
                if sel_min < sel_max && (output.response.has_focus() || show_unfocused_sel) {
                    for vr in bidi.visual_ranges_for_logical_span(sel_min, sel_max) {
                        let c_min = output
                            .galley
                            .from_ccursor(egui::text::CCursor::new(vr.start))
                            .rcursor;
                        let c_max = output
                            .galley
                            .from_ccursor(egui::text::CCursor::new(vr.end))
                            .rcursor;
                        for ri in c_min.row..=c_max.row {
                            if let Some(row) = output.galley.rows.get(ri) {
                                let x0 = if ri == c_min.row {
                                    row.x_offset(c_min.column)
                                } else {
                                    row.rect.left()
                                };
                                let x1 = if ri == c_max.row {
                                    row.x_offset(c_max.column)
                                } else {
                                    row.rect.right()
                                };
                                let lo = x0.min(x1);
                                let hi = x0.max(x1);
                                if hi > lo {
                                    let sel_rect = egui::Rect::from_min_max(
                                        output.galley_pos + Vec2::new(lo, row.min_y()),
                                        output.galley_pos + Vec2::new(hi, row.max_y()),
                                    );
                                    clip.rect_filled(sel_rect, 2.0, theme.selection_bg);
                                }
                            }
                        }
                    }
                }

                if output.response.has_focus() {
                    let vis_caret = bidi.visual_col_for_logical(p_log);
                    let caret_rect = output
                        .galley
                        .pos_from_ccursor(egui::text::CCursor::new(vis_caret))
                        .translate(output.galley_pos.to_vec2());
                    let cx = caret_rect.left().clamp(
                        output.text_clip_rect.left() - 1.0,
                        output.text_clip_rect.right() + 1.0,
                    );
                    clip.line_segment(
                        [
                            egui::pos2(cx, caret_rect.top()),
                            egui::pos2(cx, caret_rect.bottom().max(caret_rect.top() + 14.0)),
                        ],
                        Stroke::new(2.0_f32, theme.accent),
                    );
                }

                if !is_code && !state.autoscroll_active {
                    let cmd_down = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                    let hover_pos =
                        ui.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()));
                    let hovered_nav = if output.response.hovered() {
                        hover_pos.and_then(|pos| {
                            let rel = pos - output.galley_pos;
                            let row = output.galley.rows.iter().find(|r| {
                                rel.y >= r.rect.top() - 1.0 && rel.y <= r.rect.bottom() + 1.0
                            })?;
                            if row.glyphs.is_empty()
                                || pos.x < output.galley_pos.x + row.rect.min.x - 1.0
                                || pos.x > output.galley_pos.x + row.rect.max.x + 1.0
                            {
                                return None;
                            }
                            let vis_col = output
                                .galley
                                .cursor_from_pos(rel)
                                .ccursor
                                .index
                                .min(text_chars);
                            let log_idx =
                                bidi.logical_col_for_visual(&state.active_block_text, vis_col);
                            let raw_url =
                                extract_markdown_link_at_char(&state.active_block_text, log_idx)?;
                            let target = classify_link_url(&raw_url)?;
                            Some(NavTarget::Link(raw_url, target))
                        })
                    } else {
                        None
                    };
                    if let Some(ref nav) = hovered_nav {
                        let tip = nav_target_tooltip(nav, false);
                        show_link_hover_tooltip(ui, inline_id.with("active_link_tip"), &tip);
                        if cmd_down {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            if output.response.clicked()
                                || ui.input(|i| {
                                    i.pointer.button_clicked(egui::PointerButton::Primary)
                                })
                            {
                                action = nav_target_action(nav);
                            }
                        }
                    }
                }

                frame_result = Some(InlineEditorFrameResult {
                    text_resp: output.response,
                    inline_id,
                    galley: output.galley,
                    had_focus_request,
                    did_click_select,
                });

                if is_code {
                    let active_is_mermaid = is_mermaid_code_info(&state.active_code_lang_draft);
                    let active_is_math = is_math_code_info(&state.active_code_lang_draft);
                    if active_is_mermaid || active_is_math {
                        ui.add_space(6.0);
                        let (p_div, _) = ui.allocate_exact_size(
                            Vec2::new(editor_w.max(40.0), 4.0),
                            Sense::hover(),
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(p_div.left(), p_div.center().y),
                                egui::pos2(p_div.right(), p_div.center().y),
                            ],
                            Stroke::new(1.0_f32, theme.border_subtle),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(if active_is_mermaid {
                                "Live Diagram Preview"
                            } else {
                                "Live Equation Preview"
                            })
                            .size(10.5)
                            .strong()
                            .color(theme.text_muted),
                        );
                        ui.add_space(4.0);
                        if active_is_mermaid {
                            render_mermaid_in_ui(ui, &state.active_block_text, editor_w, theme);
                        } else {
                            let expr = extract_math_preview_source(&state.active_block_text);
                            egui::ScrollArea::horizontal()
                                .id_salt(("active_math_preview", src_lines.start))
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.with_layout(
                                        egui::Layout::top_down(egui::Align::Center),
                                        |ui| {
                                            ui.set_min_width(editor_w.max(120.0));
                                            render_math_in_ui(
                                                ui,
                                                expr,
                                                17.5,
                                                true,
                                                theme.text_primary,
                                                theme,
                                            );
                                        },
                                    );
                                });
                            if state.auto_calc_math {
                                let preview_opt = {
                                    let expr = state.active_block_text.clone();
                                    state.cached_math_preview(&expr)
                                };
                                if let Some(res_str) = preview_opt
                                {
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        render_calc_preview_pill(ui, theme, &res_str);
                                        if ui
                                            .small_button("Insert")
                                            .on_hover_text(
                                                "Append calculated result into the equation",
                                            )
                                            .clicked()
                                        {
                                            let (updated, _) = evaluate_math_block_text(
                                                &state.active_block_text,
                                            );
                                            if updated != state.active_block_text {
                                                state.active_block_text = updated.clone();
                                                if let Some(new_range) = replace_fenced_block_body(
                                                    buf,
                                                    src_lines,
                                                    &updated,
                                                ) {
                                                    state.active_block_lines = Some(new_range);
                                                    state.last_synced_version = buf.version;
                                                }
                                            }
                                            state.focus_inline_editor = true;
                                        }
                                    });
                                }
                            }
                        }
                    }
                }
            });
        });
    });

    let Some(res) = frame_result else {
        return action;
    };

    if !res.did_click_select {
        let margin_triple = ui.input(|i| i.pointer.button_triple_clicked(egui::PointerButton::Primary));
        let margin_double = ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
        if (margin_triple || margin_double)
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
                && outer_resp.rect.contains(pos) && !res.text_resp.rect.contains(pos) {
                    let text_chars = state.active_block_text.chars().count();
                    let (s, e) = if margin_triple {
                        (0, text_chars)
                    } else {
                        let rel_y = pos.y - res.text_resp.rect.top();
                        let row_col = res
                            .galley
                            .cursor_from_pos(Vec2::new(0.0, rel_y.max(0.0)))
                            .ccursor
                            .index
                            .min(text_chars);
                        let lr = line_bounds_at_col(&state.active_block_text, row_col);
                        (lr.start, lr.end)
                    };
                    let cr = egui::text::CCursorRange::two(
                        egui::text::CCursor::new(s),
                        egui::text::CCursor::new(e),
                    );
                    let mut edit_state =
                        egui::TextEdit::load_state(ui.ctx(), res.inline_id).unwrap_or_default();
                    edit_state.cursor.set_char_range(Some(cr));
                    egui::TextEdit::store_state(ui.ctx(), res.inline_id, edit_state);
                    ui.memory_mut(|m| m.request_focus(res.inline_id));
                    state.last_cursor_range = Some(cr);
                    let block_start = buf.line_to_char(src_lines.start);
                    let prefix_chars = state.active_block_prefix.chars().count();
                    let s_doc = local_to_doc_chars(s, &state.active_block_text, is_crlf);
                    let e_doc = local_to_doc_chars(e, &state.active_block_text, is_crlf);
                    if e > s {
                        buf.selection_anchor =
                            Some((block_start + prefix_chars + s_doc).min(buf.len_chars()));
                        buf.cursor_char =
                            (block_start + prefix_chars + e_doc).min(buf.len_chars());
                        buf.cache_current_selection();
                    } else {
                        buf.set_cursor(
                            (block_start + prefix_chars + s_doc).min(buf.len_chars()),
                            false,
                        );
                    }
                }
    }

    if !show_markup && !is_code {
        // Subtle Microsoft Word-like logical-start cursor margin indicator
        let r = res.text_resp.rect;
        let x0 = if direction == TextDirection::Rtl {
            r.right() + 4.0
        } else {
            r.left() - 6.0
        };
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x0, r.top() + 2.0),
                egui::pos2(x0 + 2.0, r.bottom() - 2.0),
            ),
            1.0,
            theme.accent_soft,
        );
    }

    let popup_open = ui.ctx().memory(|m| m.any_popup_open());
    let clicked_in_workspace = if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
        ui.input(|i| i.pointer.any_pressed())
            && !outer_resp.rect.expand(4.0).contains(pos)
            && pos.y >= workspace_top
    } else {
        false
    };

    if done_clicked
        || (!state.modal_open
            && !lang_esc_handled
            && !was_any_popup_open
            && !popup_open
            && ui.input(|i| i.key_pressed(egui::Key::Escape)))
        || (res.text_resp.lost_focus()
            && !res.had_focus_request
            && !was_any_popup_open
            && !popup_open
            && !header_interacted
            && !lang_input_active
            && clicked_in_workspace)
    {
        state.active_block_lines = None;
        state.active_code_lang_line = None;
        state.active_code_lang_draft.clear();
    }
    action
}

#[allow(clippy::too_many_arguments)]
fn handle_inactive_wysiwyg_click(
    ui: &egui::Ui,
    buf: &mut DocumentBuffer,
    rect: egui::Rect,
    id_tag: &str,
    tip_tag: &str,
    source_lines: &Range<usize>,
    kind: ActiveBlockKind,
    content_w: f32,
    theme: &WordTheme,
    show_markup: bool,
    hitboxes: &[RenderedGalleyHitbox],
    state: &mut VirtualReaderState,
) -> ReaderAction {
    let mut action = ReaderAction::None;
    let click = ui.interact(
        rect,
        ui.id().with((id_tag, source_lines.start, source_lines.end)),
        Sense::click(),
    );
    let cmd_down = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
    let pointer_pos = ui.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()));
    let hovered_link = pointer_pos.and_then(|pos| hit_test_block_links(hitboxes, pos));

    if click.hovered() {
        if let Some(target) = hovered_link {
            let tip = nav_target_tooltip(target, false);
            show_link_hover_tooltip(
                ui,
                ui.id().with((tip_tag, source_lines.start, source_lines.end)),
                &tip,
            );
            if cmd_down {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            } else {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
            }
        } else {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
    }

    let click_link = click
        .interact_pointer_pos()
        .or(pointer_pos)
        .and_then(|pos| hit_test_block_links(hitboxes, pos));
    if click.clicked() && cmd_down {
        if let Some(target) = click_link {
            action = nav_target_action(target);
        }
    } else if !(cmd_down && click_link.is_some()) {
        let click_count = if click.triple_clicked() {
            3
        } else if click.double_clicked() {
            2
        } else if click.clicked() {
            1
        } else {
            0
        };
        if click_count > 0 {
            activate_wysiwyg_block(
                Some(ui),
                buf,
                source_lines,
                kind,
                content_w,
                theme,
                show_markup,
                Some(rect),
                click.interact_pointer_pos(),
                click_count,
                state,
            );
        }
    }
    action
}

#[allow(clippy::too_many_arguments)]
fn render_doc_block(
    ui: &mut egui::Ui,
    buf: &mut DocumentBuffer,
    block: &DocBlock,
    content_w: f32,
    theme: &WordTheme,
    wysiwyg: bool,
    show_markup: bool,
    read_only: bool,
    workspace_top: f32,
    state: &mut VirtualReaderState,
    list_depth: usize,
    rendered_active: &mut bool,
) -> ReaderAction {
    let mut action = ReaderAction::None;
    let label_selectable = !wysiwyg || read_only;

    match block {
        DocBlock::Heading {
            level,
            runs,
            alignment,
            indent_level,
            direction,
            source_lines,
        } => {
            let kind = ActiveBlockKind::Heading(*level);
            let is_active =
                wysiwyg && !read_only && state.active_block_lines.as_ref() == Some(source_lines);
            if is_active {
                *rendered_active = true;
                let a = render_inline_wysiwyg_block(
                    ui,
                    buf,
                    source_lines,
                    kind,
                    content_w,
                    theme,
                    show_markup,
                    workspace_top,
                    state,
                );
                if a != ReaderAction::None {
                    action = a;
                }
            } else {
                let size = heading_font_size(*level);
                let prefix_m = if wysiwyg && show_markup {
                    Some(format!("{} ", "#".repeat((*level).clamp(1, 6) as usize)))
                } else {
                    None
                };
                let search_needles = build_block_search_needles(buf, state, source_lines);
                let mut runs_res = RunsRenderResult {
                    outcome: RunsClickOutcome::None,
                    hitboxes: Vec::new(),
                    handled_ui: false,
                    interactive_rects: Vec::new(),
                };
                let whole_word = state.search_whole_word;
                let base_dir_owned = state.base_dir.clone();
                let suppress_clicks = state.autoscroll_active;
                let galley_cache = &mut state.galley_cache;
                let image_label_editor = &mut state.image_label_editor;
                let image_lightbox = &mut state.image_lightbox;
                let resp =
                    render_indented_logical(ui, content_w, *indent_level, *direction, |ui, iw| {
                        runs_res = render_runs_with_images(
                            ui,
                            buf,
                            runs,
                            source_lines,
                            size,
                            1.28,
                            true,
                            theme.heading_color,
                            iw,
                            *alignment,
                            *direction,
                            theme,
                            wysiwyg && show_markup,
                            prefix_m,
                            label_selectable,
                            read_only,
                            suppress_clicks,
                            base_dir_owned.as_deref(),
                            &search_needles,
                            whole_word,
                            galley_cache,
                            image_label_editor,
                            image_lightbox,
                        );
                    });
                let handled_nav = runs_res.outcome != RunsClickOutcome::None || runs_res.handled_ui;
                if runs_res.handled_ui && state.image_label_editor.is_some() {
                    state.active_block_lines = None;
                }
                if let RunsClickOutcome::Action(a) = runs_res.outcome {
                    action = a;
                }
                if wysiwyg && !read_only && !handled_nav && !state.autoscroll_active
                    && let Some(click_rect) =
                        clip_inactive_click_rect(resp.rect, &runs_res.interactive_rects)
                    {
                        let a = handle_inactive_wysiwyg_click(
                            ui,
                            buf,
                            click_rect,
                            "wys_h",
                            "wys_h_tip",
                            source_lines,
                            kind,
                            content_w,
                            theme,
                            show_markup,
                            &runs_res.hitboxes,
                            state,
                        );
                        if a != ReaderAction::None {
                            action = a;
                        }
                    }
            }
            if *level <= 2 {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(content_w, 6.0), Sense::hover());
                ui.painter().line_segment(
                    [
                        egui::pos2(rect.left(), rect.center().y),
                        egui::pos2(rect.right(), rect.center().y),
                    ],
                    Stroke::new(1.0_f32, theme.border_subtle),
                );
            }
        }
        DocBlock::Paragraph {
            runs,
            alignment,
            indent_level,
            direction,
            source_lines,
        } => {
            let kind = ActiveBlockKind::Paragraph;
            let is_active =
                wysiwyg && !read_only && state.active_block_lines.as_ref() == Some(source_lines);
            if is_active {
                *rendered_active = true;
                let a = render_inline_wysiwyg_block(
                    ui,
                    buf,
                    source_lines,
                    kind,
                    content_w,
                    theme,
                    show_markup,
                    workspace_top,
                    state,
                );
                if a != ReaderAction::None {
                    action = a;
                }
            } else {
                let search_needles = build_block_search_needles(buf, state, source_lines);
                let mut runs_res = RunsRenderResult {
                    outcome: RunsClickOutcome::None,
                    hitboxes: Vec::new(),
                    handled_ui: false,
                    interactive_rects: Vec::new(),
                };
                let whole_word = state.search_whole_word;
                let base_dir_owned = state.base_dir.clone();
                let suppress_clicks = state.autoscroll_active;
                let body_line_mult = state.line_spacing.body_multiplier();
                let galley_cache = &mut state.galley_cache;
                let image_label_editor = &mut state.image_label_editor;
                let image_lightbox = &mut state.image_lightbox;
                let resp =
                    render_indented_logical(ui, content_w, *indent_level, *direction, |ui, iw| {
                        runs_res = render_runs_with_images(
                            ui,
                            buf,
                            runs,
                            source_lines,
                            15.5,
                            body_line_mult,
                            false,
                            theme.text_primary,
                            iw,
                            *alignment,
                            *direction,
                            theme,
                            wysiwyg && show_markup,
                            None,
                            label_selectable,
                            read_only,
                            suppress_clicks,
                            base_dir_owned.as_deref(),
                            &search_needles,
                            whole_word,
                            galley_cache,
                            image_label_editor,
                            image_lightbox,
                        );
                    });
                let handled_nav = runs_res.outcome != RunsClickOutcome::None || runs_res.handled_ui;
                if runs_res.handled_ui && state.image_label_editor.is_some() {
                    state.active_block_lines = None;
                }
                if let RunsClickOutcome::Action(a) = runs_res.outcome {
                    action = a;
                }
                if wysiwyg && !read_only && !handled_nav && !state.autoscroll_active
                    && let Some(click_rect) =
                        clip_inactive_click_rect(resp.rect, &runs_res.interactive_rects)
                    {
                        let a = handle_inactive_wysiwyg_click(
                            ui,
                            buf,
                            click_rect,
                            "wys_p",
                            "wys_p_tip",
                            source_lines,
                            kind,
                            content_w,
                            theme,
                            show_markup,
                            &runs_res.hitboxes,
                            state,
                        );
                        if a != ReaderAction::None {
                            action = a;
                        }
                    }
            }
        }
        DocBlock::BlockQuote {
            blocks,
            indent_level,
            direction,
            ..
        } => {
            let is_rtl = *direction == TextDirection::Rtl;
            render_indented_logical(ui, content_w, *indent_level, *direction, |ui, iw| {
                egui::Frame::new()
                    .fill(theme.quote_bg)
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin {
                        left: if is_rtl { 12 } else { 16 },
                        right: if is_rtl { 16 } else { 12 },
                        top: 8,
                        bottom: 8,
                    })
                    .show(ui, |ui| {
                        let resp = ui.vertical(|ui| {
                            for inner in blocks {
                                let a = render_doc_block(
                                    ui,
                                    buf,
                                    inner,
                                    (iw - 32.0).max(120.0),
                                    theme,
                                    wysiwyg,
                                    show_markup,
                                    read_only,
                                    workspace_top,
                                    state,
                                    list_depth,
                                    rendered_active,
                                );
                                if a != ReaderAction::None {
                                    action = a;
                                }
                            }
                        });
                        let bar_rect = if is_rtl {
                            egui::Rect::from_min_max(
                                egui::pos2(
                                    resp.response.rect.right() + 8.5,
                                    resp.response.rect.top() - 4.0,
                                ),
                                egui::pos2(
                                    resp.response.rect.right() + 12.0,
                                    resp.response.rect.bottom() + 4.0,
                                ),
                            )
                        } else {
                            egui::Rect::from_min_max(
                                egui::pos2(
                                    resp.response.rect.left() - 12.0,
                                    resp.response.rect.top() - 4.0,
                                ),
                                egui::pos2(
                                    resp.response.rect.left() - 8.5,
                                    resp.response.rect.bottom() + 4.0,
                                ),
                            )
                        };
                        ui.painter().rect_filled(bar_rect, 2.0, theme.quote_bar);
                    });
            });
        }
        DocBlock::CodeBlock {
            info,
            code,
            source_lines,
        } => {
            let is_active = wysiwyg
                && !read_only
                && state.active_block_kind == Some(ActiveBlockKind::CodeBlock)
                && state
                    .active_block_lines
                    .as_ref()
                    .is_some_and(|r| r.start == source_lines.start);
            if is_active {
                *rendered_active = true;
                let active_lines = state
                    .active_block_lines
                    .clone()
                    .unwrap_or_else(|| source_lines.clone());
                let a = render_inline_wysiwyg_block(
                    ui,
                    buf,
                    &active_lines,
                    ActiveBlockKind::CodeBlock,
                    content_w,
                    theme,
                    show_markup,
                    workspace_top,
                    state,
                );
                if a != ReaderAction::None {
                    action = a;
                }
            } else {
                let now_time = ui.input(|i| i.time);
                let recently_copied = state
                    .copied_code_block_until
                    .is_some_and(|(line, until)| line == source_lines.start && now_time <= until);
                if recently_copied {
                    ui.ctx().request_repaint_after(Duration::from_millis(200));
                }
                let mut body_rect = egui::Rect::NOTHING;
                egui::Frame::new()
                    .fill(theme.code_bg)
                    .stroke(Stroke::new(1.0_f32, theme.code_border))
                    .inner_margin(egui::Margin::same(14))
                    .corner_radius(6.0)
                    .show(ui, |ui| {
                        ui.set_width((content_w - 30.0).max(120.0));
                        ui.horizontal(|ui| {
                            if wysiwyg && !read_only {
                                egui::ComboBox::from_id_salt(("code_lang_cb", source_lines.start))
                                    .selected_text(code_language_display_label(info))
                                    .width(120.0)
                                    .show_ui(ui, |ui| {
                                        let cur_key = info
                                            .split_whitespace()
                                            .next()
                                            .unwrap_or("")
                                            .to_ascii_lowercase();
                                        for &(lang_id, label) in COMMON_CODE_LANGUAGES {
                                            let selected = if lang_id.is_empty() {
                                                info.trim().is_empty()
                                            } else {
                                                cur_key == lang_id
                                            };
                                            if ui.selectable_label(selected, label).clicked() {
                                                set_code_block_language(
                                                    buf,
                                                    source_lines.start,
                                                    lang_id,
                                                );
                                                ui.ctx().request_repaint();
                                            }
                                        }
                                    });
                                if is_mermaid_code_info(info) {
                                    render_fenced_template_picker(
                                        ui,
                                        "Template ▾",
                                        ("inactive_mermaid_tpl", source_lines.start),
                                        "Load a starter Mermaid diagram template",
                                        140.0,
                                        MERMAID_TEMPLATES,
                                        |ui, body| {
                                            let _ =
                                                replace_fenced_block_body(buf, source_lines, body);
                                            ui.ctx().request_repaint();
                                        },
                                    );
                                } else if is_math_code_info(info) {
                                    render_fenced_template_picker(
                                        ui,
                                        "Formula ▾",
                                        ("inactive_math_tpl", source_lines.start),
                                        "Load a textbook LaTeX formula template",
                                        180.0,
                                        MATH_TEXTBOOK_TEMPLATES,
                                        |ui, body| {
                                            let _ =
                                                replace_fenced_block_body(buf, source_lines, body);
                                            ui.ctx().request_repaint();
                                        },
                                    );
                                }
                            } else if !info.is_empty() {
                                egui::Frame::new()
                                    .fill(theme.pill_bg)
                                    .corner_radius(4.0)
                                    .inner_margin(egui::Margin::symmetric(6, 2))
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(info.to_uppercase())
                                                .size(10.0)
                                                .strong()
                                                .color(theme.accent),
                                        );
                                    });
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let copy_lbl = if recently_copied { "✓ Copied" } else { "Copy" };
                                if ui
                                    .small_button(copy_lbl)
                                    .on_hover_text("Copy code block to clipboard")
                                    .clicked()
                                {
                                    ui.ctx().copy_text(code.clone());
                                    state.copied_code_block_until =
                                        Some((source_lines.start, now_time + 1.5));
                                    ui.ctx().request_repaint_after(Duration::from_millis(200));
                                }
                                if !read_only
                                    && is_math_code_info(info)
                                    && ui
                                        .small_button("= Calc")
                                        .on_hover_text(
                                            "Evaluate this equation numerically and append = result",
                                        )
                                        .clicked()
                                {
                                    let (updated, _) =
                                        evaluate_math_block_text(code.trim_end_matches('\n'));
                                    if updated != code.trim_end_matches('\n') {
                                        let _ =
                                            replace_fenced_block_body(buf, source_lines, &updated);
                                        ui.ctx().request_repaint();
                                    }
                                }
                                if !read_only
                                    && delete_block_small_button(
                                        ui,
                                        theme,
                                        "Delete this block (Ctrl+Z to undo)",
                                    )
                                    .clicked()
                                    && delete_block_lines(buf, source_lines)
                                {
                                    state.active_block_lines = None;
                                    ui.ctx().request_repaint();
                                }
                            });
                        });
                        ui.add_space(4.0);
                        let (div_rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width().max(40.0), 4.0),
                            Sense::hover(),
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(div_rect.left(), div_rect.center().y),
                                egui::pos2(div_rect.right(), div_rect.center().y),
                            ],
                            Stroke::new(1.0_f32, theme.border_subtle),
                        );
                        ui.add_space(4.0);
                        let block_is_mermaid = is_mermaid_code_info(info);
                        let block_is_math = is_math_code_info(info);
                        let code_needles = build_block_search_needles(buf, state, source_lines);
                        let whole_word = state.search_whole_word;
                        let body_min_w = (content_w - 30.0).max(120.0);
                        let trimmed_code = code.trim_end_matches('\n');
                        let is_empty_code = trimmed_code.is_empty();
                        let body_resp = ui.vertical(|ui| {
                            ui.set_min_size(Vec2::new(
                                body_min_w,
                                if is_empty_code { 40.0 } else { 20.0 },
                            ));
                            if wysiwyg && show_markup {
                                ui.label(
                                    RichText::new(format!("```{}", info))
                                        .monospace()
                                        .size(11.5)
                                        .color(theme.accent),
                                );
                            }
                            if block_is_mermaid {
                                if is_empty_code {
                                    let hint = if wysiwyg && !read_only {
                                        "Click to write Mermaid diagram..."
                                    } else {
                                        "(empty mermaid diagram)"
                                    };
                                    ui.label(
                                        RichText::new(hint)
                                            .monospace()
                                            .size(12.5)
                                            .color(theme.text_muted),
                                    );
                                } else {
                                    render_mermaid_in_ui(ui, trimmed_code, body_min_w, theme);
                                }
                            } else if block_is_math {
                                if trimmed_code.trim().is_empty() {
                                    let hint = if wysiwyg && !read_only {
                                        "Click to enter LaTeX formula..."
                                    } else {
                                        "(empty math block)"
                                    };
                                    ui.label(
                                        RichText::new(hint)
                                            .monospace()
                                            .size(12.5)
                                            .color(theme.text_muted),
                                    );
                                } else {
                                    egui::ScrollArea::horizontal()
                                        .id_salt(("math_hscroll", source_lines.start))
                                        .auto_shrink([false, true])
                                        .show(ui, |ui| {
                                            ui.with_layout(
                                                egui::Layout::top_down(egui::Align::Center),
                                                |ui| {
                                                    ui.set_min_width(body_min_w);
                                                    render_math_in_ui(
                                                        ui,
                                                        trimmed_code,
                                                        17.5,
                                                        true,
                                                        theme.text_primary,
                                                        theme,
                                                    );
                                                },
                                            );
                                        });
                                    if state.auto_calc_math
                                        && let Some(res_str) =
                                            state.cached_math_preview(trimmed_code)
                                        {
                                            ui.add_space(4.0);
                                            ui.horizontal(|ui| {
                                                render_calc_preview_pill(ui, theme, &res_str);
                                            });
                                        }
                                }
                            } else if is_empty_code {
                                let hint = if wysiwyg && !read_only {
                                    "Click to type code..."
                                } else {
                                    "(empty code block)"
                                };
                                ui.label(
                                    RichText::new(hint)
                                        .monospace()
                                        .size(12.5)
                                        .color(theme.text_muted),
                                );
                            } else {
                                let code_key = {
                                    let mut hasher = FxHasher::default();
                                    0xC0DE_0001_u64.hash(&mut hasher);
                                    info.hash(&mut hasher);
                                    trimmed_code.hash(&mut hasher);
                                    code_needles.hash(&mut hasher);
                                    (0xC0DE_0001_usize, trimmed_code.len(), hasher.finish())
                                };
                                let code_galley =
                                    if let Some(cached) = state.galley_cache.get(&code_key) {
                                        cached.galley
                                    } else {
                                        let mut code_job = LayoutJob {
                                            halign: egui::Align::Min,
                                            ..Default::default()
                                        };
                                        code_job.wrap.max_width = f32::INFINITY;
                                        let code_fmt = TextFormat {
                                            font_id: FontId::new(13.0, FontFamily::Monospace),
                                            color: theme.text_primary,
                                            line_height: Some((13.0_f32 * 1.42).round()),
                                            ..Default::default()
                                        };
                                        if code_needles.is_empty() && trimmed_code.len() <= 32_768 {
                                            append_highlighted_code_block(
                                                &mut code_job,
                                                trimmed_code,
                                                info,
                                                theme,
                                                code_fmt,
                                            );
                                        } else if code_needles.is_empty() {
                                            code_job.append(trimmed_code, 0.0, code_fmt);
                                        } else {
                                            append_bidi_highlighted_run(
                                                &mut code_job,
                                                trimmed_code,
                                                code_fmt,
                                                TextDirection::Ltr,
                                                &code_needles,
                                                whole_word,
                                            );
                                        }
                                        let g = ui.fonts(|f| f.layout_job(code_job));
                                        state.galley_cache.insert_bounded(
                                            code_key,
                                            Arc::clone(&g),
                                            Arc::new(Vec::new()),
                                        );
                                        g
                                    };
                                egui::ScrollArea::horizontal()
                                    .id_salt(("code_hscroll", source_lines.start))
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::Label::new(code_galley)
                                                .selectable(label_selectable),
                                        );
                                    });
                            }
                            if wysiwyg && show_markup {
                                ui.label(
                                    RichText::new("```")
                                        .monospace()
                                        .size(11.5)
                                        .color(theme.accent),
                                );
                            }
                        });
                        body_rect = body_resp.response.rect;
                    });
                if wysiwyg
                    && !read_only
                    && !state.autoscroll_active
                    && body_rect.width() > 0.0
                    && body_rect.height() > 0.0
                {
                    let click = ui.interact(
                        body_rect,
                        ui.id().with(("wys_code", source_lines.start, source_lines.end)),
                        Sense::click(),
                    );
                    if click.hovered() {
                        ui.ctx().set_cursor_icon(
                            if is_mermaid_code_info(info) || is_math_code_info(info) {
                                egui::CursorIcon::PointingHand
                            } else {
                                egui::CursorIcon::Text
                            },
                        );
                    }
                    let click_count = if click.triple_clicked() {
                        3
                    } else if click.double_clicked() {
                        2
                    } else if click.clicked() {
                        1
                    } else {
                        0
                    };
                    if click_count > 0 {
                        activate_wysiwyg_block(
                            Some(ui),
                            buf,
                            source_lines,
                            ActiveBlockKind::CodeBlock,
                            content_w,
                            theme,
                            show_markup,
                            Some(body_rect),
                            click.interact_pointer_pos(),
                            click_count,
                            state,
                        );
                    }
                } else if !wysiwyg
                    && !read_only
                    && !state.autoscroll_active
                    && body_rect.is_positive()
                {
                    let click = ui.interact(
                        body_rect,
                        ui.id().with(("split_code_click", source_lines.start, source_lines.end)),
                        Sense::click(),
                    );
                    if click.clicked() && !source_lines.is_empty() {
                        action = ReaderAction::JumpToSourceLine(source_lines.start);
                    }
                }
            }
        }
        DocBlock::List {
            ordered,
            start,
            items,
            indent_level,
            direction,
            source_lines,
        } => {
            render_indented_logical(ui, content_w, *indent_level, *direction, |ui, iw| {
                for (idx, item) in items.iter().enumerate() {
                    let item_dir = item
                        .blocks
                        .first()
                        .map(|b| match b {
                            DocBlock::Paragraph { direction, .. }
                            | DocBlock::Heading { direction, .. } => *direction,
                            _ => *direction,
                        })
                        .unwrap_or(*direction);
                    let row_layout = if item_dir == TextDirection::Rtl {
                        egui::Layout::right_to_left(egui::Align::TOP)
                    } else {
                        egui::Layout::left_to_right(egui::Align::TOP)
                    };
                    ui.with_layout(row_layout, |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.add_space((list_depth as f32 * 16.0) + 6.0);
                        match item.checked {
                            Some(mut checked) => {
                                let resp = ui.add_enabled(
                                    !read_only,
                                    egui::Checkbox::new(&mut checked, ""),
                                );
                                if !read_only && resp.changed() {
                                    if let Some(first_b) = item.blocks.first() {
                                        let target_line = first_b.source_lines().start;
                                        toggle_task_checkbox_at_line(buf, target_line);
                                    } else {
                                        toggle_task_checkbox_at_line(
                                            buf,
                                            source_lines.start + idx,
                                        );
                                    }
                                }
                            }
                            None if *ordered => {
                                ui.label(
                                    RichText::new(format!("{}.", start + idx))
                                        .size(14.5)
                                        .color(theme.text_muted),
                                );
                                ui.add_space(4.0);
                            }
                            None => {
                                ui.label(
                                    RichText::new("•").size(15.0).color(theme.accent),
                                );
                                ui.add_space(4.0);
                            }
                        }

                        ui.vertical(|ui| {
                            let child_w = (iw - 36.0).max(120.0);
                            ui.set_width(child_w);
                            for sub in &item.blocks {
                                let a = render_doc_block(
                                    ui,
                                    buf,
                                    sub,
                                    child_w,
                                    theme,
                                    wysiwyg,
                                    show_markup,
                                    read_only,
                                    workspace_top,
                                    state,
                                    list_depth + 1,
                                    rendered_active,
                                );
                                if a != ReaderAction::None {
                                    action = a;
                                }
                            }
                        });
                    });
                }
            });
        }
        DocBlock::Table {
            alignments,
            header,
            rows,
            source_lines,
        } => {
            let cols = header
                .len()
                .max(alignments.len())
                .max(rows.iter().map(|r| r.len()).max().unwrap_or(0))
                .max(1);
            let spacing_x = 12.0_f32;
            let total_spacing = spacing_x * (cols.saturating_sub(1) as f32);
            let col_w = ((content_w - 28.0 - total_spacing) / cols as f32).max(90.0);

            ui.vertical(|ui| {
                if !read_only {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("GFM Table")
                                .size(11.0)
                                .strong()
                                .color(theme.text_muted),
                        );
                        if ui.small_button("Edit Table...").clicked() {
                            action = ReaderAction::EditTable(source_lines.clone());
                        }
                        if ui
                            .small_button("fx Recalculate")
                            .on_hover_text("Evaluate Excel formulas in this table and save cached values")
                            .clicked()
                        {
                            action = ReaderAction::RecalculateTable(source_lines.clone());
                        }
                        if delete_block_small_button(
                            ui,
                            theme,
                            "Delete this table (Ctrl+Z to undo)",
                        )
                        .clicked()
                            && delete_block_lines(buf, source_lines)
                        {
                            state.active_block_lines = None;
                            ui.ctx().request_repaint();
                        }
                    });
                    ui.add_space(4.0);
                }

                let evaluated_formulas: Option<Arc<EvaluatedTableGrid>> =
                    if state.auto_calc_tables && !(wysiwyg && show_markup) && rows.len() <= 500 {
                        state.cached_table_evaluation(buf, source_lines.start)
                    } else {
                        None
                    };

                let table_needles = build_block_search_needles(buf, state, source_lines);
                let whole_word = state.search_whole_word;
                let suppress_clicks = state.autoscroll_active;
                let base_dir_owned = state.base_dir.clone();
                let galley_cache = &mut state.galley_cache;
                let image_label_editor = &mut state.image_label_editor;
                let image_lightbox = &mut state.image_lightbox;
                let large_table = rows.len() > 80;
                let table_scroll_id = ui.id().with(("md_table_hscroll", source_lines.start));
                let approx_row_h = 26.0_f32;
                let table_max_w = (content_w - 18.0).max(160.0);
                let vis_rows = if large_table {
                    let offset_y = egui::scroll_area::State::load(ui.ctx(), table_scroll_id)
                        .map(|s| s.offset.y)
                        .unwrap_or(0.0);
                    let start = ((offset_y / approx_row_h).floor() as usize).saturating_sub(10);
                    let end = (start + 45).min(rows.len());
                    start..end
                } else {
                    0..rows.len()
                };
                egui::Frame::new()
                    .fill(theme.paper_bg)
                    .stroke(Stroke::new(1.0_f32, theme.border))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        let mut sa = if large_table {
                            egui::ScrollArea::both().max_height(560.0)
                        } else {
                            egui::ScrollArea::horizontal()
                        };
                        sa = sa.id_salt(table_scroll_id).max_width(table_max_w);
                        sa.show(ui, |ui| {
                            egui::Grid::new(ui.id().with(("md_table", source_lines.start)))
                                .striped(true)
                                .spacing([spacing_x, 8.0])
                                .min_col_width(col_w)
                                .max_col_width(col_w)
                                .show(ui, |ui| {
                                    if !header.is_empty() {
                                        for c in 0..cols {
                                            let runs =
                                                header.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
                                            let align = alignments
                                                .get(c)
                                                .copied()
                                                .unwrap_or(TableAlign::None);
                                            if let Some(cell_act) = render_table_cell(
                                                ui,
                                                buf,
                                                runs,
                                                None,
                                                source_lines,
                                                col_w,
                                                align,
                                                true,
                                                theme,
                                                wysiwyg && show_markup,
                                                read_only,
                                                suppress_clicks,
                                                base_dir_owned.as_deref(),
                                                &table_needles,
                                                whole_word,
                                                galley_cache,
                                                image_label_editor,
                                                image_lightbox,
                                            ) {
                                                action = cell_act;
                                            }
                                        }
                                        ui.end_row();
                                    }
                                    if large_table && vis_rows.start > 0 {
                                        ui.add_space(vis_rows.start as f32 * approx_row_h);
                                        for _ in 1..cols {
                                            ui.label("");
                                        }
                                        ui.end_row();
                                    }
                                    for r_idx in vis_rows.clone() {
                                        let row = &rows[r_idx];
                                        for c in 0..cols {
                                            let runs =
                                                row.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
                                            let align = alignments
                                                .get(c)
                                                .copied()
                                                .unwrap_or(TableAlign::None);
                                            let eval_text = evaluated_formulas.as_deref().and_then(
                                                |(grid, mask)| {
                                                    if mask
                                                        .get(r_idx)
                                                        .and_then(|mr| mr.get(c))
                                                        .copied()
                                                        .unwrap_or(false)
                                                    {
                                                        grid.get(r_idx)
                                                            .and_then(|er| er.get(c))
                                                            .map(|s| s.as_str())
                                                    } else {
                                                        None
                                                    }
                                                },
                                            );
                                            if let Some(cell_act) = render_table_cell(
                                                ui,
                                                buf,
                                                runs,
                                                eval_text,
                                                source_lines,
                                                col_w,
                                                align,
                                                false,
                                                theme,
                                                wysiwyg && show_markup,
                                                read_only,
                                                suppress_clicks,
                                                base_dir_owned.as_deref(),
                                                &table_needles,
                                                whole_word,
                                                galley_cache,
                                                image_label_editor,
                                                image_lightbox,
                                            ) {
                                                action = cell_act;
                                            }
                                        }
                                        ui.end_row();
                                    }
                                    if large_table && vis_rows.end < rows.len() {
                                        ui.add_space((rows.len() - vis_rows.end) as f32 * approx_row_h);
                                        for _ in 1..cols {
                                            ui.label("");
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                    });
            });
        }
        DocBlock::ThematicBreak { source_lines } => {
            let is_active =
                wysiwyg && !read_only && state.active_block_lines.as_ref() == Some(source_lines);
            if is_active {
                *rendered_active = true;
                let a = render_inline_wysiwyg_block(
                    ui,
                    buf,
                    source_lines,
                    ActiveBlockKind::Paragraph,
                    content_w,
                    theme,
                    show_markup,
                    workspace_top,
                    state,
                );
                if a != ReaderAction::None {
                    action = a;
                }
            } else {
                let sense = if wysiwyg && !read_only && !state.autoscroll_active {
                    Sense::click()
                } else {
                    Sense::hover()
                };
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(content_w, 14.0), sense);
                let hovered = wysiwyg && !read_only && resp.hovered();
                if hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    ui.painter().rect_filled(
                        rect.shrink2(Vec2::new(0.0, 2.0)),
                        3.0,
                        theme.accent_soft,
                    );
                }
                ui.painter().line_segment(
                    [
                        egui::pos2(rect.left(), rect.center().y),
                        egui::pos2(rect.right(), rect.center().y),
                    ],
                    Stroke::new(
                        if hovered { 2.0_f32 } else { 1.5_f32 },
                        if hovered { theme.accent } else { theme.border },
                    ),
                );
                if wysiwyg && show_markup {
                    ui.label(
                        RichText::new("---")
                            .monospace()
                            .size(11.0)
                            .color(theme.accent),
                    );
                }
                if wysiwyg && !read_only && !state.autoscroll_active && resp.clicked() {
                    activate_wysiwyg_block(
                        Some(ui),
                        buf,
                        source_lines,
                        ActiveBlockKind::Paragraph,
                        content_w,
                        theme,
                        show_markup,
                        Some(rect),
                        resp.interact_pointer_pos(),
                        1,
                        state,
                    );
                }
            }
        }
        DocBlock::FootnoteDefinition {
            name,
            blocks,
            source_lines: _,
        } => {
            egui::Frame::new()
                .fill(theme.code_bg)
                .stroke(Stroke::new(1.0_f32, theme.border_subtle))
                .corner_radius(5.0)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("[^{name}]"))
                                .strong()
                                .size(12.5)
                                .color(theme.link_color),
                        );
                        ui.vertical(|ui| {
                            let child_w = (content_w - 64.0).max(140.0);
                            ui.set_width(child_w);
                            for inner in blocks {
                                let a = render_doc_block(
                                    ui,
                                    buf,
                                    inner,
                                    child_w,
                                    theme,
                                    wysiwyg,
                                    show_markup,
                                    read_only,
                                    workspace_top,
                                    state,
                                    list_depth,
                                    rendered_active,
                                );
                                if a != ReaderAction::None {
                                    action = a;
                                }
                            }
                        });
                    });
                });
        }
    }

    action
}


fn build_block_search_needles(
    buf: &DocumentBuffer,
    state: &VirtualReaderState,
    src_lines: &Range<usize>,
) -> Vec<(String, bool)> {
    if state.search_matches.is_empty() || src_lines.is_empty() || src_lines.end > buf.len_lines() {
        return Vec::new();
    }
    let blk_range = buf.line_range_chars(
        src_lines.start,
        src_lines.end.saturating_sub(1).max(src_lines.start),
    );
    let mut needles: Vec<(String, bool)> = Vec::new();
    let first_m = state
        .search_matches
        .partition_point(|m| m.end <= blk_range.start);
    for m in &state.search_matches[first_m..] {
        if m.start > blk_range.end {
            break;
        }
        if m.end > blk_range.start && m.start <= blk_range.end {
            let s = m.start.max(blk_range.start);
            let e = m.end.min(blk_range.end);
            if e > s {
                let slice = buf.rope().slice(s..e).to_string();
                if !slice.is_empty() {
                    let is_active = state.active_search_match.as_ref() == Some(m);
                    if let Some(existing) = needles.iter_mut().find(|(t, _)| *t == slice) {
                        if is_active {
                            existing.1 = true;
                        }
                    } else {
                        needles.push((slice, is_active));
                    }
                }
            }
        }
    }
    needles
}

fn append_bidi_highlighted_run(
    job: &mut LayoutJob,
    text: &str,
    fmt: TextFormat,
    direction: TextDirection,
    needles: &[(String, bool)],
    whole_word: bool,
) -> usize {
    if text.is_empty() {
        return 0;
    }
    let text_chars: Vec<char> = text.chars().collect();
    let char_len = text_chars.len();
    let mut hl = vec![0u8; char_len];
    let mut any_hit = false;

    for (needle, is_active) in needles {
        let lvl = if *is_active { 2u8 } else { 1u8 };
        for (byte_start, matched) in text.match_indices(needle.as_str()) {
            let byte_end = byte_start + matched.len();
            let log_s = text[..byte_start].chars().count();
            let log_e = log_s + text[byte_start..byte_end].chars().count();
            if whole_word && !is_whole_word_match(&text_chars, log_s, log_e) {
                continue;
            }
            for slot in hl.iter_mut().take(log_e.min(char_len)).skip(log_s) {
                if lvl > *slot {
                    *slot = lvl;
                }
            }
            any_hit = true;
        }
    }

    if !any_hit {
        if text.chars().any(|c| is_rtl_char(c) || c == '\u{200D}') {
            let shaped = shape_bidi_for_display(text, direction);
            let n = shaped.chars().count();
            job.append(&shaped, 0.0, fmt);
            return n;
        } else {
            job.append(text, 0.0, fmt);
            return char_len;
        }
    }

    let match_bg = Color32::from_rgba_unmultiplied(245, 185, 35, 96);
    let active_match_bg = Color32::from_rgba_unmultiplied(255, 150, 20, 155);

    let (visual_text, vis_to_log): (String, Vec<usize>) = if text.contains('\n') {
        let m = MultilineBidiMap::new(text);
        let v_to_l = if m.is_identity {
            (0..char_len).collect()
        } else {
            m.vis_to_log
        };
        (m.visual_text, v_to_l)
    } else if text.chars().any(|c| is_rtl_char(c) || c == '\u{200D}') {
        let m = build_visual_bidi_line_map(text);
        let v_to_l = if m.is_identity {
            (0..char_len).collect()
        } else {
            m.vis_to_log
        };
        (m.visual_text, v_to_l)
    } else {
        (text.to_string(), (0..char_len).collect())
    };

    let vis_chars = visual_text.chars().count();
    let mut cur_lvl = 0u8;
    let mut chunk = String::new();
    for (vis_idx, ch) in visual_text.chars().enumerate() {
        let log_idx = vis_to_log.get(vis_idx).copied().unwrap_or(vis_idx);
        let lvl = hl.get(log_idx).copied().unwrap_or(0);
        if lvl != cur_lvl && !chunk.is_empty() {
            let mut span_fmt = fmt.clone();
            if cur_lvl == 2 {
                span_fmt.background = active_match_bg;
            } else if cur_lvl == 1 {
                span_fmt.background = match_bg;
            }
            job.append(&chunk, 0.0, span_fmt);
            chunk.clear();
        }
        cur_lvl = lvl;
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        let mut span_fmt = fmt;
        if cur_lvl == 2 {
            span_fmt.background = active_match_bg;
        } else if cur_lvl == 1 {
            span_fmt.background = match_bg;
        }
        job.append(&chunk, 0.0, span_fmt);
    }
    vis_chars
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RunsClickOutcome {
    None,
    Action(ReaderAction),
}

struct RenderedGalleyHitbox {
    galley: Arc<egui::Galley>,
    galley_pos: egui::Pos2,
    link_spans: Arc<Vec<GalleyLinkSpan>>,
}

struct RunsRenderResult {
    outcome: RunsClickOutcome,
    hitboxes: Vec<RenderedGalleyHitbox>,
    handled_ui: bool,
    interactive_rects: Vec<egui::Rect>,
}

fn clip_inactive_click_rect(
    rect: egui::Rect,
    interactive_rects: &[egui::Rect],
) -> Option<egui::Rect> {
    if interactive_rects.is_empty() {
        return Some(rect);
    }
    let min_top = interactive_rects
        .iter()
        .map(|r| r.top())
        .fold(f32::INFINITY, f32::min);
    if min_top > rect.top() + 4.0 {
        let mut clipped = rect;
        clipped.max.y = (min_top - 2.0).min(rect.bottom());
        if clipped.height() > 2.0 {
            return Some(clipped);
        }
    }
    None
}

fn label_galley_pos(rect: egui::Rect, galley: &egui::Galley) -> egui::Pos2 {
    match galley.job.halign {
        egui::Align::Min => rect.left_top(),
        egui::Align::Center => rect.center_top(),
        egui::Align::Max => rect.right_top(),
    }
}

fn hit_test_galley_link<'a>(
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    pointer_pos: egui::Pos2,
    link_spans: &'a [GalleyLinkSpan],
) -> Option<&'a NavTarget> {
    if link_spans.is_empty() {
        return None;
    }
    let rel = pointer_pos - galley_pos;
    for (row_idx, row) in galley.rows.iter().enumerate() {
        if rel.y >= row.rect.top() - 1.5 && rel.y <= row.rect.bottom() + 1.5 {
            if row.glyphs.is_empty()
                || rel.x < row.rect.min.x - 1.5
                || rel.x > row.rect.max.x + 1.5
            {
                return None;
            }
            for pad in [0.0_f32, 1.5_f32] {
                for col in 0..row.glyphs.len() {
                    let x0 = row.x_offset(col);
                    let x1 = row.x_offset(col + 1);
                    let lo = x0.min(x1) - pad;
                    let hi = x0.max(x1) + pad;
                    if rel.x >= lo && rel.x <= hi {
                        let ccursor_idx = galley
                            .from_rcursor(egui::epaint::text::cursor::RCursor {
                                row: row_idx,
                                column: col,
                            })
                            .ccursor
                            .index;
                        if let Some(span) = link_spans
                            .iter()
                            .find(|s| s.char_range.contains(&ccursor_idx))
                        {
                            return Some(&span.target);
                        }
                    }
                }
            }
            return None;
        }
    }
    None
}

fn hit_test_block_links(
    hitboxes: &[RenderedGalleyHitbox],
    pointer_pos: egui::Pos2,
) -> Option<&NavTarget> {
    for hb in hitboxes {
        if let Some(target) =
            hit_test_galley_link(&hb.galley, hb.galley_pos, pointer_pos, &hb.link_spans)
        {
            return Some(target);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn present_galley_text_chunk(
    ui: &mut egui::Ui,
    galley: Arc<egui::Galley>,
    link_spans: Arc<Vec<GalleyLinkSpan>>,
    src_lines: &Range<usize>,
    selectable: bool,
    read_only: bool,
    suppress_clicks: bool,
    cmd_down: bool,
    hitboxes: &mut Vec<RenderedGalleyHitbox>,
) -> RunsClickOutcome {
    let sense = if selectable && !suppress_clicks {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let resp = ui.add(
        egui::Label::new(Arc::clone(&galley))
            .selectable(selectable)
            .sense(sense),
    );
    let galley_pos = label_galley_pos(resp.rect, &galley);
    hitboxes.push(RenderedGalleyHitbox {
        galley: Arc::clone(&galley),
        galley_pos,
        link_spans: Arc::clone(&link_spans),
    });

    if selectable && !suppress_clicks {
        if resp.hovered() {
            let hover_pos =
                ui.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()));
            if let Some(target) = hover_pos
                .and_then(|pos| hit_test_galley_link(&galley, galley_pos, pos, &link_spans))
            {
                let tip = nav_target_tooltip(target, read_only);
                show_link_hover_tooltip(ui, resp.id.with("link_tip"), &tip);
                if read_only || cmd_down {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
        }
        if resp.clicked() {
            let click_pos = resp
                .interact_pointer_pos()
                .or_else(|| ui.input(|i| i.pointer.interact_pos()));
            if let Some(target) = click_pos
                .and_then(|pos| hit_test_galley_link(&galley, galley_pos, pos, &link_spans))
                && (read_only || cmd_down) {
                    return RunsClickOutcome::Action(nav_target_action(target));
                }
            if !read_only && !cmd_down && !src_lines.is_empty() {
                return RunsClickOutcome::Action(ReaderAction::JumpToSourceLine(src_lines.start));
            }
        }
    }

    RunsClickOutcome::None
}

fn split_run_by_newlines_and_words(run: &InlineRun) -> Vec<Vec<InlineRun>> {
    let mut rows: Vec<Vec<InlineRun>> = vec![Vec::new()];
    let mut line_iter = run.text.split('\n').peekable();
    while let Some(seg) = line_iter.next() {
        if !seg.is_empty() {
            if seg.chars().any(is_rtl_char) {
                let mut r = run.clone();
                r.text = seg.to_string();
                rows.last_mut().unwrap().push(r);
            } else {
                let mut tok_start = 0usize;
                let mut has_non_ws = false;
                let mut in_ws_tail = false;
                for (idx, ch) in seg.char_indices() {
                    if ch.is_whitespace() {
                        if has_non_ws {
                            in_ws_tail = true;
                        }
                    } else {
                        if in_ws_tail {
                            let mut r = run.clone();
                            r.text = seg[tok_start..idx].to_string();
                            rows.last_mut().unwrap().push(r);
                            tok_start = idx;
                            in_ws_tail = false;
                        }
                        has_non_ws = true;
                    }
                }
                if tok_start < seg.len() {
                    let mut r = run.clone();
                    r.text = seg[tok_start..].to_string();
                    rows.last_mut().unwrap().push(r);
                }
            }
        }
        if line_iter.peek().is_some() {
            rows.push(Vec::new());
        }
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn render_inline_math_wrapped_runs(
    ui: &mut egui::Ui,
    runs: &[InlineRun],
    src_lines: &Range<usize>,
    font_size: f32,
    line_height_mult: f32,
    force_bold: bool,
    base_color: Color32,
    content_w: f32,
    direction: TextDirection,
    theme: &WordTheme,
    show_inline_markup: bool,
    prefix_markup: Option<&str>,
    selectable: bool,
    read_only: bool,
    suppress_clicks: bool,
    cmd_down: bool,
    search_needles: &[(String, bool)],
    whole_word: bool,
    hitboxes: &mut Vec<RenderedGalleyHitbox>,
) -> RunsClickOutcome {
    enum WrappedSeg {
        Text(InlineRun),
        Math(String),
    }

    let ordered_runs: Vec<&InlineRun> = if direction == TextDirection::Rtl {
        runs.iter().rev().collect()
    } else {
        runs.iter().collect()
    };

    let mut lines: Vec<Vec<WrappedSeg>> = vec![Vec::new()];
    for run in ordered_runs {
        if run.is_math {
            lines.last_mut().unwrap().push(WrappedSeg::Math(run.text.clone()));
        } else {
            let split = split_run_by_newlines_and_words(run);
            for (r_idx, row_runs) in split.into_iter().enumerate() {
                if r_idx > 0 {
                    lines.push(Vec::new());
                }
                for tok in row_runs {
                    lines.last_mut().unwrap().push(WrappedSeg::Text(tok));
                }
            }
        }
    }

    let mut outcome = RunsClickOutcome::None;
    let mut prefix_pending = prefix_markup.map(str::to_string);

    for line_segs in lines {
        ui.horizontal_wrapped(|ui| {
            ui.set_max_width(content_w);
            ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
            if let Some(pref) = prefix_pending.take() {
                ui.label(
                    RichText::new(pref)
                        .monospace()
                        .size(font_size * 0.85)
                        .color(theme.accent),
                );
            }
            for seg in line_segs {
                match seg {
                    WrappedSeg::Math(latex) => {
                        if show_inline_markup {
                            ui.label(
                                RichText::new("$")
                                    .monospace()
                                    .size(font_size * 0.85)
                                    .color(theme.accent),
                            );
                        }
                        render_math_in_ui(ui, &latex, font_size, false, base_color, theme);
                        if show_inline_markup {
                            ui.label(
                                RichText::new("$")
                                    .monospace()
                                    .size(font_size * 0.85)
                                    .color(theme.accent),
                            );
                        }
                    }
                    WrappedSeg::Text(tok) => {
                        let (mut job, link_spans) = build_runs_layout_job(
                            &[tok],
                            font_size,
                            line_height_mult,
                            force_bold,
                            base_color,
                            direction,
                            theme,
                            show_inline_markup,
                            None,
                            search_needles,
                            whole_word,
                        );
                        job.wrap.max_width = content_w;
                        let galley = ui.fonts(|f| f.layout_job(job));
                        let res = present_galley_text_chunk(
                            ui,
                            galley,
                            Arc::new(link_spans),
                            src_lines,
                            selectable,
                            read_only,
                            suppress_clicks,
                            cmd_down,
                            hitboxes,
                        );
                        if res != RunsClickOutcome::None {
                            outcome = res;
                        }
                    }
                }
            }
        });
    }

    outcome
}

#[allow(clippy::too_many_arguments)]
fn render_runs_with_images(
    ui: &mut egui::Ui,
    buf: &mut DocumentBuffer,
    runs: &[InlineRun],
    src_lines: &Range<usize>,
    font_size: f32,
    line_height_mult: f32,
    force_bold: bool,
    base_color: Color32,
    content_w: f32,
    alignment: BlockAlignment,
    direction: TextDirection,
    theme: &WordTheme,
    show_inline_markup: bool,
    prefix_markup: Option<String>,
    selectable: bool,
    read_only: bool,
    suppress_clicks: bool,
    base_dir: Option<&Path>,
    search_needles: &[(String, bool)],
    whole_word: bool,
    galley_cache: &mut ReaderGalleyCache,
    image_label_editor: &mut Option<ImageLabelEditorState>,
    image_lightbox: &mut Option<ImageLightboxState>,
) -> RunsRenderResult {
    let egui_align = resolve_egui_align(alignment, direction);
    let cmd_down = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
    let mut outcome = RunsClickOutcome::None;
    let mut hitboxes: Vec<RenderedGalleyHitbox> = Vec::new();
    let mut handled_ui = false;
    let mut interactive_rects: Vec<egui::Rect> = Vec::new();

    ui.with_layout(egui::Layout::top_down(egui_align), |ui| {
        ui.set_width(content_w);
        ui.set_max_width(content_w);

        let has_images = runs.iter().any(|r| r.image_url.is_some());
        let has_math = runs.iter().any(|r| r.is_math);

        if !has_images && !has_math {
            let cache_key = {
                let mut hasher = FxHasher::default();
                0x5255_4E53_u64.hash(&mut hasher);
                hash_inline_runs(runs, &mut hasher);
                (content_w.round() as i32).hash(&mut hasher);
                font_size.to_bits().hash(&mut hasher);
                line_height_mult.to_bits().hash(&mut hasher);
                force_bold.hash(&mut hasher);
                core::mem::discriminant(&alignment).hash(&mut hasher);
                core::mem::discriminant(&direction).hash(&mut hasher);
                base_color.to_array().hash(&mut hasher);
                prefix_markup.hash(&mut hasher);
                search_needles.hash(&mut hasher);
                (0x5255_4E53_usize, runs.len(), hasher.finish())
            };
            let cached = if let Some(entry) = galley_cache.get(&cache_key) {
                entry
            } else {
                let (mut job, link_spans) = build_runs_layout_job(
                    runs,
                    font_size,
                    line_height_mult,
                    force_bold,
                    base_color,
                    direction,
                    theme,
                    show_inline_markup,
                    prefix_markup.as_deref(),
                    search_needles,
                    whole_word,
                );
                job.halign = egui_align;
                job.wrap.max_width = content_w;
                job.justify = alignment == BlockAlignment::Justify;
                galley_cache.insert_bounded(
                    cache_key,
                    ui.fonts(|f| f.layout_job(job)),
                    Arc::new(link_spans),
                )
            };
            outcome = present_galley_text_chunk(
                ui,
                cached.galley,
                cached.link_spans,
                src_lines,
                selectable,
                read_only,
                suppress_clicks,
                cmd_down,
                &mut hitboxes,
            );
            return;
        }

        if !has_images && has_math {
            outcome = render_inline_math_wrapped_runs(
                ui,
                runs,
                src_lines,
                font_size,
                line_height_mult,
                force_bold,
                base_color,
                content_w,
                direction,
                theme,
                show_inline_markup,
                prefix_markup.as_deref(),
                selectable,
                read_only,
                suppress_clicks,
                cmd_down,
                search_needles,
                whole_word,
                &mut hitboxes,
            );
            return;
        }

        let mut text_chunk: Vec<InlineRun> = Vec::new();
        let mut prefix_used = prefix_markup;
        let mut flush_text_chunk = |ui: &mut egui::Ui, chunk: &[InlineRun]| {
            if chunk.is_empty() {
                return;
            }
            let res = if chunk.iter().any(|r| r.is_math) {
                render_inline_math_wrapped_runs(
                    ui,
                    chunk,
                    src_lines,
                    font_size,
                    line_height_mult,
                    force_bold,
                    base_color,
                    content_w,
                    direction,
                    theme,
                    show_inline_markup,
                    prefix_used.take().as_deref(),
                    selectable,
                    read_only,
                    suppress_clicks,
                    cmd_down,
                    search_needles,
                    whole_word,
                    &mut hitboxes,
                )
            } else {
                let (mut job, link_spans) = build_runs_layout_job(
                    chunk,
                    font_size,
                    line_height_mult,
                    force_bold,
                    base_color,
                    direction,
                    theme,
                    show_inline_markup,
                    prefix_used.take().as_deref(),
                    search_needles,
                    whole_word,
                );
                job.halign = egui_align;
                job.wrap.max_width = content_w;
                job.justify = alignment == BlockAlignment::Justify;
                let galley = ui.fonts(|f| f.layout_job(job));
                present_galley_text_chunk(
                    ui,
                    galley,
                    Arc::new(link_spans),
                    src_lines,
                    selectable,
                    read_only,
                    suppress_clicks,
                    cmd_down,
                    &mut hitboxes,
                )
            };
            if res != RunsClickOutcome::None {
                outcome = res;
            }
        };

        let mut img_action: Option<ReaderAction> = None;
        let mut idx = 0;
        while idx < runs.len() {
            if let Some(ref url) = runs[idx].image_url {
                if !text_chunk.is_empty() {
                    flush_text_chunk(ui, &text_chunk);
                    text_chunk.clear();
                }

                if is_remote_image_url(url) {
                    let group_start = idx;
                    let mut group_end = idx + 1;
                    let mut remote_count = 1usize;
                    let mut scan = group_end;
                    while scan < runs.len() {
                        if let Some(ref next_url) = runs[scan].image_url {
                            if is_remote_image_url(next_url) {
                                group_end = scan + 1;
                                remote_count += 1;
                                scan += 1;
                                continue;
                            }
                            break;
                        }
                        if !runs[scan].is_math && runs[scan].text.trim().is_empty() {
                            scan += 1;
                            continue;
                        }
                        break;
                    }
                    if remote_count >= 2 || is_likely_remote_badge_run(&runs[idx], url) {
                        let row_resp = ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                            for (offset, badge_run) in runs[group_start..group_end].iter().enumerate() {
                                if let Some(ref badge_url) = badge_run.image_url
                                    && let Some(act) = render_remote_badge_pill(
                                        ui,
                                        badge_run,
                                        badge_url,
                                        group_start + offset,
                                        theme,
                                        show_inline_markup,
                                        read_only,
                                        suppress_clicks,
                                        cmd_down,
                                    ) {
                                        img_action = Some(act);
                                    }
                            }
                        });
                        interactive_rects.push(row_resp.response.rect);
                        idx = group_end;
                        continue;
                    }
                }

                let (act, ui_used, ui_rect) = render_image_run(
                    ui,
                    buf,
                    &runs[idx],
                    url,
                    src_lines,
                    idx,
                    content_w.clamp(80.0, 640.0),
                    420.0_f32,
                    true,
                    show_inline_markup,
                    base_dir,
                    theme,
                    read_only,
                    suppress_clicks,
                    cmd_down,
                    image_label_editor,
                    image_lightbox,
                );
                if ui_used {
                    handled_ui = true;
                }
                if let Some(r) = ui_rect {
                    interactive_rects.push(r);
                }
                if let Some(a) = act {
                    img_action = Some(a);
                }
            } else {
                text_chunk.push(runs[idx].clone());
            }
            idx += 1;
        }

        flush_text_chunk(ui, &text_chunk);
        drop(flush_text_chunk);
        if let Some(act) = img_action {
            outcome = RunsClickOutcome::Action(act);
        }
    });
    RunsRenderResult {
        outcome,
        hitboxes,
        handled_ui,
        interactive_rects,
    }
}

fn is_remote_image_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn is_likely_remote_badge_run(run: &InlineRun, url: &str) -> bool {
    if run.link_url.is_none() {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    lower.contains("shields.io")
        || lower.contains("/badge")
        || lower.contains("badge.svg")
        || lower.contains("codecov.io")
        || lower.contains("actions/workflows")
}

fn image_run_nav_target(link_url: Option<&str>, fallback_url: Option<&str>) -> Option<NavTarget> {
    let raw = link_url.or(fallback_url)?.trim();
    let classified = classify_link_url(raw)?;
    Some(NavTarget::Link(raw.to_string(), classified))
}

#[allow(clippy::too_many_arguments)]
fn render_remote_badge_pill(
    ui: &mut egui::Ui,
    run: &InlineRun,
    url: &str,
    run_idx: usize,
    theme: &WordTheme,
    show_inline_markup: bool,
    read_only: bool,
    suppress_clicks: bool,
    cmd_down: bool,
) -> Option<ReaderAction> {
    let nav_target = image_run_nav_target(run.link_url.as_deref(), Some(url));
    let resp = if let ImageLoadState::Loaded(tex) = load_cached_image_state(ui.ctx(), url, None, None) {
        let [tw, th] = tex.size();
        let mut draw_w = tw.max(1) as f32;
        let mut draw_h = th.max(1) as f32;
        let max_badge_h = 24.0_f32;
        let max_badge_w = 260.0_f32;
        if draw_h > max_badge_h {
            let scale = max_badge_h / draw_h;
            draw_w *= scale;
            draw_h *= scale;
        }
        if draw_w > max_badge_w {
            let scale = max_badge_w / draw_w;
            draw_w *= scale;
            draw_h *= scale;
        }
        ui.image((tex.id(), Vec2::new(draw_w, draw_h)))
            .interact(Sense::click())
    } else {
        let label_text = if !run.text.trim().is_empty() {
            run.text.trim().to_string()
        } else {
            url.trim()
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .split('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("Remote Badge")
                .to_string()
        };
        let btn = egui::Button::new(
            RichText::new(&label_text)
                .size(11.5)
                .strong()
                .color(theme.accent),
        )
        .fill(theme.accent_soft)
        .stroke(Stroke::new(1.0_f32, theme.border))
        .corner_radius(CornerRadius::same(10));
        ui.add(btn)
    };

    let mut action = None;
    if let Some(ref target) = nav_target {
        if resp.hovered() {
            let tip = nav_target_tooltip(target, read_only);
            show_link_hover_tooltip(ui, resp.id.with(("badge_tip", run_idx)), &tip);
            if !suppress_clicks && (read_only || cmd_down) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        if !suppress_clicks && (read_only || cmd_down) && resp.clicked() {
            action = Some(nav_target_action(target));
        }
    }

    if show_inline_markup {
        ui.label(
            RichText::new(format!("![{}]({})", run.text, url))
                .monospace()
                .size(11.0)
                .color(theme.accent),
        );
    }
    action
}

#[allow(clippy::too_many_arguments)]
fn render_image_label_controls(
    ui: &mut egui::Ui,
    buf: &mut DocumentBuffer,
    run: &InlineRun,
    url: &str,
    src_lines: &Range<usize>,
    run_idx: usize,
    max_w: f32,
    show_caption: bool,
    theme: &WordTheme,
    read_only: bool,
    image_label_editor: &mut Option<ImageLabelEditorState>,
) -> (bool, Option<egui::Rect>) {
    let trimmed_alt = run.text.trim();
    let has_custom_alt = !trimmed_alt.is_empty() && trimmed_alt != url.trim();
    if read_only {
        if show_caption && has_custom_alt {
            ui.label(
                RichText::new(&run.text)
                    .size(11.5)
                    .italics()
                    .color(theme.text_muted),
            );
        }
        return (false, None);
    }

    let is_editing_this = image_label_editor.as_ref().is_some_and(|ed| {
        ed.source_lines == *src_lines && ed.run_idx == run_idx && ed.image_url == url
    });
    if is_editing_this {
        let mut close_editor = false;
        let mut commit_editor = false;
        let row_resp = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if let Some(ed) = image_label_editor.as_mut() {
                let edit_w = (max_w - 72.0).clamp(110.0, 320.0);
                let te_resp = ui.add(
                    egui::TextEdit::singleline(&mut ed.draft)
                        .id_source(("img_alt_edit", src_lines.start, run_idx, url))
                        .hint_text("Image label (alt text)...")
                        .desired_width(edit_w),
                );
                let had_focus_request = ed.focus_requested;
                if ed.focus_requested {
                    te_resp.request_focus();
                    ed.focus_requested = false;
                }
                let save_resp = ui
                    .small_button("✓")
                    .on_hover_text("Save image label (Enter)");
                let cancel_resp = ui
                    .small_button("✕")
                    .on_hover_text("Cancel (Esc)");

                let esc_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
                let enter_pressed = !had_focus_request
                    && te_resp.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if cancel_resp.clicked() || esc_pressed {
                    close_editor = true;
                } else if save_resp.clicked()
                    || enter_pressed
                    || (!had_focus_request && te_resp.lost_focus() && !cancel_resp.hovered())
                {
                    commit_editor = true;
                }
            }
        });
        if commit_editor {
            if let Some(ed) = image_label_editor.take() {
                let _ = update_image_alt_text(buf, src_lines, url, &ed.old_alt, &ed.draft);
            }
        } else if close_editor {
            *image_label_editor = None;
        }
        return (true, Some(row_resp.response.rect));
    }

    let display_label = if has_custom_alt {
        format!("{}  ✎", trimmed_alt)
    } else {
        "✎ Add image label...".to_string()
    };
    let text_color = if has_custom_alt {
        theme.text_muted
    } else {
        theme.accent
    };
    let label_btn = egui::Button::new(RichText::new(display_label).size(11.5).italics().color(text_color))
        .fill(theme.code_bg)
        .stroke(Stroke::new(0.8_f32, theme.border_subtle))
        .corner_radius(CornerRadius::same(4));
    let resp = ui
        .add(label_btn)
        .on_hover_text("Click to edit image label without entering Source mode");
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        let initial_draft = if has_custom_alt {
            run.text.clone()
        } else {
            String::new()
        };
        *image_label_editor = Some(ImageLabelEditorState {
            source_lines: src_lines.clone(),
            image_url: url.to_string(),
            run_idx,
            old_alt: run.text.clone(),
            draft: initial_draft,
            focus_requested: true,
        });
        return (true, Some(resp.rect));
    }
    (resp.hovered() || resp.is_pointer_button_down_on(), Some(resp.rect))
}

#[allow(clippy::too_many_arguments)]
fn render_image_run(
    ui: &mut egui::Ui,
    buf: &mut DocumentBuffer,
    run: &InlineRun,
    url: &str,
    src_lines: &Range<usize>,
    run_idx: usize,
    max_w: f32,
    max_h: f32,
    show_caption: bool,
    show_inline_markup: bool,
    base_dir: Option<&Path>,
    theme: &WordTheme,
    read_only: bool,
    suppress_clicks: bool,
    cmd_down: bool,
    image_label_editor: &mut Option<ImageLabelEditorState>,
    image_lightbox: &mut Option<ImageLightboxState>,
) -> (Option<ReaderAction>, bool, Option<egui::Rect>) {
    let mut action = None;
    let doc_path = buf.file_path.clone();
    match load_cached_image_state(ui.ctx(), url, base_dir, doc_path.as_deref()) {
        ImageLoadState::Loaded(tex) => {
            let [tw, th] = tex.size();
            let mut draw_w = tw.max(1) as f32;
            let mut draw_h = th.max(1) as f32;
            if draw_w > max_w {
                let scale = max_w / draw_w;
                draw_w *= scale;
                draw_h *= scale;
            }
            if draw_h > max_h {
                let scale = max_h / draw_h;
                draw_w *= scale;
                draw_h *= scale;
            }
            let frame_resp = egui::Frame::new()
                .fill(theme.paper_bg)
                .stroke(Stroke::new(1.0_f32, theme.border))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| ui.image((tex.id(), Vec2::new(draw_w, draw_h))));

            if read_only && !suppress_clicks {
                let click_resp = frame_resp.inner.interact(Sense::click());
                let linked_target = image_run_nav_target(run.link_url.as_deref(), None);
                if cmd_down {
                    if let Some(ref target) = linked_target {
                        if click_resp.hovered() {
                            let tip = nav_target_tooltip(target, true);
                            show_link_hover_tooltip(
                                ui,
                                click_resp.id.with(("img_link", run_idx)),
                                &tip,
                            );
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if click_resp.clicked() {
                            action = Some(nav_target_action(target));
                        }
                    }
                } else {
                    if click_resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
                        let trimmed_alt = run.text.trim();
                        let mut tip = if !trimmed_alt.is_empty() && trimmed_alt != url.trim() {
                            format!("{trimmed_alt} — Click to enlarge")
                        } else {
                            "Click to enlarge".to_string()
                        };
                        if linked_target.is_some() {
                            tip.push_str(" (Ctrl+Click to open link)");
                        }
                        show_link_hover_tooltip(
                            ui,
                            click_resp.id.with(("img_zoom_tip", run_idx)),
                            &tip,
                        );
                    }
                    if click_resp.clicked() {
                        *image_lightbox = Some(ImageLightboxState {
                            url: url.to_string(),
                            alt: run.text.clone(),
                            link_url: run.link_url.clone(),
                            base_dir: base_dir.map(Path::to_path_buf),
                            doc_path: doc_path.clone(),
                            zoom: 1.0,
                        });
                    }
                }
            } else if let Some(target) = image_run_nav_target(run.link_url.as_deref(), None) {
                let click_resp = frame_resp.inner.interact(Sense::click());
                if click_resp.hovered() {
                    let tip = nav_target_tooltip(&target, read_only);
                    show_link_hover_tooltip(ui, click_resp.id.with(("img_link", run_idx)), &tip);
                    if !suppress_clicks && (read_only || cmd_down) {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
                if !suppress_clicks && (read_only || cmd_down) && click_resp.clicked() {
                    action = Some(nav_target_action(&target));
                }
            } else if !run.text.is_empty() && !show_caption && frame_resp.inner.hovered() {
                show_link_hover_tooltip(
                    ui,
                    frame_resp.inner.id.with(("img_alt", run_idx)),
                    &run.text,
                );
            }

            let (label_ui_used, label_rect) = render_image_label_controls(
                ui,
                buf,
                run,
                url,
                src_lines,
                run_idx,
                max_w,
                show_caption,
                theme,
                read_only,
                image_label_editor,
            );
            if show_inline_markup {
                ui.label(
                    RichText::new(format!("![{}]({})", run.text, url))
                        .monospace()
                        .size(11.0)
                        .color(theme.accent),
                );
            }
            return (action, label_ui_used, label_rect);
        }
        ImageLoadState::Loading => {
            egui::Frame::new()
                .fill(theme.code_bg)
                .stroke(Stroke::new(1.0_f32, theme.border))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    let label = if run.text.trim().is_empty() {
                        format!("Loading image ({url})...")
                    } else {
                        format!("Loading {}...", run.text.trim())
                    };
                    ui.label(RichText::new(label).size(12.0).color(theme.text_muted));
                });
        }
        ImageLoadState::Unavailable => {
            egui::Frame::new()
                .fill(theme.code_bg)
                .stroke(Stroke::new(1.0_f32, theme.border))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    let caption = if run.text.is_empty() {
                        format!("[Image: {url}]")
                    } else {
                        format!("[Image: {}] ({url})", run.text)
                    };
                    ui.label(RichText::new(caption).size(12.5).color(theme.text_muted));
                });
        }
    }
    let (label_ui_used, label_rect) = render_image_label_controls(
        ui,
        buf,
        run,
        url,
        src_lines,
        run_idx,
        max_w,
        false,
        theme,
        read_only,
        image_label_editor,
    );
    (action, label_ui_used, label_rect)
}

#[allow(clippy::too_many_arguments)]
fn render_table_cell(
    ui: &mut egui::Ui,
    buf: &mut DocumentBuffer,
    runs: &[InlineRun],
    eval_text: Option<&str>,
    src_lines: &Range<usize>,
    col_w: f32,
    align: TableAlign,
    is_header: bool,
    theme: &WordTheme,
    show_inline_markup: bool,
    read_only: bool,
    suppress_clicks: bool,
    base_dir: Option<&Path>,
    search_needles: &[(String, bool)],
    whole_word: bool,
    galley_cache: &mut ReaderGalleyCache,
    image_label_editor: &mut Option<ImageLabelEditorState>,
    image_lightbox: &mut Option<ImageLightboxState>,
) -> Option<ReaderAction> {
    let cell_dir = if let Some(eval_s) = eval_text {
        detect_paragraph_direction(eval_s)
    } else {
        let plain: String = runs.iter().map(|r| r.text.as_str()).collect();
        detect_paragraph_direction(&plain)
    };
    let egui_align = match align {
        TableAlign::Center => egui::Align::Center,
        TableAlign::Right => egui::Align::Max,
        TableAlign::Left => egui::Align::Min,
        TableAlign::None => {
            if cell_dir == TextDirection::Rtl {
                egui::Align::Max
            } else {
                egui::Align::Min
            }
        }
    };
    let cmd_down = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
    let mut cell_action: Option<ReaderAction> = None;

    ui.with_layout(egui::Layout::top_down(egui_align), |ui| {
        ui.set_width(col_w);
        ui.set_max_width(col_w);
        let color = if is_header {
            theme.heading_color
        } else {
            theme.text_primary
        };
        if eval_text.is_none() && runs.iter().any(|r| r.image_url.is_some()) {
            let max_img_w = (col_w - 8.0).max(40.0);
            let mut text_chunk: Vec<InlineRun> = Vec::new();
            let flush_cell_text = |ui: &mut egui::Ui, chunk: &[InlineRun]| -> Option<ReaderAction> {
                if chunk.is_empty() || chunk.iter().all(|r| r.text.trim().is_empty()) {
                    return None;
                }
                let (mut job, link_spans) = build_runs_layout_job(
                    chunk,
                    13.5,
                    1.32,
                    is_header,
                    color,
                    cell_dir,
                    theme,
                    show_inline_markup,
                    None,
                    search_needles,
                    whole_word,
                );
                job.halign = egui_align;
                job.wrap.max_width = col_w;
                let galley = ui.fonts(|f| f.layout_job(job));
                let mut dummy_hitboxes = Vec::new();
                match present_galley_text_chunk(
                    ui,
                    galley,
                    Arc::new(link_spans),
                    src_lines,
                    true,
                    read_only,
                    suppress_clicks,
                    cmd_down,
                    &mut dummy_hitboxes,
                ) {
                    RunsClickOutcome::Action(a) => Some(a),
                    RunsClickOutcome::None => None,
                }
            };
            for (idx, run) in runs.iter().enumerate() {
                if let Some(ref url) = run.image_url {
                    if let Some(a) = flush_cell_text(ui, &text_chunk) {
                        cell_action = Some(a);
                    }
                    text_chunk.clear();
                    let (act, _, _) = render_image_run(
                        ui,
                        buf,
                        run,
                        url,
                        src_lines,
                        idx,
                        max_img_w,
                        220.0_f32,
                        false,
                        show_inline_markup,
                        base_dir,
                        theme,
                        read_only,
                        suppress_clicks,
                        cmd_down,
                        image_label_editor,
                        image_lightbox,
                    );
                    if let Some(a) = act {
                        cell_action = Some(a);
                    }
                } else {
                    text_chunk.push(run.clone());
                }
            }
            if let Some(a) = flush_cell_text(ui, &text_chunk) {
                cell_action = Some(a);
            }
            return;
        }
        if eval_text.is_none() && runs.iter().any(|r| r.is_math) {
            let mut dummy_hitboxes = Vec::new();
            let outcome = render_inline_math_wrapped_runs(
                ui,
                runs,
                src_lines,
                13.5,
                1.32,
                is_header,
                color,
                col_w,
                cell_dir,
                theme,
                show_inline_markup,
                None,
                true,
                read_only,
                suppress_clicks,
                cmd_down,
                search_needles,
                whole_word,
                &mut dummy_hitboxes,
            );
            if let RunsClickOutcome::Action(a) = outcome {
                cell_action = Some(a);
            }
            return;
        }
        let cache_key = {
            let mut hasher = FxHasher::default();
            0x7AB1_E000_u64.hash(&mut hasher);
            let item_count = if let Some(eval_s) = eval_text {
                0xEA1_CA1C_u64.hash(&mut hasher);
                eval_s.hash(&mut hasher);
                1
            } else {
                hash_inline_runs(runs, &mut hasher);
                runs.len()
            };
            (col_w.round() as i32).hash(&mut hasher);
            is_header.hash(&mut hasher);
            core::mem::discriminant(&egui_align).hash(&mut hasher);
            core::mem::discriminant(&cell_dir).hash(&mut hasher);
            color.to_array().hash(&mut hasher);
            search_needles.hash(&mut hasher);
            (0x7AB1_E000_usize, item_count, hasher.finish())
        };
        let cached = if let Some(entry) = galley_cache.get(&cache_key) {
            entry
        } else {
            let synth = eval_text.map(|s| [InlineRun::plain(s)]);
            let effective_runs = synth.as_ref().map(|a| a.as_slice()).unwrap_or(runs);
            let (mut job, link_spans) = build_runs_layout_job(
                effective_runs,
                13.5,
                1.32,
                is_header,
                color,
                cell_dir,
                theme,
                show_inline_markup,
                None,
                search_needles,
                whole_word,
            );
            job.halign = egui_align;
            job.wrap.max_width = col_w;
            galley_cache.insert_bounded(
                cache_key,
                ui.fonts(|f| f.layout_job(job)),
                Arc::new(link_spans),
            )
        };
        let sense = if !cached.link_spans.is_empty() && !suppress_clicks {
            Sense::click()
        } else {
            Sense::hover()
        };
        let resp = ui.add(egui::Label::new(Arc::clone(&cached.galley)).sense(sense));
        if !cached.link_spans.is_empty() && !suppress_clicks {
            let galley_pos = label_galley_pos(resp.rect, &cached.galley);
            if resp.hovered() {
                let hover_pos =
                    ui.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()));
                if let Some(target) = hover_pos.and_then(|pos| {
                    hit_test_galley_link(&cached.galley, galley_pos, pos, &cached.link_spans)
                }) {
                    let tip = nav_target_tooltip(target, read_only);
                    show_link_hover_tooltip(ui, resp.id.with("tbl_link_tip"), &tip);
                    if read_only || cmd_down {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
            }
            if (read_only || cmd_down) && resp.clicked() {
                let click_pos = resp
                    .interact_pointer_pos()
                    .or_else(|| ui.input(|i| i.pointer.interact_pos()));
                if let Some(target) = click_pos.and_then(|pos| {
                    hit_test_galley_link(&cached.galley, galley_pos, pos, &cached.link_spans)
                }) {
                    cell_action = Some(nav_target_action(target));
                }
            }
        }
    });
    cell_action
}

#[allow(clippy::too_many_arguments)]
fn build_runs_layout_job(
    runs: &[InlineRun],
    font_size: f32,
    line_height_mult: f32,
    force_bold: bool,
    base_color: Color32,
    direction: TextDirection,
    theme: &WordTheme,
    show_inline_markup: bool,
    prefix_markup: Option<&str>,
    search_needles: &[(String, bool)],
    whole_word: bool,
) -> (LayoutJob, Vec<GalleyLinkSpan>) {
    let mut job = LayoutJob::default();
    let mut link_spans: Vec<GalleyLinkSpan> = Vec::new();
    let mut running_chars = 0usize;
    let push_str = |job: &mut LayoutJob, chars: &mut usize, s: &str, fmt: TextFormat| {
        *chars += s.chars().count();
        job.append(s, 0.0, fmt);
    };

    let row_line_height = Some((font_size * line_height_mult).round());
    let markup_fmt = TextFormat {
        font_id: FontId::new(font_size * 0.85, FontFamily::Monospace),
        color: theme.accent,
        line_height: row_line_height,
        ..Default::default()
    };

    if direction == TextDirection::Ltr
        && let Some(prefix) = prefix_markup {
            push_str(&mut job, &mut running_chars, prefix, markup_fmt.clone());
        }

    let ordered_runs: Vec<&InlineRun> = if direction == TextDirection::Rtl {
        runs.iter().rev().collect()
    } else {
        runs.iter().collect()
    };

    for run in ordered_runs {
        let is_bold = run.bold || force_bold;
        let family = if run.code {
            FontFamily::Monospace
        } else if is_bold || run.is_footnote_ref {
            FontFamily::Name("bold".into())
        } else {
            FontFamily::Proportional
        };

        let color = if run.link_url.is_some() || run.is_footnote_ref {
            theme.link_color
        } else if run.code {
            theme.accent
        } else {
            base_color
        };

        let mut fmt = TextFormat {
            font_id: FontId::new(
                if run.is_footnote_ref {
                    (font_size * 0.78).max(10.0)
                } else if run.code {
                    font_size * 0.9
                } else {
                    font_size
                },
                family,
            ),
            color,
            italics: run.italic,
            line_height: row_line_height,
            valign: if run.is_footnote_ref {
                egui::Align::TOP
            } else {
                egui::Align::BOTTOM
            },
            ..Default::default()
        };
        if run.code {
            fmt.background = theme.code_bg;
        }
        if run.strikethrough {
            fmt.strikethrough = Stroke::new(1.2_f32, color);
        }
        if run.link_url.is_some() {
            fmt.underline = Stroke::new(1.0_f32, theme.link_color);
        } else if run.underline {
            fmt.underline = Stroke::new(1.1_f32, color);
        }

        if show_inline_markup && !run.is_footnote_ref {
            if run.link_url.is_some() {
                push_str(&mut job, &mut running_chars, "[", markup_fmt.clone());
            }
            if !force_bold && run.bold {
                push_str(&mut job, &mut running_chars, "**", markup_fmt.clone());
            }
            if run.italic {
                push_str(&mut job, &mut running_chars, "*", markup_fmt.clone());
            }
            if run.underline {
                push_str(&mut job, &mut running_chars, "<u>", markup_fmt.clone());
            }
            if run.strikethrough {
                push_str(&mut job, &mut running_chars, "~~", markup_fmt.clone());
            }
            if run.code {
                push_str(&mut job, &mut running_chars, "`", markup_fmt.clone());
            }
        }

        let text_start = running_chars;
        if !search_needles.is_empty() {
            running_chars += append_bidi_highlighted_run(
                &mut job,
                &run.text,
                fmt,
                direction,
                search_needles,
                whole_word,
            );
        } else if run.text.chars().any(|c| is_rtl_char(c) || c == '\u{200D}') {
            let shaped = shape_bidi_for_display(&run.text, direction);
            push_str(&mut job, &mut running_chars, &shaped, fmt);
        } else {
            push_str(&mut job, &mut running_chars, &run.text, fmt);
        }
        let text_end = running_chars;

        if text_end > text_start {
            if run.is_footnote_ref {
                let name = clean_footnote_name(&run.text);
                if !name.is_empty() {
                    link_spans.push(GalleyLinkSpan {
                        char_range: text_start..text_end,
                        target: NavTarget::Footnote(name),
                    });
                }
            } else if let Some(ref url) = run.link_url
                && let Some(target) = classify_link_url(url) {
                    link_spans.push(GalleyLinkSpan {
                        char_range: text_start..text_end,
                        target: NavTarget::Link(url.clone(), target),
                    });
                }
        }

        if show_inline_markup && !run.is_footnote_ref {
            if run.code {
                push_str(&mut job, &mut running_chars, "`", markup_fmt.clone());
            }
            if run.strikethrough {
                push_str(&mut job, &mut running_chars, "~~", markup_fmt.clone());
            }
            if run.underline {
                push_str(&mut job, &mut running_chars, "</u>", markup_fmt.clone());
            }
            if run.italic {
                push_str(&mut job, &mut running_chars, "*", markup_fmt.clone());
            }
            if !force_bold && run.bold {
                push_str(&mut job, &mut running_chars, "**", markup_fmt.clone());
            }
            if let Some(ref url) = run.link_url {
                push_str(
                    &mut job,
                    &mut running_chars,
                    &format!("]({url})"),
                    markup_fmt.clone(),
                );
            }
        }
    }

    if direction == TextDirection::Rtl
        && let Some(prefix) = prefix_markup {
            push_str(&mut job, &mut running_chars, prefix, markup_fmt);
        }
    (job, link_spans)
}

fn is_code_keyword(lang: &str, word: &str) -> bool {
    match lang {
        "rust" | "rs" => matches!(
            word,
            "fn" | "let"
                | "mut"
                | "pub"
                | "struct"
                | "enum"
                | "impl"
                | "trait"
                | "for"
                | "in"
                | "while"
                | "loop"
                | "if"
                | "else"
                | "match"
                | "return"
                | "use"
                | "mod"
                | "crate"
                | "self"
                | "Self"
                | "super"
                | "where"
                | "async"
                | "await"
                | "const"
                | "static"
                | "type"
                | "as"
                | "ref"
                | "move"
                | "true"
                | "false"
                | "break"
                | "continue"
                | "unsafe"
                | "dyn"
        ),
        "py" | "python" => matches!(
            word,
            "def" | "class"
                | "import"
                | "from"
                | "as"
                | "if"
                | "elif"
                | "else"
                | "for"
                | "while"
                | "in"
                | "return"
                | "yield"
                | "with"
                | "try"
                | "except"
                | "finally"
                | "raise"
                | "pass"
                | "break"
                | "continue"
                | "and"
                | "or"
                | "not"
                | "is"
                | "None"
                | "True"
                | "False"
                | "lambda"
                | "async"
                | "await"
                | "global"
                | "nonlocal"
        ),
        "js" | "jsx" | "ts" | "tsx" | "javascript" | "typescript" => matches!(
            word,
            "function"
                | "const"
                | "let"
                | "var"
                | "return"
                | "if"
                | "else"
                | "for"
                | "while"
                | "switch"
                | "case"
                | "break"
                | "continue"
                | "default"
                | "import"
                | "export"
                | "from"
                | "as"
                | "async"
                | "await"
                | "class"
                | "extends"
                | "new"
                | "this"
                | "try"
                | "catch"
                | "finally"
                | "throw"
                | "typeof"
                | "instanceof"
                | "true"
                | "false"
                | "null"
                | "undefined"
                | "interface"
                | "type"
                | "enum"
        ),
        "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" | "java" | "cs" | "csharp" | "go" => matches!(
            word,
            "if" | "else"
                | "for"
                | "while"
                | "do"
                | "switch"
                | "case"
                | "break"
                | "continue"
                | "default"
                | "return"
                | "struct"
                | "class"
                | "enum"
                | "interface"
                | "public"
                | "private"
                | "protected"
                | "static"
                | "const"
                | "void"
                | "func"
                | "package"
                | "import"
                | "type"
                | "defer"
                | "go"
                | "select"
                | "chan"
                | "true"
                | "false"
                | "null"
                | "nullptr"
                | "nil"
                | "new"
                | "using"
                | "namespace"
                | "template"
        ),
        "json" => matches!(word, "true" | "false" | "null"),
        "toml" | "yaml" | "yml" => matches!(word, "true" | "false" | "null" | "yes" | "no"),
        "sh" | "bash" | "zsh" | "ps1" | "pwsh" | "powershell" => matches!(
            word,
            "if" | "then"
                | "else"
                | "elif"
                | "fi"
                | "for"
                | "in"
                | "do"
                | "done"
                | "while"
                | "case"
                | "esac"
                | "function"
                | "return"
                | "export"
                | "local"
                | "foreach"
                | "param"
                | "true"
                | "false"
        ),
        "sql" => matches!(
            word.to_ascii_uppercase().as_str(),
            "SELECT"
                | "FROM"
                | "WHERE"
                | "INSERT"
                | "INTO"
                | "VALUES"
                | "UPDATE"
                | "SET"
                | "DELETE"
                | "CREATE"
                | "TABLE"
                | "JOIN"
                | "LEFT"
                | "RIGHT"
                | "INNER"
                | "ON"
                | "GROUP"
                | "BY"
                | "ORDER"
                | "ASC"
                | "DESC"
                | "LIMIT"
                | "AND"
                | "OR"
                | "NOT"
                | "AS"
                | "NULL"
        ),
        _ => false,
    }
}

fn is_code_type(lang: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    match lang {
        "rust" | "rs" => {
            matches!(
                word,
                "u8" | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "usize"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "isize"
                    | "f32"
                    | "f64"
                    | "bool"
                    | "str"
                    | "char"
                    | "Option"
                    | "Result"
                    | "Vec"
                    | "String"
                    | "Arc"
                    | "Box"
                    | "Some"
                    | "None"
                    | "Ok"
                    | "Err"
            ) || (word.as_bytes()[0].is_ascii_uppercase()
                && word.bytes().any(|b| b.is_ascii_lowercase()))
        }
        "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" | "java" | "cs" | "csharp" | "go" | "ts"
        | "tsx" | "typescript" => {
            matches!(
                word,
                "int"
                    | "long"
                    | "short"
                    | "float"
                    | "double"
                    | "bool"
                    | "boolean"
                    | "char"
                    | "byte"
                    | "string"
                    | "number"
                    | "any"
                    | "unknown"
                    | "never"
                    | "uint8"
                    | "uint32"
                    | "int32"
                    | "int64"
            ) || (word.as_bytes()[0].is_ascii_uppercase()
                && word.bytes().any(|b| b.is_ascii_lowercase()))
        }
        _ => false,
    }
}

fn append_highlighted_code_block(
    job: &mut LayoutJob,
    code: &str,
    info: &str,
    theme: &WordTheme,
    base_fmt: TextFormat,
) {
    let lang = info
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_c_comment = matches!(
        lang.as_str(),
        "" | "rust"
            | "rs"
            | "c"
            | "cpp"
            | "cc"
            | "cxx"
            | "h"
            | "hpp"
            | "java"
            | "cs"
            | "csharp"
            | "go"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "javascript"
            | "typescript"
            | "css"
            | "scss"
            | "jsonc"
    );
    let is_hash_comment = matches!(
        lang.as_str(),
        "py" | "python" | "toml" | "yaml" | "yml" | "sh" | "bash" | "zsh" | "ps1" | "pwsh"
            | "powershell"
    );
    let is_sql = lang == "sql";
    let is_html = matches!(lang.as_str(), "html" | "htm" | "xml" | "svg");

    let bytes = code.as_bytes();
    let n = bytes.len();
    let mut i = 0usize;
    let mut plain_start = 0usize;

    let flush_span = |job: &mut LayoutJob,
                      plain_start: &mut usize,
                      start: usize,
                      end: usize,
                      color: Color32,
                      italic: bool| {
        if *plain_start < start {
            job.append(&code[*plain_start..start], 0.0, base_fmt.clone());
        }
        let mut fmt = base_fmt.clone();
        fmt.color = color;
        fmt.italics = italic;
        job.append(&code[start..end], 0.0, fmt);
        *plain_start = end;
    };

    while i < n {
        let b = bytes[i];
        if b >= 0x80 {
            i += 1;
            continue;
        }

        // Line comments
        if (is_c_comment && i + 1 < n && b == b'/' && bytes[i + 1] == b'/')
            || (is_hash_comment && b == b'#')
            || (is_sql && i + 1 < n && b == b'-' && bytes[i + 1] == b'-')
        {
            let start = i;
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            flush_span(job, &mut plain_start, start, i, theme.syn_comment, true);
            continue;
        }

        // Block comments /* ... */ or <!-- ... -->
        if (is_c_comment || is_sql) && i + 1 < n && b == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = if i + 1 < n { i + 2 } else { n };
            flush_span(job, &mut plain_start, start, i, theme.syn_comment, true);
            continue;
        }
        if is_html && i + 3 < n && &bytes[i..i + 4] == b"<!--" {
            let start = i;
            i += 4;
            while i + 2 < n && &bytes[i..i + 3] != b"-->" {
                i += 1;
            }
            i = if i + 2 < n { i + 3 } else { n };
            flush_span(job, &mut plain_start, start, i, theme.syn_comment, true);
            continue;
        }

        // String literals (avoid misclassifying Rust lifetimes like 'a or 'static)
        let is_quote = b == b'"'
            || b == b'\''
            || (b == b'`' && matches!(lang.as_str(), "js" | "jsx" | "ts" | "tsx" | "javascript" | "typescript"));
        if is_quote {
            let is_rust = matches!(lang.as_str(), "rust" | "rs");
            if b == b'\'' && is_rust {
                let mut probe = i + 1;
                let mut closed_char_lit = false;
                let limit = (i + 10).min(n);
                while probe < limit {
                    if bytes[probe] == b'\n' || bytes[probe] == b' ' {
                        break;
                    }
                    if bytes[probe] == b'\\' && probe + 1 < limit {
                        probe += 2;
                        continue;
                    }
                    if bytes[probe] == b'\'' {
                        closed_char_lit = probe > i + 1;
                        probe += 1;
                        break;
                    }
                    probe += 1;
                }
                if !closed_char_lit {
                    i += 1;
                    continue;
                }
                flush_span(job, &mut plain_start, i, probe, theme.syn_string, false);
                i = probe;
                continue;
            }
            let start = i;
            i += 1;
            while i < n {
                if bytes[i] == b'\n' && b != b'`' {
                    break;
                }
                if bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if bytes[i] == b {
                    i += 1;
                    break;
                }
                i += 1;
            }
            flush_span(job, &mut plain_start, start, i, theme.syn_string, false);
            continue;
        }

        // Numbers at identifier boundaries
        let prev_is_ident = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if !prev_is_ident && b.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < n
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
            {
                i += 1;
            }
            flush_span(job, &mut plain_start, start, i, theme.syn_number, false);
            continue;
        }

        // Keywords & Types
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < n && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &code[start..i];
            if is_code_keyword(&lang, word) {
                flush_span(job, &mut plain_start, start, i, theme.syn_keyword, false);
            } else if is_code_type(&lang, word) {
                flush_span(job, &mut plain_start, start, i, theme.syn_type, false);
            }
            continue;
        }

        i += 1;
    }

    if plain_start < n {
        job.append(&code[plain_start..n], 0.0, base_fmt);
    }
}

#[cfg(test)]
mod tests {
    use super::images::{decode_color_image, image_dimensions_within_limit, MAX_IMAGE_TEXTURE_DIM};
    use super::*;

    #[test]
    fn galley_cache_evicts_oldest_half_when_full_and_preserves_hot_entries() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {});
        let galley = ctx.fonts(|f| f.layout_no_wrap("cell".to_string(), FontId::default(), Color32::BLACK));
        let mut cache = ReaderGalleyCache::default();
        for i in 0..ReaderGalleyCache::MAX_ENTRIES {
            cache.insert_bounded((i, i + 1, i as u64), Arc::clone(&galley), Arc::new(Vec::new()));
        }
        assert_eq!(cache.entries.len(), ReaderGalleyCache::MAX_ENTRIES);

        // Touch entry 0 so it becomes newest
        assert!(cache.get(&(0, 1, 0)).is_some());

        // Inserting beyond capacity triggers bounded half-eviction
        let extra_key = (9999, 10000, 9999);
        cache.insert_bounded(extra_key, Arc::clone(&galley), Arc::new(Vec::new()));
        assert!(cache.entries.len() < ReaderGalleyCache::MAX_ENTRIES);
        assert!(cache.entries.len() >= ReaderGalleyCache::MAX_ENTRIES / 2);
        assert!(cache.get(&(0, 1, 0)).is_some());
        assert!(cache.get(&extra_key).is_some());
    }

    #[test]
    fn reader_state_caches_table_eval_and_math_preview() {
        let buf = DocumentBuffer::from_str("| A | B |\n|---|---|\n| 10 | =A1*2 |\n");
        let mut state = VirtualReaderState {
            table_eval_epoch: (1, buf.version),
            ..Default::default()
        };
        let first = state.cached_table_evaluation(&buf, 0).expect("table formula evaluated");
        assert_eq!(first.0[0][1], "20");
        let second = state.cached_table_evaluation(&buf, 0).expect("cache hit");
        assert!(Arc::ptr_eq(&first, &second));

        assert_eq!(state.cached_math_preview("2 + 3 * 4"), Some("14".to_string()));
        assert_eq!(state.cached_math_preview("2 + 3 * 4"), Some("14".to_string()));
        assert_eq!(state.math_preview_cache.len(), 1);
    }

    #[test]
    fn galley_cache_survives_index_version_changes_and_invalidates_on_theme_or_doc() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {});
        let mut state = VirtualReaderState::default();

        let runs_a = [InlineRun::plain("Unchanged paragraph")];
        let runs_b = [InlineRun::plain("Unchanged paragraph")];
        assert_ne!(runs_a.as_ptr(), runs_b.as_ptr());

        let key_for = |runs: &[InlineRun]| {
            let mut hasher = FxHasher::default();
            0x5255_4E53_u64.hash(&mut hasher);
            hash_inline_runs(runs, &mut hasher);
            (0x5255_4E53_usize, runs.len(), hasher.finish())
        };
        let key_a = key_for(&runs_a);
        let key_b = key_for(&runs_b);
        assert_eq!(key_a, key_b);

        let galley =
            ctx.fonts(|f| f.layout_no_wrap("Unchanged paragraph".to_string(), FontId::default(), Color32::BLACK));
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                state.prepare_galley_cache(
                    ui,
                    1,
                    ThemeMode::Light,
                    LineSpacing::Comfortable,
                    false,
                );
                state
                    .galley_cache
                    .insert_bounded(key_a, Arc::clone(&galley), Arc::new(Vec::new()));
                assert_eq!(state.galley_cache.entries.len(), 1);

                // Re-preparing with same doc_id and theme (even after simulated AST re-parse) retains cache!
                state.prepare_galley_cache(
                    ui,
                    1,
                    ThemeMode::Light,
                    LineSpacing::Comfortable,
                    false,
                );
                assert!(state.galley_cache.get(&key_b).is_some());

                // Changing theme invalidates cache
                state.prepare_galley_cache(
                    ui,
                    1,
                    ThemeMode::Dark,
                    LineSpacing::Comfortable,
                    false,
                );
                assert!(state.galley_cache.entries.is_empty());

                // Changing doc_id invalidates cache
                state
                    .galley_cache
                    .insert_bounded(key_a, Arc::clone(&galley), Arc::new(Vec::new()));
                state.prepare_galley_cache(
                    ui,
                    2,
                    ThemeMode::Dark,
                    LineSpacing::Comfortable,
                    false,
                );
                assert!(state.galley_cache.entries.is_empty());
            });
        });
    }

    #[test]
    fn decode_color_image_bounds_dimensions_and_downscales_oversized_textures() {
        assert!(!image_dimensions_within_limit(0, 100));
        assert!(!image_dimensions_within_limit(100, 0));
        assert!(image_dimensions_within_limit(8000, 8000));
        assert!(!image_dimensions_within_limit(9000, 8000));

        let dir = tempfile::tempdir().unwrap();
        let normal_path = dir.path().join("normal.png");
        let normal_img = image::RgbaImage::from_pixel(64, 32, image::Rgba([20, 120, 220, 255]));
        normal_img.save(&normal_path).unwrap();
        let decoded_normal = decode_color_image(&normal_path).expect("normal PNG must decode");
        assert_eq!(decoded_normal.size, [64, 32]);

        let wide_path = dir.path().join("wide.png");
        let wide_img = image::RgbaImage::from_pixel(5000, 100, image::Rgba([10, 200, 80, 255]));
        wide_img.save(&wide_path).unwrap();
        let decoded_wide = decode_color_image(&wide_path).expect("wide PNG must downscale");
        assert!(decoded_wide.size[0] <= MAX_IMAGE_TEXTURE_DIM as usize);
        assert!(decoded_wide.size[1] <= MAX_IMAGE_TEXTURE_DIM as usize);
        assert_eq!(decoded_wide.size[0], MAX_IMAGE_TEXTURE_DIM as usize);
    }

    #[test]
    fn table_eval_cache_evicts_oldest_half_preserving_hot_entries() {
        let mut cache = TableEvalCache::default();
        for i in 0..TableEvalCache::MAX_ENTRIES {
            let val = Some(Arc::new((vec![vec![i.to_string()]], vec![vec![true]])));
            cache.insert_bounded(i, val);
        }
        assert_eq!(cache.len(), TableEvalCache::MAX_ENTRIES);

        // Touch entry 0 so it becomes newest
        let hot = cache.get(0).flatten().expect("hot entry must be present");
        assert_eq!(hot.0[0][0], "0");

        // Trigger bounded half-eviction
        cache.insert_bounded(9999, None);
        assert!(cache.len() < TableEvalCache::MAX_ENTRIES);
        assert!(cache.len() >= TableEvalCache::MAX_ENTRIES / 2);
        assert!(cache.get(0).is_some(), "hot entry must survive LRU eviction");
        assert!(cache.get(9999).is_some(), "newly inserted entry must be present");
    }

    #[test]
    fn async_large_image_load_completes_without_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let large_path = dir.path().join("large_async.png");
        // Pseudo-random pixels resist PNG DEFLATE compression so file_len > 256 KB
        let mut img = image::RgbaImage::new(512, 512);
        let mut seed: u32 = 0x1234_5678;
        for px in img.pixels_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let r = (seed >> 24) as u8;
            let g = (seed >> 16) as u8;
            let b = (seed >> 8) as u8;
            *px = image::Rgba([r, g, b, 255]);
        }
        img.save(&large_path).unwrap();
        let file_len = std::fs::metadata(&large_path).unwrap().len();
        assert!(
            file_len > 256 * 1024,
            "test fixture must exceed async threshold, got {file_len} bytes"
        );

        let ctx = egui::Context::default();
        let url = large_path.to_str().unwrap();
        let mut first_was_loading = false;
        let mut loaded_size = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let mut state_out = ImageLoadState::Unavailable;
            let _ = ctx.run(Default::default(), |ctx| {
                state_out = load_cached_image_state(ctx, url, None, None);
            });
            match state_out {
                ImageLoadState::Loading => {
                    first_was_loading = true;
                }
                ImageLoadState::Loaded(tex) => {
                    loaded_size = Some(tex.size());
                    break;
                }
                ImageLoadState::Unavailable => {}
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert!(first_was_loading, "large image must enqueue on async pool");
        assert_eq!(
            loaded_size,
            Some([512, 512]),
            "async worker result must upload texture without deadlocking"
        );
    }

    #[test]
    fn decode_and_load_svg_and_animated_gif() {
        // 1. SVG with <rect> and <text> decodes at 2x scale and loads synchronously
        let svg_src = br##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="24"><rect width="80" height="24" fill="#1d5fc4"/><text x="8" y="16" font-size="12" fill="#ffffff">OK</text></svg>"##;
        let dec_svg =
            super::images::decode_image_bytes(svg_src, "badge.svg").expect("SVG must decode");
        assert_eq!(dec_svg.frames.len(), 1);
        assert_eq!(dec_svg.frames[0].image.size, [160, 48]);

        let dir = tempfile::tempdir().unwrap();
        let svg_path = dir.path().join("badge.svg");
        std::fs::write(&svg_path, svg_src).unwrap();
        let ctx = egui::Context::default();
        let mut svg_state = ImageLoadState::Unavailable;
        let _ = ctx.run(Default::default(), |ctx| {
            svg_state = load_cached_image_state(ctx, svg_path.to_str().unwrap(), None, None);
        });
        match svg_state {
            ImageLoadState::Loaded(tex) => assert_eq!(tex.size(), [160, 48]),
            ImageLoadState::Loading | ImageLoadState::Unavailable => {
                panic!("local SVG must load immediately")
            }
        }

        // 2. 2-frame animated GIF decodes both frames with delays and loads via worker pool
        let mut gif_bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif_bytes);
            let f1 = image::Frame::from_parts(
                image::RgbaImage::from_pixel(16, 16, image::Rgba([255, 0, 0, 255])),
                0,
                0,
                image::Delay::from_numer_denom_ms(120, 1),
            );
            let f2 = image::Frame::from_parts(
                image::RgbaImage::from_pixel(16, 16, image::Rgba([0, 0, 255, 255])),
                0,
                0,
                image::Delay::from_numer_denom_ms(180, 1),
            );
            encoder.encode_frames([f1, f2]).unwrap();
        }
        let dec_gif =
            super::images::decode_image_bytes(&gif_bytes, "anim.gif").expect("GIF must decode");
        assert_eq!(dec_gif.frames.len(), 2);
        assert!(dec_gif.frames[0].delay >= std::time::Duration::from_millis(100));
        assert!(dec_gif.frames[1].delay >= std::time::Duration::from_millis(100));

        let gif_path = dir.path().join("anim.gif");
        std::fs::write(&gif_path, &gif_bytes).unwrap();
        let mut gif_loaded = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let mut s = ImageLoadState::Unavailable;
            let _ = ctx.run(Default::default(), |ctx| {
                s = load_cached_image_state(ctx, gif_path.to_str().unwrap(), None, None);
            });
            if let ImageLoadState::Loaded(tex) = s {
                gif_loaded = Some(tex.size());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(gif_loaded, Some([16, 16]));
    }
}
