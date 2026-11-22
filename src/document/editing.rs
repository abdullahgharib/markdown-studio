use std::ops::Range;

use crate::document::ast::{build_table_markdown, extract_tables, TableData};
use crate::document::unicode::{
    detect_paragraph_direction, format_line_block_meta, is_extending_char, parse_line_block_meta,
    snap_to_grapheme_end, snap_to_grapheme_start, BlockAlignment, TextDirection,
};
use crate::document::DocumentBuffer;
use crate::export::ir::TableAlign;

fn snap_range_to_graphemes(buf: &DocumentBuffer, range: Range<usize>) -> Range<usize> {
    let (s_line, s_col) = buf.char_to_line_col(range.start);
    let s_str = buf.line_text(s_line);
    let s_snap = snap_to_grapheme_start(&s_str, s_col);
    let start = buf.line_col_to_char(s_line, s_snap);

    let (e_line, e_col) = buf.char_to_line_col(range.end);
    let e_str = buf.line_text(e_line);
    let e_snap = snap_to_grapheme_end(&e_str, e_col);
    let end = buf.line_col_to_char(e_line, e_snap);
    if start < end {
        start..end
    } else {
        range
    }
}

/// Checks if `sel` on a single line is wrapped by `(open_m, close_m)`, possibly nested inside or outside other inline markers.
fn find_surrounding_marker_layer(
    buf: &DocumentBuffer,
    sel: Range<usize>,
    open_m: &str,
    close_m: &str,
) -> Option<(Range<usize>, Range<usize>)> {
    let (s_line, _) = buf.char_to_line_col(sel.start);
    let (e_line, _) = buf.char_to_line_col(sel.end.saturating_sub(1).max(sel.start));
    if s_line != e_line {
        return None;
    }
    let line_start = buf.line_to_char(s_line);
    let line_str = buf.line_text(s_line);
    let line_chars: Vec<char> = line_str.chars().collect();
    let local_s = sel.start.saturating_sub(line_start).min(line_chars.len());
    let local_e = sel.end.saturating_sub(line_start).min(line_chars.len());

    let wrappers: [(&str, &str); 6] = [
        ("<u>", "</u>"),
        ("**", "**"),
        ("~~", "~~"),
        ("`", "`"),
        ("$", "$"),
        ("*", "*"),
    ];

    let mut cur_s = local_s;
    let mut cur_e = local_e;

    loop {
        let mut peeled = false;
        let before: String = line_chars[..cur_s].iter().collect();
        let after: String = line_chars[cur_e..].iter().collect();
        for &(om, cm) in &wrappers {
            let om_chars = om.chars().count();
            let cm_chars = cm.chars().count();
            if before.ends_with(om) && after.starts_with(cm) {
                // Prevent splitting a single `*` or `$` out of `**...**` / `$$...$$`.
                if (om == "*" || om == "$")
                    && before.ends_with(&om.repeat(2))
                    && after.starts_with(&cm.repeat(2))
                {
                    let ch = om.chars().next().unwrap();
                    let b_cnt = line_chars[..cur_s].iter().rev().take_while(|&&c| c == ch).count();
                    let a_cnt = line_chars[cur_e..].iter().take_while(|&&c| c == ch).count();
                    if b_cnt % 2 == 0 && a_cnt % 2 == 0 {
                        continue;
                    }
                }
                let open_range = (line_start + cur_s - om_chars)..(line_start + cur_s);
                let close_range = (line_start + cur_e)..(line_start + cur_e + cm_chars);
                if om == open_m && cm == close_m {
                    return Some((open_range, close_range));
                }
                cur_s -= om_chars;
                cur_e += cm_chars;
                peeled = true;
                break;
            }
        }
        if !peeled {
            break;
        }
    }
    None
}

/// Returns the char column after any leading block metadata (`<!-- ... -->`) and structural
/// Markdown prefix (`# `, `- [ ] `, `- `, `1. `, `> `) on `line_str`.
pub fn content_start_col(line_str: &str) -> usize {
    let (_, _, without_meta) = parse_line_block_meta(line_str);
    let content_h = strip_heading_prefix(without_meta);
    let content = if content_h.len() != without_meta.len() {
        content_h
    } else {
        strip_list_or_quote_prefix(without_meta)
    };
    let prefix_bytes = line_str.len().saturating_sub(content.len());
    line_str[..prefix_bytes].chars().count()
}

/// Returns the grapheme-snapped char range of the word at `col_chars` on `line_str`,
/// or `None` if `col_chars` is on whitespace, punctuation, or Markdown syntax delimiters.
pub fn word_range_in_line(line_str: &str, col_chars: usize) -> Option<Range<usize>> {
    let chars: Vec<char> = line_str.chars().collect();
    let len = chars.len();
    if len == 0 {
        return None;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || is_extending_char(c);
    let pos = col_chars.min(len);
    let anchor = if pos < len && is_word(chars[pos]) {
        pos
    } else if pos > 0 && is_word(chars[pos - 1]) {
        pos - 1
    } else {
        return None;
    };

    let mut start = anchor;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = anchor + 1;
    while end < len && is_word(chars[end]) {
        end += 1;
    }
    let start = snap_to_grapheme_start(line_str, start);
    let end = snap_to_grapheme_end(line_str, end);
    if start < end
        && chars[start..end]
            .iter()
            .any(|&c| c.is_alphanumeric() || c == '_')
    {
        Some(start..end)
    } else {
        None
    }
}

fn trim_selection_whitespace(buf: &DocumentBuffer, range: Range<usize>) -> Range<usize> {
    let mut s = range.start;
    let mut e = range.end.min(buf.len_chars());
    while s < e && buf.rope().char(s).is_whitespace() {
        s += 1;
    }
    while e > s && buf.rope().char(e - 1).is_whitespace() {
        e -= 1;
    }
    s..e
}

fn has_unnested_delimiter(inner: &str, open_marker: &str, close_marker: &str) -> bool {
    let bytes = inner.as_bytes();
    if open_marker == close_marker {
        let m_bytes = open_marker.as_bytes();
        let m_len = m_bytes.len();
        let all_same = m_bytes.iter().all(|&b| b == m_bytes[0]);
        let mut i = 0usize;
        while i + m_len <= bytes.len() {
            if all_same && bytes[i] == m_bytes[0] {
                let run = bytes[i..].iter().take_while(|&&b| b == m_bytes[0]).count();
                if open_marker == "*" {
                    if run % 2 == 1 {
                        return true;
                    }
                } else if run == m_len {
                    return true;
                }
                i += run;
                continue;
            } else if !all_same && &bytes[i..i + m_len] == m_bytes {
                return true;
            }
            i += 1;
        }
        false
    } else {
        let mut depth = 1isize;
        let mut i = 0usize;
        while i < inner.len() {
            if inner[i..].starts_with(open_marker) {
                depth += 1;
                i += open_marker.len();
            } else if inner[i..].starts_with(close_marker) {
                depth -= 1;
                if depth <= 0 {
                    return true;
                }
                i += close_marker.len();
            } else {
                i += inner[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
        }
        false
    }
}

fn is_wrapped_inside(selected: &str, open_marker: &str, close_marker: &str) -> bool {
    let open_chars = open_marker.chars().count();
    let close_chars = close_marker.chars().count();
    if selected.chars().count() < open_chars + close_chars
        || !selected.starts_with(open_marker)
        || !selected.ends_with(close_marker)
    {
        return false;
    }
    if open_marker == "*" && close_marker == "*" {
        let lead = selected.bytes().take_while(|&b| b == b'*').count();
        let trail = selected.bytes().rev().take_while(|&b| b == b'*').count();
        if lead % 2 == 0 || trail % 2 == 0 {
            return false;
        }
    }
    let inner = &selected[open_marker.len()..selected.len() - close_marker.len()];
    !has_unnested_delimiter(inner, open_marker, close_marker)
}

fn toggle_inline_wrap_multiline(
    buf: &mut DocumentBuffer,
    raw_sel: Range<usize>,
    s_line: usize,
    e_line: usize,
    open_marker: &str,
    close_marker: &str,
) {
    let open_chars = open_marker.chars().count();
    let close_chars = close_marker.chars().count();

    struct LineSpanEdit {
        line_idx: usize,
        line_str: String,
        local_sel: Range<usize>,
        already_inside: bool,
        surrounding: Option<(Range<usize>, Range<usize>)>,
        tick_surround: Option<(Range<usize>, Range<usize>)>,
    }

    let mut edits = Vec::new();
    for l in s_line..=e_line {
        let line_start = buf.line_to_char(l);
        let line_len = buf.line_len_without_newline(l);
        let line_str = buf.line_text(l);
        let c_start = content_start_col(&line_str);
        let ov_s = raw_sel.start.max(line_start + c_start);
        let ov_e = raw_sel.end.min(line_start + line_len);
        if ov_s >= ov_e {
            continue;
        }
        let trimmed = trim_selection_whitespace(buf, ov_s..ov_e);
        if trimmed.start >= trimmed.end {
            continue;
        }
        let snapped = snap_range_to_graphemes(buf, trimmed);
        let selected = buf.rope().slice(snapped.clone()).to_string();
        let already_inside = is_wrapped_inside(&selected, open_marker, close_marker);
        let surrounding = if already_inside {
            None
        } else {
            find_surrounding_marker_layer(buf, snapped.clone(), open_marker, close_marker)
        };
        let tick_surround = if !already_inside && surrounding.is_none() && open_marker != "`" {
            find_surrounding_marker_layer(buf, snapped.clone(), "`", "`")
        } else {
            None
        };
        edits.push(LineSpanEdit {
            line_idx: l,
            line_str,
            local_sel: (snapped.start - line_start)..(snapped.end - line_start),
            already_inside,
            surrounding: surrounding.map(|(o, c)| {
                (
                    (o.start - line_start)..(o.end - line_start),
                    (c.start - line_start)..(c.end - line_start),
                )
            }),
            tick_surround: tick_surround.map(|(o, c)| {
                (
                    (o.start - line_start)..(o.end - line_start),
                    (c.start - line_start)..(c.end - line_start),
                )
            }),
        });
    }

    if edits.is_empty() {
        return;
    }

    let unwrap_all = edits
        .iter()
        .all(|e| e.already_inside || e.surrounding.is_some());

    let mut new_lines = Vec::with_capacity(e_line - s_line + 1);
    let mut new_sel_start: Option<usize> = None;
    let mut new_sel_end: usize = 0;
    let block_start = buf.line_to_char(s_line);
    let mut running_chars = 0usize;

    let mut edit_iter = edits.into_iter().peekable();
    for l in s_line..=e_line {
        if edit_iter.peek().is_some_and(|e| e.line_idx == l) {
            let e = edit_iter.next().unwrap();
            let chars: Vec<char> = e.line_str.chars().collect();
            let (new_line, local_s, local_e) = if unwrap_all {
                if e.already_inside {
                    let before: String = chars[..e.local_sel.start].iter().collect();
                    let inner: String = chars[(e.local_sel.start + open_chars)..(e.local_sel.end - close_chars)]
                        .iter()
                        .collect();
                    let after: String = chars[e.local_sel.end..].iter().collect();
                    let inner_len = inner.chars().count();
                    (
                        format!("{before}{inner}{after}"),
                        e.local_sel.start,
                        e.local_sel.start + inner_len,
                    )
                } else if let Some((open_r, close_r)) = e.surrounding {
                    let before: String = chars[..open_r.start].iter().collect();
                    let inner: String = chars[open_r.end..close_r.start].iter().collect();
                    let after: String = chars[close_r.end..].iter().collect();
                    (
                        format!("{before}{inner}{after}"),
                        e.local_sel.start - open_chars,
                        e.local_sel.end - open_chars,
                    )
                } else {
                    (e.line_str, e.local_sel.start, e.local_sel.end)
                }
            } else if e.already_inside || e.surrounding.is_some() {
                (e.line_str, e.local_sel.start, e.local_sel.end)
            } else if let Some((open_t, close_t)) = e.tick_surround {
                let before: String = chars[..open_t.start].iter().collect();
                let inner: String = chars[open_t.start..close_t.end].iter().collect();
                let after: String = chars[close_t.end..].iter().collect();
                let wrapped_len = open_chars + inner.chars().count() + close_chars;
                (
                    format!("{before}{open_marker}{inner}{close_marker}{after}"),
                    open_t.start,
                    open_t.start + wrapped_len,
                )
            } else {
                let before: String = chars[..e.local_sel.start].iter().collect();
                let inner: String = chars[e.local_sel.start..e.local_sel.end].iter().collect();
                let after: String = chars[e.local_sel.end..].iter().collect();
                let wrapped_len = open_chars + inner.chars().count() + close_chars;
                (
                    format!("{before}{open_marker}{inner}{close_marker}{after}"),
                    e.local_sel.start,
                    e.local_sel.start + wrapped_len,
                )
            };

            let le = buf.line_ending();
            let le_chars = le.chars().count();
            if new_sel_start.is_none() {
                new_sel_start = Some(block_start + running_chars + local_s);
            }
            new_sel_end = block_start + running_chars + local_e;
            running_chars += new_line.chars().count() + le_chars;
            new_lines.push(new_line);
        } else {
            let le_chars = buf.line_ending().chars().count();
            let raw = buf.line_text(l);
            running_chars += raw.chars().count() + le_chars;
            new_lines.push(raw);
        }
    }

    let le = buf.line_ending();
    let range = buf.line_range_chars(s_line, e_line);
    buf.replace_range(range, &new_lines.join(le));
    if let Some(s) = new_sel_start {
        buf.selection_anchor = Some(s);
        buf.cursor_char = new_sel_end;
        buf.cache_current_selection();
    }
}

/// Toggles inline formatting delimiter pairs (symmetric like `**` or asymmetric like `<u>`/`</u>`)
/// while preserving selection on grapheme-cluster boundaries.
pub fn toggle_inline_wrap_pair(
    buf: &mut DocumentBuffer,
    open_marker: &str,
    close_marker: &str,
) {
    buf.restore_cached_selection();
    let open_chars = open_marker.chars().count();

    if let Some(raw_sel) = buf.selection_range() {
        let s_line = buf.char_to_line(raw_sel.start);
        let e_line = buf.char_to_line(raw_sel.end.saturating_sub(1).max(raw_sel.start));
        if s_line < e_line {
            toggle_inline_wrap_multiline(buf, raw_sel, s_line, e_line, open_marker, close_marker);
            return;
        }

        let line_start = buf.line_to_char(s_line);
        let line_str = buf.line_text(s_line);
        let content_abs_start = line_start + content_start_col(&line_str);
        if raw_sel.end <= content_abs_start {
            return;
        }
        let clamped_start = raw_sel.start.max(content_abs_start);
        let trimmed = trim_selection_whitespace(buf, clamped_start..raw_sel.end);
        if trimmed.start >= trimmed.end {
            return;
        }

        let sel = snap_range_to_graphemes(buf, trimmed);
        let selected = buf.rope().slice(sel.clone()).to_string();

        // 1) Check if selection itself includes the open + close markers at its ends
        if is_wrapped_inside(&selected, open_marker, close_marker) {
            let inner = &selected[open_marker.len()..selected.len() - close_marker.len()];
            let inner_owned = inner.to_string();
            let start = sel.start;
            let inner_len = inner_owned.chars().count();
            buf.replace_range(sel, &inner_owned);
            buf.selection_anchor = Some(start);
            buf.cursor_char = start + inner_len;
            buf.cache_current_selection();
            return;
        }

        // 2) Check surrounding / nested marker onion around selection
        if let Some((open_r, close_r)) =
            find_surrounding_marker_layer(buf, sel.clone(), open_marker, close_marker)
        {
            let before_mid = open_r.end;
            let after_mid = close_r.start;
            let mid_text = buf.rope().slice(before_mid..after_mid).to_string();
            let full_replacement = mid_text;
            let new_sel_start = sel.start - open_chars;
            let new_sel_end = sel.end - open_chars;
            buf.replace_range(open_r.start..close_r.end, &full_replacement);
            buf.selection_anchor = Some(new_sel_start);
            buf.cursor_char = new_sel_end;
            buf.cache_current_selection();
            return;
        }

        // 3) Wrap selection cleanly (expanding outside surrounding inline code backticks if needed)
        let wrap_sel = if open_marker != "`" {
            if let Some((open_tick, close_tick)) =
                find_surrounding_marker_layer(buf, sel.clone(), "`", "`")
            {
                open_tick.start..close_tick.end
            } else {
                sel.clone()
            }
        } else {
            sel.clone()
        };
        let wrap_text = buf.rope().slice(wrap_sel.clone()).to_string();
        let delta_left = sel.start.saturating_sub(wrap_sel.start);
        let inner_sel_len = sel.end - sel.start;
        let wrapped = format!("{open_marker}{wrap_text}{close_marker}");
        let start = wrap_sel.start;
        buf.replace_range(wrap_sel, &wrapped);
        let new_start = start + open_chars + delta_left;
        buf.selection_anchor = Some(new_start);
        buf.cursor_char = new_start + inner_sel_len;
        buf.cache_current_selection();
    } else {
        // No selection: if inside word, wrap the word; otherwise insert pair and place cursor inside
        if let Some(word_range) = word_range_at_cursor(buf) {
            let line = buf.char_to_line(word_range.start);
            let line_start = buf.line_to_char(line);
            let line_str = buf.line_text(line);
            let content_abs_start = line_start + content_start_col(&line_str);
            if word_range.start >= content_abs_start {
                buf.selection_anchor = Some(word_range.start);
                buf.cursor_char = word_range.end;
                buf.cache_current_selection();
                toggle_inline_wrap_pair(buf, open_marker, close_marker);
                return;
            }
        }
        buf.snap_cursor_to_grapheme();
        let pos = buf.cursor_char;
        let pair = format!("{open_marker}{close_marker}");
        buf.insert(pos, &pair);
        buf.set_cursor(pos + open_chars, false);
    }
}

/// Toggles inline Markdown wrapper tokens (e.g., `**`, `*`, `` ` ``, `~~`) around selection or cursor.
pub fn toggle_inline_wrap(buf: &mut DocumentBuffer, marker: &str) {
    toggle_inline_wrap_pair(buf, marker, marker);
}

fn word_range_at_cursor(buf: &DocumentBuffer) -> Option<Range<usize>> {
    let len = buf.len_chars();
    if len == 0 {
        return None;
    }
    let pos = buf.cursor_char.min(len);
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || is_extending_char(c);

    let on_word = (pos < len && is_word(buf.rope().char(pos)))
        || (pos > 0 && is_word(buf.rope().char(pos - 1)));
    if !on_word {
        return None;
    }
    let mut start = pos;
    while start > 0 && is_word(buf.rope().char(start - 1)) {
        start -= 1;
    }
    let mut end = pos;
    while end < len && is_word(buf.rope().char(end)) {
        end += 1;
    }
    let snapped = snap_range_to_graphemes(buf, start..end);
    let has_base = (snapped.start..snapped.end).any(|i| {
        let c = buf.rope().char(i);
        c.is_alphanumeric() || c == '_'
    });
    if snapped.start < snapped.end && has_base {
        Some(snapped)
    } else {
        None
    }
}

pub fn toggle_bold(buf: &mut DocumentBuffer) {
    toggle_inline_wrap(buf, "**");
}

pub fn toggle_italic(buf: &mut DocumentBuffer) {
    toggle_inline_wrap(buf, "*");
}

pub fn toggle_underline(buf: &mut DocumentBuffer) {
    toggle_inline_wrap_pair(buf, "<u>", "</u>");
}

pub fn toggle_strikethrough(buf: &mut DocumentBuffer) {
    toggle_inline_wrap(buf, "~~");
}

pub fn toggle_inline_code(buf: &mut DocumentBuffer) {
    toggle_inline_wrap(buf, "`");
}

fn selected_line_span(buf: &DocumentBuffer) -> (usize, usize) {
    if let Some(sel) = buf.selection_range() {
        let start_line = buf.char_to_line(sel.start);
        let end_char = sel.end.saturating_sub(1).max(sel.start);
        let end_line = buf.char_to_line(end_char);
        (start_line, end_line)
    } else {
        let line = buf.char_to_line(buf.cursor_char);
        (line, line)
    }
}

/// Replaces lines `[start_line, end_line]` while mapping cursor and selection through per-line prefix deltas.
fn apply_line_transforms(
    buf: &mut DocumentBuffer,
    start_line: usize,
    end_line: usize,
    transformed: &[(usize, usize, String)],
) {
    let range = buf.line_range_chars(start_line, end_line);
    let old_cur = buf.cursor_char;
    let old_anc = buf.selection_anchor;

    let (cur_l, cur_c) = buf.char_to_line_col(old_cur);
    let anc_lc = old_anc.map(|a| buf.char_to_line_col(a));

    let map_col = |line_idx: usize, col: usize, old_pref: usize, new_pref: usize, new_line: &str| {
        let _ = line_idx;
        let max_c = new_line.chars().count();
        if col >= old_pref {
            (col - old_pref + new_pref).min(max_c)
        } else {
            new_pref.min(max_c)
        }
    };

    let joined: Vec<&str> = transformed.iter().map(|(_, _, s)| s.as_str()).collect();
    let le = buf.line_ending();
    buf.replace_range(range, &joined.join(le));

    let resolve_char = |l: usize, c: usize| -> usize {
        if l >= start_line && l <= end_line {
            let (old_p, new_p, ref new_line) = transformed[l - start_line];
            let mapped = map_col(l, c, old_p, new_p, new_line);
            buf.line_col_to_char(l, mapped)
        } else {
            buf.line_col_to_char(l, c)
        }
    };

    let new_cur = resolve_char(cur_l, cur_c);
    if let Some((al, ac)) = anc_lc {
        let new_anc = resolve_char(al, ac);
        if new_anc != new_cur {
            buf.selection_anchor = Some(new_anc);
            buf.cursor_char = new_cur;
            buf.cache_current_selection();
            return;
        }
    }
    buf.set_cursor(new_cur, false);
}

fn strip_heading_prefix(line: &str) -> &str {
    let trimmed = line.trim_start_matches(' ');
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) {
        let rest = &trimmed[hashes..];
        if let Some(after_space) = rest.strip_prefix(' ') {
            return after_space;
        }
        if rest.is_empty() {
            return "";
        }
    }
    line
}

fn strip_list_or_quote_prefix(line: &str) -> &str {
    let trimmed = line.trim_start_matches(' ');
    if let Some(rest) = trimmed
        .strip_prefix("- [ ] ")
        .or_else(|| trimmed.strip_prefix("- [x] "))
        .or_else(|| trimmed.strip_prefix("- [X] "))
        .or_else(|| trimmed.strip_prefix("* [ ] "))
        .or_else(|| trimmed.strip_prefix("* [x] "))
        .or_else(|| trimmed.strip_prefix("* [X] "))
        .or_else(|| trimmed.strip_prefix("+ [ ] "))
        .or_else(|| trimmed.strip_prefix("+ [x] "))
        .or_else(|| trimmed.strip_prefix("+ [X] "))
    {
        return rest;
    }
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .or_else(|| trimmed.strip_prefix("> "))
    {
        return rest;
    }
    if trimmed == ">" {
        return "";
    }
    let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 {
        let after_digits = &trimmed[digits..];
        if let Some(rest) = after_digits
            .strip_prefix(". ")
            .or_else(|| after_digits.strip_prefix(") "))
        {
            return rest;
        }
    }
    line
}

/// Sets ATX heading level (`Some(1..=6)`) or converts back to normal paragraph (`None`), preserving selection.
pub fn set_heading(buf: &mut DocumentBuffer, level: Option<u8>) {
    buf.restore_cached_selection();
    let (start_line, end_line) = selected_line_span(buf);
    let mut transformed = Vec::with_capacity(end_line - start_line + 1);

    for l in start_line..=end_line {
        let raw = buf.line_text(l);
        let (align, indent, without_meta) = parse_line_block_meta(&raw);
        let meta_prefix = format_line_block_meta(align, indent);
        let stripped_h = strip_heading_prefix(without_meta);
        let clean = if stripped_h == without_meta {
            strip_list_or_quote_prefix(without_meta)
        } else {
            stripped_h
        };
        let old_pref_chars = raw.chars().count() - clean.chars().count();
        let (marker, body) = match level {
            Some(lvl) if (1..=6).contains(&lvl) => (format!("{} ", "#".repeat(lvl as usize)), clean),
            _ => (String::new(), clean),
        };
        let new_pref_chars = meta_prefix.chars().count() + marker.chars().count();
        transformed.push((old_pref_chars, new_pref_chars, format!("{meta_prefix}{marker}{body}")));
    }

    apply_line_transforms(buf, start_line, end_line, &transformed);
}

/// Returns the heading level (1..=6) if the cursor's current line is an ATX heading.
pub fn current_heading_level(buf: &DocumentBuffer) -> Option<u8> {
    let line = buf.char_to_line(buf.cursor_char);
    let raw = buf.line_text(line);
    let (_, _, without_meta) = parse_line_block_meta(&raw);
    let trimmed = without_meta.trim_start_matches(' ');
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) {
        let rest = &trimmed[hashes..];
        if rest.is_empty() || rest.starts_with(' ') {
            return Some(hashes as u8);
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineBlockKind {
    BulletList,
    NumberedList,
    TaskList,
    BlockQuote,
}

fn line_block_kind_of_rest(rest: &str) -> Option<LineBlockKind> {
    let t = rest.trim_start_matches(' ');
    if t.starts_with("- [ ] ")
        || t.starts_with("- [x] ")
        || t.starts_with("- [X] ")
        || t.starts_with("* [ ] ")
        || t.starts_with("* [x] ")
        || t.starts_with("* [X] ")
        || t.starts_with("+ [ ] ")
        || t.starts_with("+ [x] ")
        || t.starts_with("+ [X] ")
    {
        return Some(LineBlockKind::TaskList);
    }
    if (t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ "))
        && !t.starts_with("- [")
        && !t.starts_with("* [")
        && !t.starts_with("+ [")
    {
        return Some(LineBlockKind::BulletList);
    }
    let d = t.bytes().take_while(|b| b.is_ascii_digit()).count();
    if d > 0 && (t[d..].starts_with(". ") || t[d..].starts_with(") ")) {
        return Some(LineBlockKind::NumberedList);
    }
    if t.starts_with("> ") || t == ">" {
        return Some(LineBlockKind::BlockQuote);
    }
    None
}

/// Toggles bullet list, numbered list, task list, or blockquote on the current/selected lines.
pub fn toggle_line_prefix(buf: &mut DocumentBuffer, kind: LineBlockKind) {
    buf.restore_cached_selection();
    let (start_line, end_line) = selected_line_span(buf);
    let raw_lines: Vec<String> = (start_line..=end_line).map(|l| buf.line_text(l)).collect();

    let all_already = raw_lines.iter().all(|raw| {
        let (_, _, rest) = parse_line_block_meta(raw);
        line_block_kind_of_rest(rest) == Some(kind)
    });

    let mut transformed = Vec::with_capacity(raw_lines.len());
    for (idx, raw) in raw_lines.iter().enumerate() {
        let (align, indent, without_meta) = parse_line_block_meta(raw);
        let meta_prefix = format_line_block_meta(align, indent);
        let content = strip_list_or_quote_prefix(strip_heading_prefix(without_meta));
        let old_pref_chars = raw.chars().count() - content.chars().count();
        let marker = if all_already {
            String::new()
        } else {
            match kind {
                LineBlockKind::BulletList => "- ".to_string(),
                LineBlockKind::NumberedList => format!("{}. ", idx + 1),
                LineBlockKind::TaskList => "- [ ] ".to_string(),
                LineBlockKind::BlockQuote => "> ".to_string(),
            }
        };
        let new_pref_chars = meta_prefix.chars().count() + marker.chars().count();
        transformed.push((
            old_pref_chars,
            new_pref_chars,
            format!("{meta_prefix}{marker}{content}"),
        ));
    }

    apply_line_transforms(buf, start_line, end_line, &transformed);
}

/// Sets paragraph/block alignment across selected lines while preserving selection and cursor.
pub fn set_block_alignment(buf: &mut DocumentBuffer, target: BlockAlignment) {
    buf.restore_cached_selection();
    let (start_line, end_line) = selected_line_span(buf);
    let raw_lines: Vec<String> = (start_line..=end_line).map(|l| buf.line_text(l)).collect();

    let all_target = target != BlockAlignment::Start
        && raw_lines
            .iter()
            .all(|raw| parse_line_block_meta(raw).0 == target);
    let next_align = if all_target {
        BlockAlignment::Start
    } else {
        target
    };

    let mut transformed = Vec::with_capacity(raw_lines.len());
    for raw in &raw_lines {
        let (_cur_align, indent, rest) = parse_line_block_meta(raw);
        let old_meta_chars = raw.chars().count() - rest.chars().count();
        let new_meta = format_line_block_meta(next_align, indent);
        let new_meta_chars = new_meta.chars().count();
        transformed.push((old_meta_chars, new_meta_chars, format!("{new_meta}{rest}")));
    }

    apply_line_transforms(buf, start_line, end_line, &transformed);
}

/// Increases logical start indentation on the current/selected lines while preserving selection.
pub fn indent_lines(buf: &mut DocumentBuffer) {
    buf.restore_cached_selection();
    let (start_line, end_line) = selected_line_span(buf);
    let mut transformed = Vec::with_capacity(end_line - start_line + 1);

    for l in start_line..=end_line {
        let raw = buf.line_text(l);
        let (align, indent, rest) = parse_line_block_meta(&raw);
        let old_meta_chars = raw.chars().count() - rest.chars().count();
        let next_indent = indent.saturating_add(1).min(8);
        let new_meta = format_line_block_meta(align, next_indent);
        let new_meta_chars = new_meta.chars().count();
        transformed.push((old_meta_chars, new_meta_chars, format!("{new_meta}{rest}")));
    }

    apply_line_transforms(buf, start_line, end_line, &transformed);
}

/// Decreases logical start indentation on the current/selected lines while preserving selection.
pub fn outdent_lines(buf: &mut DocumentBuffer) {
    buf.restore_cached_selection();
    let (start_line, end_line) = selected_line_span(buf);
    let mut transformed = Vec::with_capacity(end_line - start_line + 1);

    for l in start_line..=end_line {
        let raw = buf.line_text(l);
        let (align, indent, rest) = parse_line_block_meta(&raw);
        let old_meta_chars = raw.chars().count() - rest.chars().count();
        if indent > 0 {
            let next_indent = indent - 1;
            let new_meta = format_line_block_meta(align, next_indent);
            let new_meta_chars = new_meta.chars().count();
            transformed.push((old_meta_chars, new_meta_chars, format!("{new_meta}{rest}")));
        } else {
            let leading = if rest.starts_with('\t') {
                1
            } else {
                rest.bytes().take(4).take_while(|&b| b == b' ').count()
            };
            let stripped = &rest[leading..];
            let new_meta = format_line_block_meta(align, 0);
            transformed.push((
                old_meta_chars + leading,
                new_meta.chars().count(),
                format!("{new_meta}{stripped}"),
            ));
        }
    }

    apply_line_transforms(buf, start_line, end_line, &transformed);
}

/// Current active formatting & layout state reflected in the toolbar UI.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActiveFormatState {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub inline_code: bool,
    pub heading_level: Option<u8>,
    pub block_kind: Option<LineBlockKind>,
    pub alignment: BlockAlignment,
    pub indent_level: u8,
    pub direction: TextDirection,
}

fn line_cursor_inside_marker(line_str: &str, col: usize, open_m: &str, close_m: &str) -> bool {
    let bytes = line_str.as_bytes();
    let col_b = line_str
        .char_indices()
        .nth(col)
        .map(|(b, _)| b)
        .unwrap_or(line_str.len());

    let mut i = 0usize;
    let mut active = false;
    while i < bytes.len() {
        if i >= col_b {
            break;
        }
        if open_m == "*" && close_m == "*" {
            if line_str[i..].starts_with("***") {
                active = !active;
                i += 3;
                continue;
            }
            if line_str[i..].starts_with("**") {
                i += 2;
                continue;
            }
        }
        if !active && line_str[i..].starts_with(open_m) {
            if let Some(rel_close) = line_str[i + open_m.len()..].find(close_m) {
                let end_inner = i + open_m.len() + rel_close;
                if col_b >= i + open_m.len() && col_b <= end_inner {
                    return true;
                }
                i = end_inner + close_m.len();
                continue;
            }
        }
        let ch_len = line_str[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        i += ch_len;
    }
    active
}

/// Queries all active inline marks and block-level attributes at the selection or cursor.
pub fn query_active_format(buf: &DocumentBuffer) -> ActiveFormatState {
    let eff_sel = buf.selection_range().or_else(|| {
        if buf.cached_selection_version == buf.version {
            if let Some((a, c)) = buf.cached_selection {
                let len = buf.len_chars();
                let a = a.min(len);
                let c = c.min(len);
                if a != c {
                    return Some(a.min(c)..a.max(c));
                }
            }
        }
        None
    });

    let probe_char = eff_sel
        .as_ref()
        .map(|s| s.start)
        .unwrap_or_else(|| buf.cursor_char.min(buf.len_chars()));
    let (line_idx, col) = buf.char_to_line_col(probe_char);
    let raw_line = buf.line_text(line_idx);
    let (alignment, indent_level, rest) = parse_line_block_meta(&raw_line);
    let direction = detect_paragraph_direction(rest);

    let trimmed = rest.trim_start_matches(' ');
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    let heading_level = if (1..=6).contains(&hashes) {
        let after = &trimmed[hashes..];
        if after.is_empty() || after.starts_with(' ') {
            Some(hashes as u8)
        } else {
            None
        }
    } else {
        None
    };

    let block_kind = line_block_kind_of_rest(rest);

    // Inline wrappers are single-line; skip massive selections (e.g. Ctrl+A on 10 MB) in O(1).
    let single_line_sel = eff_sel.filter(|sel| {
        if sel.end.saturating_sub(sel.start) > 4096 {
            return false;
        }
        let end_line = buf.char_to_line(sel.end.saturating_sub(1).max(sel.start));
        end_line == line_idx
    });
    let sel_text = single_line_sel
        .as_ref()
        .map(|sel| buf.rope().slice(sel.clone()).to_string());

    let check_marker = |open_m: &str, close_m: &str| -> bool {
        if let (Some(ref sel), Some(ref text)) = (&single_line_sel, &sel_text) {
            if is_wrapped_inside(text, open_m, close_m)
                || find_surrounding_marker_layer(buf, sel.clone(), open_m, close_m).is_some()
            {
                return true;
            }
        }
        line_cursor_inside_marker(&raw_line, col, open_m, close_m)
    };

    ActiveFormatState {
        bold: check_marker("**", "**"),
        italic: check_marker("*", "*"),
        underline: check_marker("<u>", "</u>"),
        strikethrough: check_marker("~~", "~~"),
        inline_code: check_marker("`", "`"),
        heading_level,
        block_kind,
        alignment,
        indent_level,
        direction,
    }
}

/// Parses an opening fenced code block line (` ```lang `, ` ~~~lang `, or `$$`) into
/// `(leading_spaces, fence_char, fence_len, trimmed_info_string)`.
pub fn parse_code_fence_line(line: &str) -> Option<(usize, char, usize, String)> {
    let clean = line.trim_end_matches(['\r', '\n']);
    let lead_spaces = clean.bytes().take_while(|&b| b == b' ').count();
    if lead_spaces > 3 {
        return None;
    }
    let rest = &clean[lead_spaces..];
    let first = *rest.as_bytes().first()?;
    if first == b'$' {
        if rest.starts_with("$$") && rest[2..].trim().is_empty() {
            return Some((lead_spaces, '$', 2, "math".to_string()));
        }
        return None;
    }
    if first != b'`' && first != b'~' {
        return None;
    }
    let fence_len = rest.bytes().take_while(|&b| b == first).count();
    if fence_len < 3 {
        return None;
    }
    let after = &rest[fence_len..];
    if first == b'`' && after.contains('`') {
        return None;
    }
    Some((lead_spaces, first as char, fence_len, after.trim().to_string()))
}

/// Updates the info/language string on a fenced code block's opening fence line (`open_line`),
/// preserving indentation, fence delimiter style/length, CRLF, and interior cursor position.
pub fn set_code_block_language(
    buf: &mut DocumentBuffer,
    open_line: usize,
    new_lang: &str,
) -> bool {
    if open_line >= buf.len_lines() {
        return false;
    }
    let old_line = buf.line_text(open_line);
    let Some((lead_spaces, fence_char, fence_len, _old_info)) = parse_code_fence_line(&old_line)
    else {
        return false;
    };
    let mut sanitized: String = new_lang
        .chars()
        .filter(|&c| c != '\r' && c != '\n')
        .collect();
    if fence_char == '`' {
        sanitized.retain(|c| c != '`');
    }
    let sanitized = sanitized.trim();
    let replacement = if fence_char == '$' {
        if sanitized.eq_ignore_ascii_case("math") {
            return false;
        }
        format!("{}{}{}", " ".repeat(lead_spaces), "```", sanitized)
    } else {
        format!(
            "{}{}{}",
            " ".repeat(lead_spaces),
            fence_char.to_string().repeat(fence_len),
            sanitized
        )
    };
    if replacement == old_line {
        return false;
    }

    let range = buf.line_range_chars(open_line, open_line);
    let prev_cursor = buf.cursor_char;
    let prev_anchor = buf.selection_anchor;
    let old_chars = old_line.chars().count();
    let new_chars = replacement.chars().count();
    let delta = new_chars as isize - old_chars as isize;

    buf.replace_range(range.clone(), &replacement);

    let len_after = buf.len_chars();
    let map_pos = |pos: usize| -> usize {
        if pos >= range.end {
            ((pos as isize + delta).max(0) as usize).min(len_after)
        } else if pos > range.start {
            (range.start + new_chars).min(len_after)
        } else {
            pos.min(len_after)
        }
    };
    let new_cursor = map_pos(prev_cursor);
    if let Some(anc) = prev_anchor {
        let new_anc = map_pos(anc);
        if new_anc != new_cursor {
            buf.selection_anchor = Some(new_anc);
            buf.cursor_char = new_cursor;
            buf.cache_current_selection();
            return true;
        }
    }
    buf.set_cursor(new_cursor, false);
    true
}

/// Inserts a Markdown horizontal rule (`---`) separated cleanly by a blank line above so CommonMark
/// never treats it as a Setext heading underline, and leaves the cursor on a new line below.
pub fn insert_horizontal_rule(buf: &mut DocumentBuffer) {
    buf.restore_cached_selection();
    let le = buf.line_ending().to_string();
    let cur_line = if let Some(sel) = buf.selection_range() {
        buf.char_to_line(sel.start)
    } else {
        buf.char_to_line(buf.cursor_char)
    };
    let line_str = buf.line_text(cur_line);
    if line_str.trim().is_empty() {
        let range = buf.line_range_chars(cur_line, cur_line);
        let prev_non_empty = cur_line > 0 && !buf.line_text(cur_line - 1).trim().is_empty();
        let mut insertion = String::new();
        if prev_non_empty {
            insertion.push_str(&le);
        }
        insertion.push_str("---");
        insertion.push_str(&le);
        insertion.push_str(&le);
        let start = range.start;
        let len_chars = insertion.chars().count();
        buf.replace_range(range, &insertion);
        buf.set_cursor(start + len_chars, false);
    } else {
        let line_end = buf.line_range_chars(cur_line, cur_line).end;
        let insertion = format!("{le}{le}---{le}{le}");
        let len_chars = insertion.chars().count();
        buf.insert(line_end, &insertion);
        buf.set_cursor(line_end + len_chars, false);
    }
}

/// Toggles inline LaTeX math (`$...$`) around the selection or word at cursor.
pub fn toggle_inline_math(buf: &mut DocumentBuffer) {
    toggle_inline_wrap(buf, "$");
}

fn insert_delimited_block(
    buf: &mut DocumentBuffer,
    open_fence: &str,
    close_fence: &str,
    default_body: &str,
) {
    buf.restore_cached_selection();
    let le = buf.line_ending().to_string();
    let open_prefix_chars = open_fence.chars().count() + le.chars().count();

    if let Some(sel) = buf.selection_range() {
        let selected = buf.rope().slice(sel.clone()).to_string();
        let trimmed = selected.trim_matches(['\r', '\n']);
        let body = if trimmed.is_empty() {
            default_body.replace('\n', &le)
        } else {
            trimmed.to_string()
        };
        let block = format!("{open_fence}{le}{body}{le}{close_fence}");
        let start = sel.start;
        buf.replace_range(sel, &block);
        buf.set_cursor(start + open_prefix_chars, false);
        return;
    }

    let line = buf.char_to_line(buf.cursor_char);
    let line_text = buf.line_text(line);
    let body = default_body.replace('\n', &le);

    if line_text.trim().is_empty() {
        let range = buf.line_range_chars(line, line);
        let leading_blank = line > 0 && !buf.line_text(line - 1).trim().is_empty();
        let mut insertion = String::new();
        if leading_blank {
            insertion.push_str(&le);
        }
        insertion.push_str(&format!("{open_fence}{le}{body}{le}{close_fence}"));
        let start = range.start;
        let offset = if leading_blank { le.chars().count() } else { 0 };
        buf.replace_range(range, &insertion);
        buf.set_cursor(start + offset + open_prefix_chars, false);
    } else {
        let line_end = buf.line_range_chars(line, line).end;
        let prefix = format!("{le}{le}");
        let prefix_chars = prefix.chars().count();
        let insertion = format!("{prefix}{open_fence}{le}{body}{le}{close_fence}");
        buf.insert(line_end, &insertion);
        buf.set_cursor(line_end + prefix_chars + open_prefix_chars, false);
    }
}

/// Toggles inline math (`$...$`) when a single-line non-empty selection is active; otherwise
/// inserts a display math block (`$$ ... $$`) and places the cursor inside. Returns `true` when
/// a multi-line `$$` block was inserted, `false` when inline math was toggled.
pub fn insert_math_block(buf: &mut DocumentBuffer) -> bool {
    buf.restore_cached_selection();
    if let Some(sel) = buf.selection_range() {
        let s_line = buf.char_to_line(sel.start);
        let e_line = buf.char_to_line(sel.end.saturating_sub(1).max(sel.start));
        if s_line == e_line {
            let selected = buf.rope().slice(sel).to_string();
            if !selected.trim().is_empty() {
                toggle_inline_math(buf);
                return false;
            }
        }
    }
    insert_delimited_block(buf, "$$", "$$", "");
    true
}

/// Inserts a `$$ ... $$` display math block populated with `template_body` at the cursor.
pub fn insert_math_template_block(buf: &mut DocumentBuffer, template_body: &str) {
    insert_delimited_block(buf, "$$", "$$", template_body);
}

/// Wraps selection or inserts a starter Mermaid diagram block (` ```mermaid `) at cursor.
pub fn insert_mermaid_block(buf: &mut DocumentBuffer, starter_body: Option<&str>) {
    const DEFAULT_MERMAID: &str = "flowchart LR\n    A([Start]) --> B{Decision}\n    B -->|Yes| C[Process]\n    B -->|No| D([End])\n    C --> D";
    insert_delimited_block(
        buf,
        "```mermaid",
        "```",
        starter_body.unwrap_or(DEFAULT_MERMAID),
    );
}

/// Wraps the selected lines (or inserts an empty block at cursor) in a fenced code block.
pub fn insert_code_block(buf: &mut DocumentBuffer, language: &str) {
    buf.restore_cached_selection();
    let le = buf.line_ending();
    let le_chars = le.chars().count();
    if let Some(sel) = buf.selection_range() {
        let selected = buf.rope().slice(sel.clone()).to_string();
        let trimmed = selected.trim_matches(['\r', '\n']);
        let fenced = format!("```{language}{le}{trimmed}{le}```");
        let start = sel.start;
        buf.replace_range(sel, &fenced);
        buf.set_cursor(start + 3 + language.chars().count() + le_chars, false);
    } else {
        let line = buf.char_to_line(buf.cursor_char);
        let line_text = buf.line_text(line);
        let range = buf.line_range_chars(line, line);
        let content = if line_text.trim().is_empty() {
            ""
        } else {
            line_text.as_str()
        };
        let fenced = format!("```{language}{le}{content}{le}```");
        let start = range.start;
        buf.replace_range(range, &fenced);
        buf.set_cursor(start + 3 + language.chars().count() + le_chars, false);
    }
}

/// Inserts a Markdown image reference `![alt](url)` cleanly at the cursor, selection, or line.
pub fn insert_image(buf: &mut DocumentBuffer, alt: &str, url: &str) {
    buf.restore_cached_selection();
    let sel = buf.selection_range();
    let clean_alt = if alt.trim().is_empty() {
        "Image"
    } else {
        alt.trim()
    };
    let escaped_alt = clean_alt
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]");
    let clean_url = url.trim().replace('\\', "/");
    let formatted_url =
        if clean_url.contains(' ') || clean_url.contains('(') || clean_url.contains(')') {
            format!("<{clean_url}>")
        } else {
            clean_url
        };
    let md = format!("![{escaped_alt}]({formatted_url})");
    let md_chars = md.chars().count();
    if let Some(sel) = sel {
        let start = sel.start;
        buf.replace_range(sel, &md);
        buf.set_cursor(start + md_chars, false);
        return;
    }
    let line = buf.char_to_line(buf.cursor_char);
    let line_text = buf.line_text(line);
    if line_text.trim().is_empty() {
        let range = buf.line_range_chars(line, line);
        let start = range.start;
        buf.replace_range(range, &md);
        buf.set_cursor(start + md_chars, false);
    } else {
        let pos = buf.cursor_char;
        let insert_str = if pos > 0 && !buf.rope().char(pos - 1).is_whitespace() {
            format!(" {md}")
        } else {
            md
        };
        let len = insert_str.chars().count();
        buf.insert(pos, &insert_str);
        buf.set_cursor(pos + len, false);
    }
}

/// Inserts a Markdown hyperlink `[text](url)` at the cursor or replaces the active selection.
pub fn insert_link(buf: &mut DocumentBuffer, text: &str, url: &str) {
    buf.restore_cached_selection();
    let sel = buf.selection_range();
    let sel_text = sel
        .as_ref()
        .map(|r| buf.rope().slice(r.clone()).to_string())
        .unwrap_or_default();
    let label = if !text.trim().is_empty() {
        text.trim().to_string()
    } else if !sel_text.trim().is_empty() {
        sel_text.trim().to_string()
    } else {
        url.trim().to_string()
    };
    let escaped_label = label
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]");
    let clean_url = url.trim().to_string();
    let formatted_url =
        if clean_url.contains(' ') || clean_url.contains('(') || clean_url.contains(')') {
            format!("<{clean_url}>")
        } else {
            clean_url
        };
    let md = format!("[{escaped_label}]({formatted_url})");
    let md_len = md.chars().count();
    if let Some(sel) = sel {
        let start = sel.start;
        buf.replace_range(sel, &md);
        buf.set_cursor(start + md_len, false);
    } else {
        let pos = buf.cursor_char;
        let insert_str = if pos > 0
            && !buf.rope().char(pos - 1).is_whitespace()
            && !matches!(buf.rope().char(pos - 1), '(' | '[')
        {
            format!(" {md}")
        } else {
            md
        };
        let len = insert_str.chars().count();
        buf.insert(pos, &insert_str);
        buf.set_cursor(pos + len, false);
    }
}

/// Toggles `- [ ]` <-> `- [x]` on a specific 0-based source line in O(log n).
pub fn toggle_task_checkbox_at_line(buf: &mut DocumentBuffer, line_idx: usize) -> bool {
    if line_idx >= buf.len_lines() {
        return false;
    }
    let text = buf.line_text(line_idx);
    let (align, indent_lvl, without_meta) = parse_line_block_meta(&text);
    let meta_prefix = format_line_block_meta(align, indent_lvl);
    let indent = without_meta.bytes().take_while(|&b| b == b' ').count();
    let rest = &without_meta[indent..];

    let replacement = if let Some(after) = rest
        .strip_prefix("- [ ] ")
        .or_else(|| rest.strip_prefix("* [ ] "))
        .or_else(|| rest.strip_prefix("+ [ ] "))
    {
        let marker = &rest[..2];
        Some(format!(
            "{meta_prefix}{}{marker}[x] {after}",
            &without_meta[..indent]
        ))
    } else if let Some(after) = rest
        .strip_prefix("- [x] ")
        .or_else(|| rest.strip_prefix("- [X] "))
        .or_else(|| rest.strip_prefix("* [x] "))
        .or_else(|| rest.strip_prefix("* [X] "))
        .or_else(|| rest.strip_prefix("+ [x] "))
        .or_else(|| rest.strip_prefix("+ [X] "))
    {
        let marker = &rest[..2];
        Some(format!(
            "{meta_prefix}{}{marker}[ ] {after}",
            &without_meta[..indent]
        ))
    } else {
        None
    };

    if let Some(new_line) = replacement {
        let range = buf.line_range_chars(line_idx, line_idx);
        buf.replace_range(range, &new_line);
        true
    } else {
        false
    }
}

/// Inserts a GFM table with `total_rows` (>= 2: 1 header + body rows) and `cols` (>= 1) at the cursor.
pub fn insert_table(buf: &mut DocumentBuffer, total_rows: usize, cols: usize) {
    let cols = cols.max(1);
    let body_rows = total_rows.saturating_sub(1).max(1);
    let header: Vec<String> = (1..=cols).map(|c| format!("Column {c}")).collect();
    let rows: Vec<Vec<String>> = (1..=body_rows)
        .map(|r| (1..=cols).map(|c| format!("Cell {r},{c}")).collect())
        .collect();
    let alignments = vec![TableAlign::None; cols];

    let table_md = build_table_markdown(&alignments, &header, &rows);
    let line = buf.char_to_line(buf.cursor_char);
    let current_line_text = buf.line_text(line);
    let mut insertion = String::new();

    if current_line_text.trim().is_empty() {
        let range = buf.line_range_chars(line, line);
        let leading_newline = line > 0 && !buf.line_text(line - 1).trim().is_empty();
        if leading_newline {
            insertion.push('\n');
        }
        insertion.push_str(&table_md);
        insertion.push('\n');
        let start = range.start;
        buf.replace_range(range, &insertion);
        buf.set_cursor(start + usize::from(leading_newline), false);
    } else {
        let line_end = buf.line_range_chars(line, line).end;
        insertion.push_str("\n\n");
        insertion.push_str(&table_md);
        insertion.push('\n');
        buf.insert(line_end, &insertion);
        buf.set_cursor(line_end + 2, false);
    }
}

/// Finds and parses the GFM table containing `line_idx` in O(block_lines * log n), safe for 10 MB files.
pub fn table_at_line(buf: &DocumentBuffer, line_idx: usize) -> Option<TableData> {
    if line_idx >= buf.len_lines() {
        return None;
    }
    // Expand outward to blank lines (capped at 4096 lines each way) so we never clone a 10 MB buffer.
    let mut block_start = line_idx;
    let min_start = line_idx.saturating_sub(4096);
    while block_start > min_start && !buf.line_text(block_start - 1).trim().is_empty() {
        block_start -= 1;
    }

    let mut block_end = line_idx;
    let max_end = (line_idx + 4096).min(buf.len_lines().saturating_sub(1));
    while block_end < max_end && !buf.line_text(block_end + 1).trim().is_empty() {
        block_end += 1;
    }

    let range = buf.line_range_chars(block_start, block_end);
    let snippet = buf.rope().slice(range).to_string();
    let mut tables = extract_tables(&snippet);
    for t in &mut tables {
        t.source_lines.start += block_start;
        t.source_lines.end += block_start;
        if line_idx >= t.source_lines.start && line_idx < t.source_lines.end {
            return Some(t.clone());
        }
    }
    None
}

/// Replaces only the table's source lines in the rope with the AST-serialized GFM table.
pub fn apply_table_edit(buf: &mut DocumentBuffer, table: &TableData) {
    if table.source_lines.is_empty() {
        return;
    }
    let last_line = table.source_lines.end.saturating_sub(1);
    let range = buf.line_range_chars(table.source_lines.start, last_line);
    let serialized = build_table_markdown(&table.alignments, &table.header, &table.rows);
    let start = range.start;
    buf.replace_range(range, &serialized);
    buf.set_cursor(start, false);
}

/// Deletes a contiguous 0-based line range `src_lines` (e.g. a code, math, Mermaid, or table
/// block) in a single undoable edit, collapsing redundant blank separator lines cleanly.
pub fn delete_block_lines(buf: &mut DocumentBuffer, src_lines: &std::ops::Range<usize>) -> bool {
    let total_lines = buf.len_lines();
    if src_lines.is_empty() || src_lines.start >= total_lines {
        return false;
    }
    let end_excl = src_lines.end.min(total_lines);
    let has_next_content = (end_excl..total_lines).any(|l| !buf.line_text(l).trim().is_empty());

    let (start_char, end_char) = if has_next_content {
        let start_c = buf.line_to_char(src_lines.start);
        let mut stop_line = end_excl;
        let prev_blank =
            src_lines.start == 0 || buf.line_text(src_lines.start - 1).trim().is_empty();
        if stop_line < total_lines && buf.line_text(stop_line).trim().is_empty() && prev_blank {
            stop_line += 1;
        }
        let end_c = if stop_line < total_lines {
            buf.line_to_char(stop_line)
        } else {
            buf.len_chars()
        };
        (start_c, end_c)
    } else {
        let mut del_start_line = src_lines.start;
        if del_start_line > 0 && buf.line_text(del_start_line - 1).trim().is_empty() {
            del_start_line -= 1;
        }
        if del_start_line == 0 {
            (0, buf.len_chars())
        } else {
            let len = buf.len_chars();
            let had_trailing_nl = len > 0 && buf.rope().char(len - 1) == '\n';
            let start_c = if had_trailing_nl {
                buf.line_to_char(del_start_line)
            } else {
                buf.line_range_chars(del_start_line - 1, del_start_line - 1).end
            };
            (start_c, len)
        }
    };

    if end_char <= start_char {
        return false;
    }
    buf.remove(start_char..end_char);
    buf.set_cursor(start_char.min(buf.len_chars()), false);
    true
}

impl TableData {
    pub fn col_count(&self) -> usize {
        self.header.len().max(self.alignments.len()).max(1)
    }

    pub fn set_cell(&mut self, body_row: Option<usize>, col: usize, value: String) {
        match body_row {
            None => {
                if col < self.header.len() {
                    self.header[col] = value;
                }
            }
            Some(r) => {
                if let Some(row) = self.rows.get_mut(r) {
                    if col < row.len() {
                        row[col] = value;
                    }
                }
            }
        }
    }

    pub fn add_row(&mut self, after_body_row: Option<usize>) {
        let cols = self.col_count();
        let new_row = vec![String::new(); cols];
        match after_body_row {
            Some(idx) if idx < self.rows.len() => self.rows.insert(idx + 1, new_row),
            _ => self.rows.push(new_row),
        }
    }

    pub fn delete_row(&mut self, body_row: usize) {
        if body_row < self.rows.len() {
            self.rows.remove(body_row);
        }
    }

    pub fn add_column(&mut self, after_col: Option<usize>) {
        let insert_idx = match after_col {
            Some(c) if c < self.header.len() => c + 1,
            _ => self.header.len(),
        };
        self.header
            .insert(insert_idx, format!("Col {}", insert_idx + 1));
        if insert_idx <= self.alignments.len() {
            self.alignments.insert(insert_idx, TableAlign::None);
        } else {
            self.alignments.push(TableAlign::None);
        }
        for row in &mut self.rows {
            while row.len() < insert_idx {
                row.push(String::new());
            }
            row.insert(insert_idx, String::new());
        }
    }

    pub fn delete_column(&mut self, col: usize) {
        if self.header.len() <= 1 || col >= self.header.len() {
            return;
        }
        self.header.remove(col);
        if col < self.alignments.len() {
            self.alignments.remove(col);
        }
        for row in &mut self.rows {
            if col < row.len() {
                row.remove(col);
            }
        }
    }

    pub fn set_alignment(&mut self, col: usize, align: TableAlign) {
        while self.alignments.len() <= col {
            self.alignments.push(TableAlign::None);
        }
        self.alignments[col] = align;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ast::parse_to_ir;
    use crate::export::ir::DocBlock;

    #[test]
    fn insert_horizontal_rule_never_creates_setext_heading() {
        // 1. Empty buffer
        let mut buf = DocumentBuffer::new();
        insert_horizontal_rule(&mut buf);
        assert_eq!(buf.as_string(), "---\n\n");
        let blocks = parse_to_ir(&buf.as_string());
        assert!(matches!(blocks.as_slice(), [DocBlock::ThematicBreak { .. }]));

        // 2. Non-empty paragraph line must stay Paragraph followed by ThematicBreak
        let mut buf2 = DocumentBuffer::from_str("Heading candidate");
        buf2.set_cursor(5, false);
        insert_horizontal_rule(&mut buf2);
        assert_eq!(buf2.as_string(), "Heading candidate\n\n---\n\n");
        let blocks2 = parse_to_ir(&buf2.as_string());
        assert_eq!(blocks2.len(), 2);
        assert!(matches!(blocks2[0], DocBlock::Paragraph { .. }));
        assert!(matches!(blocks2[1], DocBlock::ThematicBreak { .. }));

        // 3. Empty line immediately below non-empty paragraph gets an extra blank line above ---
        let mut buf3 = DocumentBuffer::from_str("Paragraph line\n");
        let line1_start = buf3.line_to_char(1);
        buf3.set_cursor(line1_start, false);
        insert_horizontal_rule(&mut buf3);
        let blocks3 = parse_to_ir(&buf3.as_string());
        assert_eq!(blocks3.len(), 2);
        assert!(matches!(blocks3[0], DocBlock::Paragraph { .. }));
        assert!(matches!(blocks3[1], DocBlock::ThematicBreak { .. }));
    }

    #[test]
    fn math_and_mermaid_editing_helpers_work_and_parse_fences() {
        assert_eq!(
            parse_code_fence_line("$$"),
            Some((0, '$', 2, "math".to_string()))
        );
        assert_eq!(parse_code_fence_line("$$E=mc^2$$"), None);

        // Single-line selection toggles inline $...$ and unwraps cleanly
        let mut buf = DocumentBuffer::from_str("Energy E=mc^2 here");
        buf.selection_anchor = Some(7);
        buf.cursor_char = 13;
        let is_block = insert_math_block(&mut buf);
        assert!(!is_block);
        assert_eq!(buf.as_string(), "Energy $E=mc^2$ here");
        let is_block_unwrap = insert_math_block(&mut buf);
        assert!(!is_block_unwrap);
        assert_eq!(buf.as_string(), "Energy E=mc^2 here");

        // No selection inserts $$ ... $$ display block and puts cursor on inner line
        let mut buf_block = DocumentBuffer::from_str("Formula:");
        buf_block.set_cursor(8, false);
        let inserted = insert_math_block(&mut buf_block);
        assert!(inserted);
        assert_eq!(buf_block.as_string(), "Formula:\n\n$$\n\n$$");
        assert_eq!(buf_block.char_to_line(buf_block.cursor_char), 3);

        // Mermaid starter insertion
        let mut buf_mmd = DocumentBuffer::new();
        insert_mermaid_block(&mut buf_mmd, None);
        let text = buf_mmd.as_string();
        assert!(text.starts_with("```mermaid\nflowchart LR"));
        assert!(text.ends_with("\n```"));
        assert_eq!(buf_mmd.char_to_line(buf_mmd.cursor_char), 1);
    }

    #[test]
    fn delete_block_lines_removes_start_middle_end_and_undoes_cleanly() {
        // Middle block with surrounding blank lines collapses to a single separator blank line
        let mut buf = DocumentBuffer::from_str("Intro\n\n$$\nx=1\n$$\n\nOutro");
        let orig = buf.as_string();
        assert!(delete_block_lines(&mut buf, &(2..5)));
        assert_eq!(buf.as_string(), "Intro\n\nOutro");
        assert!(buf.undo());
        assert_eq!(buf.as_string(), orig);

        // Block at start followed by blank line leaves next block at line 0
        let mut buf_start = DocumentBuffer::from_str("```mermaid\nA-->B\n```\n\nNext");
        assert!(delete_block_lines(&mut buf_start, &(0..3)));
        assert_eq!(buf_start.as_string(), "Next");

        // Block at end preceded by blank line removes trailing blank separator cleanly
        let mut buf_end = DocumentBuffer::from_str("Intro\n\n```rust\nfn f() {}\n```");
        assert!(delete_block_lines(&mut buf_end, &(2..5)));
        assert_eq!(buf_end.as_string(), "Intro");

        // Block as only content leaves empty document
        let mut buf_only = DocumentBuffer::from_str("$$\nx=1\n$$");
        assert!(delete_block_lines(&mut buf_only, &(0..3)));
        assert_eq!(buf_only.as_string(), "");
    }
}
