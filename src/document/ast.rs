use std::borrow::Cow;
use std::cell::RefCell;
use std::ops::Range;

use comrak::arena_tree::Node;
use comrak::nodes::{
    Ast, AstNode, ListType, NodeHeading, NodeTable, NodeValue, TableAlignment,
};
use comrak::{format_commonmark, parse_document, Arena, Options};

use crate::document::unicode::{
    detect_paragraph_direction, parse_line_block_meta, BlockAlignment, TextDirection,
};
use crate::export::ir::{DocBlock, InlineRun, ListItem, TableAlign};

/// Standard CommonMark + GFM options used across parsing, serialization, and round-trip checks.
pub fn gfm_options() -> Options<'static> {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options
}

/// Returns `true` if a fenced code block info string identifies LaTeX/TeX math.
pub fn is_math_code_info(info: &str) -> bool {
    let first = info.split_whitespace().next().unwrap_or("");
    first.eq_ignore_ascii_case("math")
        || first.eq_ignore_ascii_case("latex")
        || first.eq_ignore_ascii_case("tex")
        || first.eq_ignore_ascii_case("katex")
}

/// Returns `true` if a fenced code block info string identifies a Mermaid diagram.
pub fn is_mermaid_code_info(info: &str) -> bool {
    info.split_whitespace()
        .next()
        .unwrap_or("")
        .eq_ignore_ascii_case("mermaid")
}

const MATH_SENTINEL_INL: &str = "\x07MINL:";
const MATH_SENTINEL_DISP: &str = "\x07MDISP:";
const MATH_SENTINEL_BLK_OPEN: &str = "\x07MBLK_OPEN:";
const MATH_SENTINEL_BLK_MID: &str = "\x07MBLK_MID:";
const MATH_SENTINEL_BLK_CLOSE: &str = "\x07MBLK_CLOSE:";
const MATH_SENTINEL_END: char = '\x07';
const MATH_PIPE_ESC: &str = "\x07PIPE\x07";

fn wrap_math_sentinel(prefix: &str, payload: &str) -> String {
    let escaped = payload.replace('|', MATH_PIPE_ESC);
    let inner = format!("{prefix}{escaped}{MATH_SENTINEL_END}");
    let mut max_ticks = 0usize;
    let mut cur_ticks = 0usize;
    for b in inner.bytes() {
        if b == b'`' {
            cur_ticks += 1;
            if cur_ticks > max_ticks {
                max_ticks = cur_ticks;
            }
        } else {
            cur_ticks = 0;
        }
    }
    let ticks = "`".repeat(max_ticks + 1);
    format!("{ticks}{inner}{ticks}")
}

fn decode_math_sentinel(literal: &str, prefix: &str) -> Option<String> {
    let rest = literal.strip_prefix(prefix)?;
    let body = rest.strip_suffix(MATH_SENTINEL_END)?;
    Some(body.replace(MATH_PIPE_ESC, "|"))
}

fn is_escaped_at(bytes: &[u8], idx: usize) -> bool {
    let mut backslashes = 0usize;
    let mut k = idx;
    while k > 0 && bytes[k - 1] == b'\\' {
        backslashes += 1;
        k -= 1;
    }
    backslashes % 2 == 1
}

#[derive(Default)]
struct BacktickFailCache {
    mask: u64,
    large: Vec<usize>,
}

impl BacktickFailCache {
    #[inline]
    fn has_failed(&self, ticks: usize) -> bool {
        if ticks < 64 {
            (self.mask & (1u64 << ticks)) != 0
        } else {
            self.large.contains(&ticks)
        }
    }

    #[inline]
    fn mark_failed(&mut self, ticks: usize) {
        if ticks < 64 {
            self.mask |= 1u64 << ticks;
        } else if !self.large.contains(&ticks) {
            self.large.push(ticks);
        }
    }
}

fn find_closing_math_pair(line: &str, start: usize, close: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let close_bytes = close.as_bytes();
    let first_byte = close_bytes[0];
    let mut i = start;
    while i + close_bytes.len() <= bytes.len() {
        let rel = memchr::memchr2(b'\\', first_byte, &bytes[i..])?;
        i += rel;
        if bytes[i] == b'\\' && close == "$$" {
            i += 1;
            if i < bytes.len() {
                i += line[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            continue;
        }
        if bytes[i..].starts_with(close_bytes) && !is_escaped_at(bytes, i) {
            return Some(i);
        }
        i += line[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    None
}

#[inline]
fn has_math_operator_byte(slice: &str) -> bool {
    slice
        .bytes()
        .any(|b| matches!(b, b'=' | b'+' | b'-' | b'*' | b'/' | b'^' | b'_' | b'\\'))
}

fn find_closing_inline_dollar(
    line: &str,
    start: usize,
    open_next_digit: bool,
    failed_ticks: &mut BacktickFailCache,
) -> (Option<usize>, bool) {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = start;
    while i < len {
        let Some(rel) = memchr::memchr3(b'\\', b'`', b'$', &bytes[i..]) else {
            return (None, true);
        };
        i += rel;
        if bytes[i] == b'\\' {
            i += 1;
            if i < len {
                i += line[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            continue;
        }
        if bytes[i] == b'`' {
            let ticks = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if !failed_ticks.has_failed(ticks) {
                if let Some(rel_close) = find_exact_backtick_run(&line[i + ticks..], ticks) {
                    i = i + ticks + rel_close + ticks;
                    continue;
                }
                failed_ticks.mark_failed(ticks);
            }
            i += ticks;
            continue;
        }
        if bytes[i] == b'$' {
            if i + 1 < len && bytes[i + 1] == b'$' {
                return (None, false);
            }
            let prev_ws = i > 0 && matches!(bytes[i - 1], b' ' | b'\t');
            let next_digit = i + 1 < len && bytes[i + 1].is_ascii_digit();
            let currency_punct_pair = open_next_digit
                && i > start
                && matches!(bytes[i - 1], b'(' | b'/' | b',' | b'[')
                && !has_math_operator_byte(&line[start..i - 1]);
            if !prev_ws && !next_digit && !currency_punct_pair && i > start {
                return (Some(i), false);
            }
            return (None, false);
        }
    }
    (None, true)
}

fn preprocess_math_in_line<'a>(line: &'a str) -> Cow<'a, str> {
    if !line.contains('$') && !line.contains("\\(") && !line.contains("\\[") {
        return Cow::Borrowed(line);
    }
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut out = String::new();
    let mut copied_until = 0usize;
    let mut i = 0usize;
    let mut failed_ticks = BacktickFailCache::default();
    let mut no_more_paren_math = false;
    let mut no_more_bracket_math = false;
    let mut no_more_disp_dollar = false;
    let mut no_more_inline_dollar = false;

    while i < len {
        // Inline code spans stay verbatim
        if bytes[i] == b'`' {
            let ticks = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if !failed_ticks.has_failed(ticks) {
                if let Some(rel_close) = find_exact_backtick_run(&line[i + ticks..], ticks) {
                    i = i + ticks + rel_close + ticks;
                    continue;
                }
                failed_ticks.mark_failed(ticks);
            }
            i += ticks;
            continue;
        }

        // Backslash-escaped sequences, \(...\), and \[...\]
        if bytes[i] == b'\\' {
            if !no_more_paren_math && line[i..].starts_with("\\(") {
                if let Some(close_idx) = find_closing_math_pair(line, i + 2, "\\)") {
                    let inner = &line[i + 2..close_idx];
                    if !inner.trim().is_empty() {
                        if out.is_empty() {
                            out.reserve(len + 16);
                        }
                        out.push_str(&line[copied_until..i]);
                        out.push_str(&wrap_math_sentinel(MATH_SENTINEL_INL, inner));
                        i = close_idx + 2;
                        copied_until = i;
                        continue;
                    }
                } else {
                    no_more_paren_math = true;
                }
            } else if !no_more_bracket_math && line[i..].starts_with("\\[") {
                if let Some(close_idx) = find_closing_math_pair(line, i + 2, "\\]") {
                    let inner = &line[i + 2..close_idx];
                    if !inner.trim().is_empty() {
                        if out.is_empty() {
                            out.reserve(len + 16);
                        }
                        out.push_str(&line[copied_until..i]);
                        out.push_str(&wrap_math_sentinel(MATH_SENTINEL_DISP, inner));
                        i = close_idx + 2;
                        copied_until = i;
                        continue;
                    }
                } else {
                    no_more_bracket_math = true;
                }
            }
            i += 1;
            if i < len {
                i += line[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            continue;
        }

        // Display $$...$$ on a single line
        if line[i..].starts_with("$$") {
            if !no_more_disp_dollar {
                if let Some(close_idx) = find_closing_math_pair(line, i + 2, "$$") {
                    let inner = &line[i + 2..close_idx];
                    if !inner.trim().is_empty() {
                        if out.is_empty() {
                            out.reserve(len + 16);
                        }
                        out.push_str(&line[copied_until..i]);
                        out.push_str(&wrap_math_sentinel(MATH_SENTINEL_DISP, inner));
                        i = close_idx + 2;
                        copied_until = i;
                        continue;
                    }
                } else {
                    no_more_disp_dollar = true;
                }
            }
            i += 2;
            continue;
        }

        // Inline $...$
        if bytes[i] == b'$' {
            if !no_more_inline_dollar {
                let next_ws = i + 1 >= len || matches!(bytes[i + 1], b' ' | b'\t');
                let prev_digit = i > 0 && bytes[i - 1].is_ascii_digit();
                let open_next_digit = i + 1 < len && bytes[i + 1].is_ascii_digit();
                if !next_ws && !prev_digit {
                    let (close_opt, hit_eol) = find_closing_inline_dollar(
                        line,
                        i + 1,
                        open_next_digit,
                        &mut failed_ticks,
                    );
                    if let Some(close_idx) = close_opt {
                        let inner = &line[i + 1..close_idx];
                        if !inner.trim().is_empty() {
                            if out.is_empty() {
                                out.reserve(len + 16);
                            }
                            out.push_str(&line[copied_until..i]);
                            out.push_str(&wrap_math_sentinel(MATH_SENTINEL_INL, inner));
                            i = close_idx + 1;
                            copied_until = i;
                            continue;
                        }
                    } else if hit_eol {
                        no_more_inline_dollar = true;
                    }
                }
            }
            i += 1;
            continue;
        }

        i += line[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }

    if copied_until == 0 {
        Cow::Borrowed(line)
    } else {
        out.push_str(&line[copied_until..]);
        Cow::Owned(out)
    }
}

#[derive(Debug, Clone, Copy)]
struct LineMeta {
    alignment: BlockAlignment,
    indent: u8,
    direction: TextDirection,
}

fn is_list_marker_start(s: &str) -> bool {
    if s.starts_with("- ") || s.starts_with("* ") || s.starts_with("+ ") {
        return true;
    }
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    digits > 0 && (s[digits..].starts_with(". ") || s[digits..].starts_with(") "))
}

fn parse_fence_open(trimmed: &str) -> Option<(u8, usize)> {
    let first = *trimmed.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let count = trimmed.bytes().take_while(|&b| b == first).count();
    if count < 3 {
        return None;
    }
    if first == b'`' && trimmed[count..].contains('`') {
        return None;
    }
    Some((first, count))
}

fn is_fence_close(trimmed: &str, fence_char: u8, fence_len: usize) -> bool {
    let count = trimmed.bytes().take_while(|&b| b == fence_char).count();
    count >= fence_len && trimmed[count..].trim().is_empty()
}

fn normalize_atx_heading_space<'a>(line: &'a str) -> Cow<'a, str> {
    let trimmed = line.trim_start_matches(' ');
    let lead_spaces = line.len() - trimmed.len();
    if lead_spaces > 3 {
        return Cow::Borrowed(line);
    }
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes)
        && let Some(next_ch) = trimmed[hashes..].chars().next()
            && next_ch != '#' && !next_ch.is_whitespace() {
                let mut out = String::with_capacity(line.len() + 1);
                out.push_str(&line[..lead_spaces + hashes]);
                out.push(' ');
                out.push_str(&trimmed[hashes..]);
                return Cow::Owned(out);
            }
    Cow::Borrowed(line)
}

fn find_exact_backtick_run(haystack: &str, ticks: usize) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    while i < len {
        let rel = memchr::memchr(b'`', &bytes[i..])?;
        i += rel;
        let run_len = bytes[i..].iter().take_while(|&&b| b == b'`').count();
        if run_len == ticks {
            return Some(i);
        }
        i += run_len;
    }
    None
}

fn normalize_inline_marker_spaces<'a>(line: &'a str) -> Cow<'a, str> {
    if !line.contains("**") && !line.contains("~~") {
        return Cow::Borrowed(line);
    }
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut out = String::new();
    let mut copied_until = 0usize;
    let mut i = 0usize;
    let mut failed_ticks = BacktickFailCache::default();
    let mut no_more_star = false;
    let mut no_more_tilde = false;

    while i < len {
        // Skip backslash escapes verbatim so escaped backticks or markers are untouched
        if bytes[i] == b'\\' {
            i += 1;
            if i < len {
                i += line[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            continue;
        }

        // Preserve inline code spans verbatim (requiring an exact matching backtick count)
        if bytes[i] == b'`' {
            let ticks = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if !failed_ticks.has_failed(ticks) {
                if let Some(rel_close) = find_exact_backtick_run(&line[i + ticks..], ticks) {
                    i = i + ticks + rel_close + ticks;
                    continue;
                }
                failed_ticks.mark_failed(ticks);
            }
            i += ticks;
            continue;
        }

        // Handle symmetric 2-char markers ("**" and "~~") without breaking "***"
        let marker = if !no_more_star && i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            if (i > 0 && bytes[i - 1] == b'*') || (i + 2 < len && bytes[i + 2] == b'*') {
                None
            } else {
                Some("**")
            }
        } else if !no_more_tilde && i + 1 < len && bytes[i] == b'~' && bytes[i + 1] == b'~' {
            if (i > 0 && bytes[i - 1] == b'~') || (i + 2 < len && bytes[i + 2] == b'~') {
                None
            } else {
                Some("~~")
            }
        } else {
            None
        };

        if let Some(m) = marker {
            let m_byte = m.as_bytes()[0];
            let rest = &line[i + 2..];
            let rest_bytes = rest.as_bytes();
            let rest_len = rest_bytes.len();
            // Find matching unescaped 2-char marker outside inline code spans
            let mut scan = 0usize;
            let mut matched_end: Option<usize> = None;
            while scan < rest_len {
                if rest_bytes[scan] == b'\\' {
                    scan += 1;
                    if scan < rest_len {
                        scan += rest[scan..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                    }
                    continue;
                }
                if rest_bytes[scan] == b'`' {
                    let ticks = rest_bytes[scan..]
                        .iter()
                        .take_while(|&&b| b == b'`')
                        .count();
                    if !failed_ticks.has_failed(ticks) {
                        if let Some(rel_close) =
                            find_exact_backtick_run(&rest[scan + ticks..], ticks)
                        {
                            scan = scan + ticks + rel_close + ticks;
                            continue;
                        }
                        failed_ticks.mark_failed(ticks);
                    }
                    scan += ticks;
                    continue;
                }
                if rest[scan..].starts_with(m) {
                    let before_same = scan > 0 && rest_bytes[scan - 1] == m_byte;
                    let after_same = scan + 2 < rest_len && rest_bytes[scan + 2] == m_byte;
                    if !before_same && !after_same {
                        matched_end = Some(scan);
                        break;
                    }
                    scan += rest_bytes[scan..]
                        .iter()
                        .take_while(|&&b| b == m_byte)
                        .count();
                    continue;
                }
                scan += rest[scan..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            if matched_end.is_none() {
                if m == "**" {
                    no_more_star = true;
                } else {
                    no_more_tilde = true;
                }
            }
            if let Some(rel_close) = matched_end {
                let inner = &rest[..rel_close];
                let inner_trimmed = inner.trim();
                let has_inner_space = inner != inner_trimmed;
                let after = &rest[rel_close + 2..];
                if !inner_trimmed.is_empty() && has_inner_space {
                    if out.is_empty() {
                        out.reserve(len + 8);
                    }
                    out.push_str(&line[copied_until..i]);
                    let need_leading_space = inner.starts_with(char::is_whitespace)
                        && !out.is_empty()
                        && !out.ends_with(char::is_whitespace);
                    let need_trailing_space = inner.ends_with(char::is_whitespace)
                        && !after.is_empty()
                        && !after.starts_with(char::is_whitespace);
                    if need_leading_space {
                        out.push(' ');
                    }
                    out.push_str(m);
                    out.push_str(inner_trimmed);
                    out.push_str(m);
                    if need_trailing_space {
                        out.push(' ');
                    }
                    i = i + 2 + rel_close + 2;
                    copied_until = i;
                    continue;
                }
                i = i + 2 + rel_close + 2;
                continue;
            }
        }

        let ch_len = line[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        i += ch_len;
    }

    if copied_until == 0 {
        Cow::Borrowed(line)
    } else {
        out.push_str(&line[copied_until..]);
        Cow::Owned(out)
    }
}

fn strip_block_metadata_lines(markdown: &str) -> (String, Vec<LineMeta>) {
    let mut metas = Vec::new();
    let mut stripped = String::with_capacity(markdown.len());
    let mut iter = markdown.split('\n').peekable();
    while let Some(raw_line) = iter.next() {
        let clean_cr = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let (alignment, indent, rest) = parse_line_block_meta(clean_cr);
        let direction = detect_paragraph_direction(rest);
        metas.push(LineMeta {
            alignment,
            indent,
            direction,
        });
        stripped.push_str(rest);
        if raw_line.len() > clean_cr.len() {
            stripped.push('\r');
        }
        if iter.peek().is_some() {
            stripped.push('\n');
        }
    }
    (stripped, metas)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MathLookahead {
    Continue,
    Close,
    AbortFence,
}

#[inline]
fn classify_multiline_math_line(clean_line: &str, close_delim: &str) -> MathLookahead {
    let (_, _, rest) = parse_line_block_meta(clean_line);
    let trimmed = rest.trim();
    if parse_fence_open(trimmed).is_some() {
        return MathLookahead::AbortFence;
    }
    if let Some(prefix) = trimmed.strip_suffix(close_delim)
        && (close_delim == "\\]" || !is_escaped_at(prefix.as_bytes(), prefix.len())) {
            return MathLookahead::Close;
        }
    MathLookahead::Continue
}

#[inline]
fn needs_inline_or_heading_norm(line: &str) -> bool {
    line.as_bytes()
        .iter()
        .any(|&b| matches!(b, b'#' | b'$' | b'\\' | b'*' | b'~'))
}

#[derive(Default)]
struct PreprocessState {
    active_fence: Option<(u8, usize)>,
    active_math_block: Option<(usize, &'static str)>,
    failed_dollar_until: usize,
    failed_bracket_until: usize,
}

fn line_has_reference_or_footnote_def(trimmed: &str) -> bool {
    if !trimmed.starts_with('[') {
        return false;
    }
    trimmed.contains("]:")
}

fn update_html_block_tracker(trimmed: &str, open_end_token: &mut Option<&'static str>) {
    if let Some(end_tok) = *open_end_token {
        if trimmed.contains(end_tok) {
            *open_end_token = None;
        }
        return;
    }
    if trimmed.starts_with("<!--") && !trimmed[4..].contains("-->") {
        *open_end_token = Some("-->");
    } else if trimmed.starts_with("<![CDATA[") && !trimmed[9..].contains("]]>") {
        *open_end_token = Some("]]>");
    } else if trimmed.starts_with("<?") && !trimmed[2..].contains("?>") {
        *open_end_token = Some("?>");
    } else if trimmed.starts_with('<') {
        let lower = trimmed.get(..12).unwrap_or(trimmed).to_ascii_lowercase();
        for (open_pat, close_pat) in [
            ("<pre", "</pre>"),
            ("<script", "</script>"),
            ("<style", "</style>"),
            ("<textarea", "</textarea>"),
        ] {
            if lower.starts_with(open_pat)
                && !trimmed.to_ascii_lowercase().contains(close_pat)
            {
                *open_end_token = Some(close_pat);
                return;
            }
        }
    }
}

fn is_safe_top_level_heading_split(rest: &str, state: &PreprocessState, in_html: bool) -> bool {
    if state.active_fence.is_some() || state.active_math_block.is_some() || in_html {
        return false;
    }
    let trimmed = rest.trim_start_matches(' ');
    if rest.len() - trimmed.len() > 3 {
        return false;
    }
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes)
        && trimmed[hashes..]
            .chars()
            .next()
            .is_none_or(|c| c == ' ' || c == '\t')
}

#[allow(clippy::too_many_arguments)]
fn process_line_for_ir<'a, F>(
    state: &mut PreprocessState,
    idx: usize,
    clean_cr: &'a str,
    has_cr: bool,
    has_trailing_nl: bool,
    mut find_math_close: F,
    out: &mut String,
    metas: &mut Vec<LineMeta>,
) where
    F: FnMut(&str) -> (Option<usize>, usize),
{
    let (alignment, base_indent, mut rest) = parse_line_block_meta(clean_cr);
    let mut extra_indent = 0u8;

    let final_line: Cow<'a, str> = if let Some((f_char, f_len)) = state.active_fence {
        let trimmed = rest.trim_start_matches([' ', '\t']);
        if is_fence_close(trimmed, f_char, f_len) {
            state.active_fence = None;
        }
        Cow::Borrowed(rest)
    } else if let Some((end_idx, close_delim)) = state.active_math_block {
        if idx < end_idx {
            Cow::Owned(wrap_math_sentinel(MATH_SENTINEL_BLK_MID, rest))
        } else {
            state.active_math_block = None;
            let trimmed = rest.trim();
            let before_close = trimmed.strip_suffix(close_delim).unwrap_or(trimmed).trim_end();
            Cow::Owned(wrap_math_sentinel(MATH_SENTINEL_BLK_CLOSE, before_close))
        }
    } else {
        let after_ws = rest.trim_start_matches([' ', '\t']);
        if !is_list_marker_start(after_ws) {
            loop {
                if let Some(after_tab) = rest.strip_prefix('\t') {
                    extra_indent = extra_indent.saturating_add(1);
                    rest = after_tab;
                } else if let Some(after_sp) = rest.strip_prefix("    ") {
                    extra_indent = extra_indent.saturating_add(1);
                    rest = after_sp;
                } else {
                    break;
                }
            }
        }
        let trimmed = rest.trim_start_matches(' ');
        let trimmed_both = rest.trim();
        let normalize_line = |s: &'a str| -> Cow<'a, str> {
            if !needs_inline_or_heading_norm(s) {
                return Cow::Borrowed(s);
            }
            let h = normalize_atx_heading_space(s);
            let m = match h {
                Cow::Borrowed(v) => preprocess_math_in_line(v),
                Cow::Owned(v) => Cow::Owned(preprocess_math_in_line(&v).into_owned()),
            };
            match m {
                Cow::Borrowed(v) => normalize_inline_marker_spaces(v),
                Cow::Owned(v) => Cow::Owned(normalize_inline_marker_spaces(&v).into_owned()),
            }
        };

        if rest.len() - trimmed.len() <= 3 {
            let math_delim = if trimmed_both.starts_with("$$") {
                Some(("$$", "$$"))
            } else if trimmed_both.starts_with("\\[") {
                Some(("\\[", "\\]"))
            } else {
                None
            };
            if let Some((f_char, f_len)) = parse_fence_open(trimmed) {
                state.active_fence = Some((f_char, f_len));
                Cow::Borrowed(rest)
            } else if let Some((_, _)) = math_delim.filter(|&(_, close_d)| {
                if find_closing_math_pair(trimmed_both, 2, close_d).is_some() {
                    return false;
                }
                let failed_until = if close_d == "$$" {
                    &mut state.failed_dollar_until
                } else {
                    &mut state.failed_bracket_until
                };
                if idx < *failed_until {
                    return false;
                }
                let (found, scanned_until) = find_math_close(close_d);
                if let Some(end_j) = found {
                    state.active_math_block = Some((end_j, close_d));
                    true
                } else {
                    *failed_until = (*failed_until).max(scanned_until);
                    false
                }
            }) {
                let after_open = trimmed_both[2..].trim_start();
                Cow::Owned(wrap_math_sentinel(MATH_SENTINEL_BLK_OPEN, after_open))
            } else {
                normalize_line(rest)
            }
        } else {
            normalize_line(rest)
        }
    };

    let direction = detect_paragraph_direction(&final_line);
    metas.push(LineMeta {
        alignment,
        indent: base_indent.saturating_add(extra_indent).min(8),
        direction,
    });
    out.push_str(&final_line);
    if has_cr {
        out.push('\r');
    }
    if has_trailing_nl {
        out.push('\n');
    }
}

fn preprocess_markdown_for_ir(markdown: &str) -> (String, Vec<LineMeta>) {
    let mut metas = Vec::new();
    let mut out = String::with_capacity(markdown.len() + 16);
    let mut state = PreprocessState::default();
    let mut iter = markdown.split('\n').enumerate().peekable();

    while let Some((idx, raw_line)) = iter.next() {
        let has_trailing_nl = iter.peek().is_some();
        let clean_cr = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let has_cr = raw_line.len() > clean_cr.len();

        process_line_for_ir(
            &mut state,
            idx,
            clean_cr,
            has_cr,
            has_trailing_nl,
            |close_delim| {
                let mut last_seen = idx + 1;
                for (j, raw_j) in iter.clone() {
                    last_seen = j;
                    let clean_j = raw_j.strip_suffix('\r').unwrap_or(raw_j);
                    match classify_multiline_math_line(clean_j, close_delim) {
                        MathLookahead::Close => return (Some(j), j),
                        MathLookahead::AbortFence => return (None, j),
                        MathLookahead::Continue => {}
                    }
                }
                (None, last_seen.saturating_add(1))
            },
            &mut out,
            &mut metas,
        );
    }

    (out, metas)
}

#[derive(Debug, Clone, Copy)]
struct RopeChunkBoundary {
    line_idx: usize,
    stripped_byte_offset: usize,
}

fn offset_block_source_lines(block: &mut DocBlock, delta: usize) {
    if delta == 0 {
        return;
    }
    match block {
        DocBlock::Heading { source_lines, .. }
        | DocBlock::Paragraph { source_lines, .. }
        | DocBlock::CodeBlock { source_lines, .. }
        | DocBlock::Table { source_lines, .. }
        | DocBlock::ThematicBreak { source_lines } => {
            source_lines.start += delta;
            source_lines.end += delta;
        }
        DocBlock::BlockQuote {
            blocks,
            source_lines,
            ..
        }
        | DocBlock::FootnoteDefinition {
            blocks,
            source_lines,
            ..
        } => {
            source_lines.start += delta;
            source_lines.end += delta;
            for inner in blocks {
                offset_block_source_lines(inner, delta);
            }
        }
        DocBlock::List {
            items,
            source_lines,
            ..
        } => {
            source_lines.start += delta;
            source_lines.end += delta;
            for item in items {
                for inner in &mut item.blocks {
                    offset_block_source_lines(inner, delta);
                }
            }
        }
    }
}

fn preprocess_rope_for_ir_cancellable(
    rope: &ropey::Rope,
    cancel: Option<(&std::sync::atomic::AtomicU64, u64)>,
) -> Option<(String, Vec<LineMeta>, Vec<RopeChunkBoundary>, bool)> {
    let total_lines = rope.len_lines();
    let mut metas = Vec::with_capacity(total_lines);
    let mut out = String::with_capacity(rope.len_bytes() + 16);
    let mut state = PreprocessState::default();
    let mut line_scratch = String::with_capacity(256);
    let mut lookahead_scratch = String::with_capacity(256);
    let mut split_points: Vec<RopeChunkBoundary> = Vec::new();
    let mut has_cross_chunk_refs = false;
    let mut prev_was_blank = false;
    let mut open_html_end: Option<&'static str> = None;
    let collect_splits = rope.len_bytes() >= 256 * 1024;

    for (idx, line_slice) in rope.lines().enumerate() {
        if idx & 0xFF == 0
            && let Some((atom, expected)) = cancel
            && atom.load(std::sync::atomic::Ordering::Relaxed) != expected
        {
            return None;
        }
        let has_trailing_nl = idx + 1 < total_lines;
        if has_trailing_nl {
            let lc = line_slice.len_chars();
            if lc == 0 || line_slice.char(lc - 1) != '\n' {
                // Non-`\n` line break (e.g. lone `\r` or U+2028) — match `split('\n')` verbatim.
                let (s, m) = preprocess_markdown_for_ir(&rope.to_string());
                return Some((s, m, Vec::new(), true));
            }
        }
        let raw_line: &str = if let Some(s) = line_slice.as_str() {
            s
        } else {
            line_scratch.clear();
            for chunk in line_slice.chunks() {
                line_scratch.push_str(chunk);
            }
            &line_scratch
        };
        let clean_nl = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let clean_cr = clean_nl.strip_suffix('\r').unwrap_or(clean_nl);
        let has_cr = clean_nl.len() > clean_cr.len();

        let line_byte_start = out.len();
        let (_, _, post_meta) = parse_line_block_meta(clean_cr);
        let post_trimmed = post_meta.trim_start_matches([' ', '\t']);
        if state.active_fence.is_none() && state.active_math_block.is_none() {
            if !has_cross_chunk_refs && line_has_reference_or_footnote_def(post_trimmed) {
                has_cross_chunk_refs = true;
            }
            if collect_splits
                && idx > 0
                && prev_was_blank
                && !has_cross_chunk_refs
                && is_safe_top_level_heading_split(post_meta, &state, open_html_end.is_some())
            {
                split_points.push(RopeChunkBoundary {
                    line_idx: idx,
                    stripped_byte_offset: line_byte_start,
                });
            }
            update_html_block_tracker(post_trimmed, &mut open_html_end);
        }

        process_line_for_ir(
            &mut state,
            idx,
            clean_cr,
            has_cr,
            has_trailing_nl,
            |close_delim| {
                for j in (idx + 1)..total_lines {
                    if (j - idx) & 0xFF == 0
                        && let Some((atom, expected)) = cancel
                        && atom.load(std::sync::atomic::Ordering::Relaxed) != expected
                    {
                        return (None, j);
                    }
                    let ls = rope.line(j);
                    let raw_j: &str = if let Some(s) = ls.as_str() {
                        s
                    } else {
                        lookahead_scratch.clear();
                        for chunk in ls.chunks() {
                            lookahead_scratch.push_str(chunk);
                        }
                        &lookahead_scratch
                    };
                    let clean_nl_j = raw_j.strip_suffix('\n').unwrap_or(raw_j);
                    let clean_j = clean_nl_j.strip_suffix('\r').unwrap_or(clean_nl_j);
                    match classify_multiline_math_line(clean_j, close_delim) {
                        MathLookahead::Close => return (Some(j), j),
                        MathLookahead::AbortFence => return (None, j),
                        MathLookahead::Continue => {}
                    }
                }
                (None, total_lines)
            },
            &mut out,
            &mut metas,
        );

        prev_was_blank = post_trimmed.is_empty();
    }

    Some((out, metas, split_points, has_cross_chunk_refs))
}

fn convert_preprocessed_to_ir(stripped: &str, line_metas: &[LineMeta]) -> Vec<DocBlock> {
    let arena = Arena::new();
    let opts = gfm_options();
    let root = parse_document(&arena, stripped, &opts);
    let mut blocks = Vec::new();
    for child in root.children() {
        convert_block_into(child, line_metas, 0, &mut blocks);
    }
    blocks
}

/// Parses a Markdown string into the shared `DocBlock` intermediate representation.
pub fn parse_to_ir(markdown: &str) -> Vec<DocBlock> {
    let (stripped, line_metas) = preprocess_markdown_for_ir(markdown);
    convert_preprocessed_to_ir(&stripped, &line_metas)
}

/// Streams a `ropey::Rope` directly into the shared `DocBlock` intermediate representation
/// without first copying the entire buffer into an intermediate `String`, parallelizing large
/// safe multi-section documents across CPU cores.
pub fn parse_rope_to_ir(rope: &ropey::Rope) -> Vec<DocBlock> {
    parse_rope_to_ir_cancellable(rope, None).unwrap_or_default()
}

/// Cancellable variant of `parse_rope_to_ir` that aborts early if `*cancel.0 != cancel.1`.
pub fn parse_rope_to_ir_cancellable(
    rope: &ropey::Rope,
    cancel: Option<(&std::sync::atomic::AtomicU64, u64)>,
) -> Option<Vec<DocBlock>> {
    let (stripped, line_metas, split_points, has_cross_chunk_refs) =
        preprocess_rope_for_ir_cancellable(rope, cancel)?;

    if let Some((atom, expected)) = cancel
        && atom.load(std::sync::atomic::Ordering::Relaxed) != expected
    {
        return None;
    }

    let workers = crate::document::adaptive_worker_count(8);
    if has_cross_chunk_refs || workers < 2 || split_points.len() < 2 || stripped.len() < 256 * 1024 {
        return Some(convert_preprocessed_to_ir(&stripped, &line_metas));
    }

    // Group heading split points into ~`workers` coarse contiguous chunks of >= 48 KB each.
    let target_chunk_bytes = (stripped.len() / workers).max(48 * 1024);
    let mut chunks: Vec<(usize, usize, usize, usize)> = Vec::with_capacity(workers);
    let mut cur_line = 0usize;
    let mut cur_byte = 0usize;
    for sp in &split_points {
        if sp.stripped_byte_offset.saturating_sub(cur_byte) >= target_chunk_bytes
            && sp.line_idx > cur_line
        {
            chunks.push((cur_line, sp.line_idx, cur_byte, sp.stripped_byte_offset));
            cur_line = sp.line_idx;
            cur_byte = sp.stripped_byte_offset;
        }
    }
    if cur_byte < stripped.len() {
        chunks.push((cur_line, line_metas.len(), cur_byte, stripped.len()));
    }

    if chunks.len() < 2 {
        return Some(convert_preprocessed_to_ir(&stripped, &line_metas));
    }

    let mut parsed_chunks: Vec<Option<Vec<DocBlock>>> = vec![None; chunks.len()];
    let stripped_ref = stripped.as_str();
    let metas_ref = line_metas.as_slice();
    std::thread::scope(|s| {
        for (slot, &(start_line, end_line, start_byte, end_byte)) in
            parsed_chunks.iter_mut().zip(chunks.iter())
        {
            s.spawn(move || {
                if let Some((atom, expected)) = cancel
                    && atom.load(std::sync::atomic::Ordering::Relaxed) != expected
                {
                    return;
                }
                let mut blocks = convert_preprocessed_to_ir(
                    &stripped_ref[start_byte..end_byte],
                    &metas_ref[start_line..end_line.min(metas_ref.len())],
                );
                if start_line > 0 {
                    for b in &mut blocks {
                        offset_block_source_lines(b, start_line);
                    }
                }
                *slot = Some(blocks);
            });
        }
    });

    let mut all_blocks = Vec::new();
    for slot in parsed_chunks {
        let blocks = slot?;
        all_blocks.extend(blocks);
    }
    Some(all_blocks)
}

/// Serializes a `comrak` AST back to canonical CommonMark + GFM Markdown.
pub fn serialize_commonmark<'a>(root: &'a AstNode<'a>) -> String {
    let opts = gfm_options();
    let mut out = Vec::new();
    let _ = format_commonmark(root, &opts, &mut out);
    String::from_utf8(out).unwrap_or_default()
}

/// Parses and re-serializes Markdown through `comrak`.
pub fn roundtrip_markdown(markdown: &str) -> String {
    let (stripped, _) = strip_block_metadata_lines(markdown);
    let arena = Arena::new();
    let opts = gfm_options();
    let root = parse_document(&arena, &stripped, &opts);
    serialize_commonmark(root)
}

/// Compares two Markdown documents by their parsed `comrak` AST structure (ignoring sourcepos).
pub fn ast_structurally_equal(left_md: &str, right_md: &str) -> bool {
    let (stripped_a, _) = strip_block_metadata_lines(left_md);
    let (stripped_b, _) = strip_block_metadata_lines(right_md);
    let arena_a = Arena::new();
    let arena_b = Arena::new();
    let opts = gfm_options();
    let root_a = parse_document(&arena_a, &stripped_a, &opts);
    let root_b = parse_document(&arena_b, &stripped_b, &opts);
    nodes_equal(root_a, root_b)
}

/// Verifies the round-trip invariant `parse(serialize(ast)) == ast` for a given Markdown string.
pub fn verify_roundtrip_invariant(markdown: &str) -> bool {
    let serialized = roundtrip_markdown(markdown);
    ast_structurally_equal(markdown, &serialized)
}

fn is_end_list_sentinel<'a>(node: &'a AstNode<'a>) -> bool {
    matches!(
        &node.data.borrow().value,
        NodeValue::HtmlBlock(hb) if hb.literal.trim() == "<!-- end list -->"
    )
}

fn nodes_equal<'a, 'b>(a: &'a AstNode<'a>, b: &'b AstNode<'b>) -> bool {
    let val_a = &a.data.borrow().value;
    let val_b = &b.data.borrow().value;
    if !node_values_equivalent(val_a, val_b) {
        return false;
    }
    let children_a: Vec<_> = a.children().filter(|c| !is_end_list_sentinel(c)).collect();
    let children_b: Vec<_> = b.children().filter(|c| !is_end_list_sentinel(c)).collect();
    if children_a.len() != children_b.len() {
        return false;
    }
    children_a
        .iter()
        .zip(children_b.iter())
        .all(|(ca, cb)| nodes_equal(ca, cb))
}

fn node_values_equivalent(a: &NodeValue, b: &NodeValue) -> bool {
    match (a, b) {
        (NodeValue::Heading(ha), NodeValue::Heading(hb)) => ha.level == hb.level,
        (NodeValue::List(la), NodeValue::List(lb)) => {
            la.list_type == lb.list_type
                && la.start == lb.start
                && la.is_task_list == lb.is_task_list
                && la.tight == lb.tight
        }
        (NodeValue::Item(la), NodeValue::Item(lb)) => {
            la.list_type == lb.list_type
                && la.start == lb.start
                && la.is_task_list == lb.is_task_list
                && la.tight == lb.tight
        }
        (NodeValue::CodeBlock(ca), NodeValue::CodeBlock(cb)) => {
            ca.info == cb.info && ca.literal == cb.literal
        }
        (NodeValue::TaskItem(ta), NodeValue::TaskItem(tb)) => ta.is_some() == tb.is_some(),
        _ => a == b,
    }
}

fn source_lines_from_node<'a>(node: &'a AstNode<'a>) -> Range<usize> {
    let sp = node.data.borrow().sourcepos;
    let start = sp.start.line.saturating_sub(1);
    let end = sp.end.line.max(sp.start.line);
    start..end
}

fn meta_for_lines(line_metas: &[LineMeta], source_lines: &Range<usize>) -> LineMeta {
    line_metas
        .get(source_lines.start)
        .copied()
        .unwrap_or(LineMeta {
            alignment: BlockAlignment::Start,
            indent: 0,
            direction: TextDirection::Ltr,
        })
}

fn detect_runs_direction(runs: &[InlineRun]) -> Option<TextDirection> {
    for r in runs {
        if r.text.is_empty() {
            continue;
        }
        if r.text.bytes().all(|b| b.is_ascii()) {
            if r.text.bytes().any(|b| b.is_ascii_alphabetic()) {
                return Some(TextDirection::Ltr);
            }
            continue;
        }
        let d = detect_paragraph_direction(&r.text);
        if d == TextDirection::Rtl || r.text.chars().any(|c| c.is_alphabetic()) {
            return Some(d);
        }
    }
    None
}

const MAX_AST_DEPTH: usize = 128;

fn convert_block_into<'a>(
    node: &'a AstNode<'a>,
    line_metas: &[LineMeta],
    parent_indent: u8,
    out: &mut Vec<DocBlock>,
) {
    convert_block_into_depth(node, line_metas, parent_indent, out, 0);
}

fn convert_block_into_depth<'a>(
    node: &'a AstNode<'a>,
    line_metas: &[LineMeta],
    parent_indent: u8,
    out: &mut Vec<DocBlock>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let source_lines = source_lines_from_node(node);
    let meta = meta_for_lines(line_metas, &source_lines);
    let value = std::mem::replace(&mut node.data.borrow_mut().value, NodeValue::Document);
    match value {
        NodeValue::Heading(NodeHeading { level, .. }) => {
            let mut runs = Vec::new();
            let mut style = InlineStyle::default();
            collect_inline_runs(node, &mut style, &mut runs);
            let direction = detect_runs_direction(&runs).unwrap_or(meta.direction);
            out.push(DocBlock::Heading {
                level,
                runs,
                alignment: meta.alignment,
                indent_level: meta.indent.saturating_sub(parent_indent),
                direction,
                source_lines,
            });
        }
        NodeValue::Paragraph => {
            let mut line_buckets: Vec<LineBucket> = vec![LineBucket::default()];
            let mut style = InlineStyle::default();
            collect_inline_runs_lines(node, &mut style, &mut line_buckets);
            while line_buckets.len() > 1
                && line_buckets
                    .last()
                    .is_some_and(|b| b.runs.is_empty() && b.math_marker.is_none())
            {
                line_buckets.pop();
            }

            if line_buckets.iter().any(|b| b.math_marker.is_some()) {
                emit_paragraph_with_math_blocks(
                    &mut line_buckets,
                    &source_lines,
                    line_metas,
                    parent_indent,
                    out,
                );
                return;
            }

            let mut plain_buckets: Vec<Vec<InlineRun>> =
                line_buckets.into_iter().map(|b| b.runs).collect();
            let max_lines = source_lines.len().max(1);
            if plain_buckets.len() <= 1 || max_lines <= 1 {
                let mut runs = Vec::new();
                for (idx, bucket) in plain_buckets.into_iter().enumerate() {
                    if idx > 0 && !runs.is_empty() && !bucket.is_empty() {
                        runs.push(InlineRun::plain("\n"));
                    }
                    runs.extend(bucket);
                }
                let direction = detect_runs_direction(&runs).unwrap_or(meta.direction);
                out.push(DocBlock::Paragraph {
                    runs,
                    alignment: meta.alignment,
                    indent_level: meta.indent.saturating_sub(parent_indent),
                    direction,
                    source_lines,
                });
                return;
            }

            // Merge any extra line buckets beyond source_lines.len() into the last bucket
            while plain_buckets.len() > max_lines {
                let tail = plain_buckets.pop().unwrap();
                if let Some(last) = plain_buckets.last_mut() {
                    if !last.is_empty() && !tail.is_empty() {
                        last.push(InlineRun::plain(" "));
                    }
                    last.extend(tail);
                }
            }

            let total_buckets = plain_buckets.len();
            for (idx, runs) in plain_buckets.into_iter().enumerate() {
                let line_start = source_lines.start + idx;
                let line_end = if idx + 1 == total_buckets {
                    source_lines.end
                } else {
                    (line_start + 1).min(source_lines.end)
                };
                let line_range = line_start..line_end;
                let line_meta = meta_for_lines(line_metas, &line_range);
                let direction = detect_runs_direction(&runs).unwrap_or(line_meta.direction);
                out.push(DocBlock::Paragraph {
                    runs,
                    alignment: line_meta.alignment,
                    indent_level: line_meta.indent.saturating_sub(parent_indent),
                    direction,
                    source_lines: line_range,
                });
            }
        }
        NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
            let own_indent = meta.indent.saturating_sub(parent_indent);
            let child_parent_indent = parent_indent.saturating_add(own_indent);
            let mut blocks = Vec::new();
            for child in node.children() {
                convert_block_into_depth(child, line_metas, child_parent_indent, &mut blocks, depth + 1);
            }
            out.push(DocBlock::BlockQuote {
                blocks,
                indent_level: own_indent,
                direction: meta.direction,
                source_lines,
            });
        }
        NodeValue::CodeBlock(cb) => out.push(DocBlock::CodeBlock {
            info: cb.info,
            code: cb.literal,
            source_lines,
        }),
        NodeValue::List(nl) => {
            let ordered = nl.list_type == ListType::Ordered;
            let own_indent = meta.indent.saturating_sub(parent_indent);
            let child_parent_indent = parent_indent.saturating_add(own_indent);
            let mut items = Vec::new();
            for child in node.children() {
                let child_val =
                    std::mem::replace(&mut child.data.borrow_mut().value, NodeValue::Document);
                match child_val {
                    NodeValue::Item(_) => {
                        let mut blocks = Vec::new();
                        for sub in child.children() {
                            convert_block_into_depth(sub, line_metas, child_parent_indent, &mut blocks, depth + 1);
                        }
                        if blocks.is_empty() {
                            let item_lines = source_lines_from_node(child);
                            let item_meta = meta_for_lines(line_metas, &item_lines);
                            blocks.push(DocBlock::Paragraph {
                                runs: Vec::new(),
                                alignment: item_meta.alignment,
                                indent_level: item_meta.indent.saturating_sub(child_parent_indent),
                                direction: item_meta.direction,
                                source_lines: item_lines,
                            });
                        }
                        items.push(ListItem {
                            checked: None,
                            blocks,
                        });
                    }
                    NodeValue::TaskItem(mark) => {
                        let mut blocks = Vec::new();
                        for sub in child.children() {
                            convert_block_into_depth(sub, line_metas, child_parent_indent, &mut blocks, depth + 1);
                        }
                        if blocks.is_empty() {
                            let item_lines = source_lines_from_node(child);
                            let item_meta = meta_for_lines(line_metas, &item_lines);
                            blocks.push(DocBlock::Paragraph {
                                runs: Vec::new(),
                                alignment: item_meta.alignment,
                                indent_level: item_meta.indent.saturating_sub(child_parent_indent),
                                direction: item_meta.direction,
                                source_lines: item_lines,
                            });
                        }
                        items.push(ListItem {
                            checked: Some(mark.is_some()),
                            blocks,
                        });
                    }
                    _ => {}
                }
            }
            let first_item_dir = items
                .first()
                .and_then(|it| it.blocks.first())
                .map(|b| match b {
                    DocBlock::Paragraph { direction, .. }
                    | DocBlock::Heading { direction, .. } => *direction,
                    _ => meta.direction,
                })
                .unwrap_or(meta.direction);
            out.push(DocBlock::List {
                ordered,
                start: nl.start.max(1),
                items,
                indent_level: own_indent,
                direction: first_item_dir,
                source_lines,
            });
        }
        NodeValue::Table(nt) => {
            let alignments = nt
                .alignments
                .iter()
                .map(|a| match a {
                    TableAlignment::None => TableAlign::None,
                    TableAlignment::Left => TableAlign::Left,
                    TableAlignment::Center => TableAlign::Center,
                    TableAlignment::Right => TableAlign::Right,
                })
                .collect();
            let mut header = Vec::new();
            let mut rows = Vec::new();
            for row_node in node.children() {
                if let NodeValue::TableRow(is_header) = row_node.data.borrow().value {
                    let mut cells = Vec::new();
                    for cell_node in row_node.children() {
                        let mut runs = Vec::new();
                        let mut style = InlineStyle::default();
                        collect_inline_runs(cell_node, &mut style, &mut runs);
                        cells.push(runs);
                    }
                    if is_header {
                        header = cells;
                    } else {
                        rows.push(cells);
                    }
                }
            }
            out.push(DocBlock::Table {
                alignments,
                header,
                rows,
                source_lines,
            });
        }
        NodeValue::ThematicBreak => out.push(DocBlock::ThematicBreak { source_lines }),
        NodeValue::FootnoteDefinition(fd) => {
            let mut blocks = Vec::new();
            for child in node.children() {
                convert_block_into_depth(child, line_metas, parent_indent, &mut blocks, depth + 1);
            }
            out.push(DocBlock::FootnoteDefinition {
                name: fd.name,
                blocks,
                source_lines,
            });
        }
        NodeValue::HtmlBlock(hb) => out.push(DocBlock::CodeBlock {
            info: "html".to_string(),
            code: hb.literal,
            source_lines,
        }),
        _ => {}
    }
}

#[derive(Debug, Clone, Default)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    link_url: Option<String>,
}

#[derive(Debug, Clone)]
enum LineMathMarker {
    SingleDisplay(String),
    BlockOpen(String),
    BlockMid(String),
    BlockClose(String),
}

#[derive(Debug, Clone, Default)]
struct LineBucket {
    runs: Vec<InlineRun>,
    math_marker: Option<LineMathMarker>,
}

fn emit_paragraph_with_math_blocks(
    line_buckets: &mut [LineBucket],
    source_lines: &Range<usize>,
    line_metas: &[LineMeta],
    parent_indent: u8,
    out: &mut Vec<DocBlock>,
) {
    let total = line_buckets.len();
    let mut idx = 0usize;
    while idx < total {
        match line_buckets[idx].math_marker.clone() {
            Some(LineMathMarker::BlockOpen(open_s)) => {
                let mut block_lines: Vec<String> = Vec::new();
                if !open_s.is_empty() {
                    block_lines.push(open_s);
                }
                let mut end_idx = idx;
                let mut j = idx + 1;
                while j < total {
                    match line_buckets[j].math_marker.clone() {
                        Some(LineMathMarker::BlockMid(m)) => {
                            block_lines.push(m);
                            end_idx = j;
                            j += 1;
                        }
                        Some(LineMathMarker::BlockClose(c)) => {
                            if !c.is_empty() {
                                block_lines.push(c);
                            }
                            end_idx = j;
                            break;
                        }
                        _ => break,
                    }
                }
                let code = if block_lines.len() == 1 && block_lines[0].is_empty() {
                    String::new()
                } else {
                    block_lines.join("\n")
                };
                let blk_start = (source_lines.start + idx).min(source_lines.end.saturating_sub(1));
                let blk_end = (source_lines.start + end_idx + 1)
                    .min(source_lines.end)
                    .max(blk_start + 1);
                out.push(DocBlock::CodeBlock {
                    info: "math".to_string(),
                    code,
                    source_lines: blk_start..blk_end,
                });
                idx = end_idx + 1;
            }
            Some(LineMathMarker::SingleDisplay(expr)) if line_buckets[idx].runs.len() <= 1 => {
                let line_start = (source_lines.start + idx).min(source_lines.end.saturating_sub(1));
                let line_end = (line_start + 1).min(source_lines.end).max(line_start + 1);
                out.push(DocBlock::CodeBlock {
                    info: "math".to_string(),
                    code: expr.trim().to_string(),
                    source_lines: line_start..line_end,
                });
                idx += 1;
            }
            _ => {
                let runs = std::mem::take(&mut line_buckets[idx].runs);
                let line_start = (source_lines.start + idx).min(source_lines.end.saturating_sub(1));
                let line_end = if idx + 1 == total {
                    source_lines.end.max(line_start + 1)
                } else {
                    (line_start + 1).min(source_lines.end).max(line_start + 1)
                };
                let line_range = line_start..line_end;
                let line_meta = meta_for_lines(line_metas, &line_range);
                let direction = detect_runs_direction(&runs).unwrap_or(line_meta.direction);
                out.push(DocBlock::Paragraph {
                    runs,
                    alignment: line_meta.alignment,
                    indent_level: line_meta.indent.saturating_sub(parent_indent),
                    direction,
                    source_lines: line_range,
                });
                idx += 1;
            }
        }
    }
}

fn collect_inline_runs<'a>(
    node: &'a AstNode<'a>,
    cur_style: &mut InlineStyle,
    out: &mut Vec<InlineRun>,
) {
    collect_inline_runs_depth(node, cur_style, out, 0);
}

fn collect_inline_runs_depth<'a>(
    node: &'a AstNode<'a>,
    cur_style: &mut InlineStyle,
    out: &mut Vec<InlineRun>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    for child in node.children() {
        let val = std::mem::replace(&mut child.data.borrow_mut().value, NodeValue::Document);
        match val {
            NodeValue::Text(t) => push_run(out, t, cur_style, false),
            NodeValue::Code(c) => {
                if let Some(expr) = decode_math_sentinel(&c.literal, MATH_SENTINEL_INL)
                    .or_else(|| decode_math_sentinel(&c.literal, MATH_SENTINEL_DISP))
                {
                    out.push(InlineRun {
                        text: expr,
                        bold: cur_style.bold,
                        italic: cur_style.italic,
                        underline: cur_style.underline,
                        strikethrough: cur_style.strikethrough,
                        code: false,
                        link_url: cur_style.link_url.clone(),
                        image_url: None,
                        is_footnote_ref: false,
                        is_math: true,
                    });
                } else {
                    push_run(out, c.literal, cur_style, true);
                }
            }
            NodeValue::SoftBreak => push_run(out, " ".to_string(), cur_style, false),
            NodeValue::LineBreak => push_run(out, "\n".to_string(), cur_style, false),
            NodeValue::Strong => {
                let prev = cur_style.bold;
                cur_style.bold = true;
                collect_inline_runs_depth(child, cur_style, out, depth + 1);
                cur_style.bold = prev;
            }
            NodeValue::Emph => {
                let prev = cur_style.italic;
                cur_style.italic = true;
                collect_inline_runs_depth(child, cur_style, out, depth + 1);
                cur_style.italic = prev;
            }
            NodeValue::Strikethrough => {
                let prev = cur_style.strikethrough;
                cur_style.strikethrough = true;
                collect_inline_runs_depth(child, cur_style, out, depth + 1);
                cur_style.strikethrough = prev;
            }
            NodeValue::Link(link) => {
                let prev = cur_style.link_url.take();
                cur_style.link_url = Some(link.url);
                collect_inline_runs_depth(child, cur_style, out, depth + 1);
                cur_style.link_url = prev;
            }
            NodeValue::Image(img) => {
                let mut alt = Vec::new();
                let mut alt_style = cur_style.clone();
                collect_inline_runs_depth(child, &mut alt_style, &mut alt, depth + 1);
                let alt_text: String = alt.into_iter().map(|r| r.text).collect();
                let label = if alt_text.trim().is_empty() {
                    img.url.clone()
                } else {
                    alt_text
                };
                out.push(InlineRun {
                    text: label,
                    bold: cur_style.bold,
                    italic: cur_style.italic,
                    underline: cur_style.underline,
                    strikethrough: cur_style.strikethrough,
                    code: false,
                    link_url: cur_style.link_url.clone(),
                    image_url: Some(img.url),
                    is_footnote_ref: false,
                    is_math: false,
                });
            }
            NodeValue::FootnoteReference(fr) => {
                out.push(InlineRun {
                    text: format!("[^{}]", fr.name),
                    bold: cur_style.bold,
                    italic: cur_style.italic,
                    underline: cur_style.underline,
                    strikethrough: cur_style.strikethrough,
                    code: false,
                    link_url: cur_style.link_url.clone(),
                    image_url: None,
                    is_footnote_ref: true,
                    is_math: false,
                });
            }
            NodeValue::HtmlInline(raw) => {
                let tag = raw.trim().to_ascii_lowercase();
                if tag == "<u>" || tag == "<ins>" {
                    cur_style.underline = true;
                } else if tag == "</u>" || tag == "</ins>" {
                    cur_style.underline = false;
                } else if tag == "<br>" || tag == "<br/>" || tag == "<br />" {
                    push_run(out, "\n".to_string(), cur_style, false);
                } else if tag.starts_with("<!--") && tag.ends_with("-->") {
                    // Omit inline HTML comments (including cached Excel table formulas) from rendered runs.
                } else {
                    push_run(out, raw, cur_style, false);
                }
            }
            _ => collect_inline_runs_depth(child, cur_style, out, depth + 1),
        }
    }
}

fn collect_inline_runs_lines<'a>(
    node: &'a AstNode<'a>,
    cur_style: &mut InlineStyle,
    lines: &mut Vec<LineBucket>,
) {
    collect_inline_runs_lines_depth(node, cur_style, lines, 0);
}

fn collect_inline_runs_lines_depth<'a>(
    node: &'a AstNode<'a>,
    cur_style: &mut InlineStyle,
    lines: &mut Vec<LineBucket>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    for child in node.children() {
        let val = std::mem::replace(&mut child.data.borrow_mut().value, NodeValue::Document);
        match val {
            NodeValue::Text(t) => push_run(&mut lines.last_mut().unwrap().runs, t, cur_style, false),
            NodeValue::Code(c) => {
                let bucket = lines.last_mut().unwrap();
                if let Some(expr) = decode_math_sentinel(&c.literal, MATH_SENTINEL_INL) {
                    bucket.runs.push(InlineRun {
                        text: expr,
                        bold: cur_style.bold,
                        italic: cur_style.italic,
                        underline: cur_style.underline,
                        strikethrough: cur_style.strikethrough,
                        code: false,
                        link_url: cur_style.link_url.clone(),
                        image_url: None,
                        is_footnote_ref: false,
                        is_math: true,
                    });
                } else if let Some(expr) = decode_math_sentinel(&c.literal, MATH_SENTINEL_DISP) {
                    if bucket.runs.is_empty() && bucket.math_marker.is_none() {
                        bucket.math_marker = Some(LineMathMarker::SingleDisplay(expr.clone()));
                    } else {
                        bucket.math_marker = None;
                    }
                    bucket.runs.push(InlineRun {
                        text: expr,
                        bold: cur_style.bold,
                        italic: cur_style.italic,
                        underline: cur_style.underline,
                        strikethrough: cur_style.strikethrough,
                        code: false,
                        link_url: cur_style.link_url.clone(),
                        image_url: None,
                        is_footnote_ref: false,
                        is_math: true,
                    });
                } else if let Some(s) = decode_math_sentinel(&c.literal, MATH_SENTINEL_BLK_OPEN) {
                    bucket.math_marker = Some(LineMathMarker::BlockOpen(s));
                } else if let Some(s) = decode_math_sentinel(&c.literal, MATH_SENTINEL_BLK_MID) {
                    bucket.math_marker = Some(LineMathMarker::BlockMid(s));
                } else if let Some(s) = decode_math_sentinel(&c.literal, MATH_SENTINEL_BLK_CLOSE) {
                    bucket.math_marker = Some(LineMathMarker::BlockClose(s));
                } else {
                    push_run(&mut bucket.runs, c.literal, cur_style, true);
                }
            }
            NodeValue::SoftBreak | NodeValue::LineBreak => {
                lines.push(LineBucket::default());
            }
            NodeValue::Strong => {
                let prev = cur_style.bold;
                cur_style.bold = true;
                collect_inline_runs_lines_depth(child, cur_style, lines, depth + 1);
                cur_style.bold = prev;
            }
            NodeValue::Emph => {
                let prev = cur_style.italic;
                cur_style.italic = true;
                collect_inline_runs_lines_depth(child, cur_style, lines, depth + 1);
                cur_style.italic = prev;
            }
            NodeValue::Strikethrough => {
                let prev = cur_style.strikethrough;
                cur_style.strikethrough = true;
                collect_inline_runs_lines_depth(child, cur_style, lines, depth + 1);
                cur_style.strikethrough = prev;
            }
            NodeValue::Link(link) => {
                let prev = cur_style.link_url.take();
                cur_style.link_url = Some(link.url);
                collect_inline_runs_lines_depth(child, cur_style, lines, depth + 1);
                cur_style.link_url = prev;
            }
            NodeValue::Image(img) => {
                let mut alt = Vec::new();
                let mut alt_style = cur_style.clone();
                collect_inline_runs_depth(child, &mut alt_style, &mut alt, depth + 1);
                let alt_text: String = alt.into_iter().map(|r| r.text).collect();
                let label = if alt_text.trim().is_empty() {
                    img.url.clone()
                } else {
                    alt_text
                };
                lines.last_mut().unwrap().runs.push(InlineRun {
                    text: label,
                    bold: cur_style.bold,
                    italic: cur_style.italic,
                    underline: cur_style.underline,
                    strikethrough: cur_style.strikethrough,
                    code: false,
                    link_url: cur_style.link_url.clone(),
                    image_url: Some(img.url),
                    is_footnote_ref: false,
                    is_math: false,
                });
            }
            NodeValue::FootnoteReference(fr) => {
                lines.last_mut().unwrap().runs.push(InlineRun {
                    text: format!("[^{}]", fr.name),
                    bold: cur_style.bold,
                    italic: cur_style.italic,
                    underline: cur_style.underline,
                    strikethrough: cur_style.strikethrough,
                    code: false,
                    link_url: cur_style.link_url.clone(),
                    image_url: None,
                    is_footnote_ref: true,
                    is_math: false,
                });
            }
            NodeValue::HtmlInline(raw) => {
                let tag = raw.trim().to_ascii_lowercase();
                if tag == "<u>" || tag == "<ins>" {
                    cur_style.underline = true;
                } else if tag == "</u>" || tag == "</ins>" {
                    cur_style.underline = false;
                } else if tag == "<br>" || tag == "<br/>" || tag == "<br />" {
                    lines.push(LineBucket::default());
                } else if tag.starts_with("<!--") && tag.ends_with("-->") {
                    // Omit inline HTML comments (including cached Excel table formulas) from rendered runs.
                } else {
                    push_run(&mut lines.last_mut().unwrap().runs, raw, cur_style, false);
                }
            }
            _ => collect_inline_runs_lines_depth(child, cur_style, lines, depth + 1),
        }
    }
}

fn push_run(out: &mut Vec<InlineRun>, text: String, style: &InlineStyle, code: bool) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.image_url.is_none()
            && !last.is_footnote_ref
            && !last.is_math
            && last.bold == style.bold
            && last.italic == style.italic
            && last.underline == style.underline
            && last.strikethrough == style.strikethrough
            && last.code == code
            && last.link_url == style.link_url
        {
            last.text.push_str(&text);
            return;
        }
    out.push(InlineRun {
        text,
        bold: style.bold,
        italic: style.italic,
        underline: style.underline,
        strikethrough: style.strikethrough,
        code,
        link_url: style.link_url.clone(),
        image_url: None,
        is_footnote_ref: false,
        is_math: false,
    });
}

/// Editable GFM table snapshot extracted from the AST with its 0-based source line span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableData {
    pub alignments: Vec<TableAlign>,
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub source_lines: Range<usize>,
}

/// Extracts all GFM tables from Markdown along with their editable cell strings and source lines.
pub fn extract_tables(markdown: &str) -> Vec<TableData> {
    let (stripped, _) = strip_block_metadata_lines(markdown);
    let arena = Arena::new();
    let opts = gfm_options();
    let root = parse_document(&arena, &stripped, &opts);
    let mut tables = Vec::new();

    for node in root.descendants() {
        if let NodeValue::Table(ref nt) = node.data.borrow().value {
            let source_lines = source_lines_from_node(node);
            let alignments = nt
                .alignments
                .iter()
                .map(|a| match a {
                    TableAlignment::None => TableAlign::None,
                    TableAlignment::Left => TableAlign::Left,
                    TableAlignment::Center => TableAlign::Center,
                    TableAlignment::Right => TableAlign::Right,
                })
                .collect();
            let mut header = Vec::new();
            let mut rows = Vec::new();
            for row_node in node.children() {
                if let NodeValue::TableRow(is_header) = row_node.data.borrow().value {
                    let mut row_cells = Vec::new();
                    for cell_node in row_node.children() {
                        row_cells.push(format_cell_inlines(cell_node, &opts));
                    }
                    if is_header {
                        header = row_cells;
                    } else {
                        rows.push(row_cells);
                    }
                }
            }
            tables.push(TableData {
                alignments,
                header,
                rows,
                source_lines,
            });
        }
    }
    tables
}

fn is_plain_table_text_fast(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b' ' | b',' | b'.' | b'/' | b'%' | b'$' | b':')
        })
        && !s.contains("http:")
        && !s.contains("https:")
        && !s.contains("www.")
        && !s.as_bytes().first().is_some_and(|b| b.is_ascii_digit())
}

fn format_cell_inlines<'a>(cell_node: &'a AstNode<'a>, opts: &Options<'_>) -> String {
    let Some(first) = cell_node.first_child() else {
        return String::new();
    };
    if first.next_sibling().is_none()
        && let NodeValue::Text(ref t) = first.data.borrow().value
        && is_plain_table_text_fast(t)
    {
        return t.clone();
    }
    let arena = Arena::new();
    let doc = arena.alloc(Node::new(RefCell::new(Ast::new(
        NodeValue::Document,
        (1, 1).into(),
    ))));
    let para = arena.alloc(Node::new(RefCell::new(Ast::new(
        NodeValue::Paragraph,
        (1, 1).into(),
    ))));
    doc.append(para);
    for child in cell_node.children() {
        let cloned = clone_ast(&arena, child);
        para.append(cloned);
    }
    let mut buf = Vec::new();
    let _ = format_commonmark(doc, opts, &mut buf);
    let mut s = String::from_utf8(buf).unwrap_or_default();
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    s
}

fn clone_ast<'a, 'b>(arena: &'a Arena<AstNode<'a>>, src: &'b AstNode<'b>) -> &'a AstNode<'a> {
    let cloned = arena.alloc(Node::new(RefCell::new(Ast::new(
        src.data.borrow().value.clone(),
        src.data.borrow().sourcepos.start,
    ))));
    for child in src.children() {
        cloned.append(clone_ast(arena, child));
    }
    cloned
}

/// Builds and serializes a valid GFM table via `comrak` AST construction + `format_commonmark`.
pub fn build_table_markdown(
    alignments: &[TableAlign],
    header: &[String],
    rows: &[Vec<String>],
) -> String {
    let num_cols = header
        .len()
        .max(alignments.len())
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0))
        .max(1);

    let mut norm_aligns = Vec::with_capacity(num_cols);
    for i in 0..num_cols {
        let a = alignments.get(i).copied().unwrap_or(TableAlign::None);
        norm_aligns.push(match a {
            TableAlign::None => TableAlignment::None,
            TableAlign::Left => TableAlignment::Left,
            TableAlign::Center => TableAlignment::Center,
            TableAlign::Right => TableAlignment::Right,
        });
    }

    let arena = Arena::new();
    let opts = gfm_options();
    let root = arena.alloc(Node::new(RefCell::new(Ast::new(
        NodeValue::Document,
        (1, 1).into(),
    ))));

    let table = arena.alloc(Node::new(RefCell::new(Ast::new(
        NodeValue::Table(NodeTable {
            alignments: norm_aligns,
            num_columns: num_cols,
            num_rows: 1 + rows.len(),
            num_nonempty_cells: 0,
        }),
        (1, 1).into(),
    ))));
    root.append(table);

    let append_row = |is_header: bool, cells: &[String]| {
        let row_node = arena.alloc(Node::new(RefCell::new(Ast::new(
            NodeValue::TableRow(is_header),
            (1, 1).into(),
        ))));
        table.append(row_node);
        for c in 0..num_cols {
            let cell_node = arena.alloc(Node::new(RefCell::new(Ast::new(
                NodeValue::TableCell,
                (1, 1).into(),
            ))));
            row_node.append(cell_node);
            let raw = cells.get(c).map(|s| s.as_str()).unwrap_or("");
            if raw.is_empty() {
                continue;
            }
            if !raw.contains(['\n', '|']) && is_plain_table_text_fast(raw) {
                let txt = arena.alloc(Node::new(RefCell::new(Ast::new(
                    NodeValue::Text(raw.to_string()),
                    (1, 1).into(),
                ))));
                cell_node.append(txt);
                continue;
            }
            let sanitized = raw.replace('\n', " ").replace('|', "\\|");
            if !sanitized.is_empty() {
                let cell_doc = parse_document(&arena, &sanitized, &opts);
                if let Some(first_child) = cell_doc.first_child() {
                    if matches!(first_child.data.borrow().value, NodeValue::Paragraph) {
                        for inline in first_child.children() {
                            cell_node.append(clone_ast(&arena, inline));
                        }
                    } else {
                        let txt = arena.alloc(Node::new(RefCell::new(Ast::new(
                            NodeValue::Text(sanitized),
                            (1, 1).into(),
                        ))));
                        cell_node.append(txt);
                    }
                }
            }
        }
    };

    append_row(true, header);
    for r in rows {
        append_row(false, r);
    }

    let mut out = serialize_commonmark(root);
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_table_escapes_pipes_and_roundtrips() {
        let md = build_table_markdown(
            &[TableAlign::Left, TableAlign::Right],
            &["Col A".into(), "Col B".into()],
            &[vec!["a | b".into(), "42".into()]],
        );
        assert!(verify_roundtrip_invariant(&md));
        let tables = extract_tables(&md);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].alignments, vec![TableAlign::Left, TableAlign::Right]);
    }

    #[test]
    fn parses_inline_and_display_math_preserving_code_currency_and_source_lines() {
        let md = concat!(
            "# Formula $E=mc^2$\n\n",
            "Inline $a^2 + b^2 = c^2$ and \\(\\alpha + \\beta\\), cost $10 and $20, escaped \\$5, code `$x$`.\n\n",
            "$$\n\\frac{1}{2}\n\n+ \\sqrt{x}\n$$\n\n",
            "\\[\\int_0^1 x\\,dx\\]\n\n",
            "| Expr |\n| --- |\n| $|x| + |y|$ |\n\n",
            "```math\ny = mx + b\n```\n\n",
            "```mermaid\nflowchart TD\nA-->B\n```\n",
        );

        let blocks = parse_to_ir(md);
        assert!(is_math_code_info("math"));
        assert!(is_math_code_info("LaTeX"));
        assert!(is_mermaid_code_info("mermaid"));

        // Heading has inline math
        match &blocks[0] {
            DocBlock::Heading { runs, .. } => {
                assert!(runs.iter().any(|r| r.is_math && r.text == "E=mc^2"));
            }
            other => panic!("expected heading, got {other:?}"),
        }

        // Paragraph has inline math, NOT currency or code span
        match &blocks[1] {
            DocBlock::Paragraph { runs, .. } => {
                let math_texts: Vec<&str> =
                    runs.iter().filter(|r| r.is_math).map(|r| r.text.as_str()).collect();
                assert_eq!(math_texts, vec!["a^2 + b^2 = c^2", "\\alpha + \\beta"]);
                assert!(runs.iter().any(|r| r.code && r.text == "$x$"));
                let joined_plain: String = runs
                    .iter()
                    .filter(|r| !r.is_math && !r.code)
                    .map(|r| r.text.as_str())
                    .collect();
                assert!(joined_plain.contains("$10 and $20"));
                assert!(joined_plain.contains("$5"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }

        // Multi-line $$...$$ with blank line inside stays one math CodeBlock with exact source_lines
        match &blocks[2] {
            DocBlock::CodeBlock {
                info,
                code,
                source_lines,
            } => {
                assert_eq!(info, "math");
                assert_eq!(code, "\\frac{1}{2}\n\n+ \\sqrt{x}");
                assert_eq!(source_lines, &(4..9));
            }
            other => panic!("expected multiline display math block, got {other:?}"),
        }

        // Single-line \[...\] becomes display math CodeBlock
        match &blocks[3] {
            DocBlock::CodeBlock { info, code, .. } => {
                assert_eq!(info, "math");
                assert_eq!(code, "\\int_0^1 x\\,dx");
            }
            other => panic!("expected single-line display math, got {other:?}"),
        }

        // Table cell with pipe inside inline math stays 1 column
        match &blocks[4] {
            DocBlock::Table { rows, .. } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].len(), 1);
                assert!(rows[0][0].iter().any(|r| r.is_math && r.text == "|x| + |y|"));
            }
            other => panic!("expected table, got {other:?}"),
        }

        assert!(matches!(&blocks[5], DocBlock::CodeBlock { info, .. } if is_math_code_info(info)));
        assert!(matches!(&blocks[6], DocBlock::CodeBlock { info, .. } if is_mermaid_code_info(info)));
        assert!(verify_roundtrip_invariant(md));
    }

    #[test]
    fn table_cached_formula_comments_hide_in_display_ir_and_roundtrip() {
        let md = "| Item | Total |\n| :--- | ---: |\n| A | 150 <!-- =SUM(B1:B3) --> |\n";
        let tables = extract_tables(md);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows[0][1], "150 <!-- =SUM(B1:B3) -->");

        let blocks = parse_to_ir(md);
        match &blocks[0] {
            DocBlock::Table { rows, .. } => {
                let text = crate::export::ir::runs_to_plain_text(&rows[0][1]);
                assert_eq!(text.trim(), "150");
            }
            other => panic!("expected table, got {other:?}"),
        }

        let rebuilt = build_table_markdown(&tables[0].alignments, &tables[0].header, &tables[0].rows);
        assert!(rebuilt.contains("150 <!-- =SUM(B1:B3) -->"));
        assert!(verify_roundtrip_invariant(md));
    }

    #[test]
    fn currency_punctuation_pairs_stay_plain_text_while_inline_math_parses() {
        let md = "Cost is $5 ($USD), Price $5/$10, Budget $5,$EUR and $100.\n\nMath: $E=mc^2$, $(a+b)^2$, $5x$.\n";
        let blocks = parse_to_ir(md);
        assert_eq!(blocks.len(), 2);

        match &blocks[0] {
            DocBlock::Paragraph { runs, .. } => {
                assert!(
                    runs.iter().all(|r| !r.is_math),
                    "currency expressions must not parse as math: {runs:?}"
                );
                let plain = crate::export::ir::runs_to_plain_text(runs);
                assert!(plain.contains("$5 ($USD)"));
                assert!(plain.contains("$5/$10"));
                assert!(plain.contains("$5,$EUR"));
                assert!(plain.contains("$100"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }

        match &blocks[1] {
            DocBlock::Paragraph { runs, .. } => {
                let math_texts: Vec<&str> = runs
                    .iter()
                    .filter(|r| r.is_math)
                    .map(|r| r.text.as_str())
                    .collect();
                assert_eq!(math_texts, vec!["E=mc^2", "(a+b)^2", "5x"]);
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn preprocessing_50k_unmatched_delimiters_scales_linearly() {
        let pattern = "\\( \\[ $x ";
        let mut line = String::with_capacity(50_050);
        while line.len() < 50_000 {
            line.push_str(pattern);
        }
        let start = std::time::Instant::now();
        let math_out = preprocess_math_in_line(&line);
        let marker_out = normalize_inline_marker_spaces(&math_out);
        let elapsed = start.elapsed();
        assert_eq!(marker_out.as_ref(), line.as_str());
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "50k unmatched-delimiter line took too long: {elapsed:?}"
        );

        let mixed_pattern = "` ** ~~ $ \\( \\[ ";
        let mut mixed = String::with_capacity(50_050);
        while mixed.len() < 50_000 {
            mixed.push_str(mixed_pattern);
        }
        mixed.push_str("``");
        let start2 = std::time::Instant::now();
        let math_out2 = preprocess_math_in_line(&mixed);
        let marker_out2 = normalize_inline_marker_spaces(&math_out2);
        let elapsed2 = start2.elapsed();
        assert!(!marker_out2.is_empty());
        assert!(
            elapsed2 < std::time::Duration::from_millis(500),
            "50k mixed-delimiter line took too long: {elapsed2:?}"
        );
    }

    #[test]
    fn parse_rope_to_ir_matches_parse_to_ir_across_features() {
        let md = concat!(
            "<!-- align:center,indent:1 -->\r\n",
            "#Heading Without Space\r\n\r\n",
            "Plain paragraph without special chars.\r\n\r\n",
            "Inline math $E=mc^2$ and \\(a+b\\) plus ** bold ** and ~~ strike ~~.\r\n\r\n",
            "$$\r\n",
            "\\int_0^1 x^2 \\, dx = \\frac{1}{3}\r\n",
            "$$\r\n\r\n",
            "```rust\r\n",
            "let price = \"$5\";\r\n",
            "```\r\n\r\n",
            "| A | B |\r\n",
            "|---|---|\r\n",
            "| 10 | =A1*2 <!-- =A1*2 --> |\r\n\r\n",
            "مرحبا بالعالم في Rust\r\n"
        );
        // Split into multiple rope chunks via edits to test non-contiguous line slices too
        let mut rope = ropey::Rope::from_str(md);
        let extra = "\nAnother plain paragraph across chunk seams.\n";
        for _ in 0..40 {
            rope.insert(rope.len_chars(), extra);
        }
        let full_str = rope.to_string();
        assert_eq!(parse_rope_to_ir(&rope), parse_to_ir(&full_str));
    }
}
