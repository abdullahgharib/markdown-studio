/// Bidirectional text, extended grapheme-cluster segmentation, and per-block alignment/indent metadata.
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextDirection {
    #[default]
    Ltr,
    Rtl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockAlignment {
    #[default]
    Start,
    Left,
    Center,
    Right,
    Justify,
    End,
}

impl BlockAlignment {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockAlignment::Start => "start",
            BlockAlignment::Left => "left",
            BlockAlignment::Center => "center",
            BlockAlignment::Right => "right",
            BlockAlignment::Justify => "justify",
            BlockAlignment::End => "end",
        }
    }

    pub fn from_str_token(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "start" => Some(BlockAlignment::Start),
            "left" => Some(BlockAlignment::Left),
            "center" => Some(BlockAlignment::Center),
            "right" => Some(BlockAlignment::Right),
            "justify" => Some(BlockAlignment::Justify),
            "end" => Some(BlockAlignment::End),
            _ => None,
        }
    }
}

fn parse_single_block_meta(s: &str) -> Option<(Option<BlockAlignment>, Option<u8>, &str)> {
    let trimmed = s.trim_start_matches([' ', '\t']);
    let after_open = trimmed.strip_prefix("<!--")?;
    let close_pos = after_open.find("-->")?;
    let inner = after_open[..close_pos].trim();
    let payload = inner.strip_prefix("md:").unwrap_or(inner).trim();
    if payload.is_empty() {
        return None;
    }

    let mut align: Option<BlockAlignment> = None;
    let mut indent: Option<u8> = None;
    let mut matched_any = false;

    for part in payload.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = part.split_once(':').or_else(|| part.split_once('='))?;
        match k.trim().to_ascii_lowercase().as_str() {
            "align" => {
                align = Some(BlockAlignment::from_str_token(v)?);
                matched_any = true;
            }
            "indent" => {
                let n = v.trim().parse::<u8>().ok()?;
                indent = Some(n.min(8));
                matched_any = true;
            }
            _ => return None,
        }
    }

    if matched_any {
        let rest = &after_open[close_pos + 3..];
        Some((align, indent, rest))
    } else {
        None
    }
}

/// Parses optional leading block metadata (`<!-- align:...,indent:... -->` or `<!-- md:align=...,indent=... -->`).
/// Returns `(alignment, indent_level, rest_of_line)`. Non-metadata HTML comments are left untouched.
pub fn parse_line_block_meta(line: &str) -> (BlockAlignment, u8, &str) {
    let mut align = BlockAlignment::Start;
    let mut indent = 0u8;
    let mut cur = line;

    while let Some((a, ind, rest)) = parse_single_block_meta(cur) {
        if let Some(a) = a {
            align = a;
        }
        if let Some(ind) = ind {
            indent = indent.saturating_add(ind).min(8);
        }
        cur = rest;
    }

    (align, indent, cur)
}

/// Serializes alignment and indentation metadata as an inline Markdown comment prefix.
pub fn format_line_block_meta(align: BlockAlignment, indent: u8) -> String {
    match (align, indent) {
        (BlockAlignment::Start, 0) => String::new(),
        (a, 0) => format!("<!-- align:{} -->", a.as_str()),
        (BlockAlignment::Start, n) => format!("<!-- indent:{} -->", n.min(8)),
        (a, n) => format!("<!-- align:{},indent:{} -->", a.as_str(), n.min(8)),
    }
}

pub fn is_rtl_char(c: char) -> bool {
    matches!(
        c,
        '\u{0590}'..='\u{08FF}'
            | '\u{FB1D}'..='\u{FDFF}'
            | '\u{FE70}'..='\u{FEFF}'
            | '\u{200F}'
    )
}

fn is_ltr_strong_char(c: char) -> bool {
    if c == '\u{200E}' {
        return true;
    }
    if is_rtl_char(c) || is_extending_char(c) {
        return false;
    }
    c.is_alphabetic()
}

/// Detects paragraph direction (`dir="auto"` per UAX #9 P2/P3: first strong directional character).
pub fn detect_paragraph_direction(text: &str) -> TextDirection {
    let (_, clean) = {
        let (a, _i, rest) = parse_line_block_meta(text);
        (a, rest)
    };

    let mut chars = clean.chars().peekable();
    while let Some(c) = chars.next() {
        // Skip inline HTML tags such as <u> / </u> / <!-- ... --> so tag names do not bias LTR.
        if c == '<' {
            for next_c in chars.by_ref() {
                if next_c == '>' {
                    break;
                }
            }
            continue;
        }
        // Skip task checkbox markers like `[x]` or `[ ]` before first strong character.
        if c == '[' {
            let mut look = chars.clone();
            if let (Some(inner), Some(']')) = (look.next(), look.next())
                && matches!(inner, ' ' | 'x' | 'X') {
                    chars.next();
                    chars.next();
                    continue;
                }
        }
        if is_rtl_char(c) {
            return TextDirection::Rtl;
        }
        if is_ltr_strong_char(c) {
            return TextDirection::Ltr;
        }
    }
    TextDirection::Ltr
}

/// True for combining marks, Arabic harakat/tashkeel, variation selectors, skin tones, and ZWJ.
pub fn is_extending_char(c: char) -> bool {
    matches!(
        c,
        '\u{0300}'..='\u{036F}'
            | '\u{0483}'..='\u{0489}'
            | '\u{0591}'..='\u{05BD}'
            | '\u{05BF}'
            | '\u{05C1}'..='\u{05C2}'
            | '\u{05C4}'..='\u{05C5}'
            | '\u{05C7}'
            | '\u{0610}'..='\u{061A}'
            | '\u{064B}'..='\u{065F}'
            | '\u{0670}'
            | '\u{06D6}'..='\u{06ED}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{1F3FB}'..='\u{1F3FF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{200D}'
    )
}

/// True for person/family emoji base codepoints kept atomic in both ZWJ and contiguous sequences.
pub fn is_person_family_emoji(c: char) -> bool {
    matches!(c, '\u{1F466}'..='\u{1F469}' | '\u{1F9D1}' | '\u{1F474}'..='\u{1F476}')
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// Returns sorted grapheme-cluster boundary offsets in `char` coordinates `[0, ..., char_count]`.
pub fn grapheme_char_boundaries(text: &str) -> Vec<usize> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![0];
    }

    let mut bounds = Vec::with_capacity(chars.len() + 1);
    bounds.push(0);

    let mut cluster_has_person_emoji = is_person_family_emoji(chars[0]);
    let mut ri_run = usize::from(is_regional_indicator(chars[0]));

    for i in 1..chars.len() {
        let prev = chars[i - 1];
        let cur = chars[i];

        let join = if prev == '\r' && cur == '\n' {
            true
        } else if prev == '\r' || prev == '\n' || cur == '\r' || cur == '\n' {
            false
        } else {
            prev == '\u{200D}'
                || is_extending_char(cur)
                || (cluster_has_person_emoji && is_person_family_emoji(cur))
                || (is_regional_indicator(cur) && ri_run % 2 == 1)
        };

        if !join {
            bounds.push(i);
            cluster_has_person_emoji = is_person_family_emoji(cur);
            ri_run = usize::from(is_regional_indicator(cur));
        } else {
            if is_person_family_emoji(cur) {
                cluster_has_person_emoji = true;
            }
            if is_regional_indicator(cur) {
                ri_run += 1;
            } else {
                ri_run = 0;
            }
        }
    }

    bounds.push(chars.len());
    bounds
}

/// Returns the nearest grapheme boundary strictly before `char_idx` (or `0`).
pub fn prev_grapheme_char_idx(text: &str, char_idx: usize) -> usize {
    let bounds = grapheme_char_boundaries(text);
    let max_c = *bounds.last().unwrap_or(&0);
    let c = char_idx.min(max_c);
    bounds
        .iter()
        .copied()
        .rev()
        .find(|&b| b < c)
        .unwrap_or(0)
}

/// Returns the nearest grapheme boundary strictly after `char_idx` (or `char_count`).
pub fn next_grapheme_char_idx(text: &str, char_idx: usize) -> usize {
    let bounds = grapheme_char_boundaries(text);
    let max_c = *bounds.last().unwrap_or(&0);
    let c = char_idx.min(max_c);
    bounds
        .iter()
        .copied()
        .find(|&b| b > c)
        .unwrap_or(max_c)
}

/// Snaps `char_idx` backward to the start of the grapheme cluster containing it.
pub fn snap_to_grapheme_start(text: &str, char_idx: usize) -> usize {
    let bounds = grapheme_char_boundaries(text);
    let max_c = *bounds.last().unwrap_or(&0);
    let c = char_idx.min(max_c);
    bounds
        .iter()
        .copied()
        .rev()
        .find(|&b| b <= c)
        .unwrap_or(0)
}

/// Snaps `char_idx` forward to the end of the grapheme cluster containing it (if inside a cluster).
pub fn snap_to_grapheme_end(text: &str, char_idx: usize) -> usize {
    let bounds = grapheme_char_boundaries(text);
    let max_c = *bounds.last().unwrap_or(&0);
    let c = char_idx.min(max_c);
    bounds
        .iter()
        .copied()
        .find(|&b| b >= c)
        .unwrap_or(max_c)
}

/// Maps isolated Arabic letters (`U+0621..=U+064A` and Tatweel `U+0640`) to Presentation Forms-B:
/// `(isolated, final, initial, medial)`.
fn arabic_forms(c: char) -> Option<(char, char, Option<char>, Option<char>)> {
    Some(match c {
        '\u{0621}' => ('\u{FE80}', '\u{FE80}', None, None),
        '\u{0622}' => ('\u{FE81}', '\u{FE82}', None, None),
        '\u{0623}' => ('\u{FE83}', '\u{FE84}', None, None),
        '\u{0624}' => ('\u{FE85}', '\u{FE86}', None, None),
        '\u{0625}' => ('\u{FE87}', '\u{FE88}', None, None),
        '\u{0626}' => ('\u{FE89}', '\u{FE8A}', Some('\u{FE8B}'), Some('\u{FE8C}')),
        '\u{0627}' => ('\u{FE8D}', '\u{FE8E}', None, None),
        '\u{0628}' => ('\u{FE8F}', '\u{FE90}', Some('\u{FE91}'), Some('\u{FE92}')),
        '\u{0629}' => ('\u{FE93}', '\u{FE94}', None, None),
        '\u{062A}' => ('\u{FE95}', '\u{FE96}', Some('\u{FE97}'), Some('\u{FE98}')),
        '\u{062B}' => ('\u{FE99}', '\u{FE9A}', Some('\u{FE9B}'), Some('\u{FE9C}')),
        '\u{062C}' => ('\u{FE9D}', '\u{FE9E}', Some('\u{FE9F}'), Some('\u{FEA0}')),
        '\u{062D}' => ('\u{FEA1}', '\u{FEA2}', Some('\u{FEA3}'), Some('\u{FEA4}')),
        '\u{062E}' => ('\u{FEA5}', '\u{FEA6}', Some('\u{FEA7}'), Some('\u{FEA8}')),
        '\u{062F}' => ('\u{FEA9}', '\u{FEAA}', None, None),
        '\u{0630}' => ('\u{FEAB}', '\u{FEAC}', None, None),
        '\u{0631}' => ('\u{FEAD}', '\u{FEAE}', None, None),
        '\u{0632}' => ('\u{FEAF}', '\u{FEB0}', None, None),
        '\u{0633}' => ('\u{FEB1}', '\u{FEB2}', Some('\u{FEB3}'), Some('\u{FEB4}')),
        '\u{0634}' => ('\u{FEB5}', '\u{FEB6}', Some('\u{FEB7}'), Some('\u{FEB8}')),
        '\u{0635}' => ('\u{FEB9}', '\u{FEBA}', Some('\u{FEBB}'), Some('\u{FEBC}')),
        '\u{0636}' => ('\u{FEBD}', '\u{FEBE}', Some('\u{FEBF}'), Some('\u{FEC0}')),
        '\u{0637}' => ('\u{FEC1}', '\u{FEC2}', Some('\u{FEC3}'), Some('\u{FEC4}')),
        '\u{0638}' => ('\u{FEC5}', '\u{FEC6}', Some('\u{FEC7}'), Some('\u{FEC8}')),
        '\u{0639}' => ('\u{FEC9}', '\u{FECA}', Some('\u{FECB}'), Some('\u{FECC}')),
        '\u{063A}' => ('\u{FECD}', '\u{FECE}', Some('\u{FECF}'), Some('\u{FED0}')),
        '\u{0640}' => ('\u{0640}', '\u{0640}', Some('\u{0640}'), Some('\u{0640}')),
        '\u{0641}' => ('\u{FED1}', '\u{FED2}', Some('\u{FED3}'), Some('\u{FED4}')),
        '\u{0642}' => ('\u{FED5}', '\u{FED6}', Some('\u{FED7}'), Some('\u{FED8}')),
        '\u{0643}' => ('\u{FED9}', '\u{FEDA}', Some('\u{FEDB}'), Some('\u{FEDC}')),
        '\u{0644}' => ('\u{FEDD}', '\u{FEDE}', Some('\u{FEDF}'), Some('\u{FEE0}')),
        '\u{0645}' => ('\u{FEE1}', '\u{FEE2}', Some('\u{FEE3}'), Some('\u{FEE4}')),
        '\u{0646}' => ('\u{FEE5}', '\u{FEE6}', Some('\u{FEE7}'), Some('\u{FEE8}')),
        '\u{0647}' => ('\u{FEE9}', '\u{FEEA}', Some('\u{FEEB}'), Some('\u{FEEC}')),
        '\u{0648}' => ('\u{FEED}', '\u{FEEE}', None, None),
        '\u{0649}' => ('\u{FEEF}', '\u{FEF0}', None, None),
        '\u{064A}' => ('\u{FEF1}', '\u{FEF2}', Some('\u{FEF3}'), Some('\u{FEF4}')),
        // Persian / Urdu Presentation Forms-A
        '\u{0679}' => ('\u{FB66}', '\u{FB67}', Some('\u{FB68}'), Some('\u{FB69}')),
        '\u{067E}' => ('\u{FB56}', '\u{FB57}', Some('\u{FB58}'), Some('\u{FB59}')),
        '\u{0686}' => ('\u{FB7A}', '\u{FB7B}', Some('\u{FB7C}'), Some('\u{FB7D}')),
        '\u{0688}' => ('\u{FB88}', '\u{FB89}', None, None),
        '\u{0691}' => ('\u{FB8C}', '\u{FB8D}', None, None),
        '\u{0698}' => ('\u{FB8A}', '\u{FB8B}', None, None),
        '\u{06A9}' => ('\u{FB8E}', '\u{FB8F}', Some('\u{FB90}'), Some('\u{FB91}')),
        '\u{06AF}' => ('\u{FB92}', '\u{FB93}', Some('\u{FB94}'), Some('\u{FB95}')),
        '\u{06BA}' => ('\u{FB9E}', '\u{FB9F}', None, None),
        '\u{06C1}' => ('\u{FBA6}', '\u{FBA7}', Some('\u{FBA8}'), Some('\u{FBA9}')),
        '\u{06CC}' => ('\u{FBFC}', '\u{FBFD}', Some('\u{FBFE}'), Some('\u{FBFF}')),
        '\u{06D2}' => ('\u{FBAE}', '\u{FBAF}', None, None),
        _ => return None,
    })
}

fn connects_after(c: char) -> bool {
    arabic_forms(c).is_some_and(|(_, _, init, _)| init.is_some())
}

fn connects_before(c: char) -> bool {
    c != '\u{0621}' && arabic_forms(c).is_some()
}

fn prev_base_char(chars: &[char], idx: usize) -> Option<char> {
    let mut i = idx;
    while i > 0 {
        i -= 1;
        if !is_extending_char(chars[i]) {
            return Some(chars[i]);
        }
    }
    None
}

fn next_base_char_idx(chars: &[char], idx: usize) -> Option<usize> {
    let mut i = idx + 1;
    while i < chars.len() {
        if !is_extending_char(chars[i]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[inline]
fn has_arabic_or_bidi_controls(text: &str) -> bool {
    !text.is_ascii()
        && text
            .chars()
            .any(|c| ('\u{0600}'..='\u{06FF}').contains(&c) || matches!(c, '\u{200D}'..='\u{200F}'))
}

/// Performs 1-to-1 Arabic contextual shaping preserving exact character indices and logical order.
pub fn shape_arabic_1to1(text: &str) -> String {
    if !has_arabic_or_bidi_controls(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        if let Some((iso, fin, init, med)) = arabic_forms(c) {
            let join_prev = connects_before(c) && prev_base_char(&chars, i).is_some_and(connects_after);
            let join_next = connects_after(c)
                && next_base_char_idx(&chars, i)
                    .map(|j| chars[j])
                    .is_some_and(connects_before);
            let shaped = match (join_prev, join_next) {
                (true, true) => med.unwrap_or(fin),
                (true, false) => fin,
                (false, true) => init.unwrap_or(iso),
                (false, false) => iso,
            };
            out.push(shaped);
        } else {
            out.push(c);
        }
    }
    out
}

fn shape_arabic_with_ligatures(text: &str) -> String {
    if !has_arabic_or_bidi_controls(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '\u{200D}' | '\u{200E}' | '\u{200F}') {
            i += 1;
            continue;
        }
        // Lam + Alef ligature substitution (including transparent haraka between Lam and Alef)
        if c == '\u{0644}'
            && let Some(j) = next_base_char_idx(&chars, i) {
                let alef = chars[j];
                if matches!(alef, '\u{0622}' | '\u{0623}' | '\u{0625}' | '\u{0627}') {
                    let join_prev = prev_base_char(&chars, i).is_some_and(connects_after);
                    let lig = match alef {
                        '\u{0622}' => if join_prev { '\u{FEF6}' } else { '\u{FEF5}' },
                        '\u{0623}' => if join_prev { '\u{FEF8}' } else { '\u{FEF7}' },
                        '\u{0625}' => if join_prev { '\u{FEFA}' } else { '\u{FEF9}' },
                        _ => if join_prev { '\u{FEFC}' } else { '\u{FEFB}' },
                    };
                    out.push(lig);
                    for &mark in &chars[(i + 1)..j] {
                        if !matches!(mark, '\u{200D}' | '\u{200E}' | '\u{200F}') {
                            out.push(mark);
                        }
                    }
                    i = j + 1;
                    continue;
                }
            }
        if let Some((iso, fin, init, med)) = arabic_forms(c) {
            let join_prev = connects_before(c) && prev_base_char(&chars, i).is_some_and(connects_after);
            let join_next = connects_after(c)
                && next_base_char_idx(&chars, i)
                    .map(|j| chars[j])
                    .is_some_and(connects_before);
            let shaped = match (join_prev, join_next) {
                (true, true) => med.unwrap_or(fin),
                (true, false) => fin,
                (false, true) => init.unwrap_or(iso),
                (false, false) => iso,
            };
            out.push(shaped);
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClusterDir {
    Rtl,
    LtrStrong,
    Number,
    Neutral,
}

#[inline]
fn classify_display_char(base: char) -> ClusterDir {
    if is_rtl_char(base) {
        ClusterDir::Rtl
    } else if base.is_ascii_digit() || ('\u{0660}'..='\u{0669}').contains(&base) {
        ClusterDir::Number
    } else if is_person_family_emoji(base) || (base as u32) >= 0x1F000 || base.is_alphabetic() {
        ClusterDir::LtrStrong
    } else {
        ClusterDir::Neutral
    }
}

#[inline]
fn resolve_neutral_directions(
    dirs: &[ClusterDir],
    base_dir: TextDirection,
    number_neutral_dir: TextDirection,
) -> Vec<TextDirection> {
    let n = dirs.len();
    let mut prev_strong = vec![None; n];
    let mut cur_prev: Option<TextDirection> = None;
    for i in 0..n {
        prev_strong[i] = cur_prev;
        match dirs[i] {
            ClusterDir::Rtl => cur_prev = Some(TextDirection::Rtl),
            ClusterDir::LtrStrong => cur_prev = Some(TextDirection::Ltr),
            ClusterDir::Number => cur_prev = Some(number_neutral_dir),
            ClusterDir::Neutral => {}
        }
    }

    let mut next_strong = vec![None; n];
    let mut cur_next: Option<TextDirection> = None;
    for i in (0..n).rev() {
        next_strong[i] = cur_next;
        match dirs[i] {
            ClusterDir::Rtl => cur_next = Some(TextDirection::Rtl),
            ClusterDir::LtrStrong => cur_next = Some(TextDirection::Ltr),
            ClusterDir::Number => cur_next = Some(number_neutral_dir),
            ClusterDir::Neutral => {}
        }
    }

    let mut resolved = vec![base_dir; n];
    for i in 0..n {
        resolved[i] = match dirs[i] {
            ClusterDir::Rtl => TextDirection::Rtl,
            ClusterDir::LtrStrong | ClusterDir::Number => TextDirection::Ltr,
            ClusterDir::Neutral => match (prev_strong[i], next_strong[i]) {
                (Some(a), Some(b)) if a == b => a,
                _ => base_dir,
            },
        };
    }
    resolved
}

/// Shapes Arabic letters into connected presentation forms (plus Lam-Alef ligatures) and reorders
/// directional runs according to UAX #9 for left-to-right canvas drawing (`egui`), without mutating
/// the underlying logical UTF-8 document buffer.
pub fn shape_bidi_for_display(text: &str, base_dir: TextDirection) -> String {
    if text.is_empty() {
        return String::new();
    }
    if base_dir == TextDirection::Ltr && text.is_ascii() {
        return text.to_string();
    }
    let shaped = shape_arabic_with_ligatures(text);
    if base_dir == TextDirection::Ltr && !shaped.chars().any(is_rtl_char) {
        return shaped;
    }

    // Split into display clusters as byte-range slices into `shaped` (zero per-cluster String alloc)
    let mut cluster_ranges: Vec<Range<usize>> = Vec::new();
    let mut cluster_dirs: Vec<ClusterDir> = Vec::new();
    let bytes_len = shaped.len();
    let mut idx = 0usize;
    while idx < bytes_len {
        let start = idx;
        let first_ch = shaped[idx..].chars().next().unwrap();
        idx += first_ch.len_utf8();
        while idx < bytes_len {
            let next_ch = shaped[idx..].chars().next().unwrap();
            if !is_extending_char(next_ch) {
                break;
            }
            idx += next_ch.len_utf8();
        }
        cluster_ranges.push(start..idx);
        cluster_dirs.push(classify_display_char(first_ch));
    }

    let resolved = resolve_neutral_directions(&cluster_dirs, base_dir, base_dir);

    // Group contiguous clusters of the same resolved TextDirection into visual runs
    let mut runs: Vec<(TextDirection, Vec<&str>)> = Vec::new();
    for (i, r) in cluster_ranges.into_iter().enumerate() {
        let d = resolved[i];
        let c_str = &shaped[r];
        if let Some(last) = runs.last_mut()
            && last.0 == d {
                last.1.push(c_str);
                continue;
            }
        runs.push((d, vec![c_str]));
    }

    // Within each RTL run, reverse clusters visually; when base_dir is RTL, reverse top-level runs
    if base_dir == TextDirection::Rtl {
        runs.reverse();
    }

    let mut out = String::with_capacity(shaped.len());
    for (dir, run_clusters) in runs {
        if dir == TextDirection::Rtl {
            for c_str in run_clusters.into_iter().rev() {
                for ch in c_str.chars() {
                    out.push(mirror_rtl_bracket(ch));
                }
            }
        } else {
            for c_str in run_clusters {
                out.push_str(c_str);
            }
        }
    }
    out
}

/// Mirrors UAX #9 bidirectional brackets when rendered inside an RTL visual run.
#[inline]
fn mirror_rtl_bracket(ch: char) -> char {
    match ch {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '\u{00AB}' => '\u{00BB}',
        '\u{00BB}' => '\u{00AB}',
        '\u{2039}' => '\u{203A}',
        '\u{203A}' => '\u{2039}',
        _ => ch,
    }
}

/// Bijective logical-to-visual cluster mapping for a single line, preserving 1-to-1 char indices
/// across shaped Arabic RTL runs, mixed RTL/LTR runs, and multi-codepoint emoji grapheme clusters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidiClusterMap {
    pub log_range: Range<usize>,
    pub vis_range: Range<usize>,
    pub dir: TextDirection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualBidiLineMap {
    pub visual_text: String,
    pub base_dir: TextDirection,
    pub is_identity: bool,
    pub logical_char_len: usize,
    pub vis_to_log: Vec<usize>,
    pub clusters: Vec<BidiClusterMap>,
}

impl VisualBidiLineMap {
    /// Maps a logical caret column `0..=logical_char_len` to its visual `CCursor` index in `visual_text`.
    pub fn visual_col_for_logical(&self, log_col: usize) -> usize {
        let col = log_col.min(self.logical_char_len);
        if self.is_identity || self.clusters.is_empty() {
            return col;
        }
        if col == self.logical_char_len {
            if let Some(c) = self.clusters.iter().find(|c| c.log_range.end == self.logical_char_len) {
                return match c.dir {
                    TextDirection::Rtl => c.vis_range.start,
                    TextDirection::Ltr => c.vis_range.end,
                };
            }
            return self.logical_char_len;
        }
        if let Some(c) = self.clusters.iter().find(|c| col >= c.log_range.start && col < c.log_range.end) {
            return match c.dir {
                TextDirection::Rtl => {
                    if col == c.log_range.start {
                        c.vis_range.end
                    } else {
                        c.vis_range.start
                    }
                }
                TextDirection::Ltr => {
                    if col == c.log_range.start {
                        c.vis_range.start
                    } else {
                        c.vis_range.end
                    }
                }
            };
        }
        col
    }

    /// Maps a visual `CCursor` index in `visual_text` back to a grapheme-snapped logical char column.
    pub fn logical_col_for_visual(&self, line_str: &str, vis_col: usize) -> usize {
        if self.is_identity || self.clusters.is_empty() {
            let bounds = grapheme_char_boundaries(line_str);
            let max_c = *bounds.last().unwrap_or(&0);
            let vc = vis_col.min(max_c);
            for w in bounds.windows(2) {
                let s = w[0];
                let e = w[1];
                if vc >= s && vc < e {
                    return if (vc - s) * 2 >= (e - s) { e } else { s };
                }
            }
            return max_c;
        }
        let vc = vis_col.min(self.logical_char_len);
        if vc == 0 {
            let first = &self.clusters[0];
            return match first.dir {
                TextDirection::Rtl => first.log_range.end,
                TextDirection::Ltr => first.log_range.start,
            };
        }
        if vc >= self.logical_char_len {
            let last = self.clusters.last().unwrap();
            return match last.dir {
                TextDirection::Rtl => last.log_range.start,
                TextDirection::Ltr => last.log_range.end,
            };
        }
        // Check if vc is inside a cluster or at a cluster boundary
        if let Some(c) = self.clusters.iter().find(|c| vc >= c.vis_range.start && vc < c.vis_range.end) {
            let rel = vc - c.vis_range.start;
            let len = c.vis_range.end - c.vis_range.start;
            return match c.dir {
                TextDirection::Rtl => {
                    if rel * 2 < len {
                        c.log_range.end
                    } else {
                        c.log_range.start
                    }
                }
                TextDirection::Ltr => {
                    if rel * 2 >= len {
                        c.log_range.end
                    } else {
                        c.log_range.start
                    }
                }
            };
        }
        vc.min(self.logical_char_len)
    }

    /// Maps a logical selection or search range `log_start..log_end` into merged visual char spans.
    pub fn visual_ranges_for_logical_span(
        &self,
        log_start: usize,
        log_end: usize,
    ) -> Vec<std::ops::Range<usize>> {
        let s = log_start.min(self.logical_char_len);
        let e = log_end.min(self.logical_char_len);
        if s >= e {
            return Vec::new();
        }
        if self.is_identity {
            return std::iter::once(s..e).collect();
        }
        let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
        for c in &self.clusters {
            if c.log_range.end > s && c.log_range.start < e {
                ranges.push(c.vis_range.clone());
            }
        }
        ranges.sort_by_key(|r| r.start);
        let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
        for r in ranges {
            if let Some(last) = merged.last_mut()
                && r.start <= last.end {
                    last.end = last.end.max(r.end);
                    continue;
                }
            merged.push(r);
        }
        merged
    }
}

fn compute_ltr_markup_mask(logical_chars: &[char]) -> Vec<bool> {
    let n = logical_chars.len();
    let mut mask = vec![false; n];
    let mut i = 0usize;
    while i < n {
        if logical_chars[i] == '<' {
            // HTML comment: <!-- ... -->
            if i + 3 < n
                && logical_chars[i + 1] == '!'
                && logical_chars[i + 2] == '-'
                && logical_chars[i + 3] == '-'
                && let Some(rel_end) = logical_chars[i + 4..]
                    .windows(3)
                    .position(|w| w == ['-', '-', '>'])
                {
                    let end = i + 4 + rel_end + 3;
                    for slot in &mut mask[i..end] {
                        *slot = true;
                    }
                    i = end;
                    continue;
                }
            // Inline HTML tag: <tag...> or </tag>
            if i + 1 < n
                && (logical_chars[i + 1].is_ascii_alphabetic() || logical_chars[i + 1] == '/')
                && let Some(rel_gt) = logical_chars[i + 1..].iter().position(|&c| c == '>') {
                    let end = i + 1 + rel_gt + 1;
                    if end - i <= 64
                        && !logical_chars[i + 1..end - 1].iter().any(|&c| is_rtl_char(c))
                    {
                        for slot in &mut mask[i..end] {
                            *slot = true;
                        }
                        i = end;
                        continue;
                    }
                }
        }
        i += 1;
    }
    mask
}

/// Builds a bijective 1-to-1 visual-to-logical mapping for `line` with connected Arabic forms
/// and UAX #9 run reordering. Pure LTR lines without RTL chars use a zero-overhead identity map.
pub fn build_visual_bidi_line_map(line: &str) -> VisualBidiLineMap {
    if line.is_ascii() {
        return VisualBidiLineMap {
            visual_text: line.to_string(),
            base_dir: TextDirection::Ltr,
            is_identity: true,
            logical_char_len: line.len(),
            vis_to_log: Vec::new(),
            clusters: Vec::new(),
        };
    }

    let base_dir = detect_paragraph_direction(line);
    let logical_chars: Vec<char> = line.chars().collect();
    let n = logical_chars.len();
    let has_rtl = logical_chars.iter().copied().any(is_rtl_char);

    if !has_rtl && base_dir == TextDirection::Ltr {
        return VisualBidiLineMap {
            visual_text: line.to_string(),
            base_dir,
            is_identity: true,
            logical_char_len: n,
            vis_to_log: Vec::new(),
            clusters: Vec::new(),
        };
    }

    let ltr_markup_mask = compute_ltr_markup_mask(&logical_chars);
    let shaped_chars: Vec<char> = shape_arabic_1to1(line).chars().collect();
    let bounds = grapheme_char_boundaries(line);
    let mut raw_ranges: Vec<Range<usize>> = Vec::with_capacity(bounds.len().saturating_sub(1));
    let mut raw_dirs: Vec<ClusterDir> = Vec::with_capacity(bounds.len().saturating_sub(1));

    for w in bounds.windows(2) {
        let s = w[0];
        let e = w[1];
        if s >= e {
            continue;
        }
        let in_ltr_markup = (s..e).all(|idx| ltr_markup_mask[idx]);
        let cls = if in_ltr_markup {
            ClusterDir::LtrStrong
        } else {
            classify_display_char(logical_chars[s])
        };
        raw_ranges.push(s..e);
        raw_dirs.push(cls);
    }

    let resolved = resolve_neutral_directions(&raw_dirs, base_dir, TextDirection::Ltr);

    let mut runs: Vec<(TextDirection, Vec<std::ops::Range<usize>>)> = Vec::new();
    for (i, log_r) in raw_ranges.into_iter().enumerate() {
        let d = resolved[i];
        if let Some(last) = runs.last_mut()
            && last.0 == d {
                last.1.push(log_r);
                continue;
            }
        runs.push((d, vec![log_r]));
    }

    if base_dir == TextDirection::Rtl {
        runs.reverse();
    }

    let mut visual_text = String::with_capacity(line.len() + 8);
    let mut vis_to_log = Vec::with_capacity(n);
    let mut clusters = Vec::new();
    let mut vis_cursor = 0usize;

    for (dir, mut run_clusters) in runs {
        if dir == TextDirection::Rtl {
            run_clusters.reverse();
        }
        for log_r in run_clusters {
            let v_start = vis_cursor;
            for idx in log_r.clone() {
                let raw_ch = shaped_chars[idx];
                let disp_ch = if dir == TextDirection::Rtl && !ltr_markup_mask[idx] {
                    mirror_rtl_bracket(raw_ch)
                } else {
                    raw_ch
                };
                visual_text.push(disp_ch);
                vis_to_log.push(idx);
                vis_cursor += 1;
            }
            clusters.push(BidiClusterMap {
                log_range: log_r,
                vis_range: v_start..vis_cursor,
                dir,
            });
        }
    }

    VisualBidiLineMap {
        visual_text,
        base_dir,
        is_identity: false,
        logical_char_len: n,
        vis_to_log,
        clusters,
    }
}

#[derive(Debug, Clone)]
struct MultilineBidiLineEntry {
    log_start: usize,
    log_end: usize,
    vis_start: usize,
    vis_end: usize,
    byte_start: usize,
    byte_end: usize,
    has_cr: bool,
    map: VisualBidiLineMap,
}

/// Multi-line wrapper around `VisualBidiLineMap` keeping 1-to-1 char mapping across `\n` boundaries.
#[derive(Debug, Clone)]
pub struct MultilineBidiMap {
    pub visual_text: String,
    pub vis_to_log: Vec<usize>,
    pub is_identity: bool,
    pub total_chars: usize,
    lines: Vec<MultilineBidiLineEntry>,
}

impl MultilineBidiMap {
    pub fn new(text: &str) -> Self {
        if text.is_ascii() {
            return Self {
                visual_text: text.to_string(),
                vis_to_log: Vec::new(),
                is_identity: true,
                total_chars: text.len(),
                lines: Vec::new(),
            };
        }

        let mut lines = Vec::new();
        let mut visual_text = String::with_capacity(text.len() + 8);
        let mut vis_to_log = Vec::new();
        let mut is_identity = true;

        let mut log_cursor = 0usize;
        let mut vis_cursor = 0usize;
        let mut byte_cursor = 0usize;

        for raw_line in text.split('\n') {
            let line_chars = raw_line.chars().count();
            let line_bytes = raw_line.len();
            let has_cr = raw_line.ends_with('\r');
            let clean_bytes = if has_cr { line_bytes - 1 } else { line_bytes };
            let clean_line = &raw_line[..clean_bytes];
            let clean_chars = if has_cr {
                line_chars.saturating_sub(1)
            } else {
                line_chars
            };

            let map = build_visual_bidi_line_map(clean_line);
            if !map.is_identity && is_identity {
                is_identity = false;
                vis_to_log.reserve(text.chars().count());
                for prev_idx in 0..vis_cursor {
                    vis_to_log.push(prev_idx);
                }
            }
            let l_start = log_cursor;
            let l_end = log_cursor + line_chars;
            let v_start = vis_cursor;
            let v_end = vis_cursor + line_chars;

            visual_text.push_str(&map.visual_text);
            if !is_identity {
                if map.is_identity {
                    for local_l in 0..clean_chars {
                        vis_to_log.push(l_start + local_l);
                    }
                } else {
                    for &local_l in &map.vis_to_log {
                        vis_to_log.push(l_start + local_l);
                    }
                }
            }
            if has_cr {
                visual_text.push('\r');
                if !is_identity {
                    vis_to_log.push(l_start + clean_chars);
                }
            }

            lines.push(MultilineBidiLineEntry {
                log_start: l_start,
                log_end: l_end,
                vis_start: v_start,
                vis_end: v_end,
                byte_start: byte_cursor,
                byte_end: byte_cursor + clean_bytes,
                has_cr,
                map,
            });

            log_cursor = l_end;
            vis_cursor = v_end;
            byte_cursor += line_bytes;

            if byte_cursor < text.len() && text.as_bytes()[byte_cursor] == b'\n' {
                visual_text.push('\n');
                if !is_identity {
                    vis_to_log.push(log_cursor);
                }
                log_cursor += 1;
                vis_cursor += 1;
                byte_cursor += 1;
            }
        }

        Self {
            visual_text,
            vis_to_log,
            is_identity,
            total_chars: log_cursor,
            lines,
        }
    }

    pub fn visual_col_for_logical(&self, log_idx: usize) -> usize {
        if self.is_identity {
            return log_idx.min(self.total_chars);
        }
        let c = log_idx.min(self.total_chars);
        for entry in &self.lines {
            if c <= entry.log_end {
                let clean_len = entry.map.logical_char_len;
                let local = c.saturating_sub(entry.log_start);
                if entry.has_cr && local > clean_len {
                    return entry.vis_start + clean_len + 1;
                }
                return entry.vis_start + entry.map.visual_col_for_logical(local.min(clean_len));
            }
        }
        c
    }

    pub fn logical_col_for_visual(&self, text: &str, vis_idx: usize) -> usize {
        let vc = vis_idx.min(self.total_chars);
        if self.is_identity {
            return snap_to_grapheme_start(text, vc);
        }
        for entry in &self.lines {
            if vc <= entry.vis_end {
                let clean_len = entry.map.logical_char_len;
                let local_v = vc.saturating_sub(entry.vis_start);
                if entry.has_cr && local_v > clean_len {
                    return entry.log_start + clean_len;
                }
                let line_slice = text.get(entry.byte_start..entry.byte_end).unwrap_or("");
                return entry.log_start
                    + entry
                        .map
                        .logical_col_for_visual(line_slice, local_v.min(clean_len));
            }
        }
        vc
    }

    pub fn visual_ranges_for_logical_span(
        &self,
        log_start: usize,
        log_end: usize,
    ) -> Vec<Range<usize>> {
        let s = log_start.min(self.total_chars);
        let e = log_end.min(self.total_chars);
        if s >= e {
            return Vec::new();
        }
        if self.is_identity {
            return std::iter::once(s..e).collect();
        }
        let mut out = Vec::new();
        for entry in &self.lines {
            if e > entry.log_start && s < entry.log_end {
                let ls = s.max(entry.log_start) - entry.log_start;
                let le = e.min(entry.log_end) - entry.log_start;
                for vr in entry.map.visual_ranges_for_logical_span(ls, le) {
                    out.push((entry.vis_start + vr.start)..(entry.vis_start + vr.end));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidi_shaping_connects_and_reorders_arabic_and_mixed_runs() {
        let shaped = shape_bidi_for_display("مرحبا", TextDirection::Rtl);
        let got: Vec<char> = shaped.chars().collect();
        assert_eq!(got, vec!['\u{FE8E}', '\u{FE92}', '\u{FEA3}', '\u{FEAE}', '\u{FEE3}']);

        let lam_alef = shape_bidi_for_display("لا", TextDirection::Rtl);
        assert_eq!(lam_alef, "\u{FEFB}");

        let mixed = shape_bidi_for_display("Hello مرحبا 2026", TextDirection::Ltr);
        assert!(mixed.starts_with("Hello "));
        assert!(mixed.ends_with(" 2026"));

        let onetoone = shape_arabic_1to1("مرحبا");
        assert_eq!(onetoone.chars().count(), "مرحبا".chars().count());
    }

    #[test]
    fn bidi_direction_auto_detection() {
        assert_eq!(detect_paragraph_direction("Hello world"), TextDirection::Ltr);
        assert_eq!(
            detect_paragraph_direction("مرحبا بالعالم"),
            TextDirection::Rtl
        );
        assert_eq!(
            detect_paragraph_direction("<!-- align:right -->## مرحبا **Hello**"),
            TextDirection::Rtl
        );
        assert_eq!(
            detect_paragraph_direction("- [ ] <u>مرحبا</u>"),
            TextDirection::Rtl
        );
        assert_eq!(
            detect_paragraph_direction("1. English then مرحبا"),
            TextDirection::Ltr
        );
    }

    #[test]
    fn grapheme_clusters_preserve_zwj_family_and_arabic_ligatures() {
        let zwj_family = "👨‍👩‍👧‍👦";
        let bounds_zwj = grapheme_char_boundaries(zwj_family);
        assert_eq!(bounds_zwj, vec![0, zwj_family.chars().count()]);

        let seq_family = "👨👩👧👦";
        let bounds_seq = grapheme_char_boundaries(seq_family);
        assert_eq!(bounds_seq, vec![0, 4]);
        assert_eq!(snap_to_grapheme_start(seq_family, 2), 0);
        assert_eq!(snap_to_grapheme_end(seq_family, 2), 4);

        let mixed = "مرحبا 👨👩👧👦!";
        let len = mixed.chars().count();
        // 'م','ر','ح','ب','ا',' ', '👨👩👧👦' (at 6..10), '!' (at 10..11)
        assert_eq!(next_grapheme_char_idx(mixed, 6), 10);
        assert_eq!(prev_grapheme_char_idx(mixed, 10), 6);
        assert_eq!(len, 11);

        // Arabic haraka + Lam-Alef ligature
        let arabic = "سَلَامٌ";
        assert!(grapheme_char_boundaries(arabic).len() < arabic.chars().count());
    }

    #[test]
    fn block_metadata_roundtrip_and_non_interference() {
        let raw = "<!-- align:center,indent:2 -->Hello";
        let (align, indent, rest) = parse_line_block_meta(raw);
        assert_eq!(align, BlockAlignment::Center);
        assert_eq!(indent, 2);
        assert_eq!(rest, "Hello");
        assert_eq!(
            format_line_block_meta(align, indent),
            "<!-- align:center,indent:2 -->"
        );

        let user_comment = "<!-- regular html comment -->Hello";
        assert_eq!(
            parse_line_block_meta(user_comment),
            (BlockAlignment::Start, 0, user_comment)
        );
    }
}
