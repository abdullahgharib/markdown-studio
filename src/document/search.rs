use std::ops::Range;

use memchr::memmem::Finder;
use regex::RegexBuilder;
use ropey::Rope;

use crate::document::DocumentBuffer;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
    pub selection_scope: Option<Range<usize>>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub matches: Vec<Range<usize>>,
    pub error: Option<String>,
}

#[inline]
fn is_word_combining_mark(c: char) -> bool {
    matches!(
        c,
        '\u{0300}'..='\u{036F}'
            | '\u{0591}'..='\u{05C7}'
            | '\u{0610}'..='\u{061A}'
            | '\u{064B}'..='\u{065F}'
            | '\u{0670}'
            | '\u{06D6}'..='\u{06ED}'
            | '\u{0730}'..='\u{074A}'
            | '\u{08D3}'..='\u{08FF}'
            | '\u{0900}'..='\u{0903}'
            | '\u{093A}'..='\u{094F}'
            | '\u{0962}'..='\u{0963}'
            | '\u{0E31}'
            | '\u{0E34}'..='\u{0E3A}'
            | '\u{0E47}'..='\u{0E4E}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

#[inline]
fn is_emoji_join_or_mod(c: char) -> bool {
    matches!(
        c,
        '\u{200D}' | '\u{FE0E}' | '\u{FE0F}' | '\u{1F3FB}'..='\u{1F3FF}'
    )
}

/// Unicode-aware word character check (`[\p{L}\p{N}_]` + combining diacritics/harakat).
#[inline]
pub fn is_unicode_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || is_word_combining_mark(c)
}

#[inline]
fn is_whole_word_boundary(
    prev_ch: Option<char>,
    first_ch: char,
    last_ch: char,
    next_ch: Option<char>,
) -> bool {
    if prev_ch.is_some_and(|c| c == '\u{200D}') || is_emoji_join_or_mod(first_ch) {
        return false;
    }
    if next_ch.is_some_and(is_emoji_join_or_mod) {
        return false;
    }
    let first_is_word = is_unicode_word_char(first_ch);
    let last_is_word = is_unicode_word_char(last_ch);
    let left_ok = !first_is_word || !prev_ch.is_some_and(is_unicode_word_char);
    let right_ok = !last_is_word || !next_ch.is_some_and(is_unicode_word_char);
    left_ok && right_ok
}

/// Checks Unicode word and emoji-cluster boundaries around `chars[start..end]`.
pub fn is_whole_word_match(chars: &[char], start: usize, end: usize) -> bool {
    if start >= end || end > chars.len() {
        return false;
    }
    let prev_ch = (start > 0).then(|| chars[start - 1]);
    let next_ch = (end < chars.len()).then(|| chars[end]);
    is_whole_word_boundary(prev_ch, chars[start], chars[end - 1], next_ch)
}

#[inline]
fn is_whole_word_in_rope(rope: &Rope, start_char: usize, end_char: usize) -> bool {
    let len = rope.len_chars();
    if start_char >= end_char || end_char > len {
        return false;
    }
    let prev_ch = (start_char > 0).then(|| rope.char(start_char - 1));
    let first_ch = rope.char(start_char);
    let last_ch = rope.char(end_char - 1);
    let next_ch = (end_char < len).then(|| rope.char(end_char));
    is_whole_word_boundary(prev_ch, first_ch, last_ch, next_ch)
}

#[inline]
fn is_whole_word_in_str(
    s: &str,
    start_byte: usize,
    end_byte: usize,
    left_bound: Option<char>,
    right_bound: Option<char>,
) -> bool {
    if start_byte >= end_byte || end_byte > s.len() {
        return false;
    }
    let matched = &s[start_byte..end_byte];
    let Some(first_ch) = matched.chars().next() else {
        return false;
    };
    let Some(last_ch) = matched.chars().next_back() else {
        return false;
    };
    let prev_ch = s[..start_byte].chars().next_back().or(left_bound);
    let next_ch = s[end_byte..].chars().next().or(right_bound);
    is_whole_word_boundary(prev_ch, first_ch, last_ch, next_ch)
}

fn build_search_regex(
    pattern: &str,
    case_sensitive: bool,
    multi_line: bool,
) -> Result<regex::Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .unicode(true)
        .multi_line(multi_line)
        .crlf(true)
        .size_limit(10 * 1024 * 1024)
        .build()
        .map_err(|e| format!("Invalid regex: {e}"))
}

/// Expands regex replacement backreferences `$0..$9` and `$$` given match slices.
pub fn expand_regex_replacement(
    template: &str,
    full_match: &str,
    groups: &[Option<String>],
) -> String {
    if !template.contains('$') {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len() + full_match.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            match chars.peek().copied() {
                Some('$') => {
                    chars.next();
                    out.push('$');
                }
                Some('0') => {
                    chars.next();
                    out.push_str(full_match);
                }
                Some(d @ '1'..='9') => {
                    chars.next();
                    let idx = (d as usize) - ('1' as usize);
                    if let Some(Some(val)) = groups.get(idx) {
                        out.push_str(val);
                    }
                }
                _ => out.push('$'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Converts sequential/monotonic byte offsets in `s` to character offsets in O(N) total time.
struct LinearByteToChar<'a> {
    s: &'a str,
    is_ascii: bool,
    base_char: usize,
    cur_byte: usize,
    cur_char: usize,
}

impl<'a> LinearByteToChar<'a> {
    fn new(s: &'a str, base_char: usize, is_ascii: bool) -> Self {
        Self {
            s,
            is_ascii,
            base_char,
            cur_byte: 0,
            cur_char: base_char,
        }
    }

    #[inline]
    fn map_byte_to_char(&mut self, byte_idx: usize) -> usize {
        if self.is_ascii {
            return self.base_char + byte_idx;
        }
        if byte_idx >= self.cur_byte {
            self.cur_char += self.s[self.cur_byte..byte_idx].chars().count();
        } else {
            self.cur_char -= self.s[byte_idx..self.cur_byte].chars().count();
        }
        self.cur_byte = byte_idx;
        self.cur_char
    }
}

struct MatchSearchData {
    matches: Vec<Range<usize>>,
    captures: Vec<Vec<Option<Range<usize>>>>,
    error: Option<String>,
}

#[inline]
fn find_ci_ascii_in_slice(haystack: &[u8], needle_lower: &[u8]) -> Option<usize> {
    let q_len = needle_lower.len();
    if q_len == 0 {
        return Some(0);
    }
    if haystack.len() < q_len {
        return None;
    }
    let first_l = needle_lower[0];
    let first_u = first_l.to_ascii_uppercase();
    let search_limit = haystack.len() - q_len + 1;
    let mut pos = 0usize;
    while pos < search_limit {
        let rel = if first_l == first_u {
            memchr::memchr(first_l, &haystack[pos..search_limit])
        } else {
            memchr::memchr2(first_l, first_u, &haystack[pos..search_limit])
        }?;
        let cand = pos + rel;
        if haystack[cand..cand + q_len].eq_ignore_ascii_case(needle_lower) {
            return Some(cand);
        }
        pos = cand + 1;
    }
    None
}

fn find_all_literal_bytes_streaming<F>(
    rope: &Rope,
    scope_start: usize,
    scope_end: usize,
    q_len: usize,
    whole_word: bool,
    find_in: F,
) -> Vec<Range<usize>>
where
    F: Fn(&[u8]) -> Option<usize>,
{
    let scope_slice = rope.slice(scope_start..scope_end);
    if q_len == 0 || scope_slice.len_bytes() < q_len {
        return Vec::new();
    }

    let scope_start_byte = rope.char_to_byte(scope_start);
    let scope_is_ascii = scope_slice.len_bytes() == scope_slice.len_chars();
    let overlap_len = q_len.saturating_sub(1);

    let mut matches = Vec::new();
    let mut tail: Vec<u8> = Vec::with_capacity(overlap_len.saturating_mul(2));
    let mut chunk_base_byte = scope_start_byte;
    let mut next_allowed_byte = scope_start_byte;

    let to_doc_char = |abs_byte: usize| -> usize {
        if scope_is_ascii {
            scope_start + (abs_byte - scope_start_byte)
        } else {
            rope.byte_to_char(abs_byte)
        }
    };

    for chunk in scope_slice.chunks() {
        let bytes = chunk.as_bytes();
        let chunk_len = bytes.len();

        if tail.len() + chunk_len <= overlap_len {
            tail.extend_from_slice(bytes);
            chunk_base_byte += chunk_len;
            continue;
        }

        // 1) Bridge matches starting in the previous chunk's tail and crossing into `bytes`
        if !tail.is_empty() {
            let prev_tail_len = tail.len();
            let tail_start_byte = chunk_base_byte - prev_tail_len;
            let take = overlap_len.min(chunk_len);
            tail.extend_from_slice(&bytes[..take]);

            let mut cur = next_allowed_byte.saturating_sub(tail_start_byte);
            while cur < prev_tail_len && cur + q_len <= tail.len() {
                let Some(rel) = find_in(&tail[cur..]) else {
                    break;
                };
                let m_start = cur + rel;
                if m_start >= prev_tail_len {
                    break;
                }
                let abs_start = tail_start_byte + m_start;
                let abs_end = abs_start + q_len;
                let s_char = to_doc_char(abs_start);
                let e_char = to_doc_char(abs_end);
                if !whole_word || is_whole_word_in_rope(rope, s_char, e_char) {
                    matches.push(s_char..e_char);
                    next_allowed_byte = abs_end;
                    cur = m_start + q_len;
                } else {
                    cur = m_start + 1;
                }
            }
        }

        // 2) Matches starting inside the current `bytes` chunk
        let mut cur = next_allowed_byte.saturating_sub(chunk_base_byte);
        while cur + q_len <= chunk_len {
            let Some(rel) = find_in(&bytes[cur..]) else {
                break;
            };
            let m_start = cur + rel;
            let abs_start = chunk_base_byte + m_start;
            let abs_end = abs_start + q_len;
            let s_char = to_doc_char(abs_start);
            let e_char = to_doc_char(abs_end);
            if !whole_word || is_whole_word_in_rope(rope, s_char, e_char) {
                matches.push(s_char..e_char);
                next_allowed_byte = abs_end;
                cur = m_start + q_len;
            } else {
                cur = m_start + 1;
            }
        }

        // 3) Retain last `overlap_len` bytes for the next chunk bridge
        if overlap_len > 0 {
            if chunk_len >= overlap_len {
                tail.clear();
                tail.extend_from_slice(&bytes[chunk_len - overlap_len..]);
            } else {
                let excess = tail.len().saturating_sub(overlap_len);
                if excess > 0 {
                    tail.drain(..excess);
                }
            }
        }

        chunk_base_byte += chunk_len;
    }

    matches
}

fn find_all_literal_cs_streaming(
    rope: &Rope,
    scope_start: usize,
    scope_end: usize,
    query: &str,
    whole_word: bool,
) -> Vec<Range<usize>> {
    let needle = query.as_bytes();
    let finder = Finder::new(needle);
    find_all_literal_bytes_streaming(
        rope,
        scope_start,
        scope_end,
        needle.len(),
        whole_word,
        |hay| finder.find(hay),
    )
}

fn find_all_literal_ci_ascii_streaming(
    rope: &Rope,
    scope_start: usize,
    scope_end: usize,
    query: &str,
    whole_word: bool,
) -> Vec<Range<usize>> {
    debug_assert!(query.is_ascii());
    let needle_lower: Vec<u8> = query.bytes().map(|b| b.to_ascii_lowercase()).collect();
    find_all_literal_bytes_streaming(
        rope,
        scope_start,
        scope_end,
        needle_lower.len(),
        whole_word,
        |hay| find_ci_ascii_in_slice(hay, &needle_lower),
    )
}

fn can_stream_regex_by_line(pattern: &str) -> bool {
    if pattern.contains('\n') || pattern.contains('\r') {
        return false;
    }
    for forbidden in [
        "\\n", "\\r", "\\s", "\\v", "\\W", "\\D", "\\p", "\\P", "\\x", "\\u", "\\U", "\\A",
        "\\z", "\\Z", "\\R", "[^", "[[:",
    ] {
        if pattern.contains(forbidden) {
            return false;
        }
    }
    // Reject inline DOTALL `(?s)` / `(?is)` / etc. flag blocks
    let mut rest = pattern;
    while let Some(idx) = rest.find("(?") {
        let after = &rest[idx + 2..];
        for ch in after.chars() {
            if ch == 's' {
                return false;
            }
            if ch == ':' || ch == ')' || (!ch.is_ascii_alphabetic() && ch != '-') {
                break;
            }
        }
        rest = after;
    }
    true
}

fn find_all_regex_streaming_lines(
    rope: &Rope,
    scope_start: usize,
    scope_end: usize,
    re: &regex::Regex,
    whole_word: bool,
    left_bound: Option<char>,
    right_bound: Option<char>,
) -> Option<Vec<Range<usize>>> {
    let scope_slice = rope.slice(scope_start..scope_end);
    let mut matches = Vec::new();
    let mut line_buf = String::with_capacity(256);
    let mut line_start_char = scope_start;
    let mut prev_line_tail_char = left_bound;

    for line_slice in scope_slice.lines() {
        let line_chars = line_slice.len_chars();
        if line_chars == 0 {
            continue;
        }
        line_buf.clear();
        for chunk in line_slice.chunks() {
            line_buf.push_str(chunk);
        }
        let line_end_char = line_start_char + line_chars;
        // If `ropey` split on a non-`\n` line break (e.g. lone `\r` or U+2028), bail out to
        // full-buffer regex so `^`/`$`/`crlf` semantics match 100%.
        if line_end_char < scope_end && !line_buf.ends_with('\n') {
            return None;
        }
        let line_right_bound = if line_end_char >= scope_end {
            right_bound
        } else {
            None
        };
        let line_is_ascii = line_buf.len() == line_chars;
        let mut mapper = LinearByteToChar::new(&line_buf, line_start_char, line_is_ascii);

        for m in re.find_iter(&line_buf) {
            if m.start() >= m.end() {
                continue;
            }
            if whole_word
                && !is_whole_word_in_str(
                    &line_buf,
                    m.start(),
                    m.end(),
                    prev_line_tail_char,
                    line_right_bound,
                )
            {
                continue;
            }
            let s_char = mapper.map_byte_to_char(m.start());
            let e_char = mapper.map_byte_to_char(m.end());
            matches.push(s_char..e_char);
        }

        prev_line_tail_char = line_buf.chars().next_back();
        line_start_char = line_end_char;
    }

    Some(matches)
}

const STREAMING_REGEX_BYTES_THRESHOLD: usize = 256 * 1024;

fn find_all_matches_internal_rope(
    rope: &Rope,
    query: &str,
    opts: &SearchOptions,
    collect_captures: bool,
) -> MatchSearchData {
    if query.is_empty() {
        return MatchSearchData {
            matches: Vec::new(),
            captures: Vec::new(),
            error: None,
        };
    }

    let total_chars = rope.len_chars();
    let (scope_start, scope_end) = match &opts.selection_scope {
        Some(r) => {
            let s = r.start.min(total_chars);
            let e = r.end.min(total_chars).max(s);
            (s, e)
        }
        None => (0, total_chars),
    };

    if scope_start >= scope_end {
        return MatchSearchData {
            matches: Vec::new(),
            captures: Vec::new(),
            error: None,
        };
    }

    // Fast Path 1: Case-sensitive literal substring search via zero-copy SIMD memmem
    if !opts.use_regex && opts.case_sensitive {
        let matches =
            find_all_literal_cs_streaming(rope, scope_start, scope_end, query, opts.whole_word);
        return MatchSearchData {
            matches,
            captures: Vec::new(),
            error: None,
        };
    }

    // Fast Path 2: Case-insensitive ASCII literal search via zero-copy streaming SIMD memchr2
    if !opts.use_regex && !opts.case_sensitive && query.is_ascii() {
        let matches = find_all_literal_ci_ascii_streaming(
            rope,
            scope_start,
            scope_end,
            query,
            opts.whole_word,
        );
        return MatchSearchData {
            matches,
            captures: Vec::new(),
            error: None,
        };
    }

    // Fast Path 3: Unicode case-insensitive literal or Regex via linear-time `regex` DFA/PikeVM
    let re = if opts.use_regex {
        match build_search_regex(query, opts.case_sensitive, true) {
            Ok(re) => re,
            Err(err) => {
                return MatchSearchData {
                    matches: Vec::new(),
                    captures: Vec::new(),
                    error: Some(err),
                };
            }
        }
    } else {
        let escaped = regex::escape(query);
        match build_search_regex(&escaped, false, false) {
            Ok(re) => re,
            Err(err) => {
                return MatchSearchData {
                    matches: Vec::new(),
                    captures: Vec::new(),
                    error: Some(err),
                };
            }
        }
    };

    let left_bound = (scope_start > 0).then(|| rope.char(scope_start - 1));
    let right_bound = (scope_end < total_chars).then(|| rope.char(scope_end));
    let scope_slice = rope.slice(scope_start..scope_end);

    // Stream large scopes line-by-line for single-line regexes / non-ASCII CI literals to avoid
    // cloning huge documents into a contiguous heap `String`.
    let can_stream_lines = !collect_captures
        && scope_slice.len_bytes() >= STREAMING_REGEX_BYTES_THRESHOLD
        && ((!opts.use_regex && !query.contains('\n') && !query.contains('\r'))
            || (opts.use_regex && can_stream_regex_by_line(query)));
    if can_stream_lines
        && let Some(matches) = find_all_regex_streaming_lines(
            rope,
            scope_start,
            scope_end,
            &re,
            opts.whole_word,
            left_bound,
            right_bound,
        ) {
            return MatchSearchData {
                matches,
                captures: Vec::new(),
                error: None,
            };
        }

    let scope_is_ascii = scope_slice.len_bytes() == scope_slice.len_chars();
    let scope_str = scope_slice.to_string();

    let mut mapper = LinearByteToChar::new(&scope_str, scope_start, scope_is_ascii);
    let mut matches = Vec::new();
    let mut captures = Vec::new();

    if collect_captures {
        for caps in re.captures_iter(&scope_str) {
            let Some(m) = caps.get(0) else {
                continue;
            };
            if m.start() >= m.end() {
                continue;
            }
            if opts.whole_word
                && !is_whole_word_in_str(&scope_str, m.start(), m.end(), left_bound, right_bound)
            {
                continue;
            }
            let s_char = mapper.map_byte_to_char(m.start());
            let mut group_ranges = Vec::with_capacity(caps.len().saturating_sub(1).min(9));
            for g_idx in 1..caps.len().min(10) {
                group_ranges.push(caps.get(g_idx).map(|g| {
                    let gs = mapper.map_byte_to_char(g.start());
                    let ge = mapper.map_byte_to_char(g.end());
                    gs..ge
                }));
            }
            let e_char = mapper.map_byte_to_char(m.end());
            matches.push(s_char..e_char);
            captures.push(group_ranges);
        }
    } else {
        for m in re.find_iter(&scope_str) {
            if m.start() >= m.end() {
                continue;
            }
            if opts.whole_word
                && !is_whole_word_in_str(&scope_str, m.start(), m.end(), left_bound, right_bound)
            {
                continue;
            }
            let s_char = mapper.map_byte_to_char(m.start());
            let e_char = mapper.map_byte_to_char(m.end());
            matches.push(s_char..e_char);
        }
    }

    MatchSearchData {
        matches,
        captures,
        error: None,
    }
}

/// Finds all matches in `rope` using the requested search options without borrowing `DocumentBuffer`.
/// Returned ranges are in document `char` coordinates.
pub fn find_all_matches_in_rope(
    rope: &Rope,
    query: &str,
    opts: &SearchOptions,
) -> SearchResult {
    let data = find_all_matches_internal_rope(rope, query, opts, false);
    SearchResult {
        matches: data.matches,
        error: data.error,
    }
}

/// Finds all matches in `buf` using the requested search options.
/// Returned ranges are in document `char` coordinates.
pub fn find_all_matches(
    buf: &DocumentBuffer,
    query: &str,
    opts: &SearchOptions,
) -> SearchResult {
    find_all_matches_in_rope(buf.rope(), query, opts)
}

/// Replaces a single match range in `buf` and returns the inserted char range.
pub fn replace_single_match(
    buf: &mut DocumentBuffer,
    match_range: Range<usize>,
    replacement: &str,
) -> Range<usize> {
    let start = match_range.start.min(buf.len_chars());
    let rep_chars = replacement.chars().count();
    buf.replace_range(match_range, replacement);
    start..(start + rep_chars)
}

/// Replaces a single match range in `buf`, expanding regex `$0..$9`/`$$` capture groups when `opts.use_regex` is set.
pub fn replace_single_match_with_opts(
    buf: &mut DocumentBuffer,
    query: &str,
    match_range: Range<usize>,
    replacement: &str,
    opts: &SearchOptions,
) -> Result<Range<usize>, String> {
    if !opts.use_regex || !replacement.contains('$') {
        return Ok(replace_single_match(buf, match_range, replacement));
    }
    let re = build_search_regex(query, opts.case_sensitive, true)?;
    let total = buf.len_chars();
    let s = match_range.start.min(total);
    let e = match_range.end.min(total).max(s);
    let full_text = buf.rope().slice(s..e).to_string();
    let expanded = if let Some(caps) = re.captures(&full_text) {
        let groups: Vec<Option<String>> = (1..caps.len().min(10))
            .map(|idx| caps.get(idx).map(|m| m.as_str().to_string()))
            .collect();
        expand_regex_replacement(replacement, &full_text, &groups)
    } else {
        replacement.to_string()
    };
    Ok(replace_single_match(buf, s..e, &expanded))
}

/// Replaces all matches in `buf` as a SINGLE atomic undo transaction (`buf.replace_range`).
/// Pressing `Ctrl+Z` (`buf.undo()`) reverts every occurrence replaced in one step.
pub fn replace_all_matches(
    buf: &mut DocumentBuffer,
    query: &str,
    replacement: &str,
    opts: &SearchOptions,
) -> Result<(usize, Vec<Range<usize>>), String> {
    let want_caps = opts.use_regex && replacement.contains('$');
    let res = find_all_matches_internal_rope(buf.rope(), query, opts, want_caps);
    if let Some(err) = res.error {
        return Err(err);
    }
    if res.matches.is_empty() {
        return Ok((0, Vec::new()));
    }

    let first_start = res.matches[0].start;
    let last_end = res.matches[res.matches.len() - 1].end;
    let count = res.matches.len();

    let span_slice_rope = buf.rope().slice(first_start..last_end);
    let span_is_ascii = span_slice_rope.len_bytes() == span_slice_rope.len_chars();
    let span_slice = span_slice_rope.to_string();

    let mut combined = String::with_capacity(span_slice.len());
    let mut new_ranges = Vec::with_capacity(count);
    let mut running_doc_char = first_start;
    let mut last_char = first_start;
    let mut last_byte = 0usize;
    let mut cur_char = first_start;
    let mut cur_byte = 0usize;

    let mut char_to_span_byte = |target_char: usize| -> usize {
        if span_is_ascii {
            return target_char - first_start;
        }
        if target_char >= cur_char {
            let mut delta = target_char - cur_char;
            if delta > 0 {
                for (off, _) in span_slice[cur_byte..].char_indices() {
                    if delta == 0 {
                        cur_byte += off;
                        cur_char = target_char;
                        return cur_byte;
                    }
                    delta -= 1;
                }
                cur_byte = span_slice.len();
                cur_char = last_end;
            }
            cur_byte
        } else {
            let mut delta = cur_char - target_char;
            let mut b = cur_byte;
            for ch in span_slice[..cur_byte].chars().rev() {
                if delta == 0 {
                    break;
                }
                b -= ch.len_utf8();
                delta -= 1;
            }
            cur_byte = b;
            cur_char = target_char;
            cur_byte
        }
    };

    for (idx, m) in res.matches.iter().enumerate() {
        let m_start_byte = char_to_span_byte(m.start);
        let m_end_byte = char_to_span_byte(m.end);
        if m_start_byte > last_byte {
            combined.push_str(&span_slice[last_byte..m_start_byte]);
            running_doc_char += m.start - last_char;
        }

        let rep_start = running_doc_char;
        let actual_rep = if want_caps {
            let full_match = &span_slice[m_start_byte..m_end_byte];
            let groups: Vec<Option<String>> = res
                .captures
                .get(idx)
                .map(|caps| {
                    caps.iter()
                        .map(|opt| {
                            opt.as_ref().map(|r| {
                                let gs = char_to_span_byte(r.start);
                                let ge = char_to_span_byte(r.end);
                                span_slice[gs..ge].to_string()
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            expand_regex_replacement(replacement, full_match, &groups)
        } else {
            replacement.to_string()
        };
        let actual_rep_chars = actual_rep.chars().count();
        combined.push_str(&actual_rep);
        running_doc_char += actual_rep_chars;
        new_ranges.push(rep_start..running_doc_char);
        last_byte = m_end_byte;
        last_char = m.end;
    }

    if last_byte < span_slice.len() {
        combined.push_str(&span_slice[last_byte..]);
    }

    // Single replace_range => single EditOp on undo_stack!
    buf.replace_range(first_start..last_end, &combined);
    Ok((count, new_ranges))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_word_and_case_sensitivity_english_and_arabic() {
        let mut buf = DocumentBuffer::from_str("cat category Cat\nمرحبا ومرحبا مرحبا!");

        let res_sub = find_all_matches(
            &buf,
            "cat",
            &SearchOptions {
                case_sensitive: false,
                whole_word: false,
                use_regex: false,
                selection_scope: None,
            },
        );
        assert_eq!(res_sub.matches.len(), 3);

        let res_word = find_all_matches(
            &buf,
            "cat",
            &SearchOptions {
                case_sensitive: true,
                whole_word: true,
                use_regex: false,
                selection_scope: None,
            },
        );
        assert_eq!(res_word.matches, vec![0..3]);

        let res_ar_word = find_all_matches(
            &buf,
            "مرحبا",
            &SearchOptions {
                case_sensitive: true,
                whole_word: true,
                use_regex: false,
                selection_scope: None,
            },
        );
        // Should match first "مرحبا" and last "مرحبا" (before '!'), NOT "ومرحبا"
        assert_eq!(res_ar_word.matches.len(), 2);

        // Replace All must undo in ONE atomic step
        let (count, _) = replace_all_matches(
            &mut buf,
            "مرحبا",
            "أهلا",
            &SearchOptions {
                case_sensitive: true,
                whole_word: true,
                use_regex: false,
                selection_scope: None,
            },
        )
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(buf.as_string(), "cat category Cat\nأهلا ومرحبا أهلا!");

        assert!(buf.undo());
        assert_eq!(buf.as_string(), "cat category Cat\nمرحبا ومرحبا مرحبا!");
    }

    #[test]
    fn regex_search_and_inline_error_feedback() {
        let buf = DocumentBuffer::from_str("Item 42 and Item 108 and cat");
        let res = find_all_matches(
            &buf,
            r"Item \d+",
            &SearchOptions {
                case_sensitive: true,
                whole_word: false,
                use_regex: true,
                selection_scope: None,
            },
        );
        assert!(res.error.is_none());
        assert_eq!(res.matches.len(), 2);

        let bad = find_all_matches(
            &buf,
            "[unclosed",
            &SearchOptions {
                case_sensitive: true,
                whole_word: false,
                use_regex: true,
                selection_scope: None,
            },
        );
        assert!(bad.error.is_some());
        assert!(bad.matches.is_empty());
    }

    #[test]
    fn find_all_matches_in_rope_matches_buffer_search() {
        let buf = DocumentBuffer::from_str("Rust rope search\nrust ROPE regex 42\n");
        for opts in [
            SearchOptions {
                case_sensitive: false,
                whole_word: true,
                use_regex: false,
                selection_scope: None,
            },
            SearchOptions {
                case_sensitive: true,
                whole_word: false,
                use_regex: true,
                selection_scope: None,
            },
        ] {
            let q = if opts.use_regex { r"R\w+" } else { "rope" };
            let r1 = find_all_matches(&buf, q, &opts);
            let r2 = find_all_matches_in_rope(buf.rope(), q, &opts);
            assert_eq!(r1.matches, r2.matches);
            assert_eq!(r1.error, r2.error);
        }
    }

    #[test]
    fn streaming_ci_ascii_handles_chunk_boundaries_unicode_and_whole_word() {
        // Build a multi-chunk Rope (~12 KB) with matches spanning 1 KB chunk seams and Arabic neighbors.
        let mut s = "ألف ".repeat(300);
        let match_char_start = s.chars().count();
        s.push_str("MarkDown_Token ");
        let mut rope = Rope::from_str(&s);
        // Append enough small edits to split internal rope chunks around the boundary
        for i in 0..40 {
            let chunk = if i % 2 == 0 {
                "padding text MARKDOWN_TOKEN more text\n"
            } else {
                "sub_markdown_token_x and markdown_token!\n"
            };
            rope.insert(rope.len_chars(), chunk);
        }
        // Insert a match straddling a line start near the middle via two consecutive small edits
        let mid_line_pos = rope.line_to_char(rope.len_lines() / 2);
        rope.insert(mid_line_pos, "mArK");
        rope.insert(mid_line_pos + 4, "dOwN_tOkEn ");

        let opts_sub = SearchOptions {
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
            selection_scope: None,
        };
        let res_sub = find_all_matches_in_rope(&rope, "markdown_token", &opts_sub);
        // Expected: 1 initial + 20*1 (even) + 20*2 (odd) + 1 inserted = 62
        assert_eq!(res_sub.matches.len(), 62);
        assert_eq!(
            res_sub.matches[0],
            match_char_start..(match_char_start + "MarkDown_Token".len())
        );

        let opts_word = SearchOptions {
            case_sensitive: false,
            whole_word: true,
            use_regex: false,
            selection_scope: None,
        };
        let res_word = find_all_matches_in_rope(&rope, "markdown_token", &opts_word);
        // Excludes the 20 "sub_markdown_token_x" occurrences
        assert_eq!(res_word.matches.len(), 42);

        // Selection scope test
        let opts_scoped = SearchOptions {
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
            selection_scope: Some(match_char_start..(match_char_start + 20)),
        };
        let res_scoped = find_all_matches_in_rope(&rope, "markdown_token", &opts_scoped);
        assert_eq!(
            res_scoped.matches,
            vec![match_char_start..(match_char_start + "MarkDown_Token".len())]
        );
    }

    #[test]
    fn large_rope_regex_streaming_matches_exactly() {
        assert!(can_stream_regex_by_line(r"^Heading \d+$"));
        assert!(can_stream_regex_by_line(r"ID-\d+"));
        assert!(!can_stream_regex_by_line(r"foo\s+bar"));
        assert!(!can_stream_regex_by_line(r"(?is)foo.*bar"));
        assert!(!can_stream_regex_by_line(r"[^x]+"));

        // Exceed STREAMING_REGEX_BYTES_THRESHOLD (256 KB) to exercise line-by-line regex streaming.
        let unit = "Heading 12\nمرحبا ID-4201 و ID-99!\n";
        let mut text = String::with_capacity(300 * 1024);
        let mut copies = 0usize;
        while text.len() < 280 * 1024 {
            text.push_str(unit);
            copies += 1;
        }
        let rope = Rope::from_str(&text);

        let re_opts = SearchOptions {
            case_sensitive: true,
            whole_word: true,
            use_regex: true,
            selection_scope: None,
        };
        let res = find_all_matches_in_rope(&rope, r"ID-\d+", &re_opts);
        assert!(res.error.is_none());
        assert_eq!(res.matches.len(), copies * 2);

        let anchor_opts = SearchOptions {
            case_sensitive: false,
            whole_word: false,
            use_regex: true,
            selection_scope: None,
        };
        let anchor_res = find_all_matches_in_rope(&rope, r"^heading \d+$", &anchor_opts);
        assert!(anchor_res.error.is_none());
        assert_eq!(anchor_res.matches.len(), copies);

        // Verify character ranges correspond to actual substrings in the rope.
        let first_m = &anchor_res.matches[0];
        assert_eq!(rope.slice(first_m.clone()).to_string(), "Heading 12");
        let last_id = &res.matches[res.matches.len() - 1];
        assert_eq!(rope.slice(last_id.clone()).to_string(), "ID-99");
    }
}
