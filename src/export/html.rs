use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use rustc_hash::FxHashMap;

use crate::document::ast::{is_math_code_info, is_mermaid_code_info};
use crate::document::math;
use crate::document::mermaid;
use crate::document::unicode::{BlockAlignment, TextDirection};
use crate::export::encode_base64;
use crate::export::ir::{
    heading_slug, runs_to_plain_text, DocBlock, InlineRun, SlugTracker, TableAlign,
};

struct OutlineHeading {
    level: u8,
    title: String,
    slug: String,
}

/// Renders `blocks` into a standalone HTML5 document string with an interactive clickable
/// outline sidebar, smooth section anchors, responsive styling, and embedded local/diagram assets.
pub fn render_html_string(blocks: &[DocBlock], title: &str) -> String {
    render_html_string_with_base_dir(blocks, title, None)
}

/// Renders `blocks` into a standalone HTML5 document string, resolving relative image assets
/// against `base_dir` and embedding them as data URIs when readable.
pub fn render_html_string_with_base_dir(
    blocks: &[DocBlock],
    title: &str,
    base_dir: Option<&Path>,
) -> String {
    let mut buf = Vec::with_capacity(blocks.len() * 192 + 4096);
    let _ = write_html_stream(&mut buf, blocks, title, base_dir, |_| {});
    String::from_utf8(buf).unwrap_or_default()
}

/// Streams a standalone HTML5 file directly to `path` with progress reporting.
pub fn export_html_to_file_with_base_dir(
    blocks: &[DocBlock],
    title: &str,
    path: impl AsRef<Path>,
    base_dir: Option<&Path>,
    on_progress: impl FnMut(f32),
) -> Result<(), String> {
    let file = File::create(path.as_ref()).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    write_html_stream(&mut writer, blocks, title, base_dir, on_progress)
        .map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

fn write_html_stream<W: Write>(
    w: &mut W,
    blocks: &[DocBlock],
    title: &str,
    base_dir: Option<&Path>,
    mut on_progress: impl FnMut(f32),
) -> io::Result<()> {
    let mut collect_slugs = SlugTracker::new();
    let mut outline = Vec::new();
    collect_outline_headings(blocks, &mut collect_slugs, &mut outline);

    let known_slugs: HashSet<String> = outline.iter().map(|h| h.slug.clone()).collect();
    let mut render_slugs = SlugTracker::new();
    let mut image_cache: FxHashMap<String, String> = FxHashMap::default();

    let safe_title = if title.trim().is_empty() {
        "Markdown Studio Document"
    } else {
        title.trim()
    };

    w.write_all(b"<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n")?;
    w.write_all(b"<meta charset=\"utf-8\">\n")?;
    w.write_all(b"<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n")?;
    w.write_all(
        b"<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data: https: http:; style-src 'unsafe-inline';\">\n",
    )?;
    w.write_all(b"<title>")?;
    write_escaped_html(w, safe_title)?;
    w.write_all(b"</title>\n<style>\n")?;
    w.write_all(HTML_STYLESHEET.as_bytes())?;
    w.write_all(b"\n</style>\n</head>\n<body>\n<div class=\"layout\">\n")?;

    if !outline.is_empty() {
        w.write_all(b"<nav class=\"outline\" aria-label=\"Document Outline\">\n")?;
        w.write_all(b"  <details open>\n")?;
        w.write_all(b"    <summary class=\"outline-title\">Outline</summary>\n")?;
        w.write_all(b"    <ul class=\"outline-list\">\n")?;
        for item in &outline {
            let lvl = item.level.clamp(1, 6);
            write!(w, "      <li class=\"outline-item lvl-{lvl}\"><a href=\"#")?;
            write_escaped_html(w, &item.slug)?;
            w.write_all(b"\">")?;
            write_escaped_html(w, &item.title)?;
            w.write_all(b"</a></li>\n")?;
        }
        w.write_all(b"    </ul>\n  </details>\n</nav>\n")?;
    }

    w.write_all(b"<main class=\"document\" role=\"main\">\n")?;
    let total = blocks.len().max(1);
    for (idx, block) in blocks.iter().enumerate() {
        write_block_html(
            w,
            block,
            base_dir,
            &mut render_slugs,
            &known_slugs,
            &mut image_cache,
        )?;
        on_progress((idx + 1) as f32 / total as f32);
    }
    w.write_all(b"</main>\n</div>\n</body>\n</html>\n")
}

fn collect_outline_headings(
    blocks: &[DocBlock],
    tracker: &mut SlugTracker,
    out: &mut Vec<OutlineHeading>,
) {
    for block in blocks {
        match block {
            DocBlock::Heading { level, runs, .. } => {
                let raw = runs_to_plain_text(runs);
                let title = raw.trim();
                if !title.is_empty() && *level <= 6 {
                    let slug = tracker.next_slug(title);
                    out.push(OutlineHeading {
                        level: *level,
                        title: title.to_string(),
                        slug,
                    });
                }
            }
            DocBlock::BlockQuote { blocks, .. } | DocBlock::FootnoteDefinition { blocks, .. } => {
                collect_outline_headings(blocks, tracker, out);
            }
            DocBlock::List { items, .. } => {
                for item in items {
                    collect_outline_headings(&item.blocks, tracker, out);
                }
            }
            _ => {}
        }
    }
}

fn write_block_attrs<W: Write>(
    w: &mut W,
    alignment: BlockAlignment,
    direction: TextDirection,
) -> io::Result<()> {
    let align_cls = match alignment {
        BlockAlignment::Center => Some("align-center"),
        BlockAlignment::Right => Some("align-right"),
        BlockAlignment::Justify => Some("align-justify"),
        BlockAlignment::End => {
            if direction == TextDirection::Rtl {
                Some("align-left")
            } else {
                Some("align-right")
            }
        }
        BlockAlignment::Start | BlockAlignment::Left => None,
    };
    if let Some(cls) = align_cls {
        write!(w, " class=\"{cls}\"")?;
    }
    if direction == TextDirection::Rtl {
        w.write_all(b" dir=\"rtl\"")?;
    }
    Ok(())
}

fn write_indent_style<W: Write>(w: &mut W, indent_level: u8) -> io::Result<()> {
    if indent_level > 0 {
        write!(
            w,
            " style=\"margin-inline-start: {}em\"",
            indent_level as f32 * 1.5
        )?;
    }
    Ok(())
}

fn write_block_html<W: Write>(
    w: &mut W,
    block: &DocBlock,
    base_dir: Option<&Path>,
    slug_tracker: &mut SlugTracker,
    known_slugs: &HashSet<String>,
    image_cache: &mut FxHashMap<String, String>,
) -> io::Result<()> {
    match block {
        DocBlock::Heading {
            level,
            runs,
            alignment,
            direction,
            indent_level,
            ..
        } => {
            let lvl = (*level).clamp(1, 6);
            let raw = runs_to_plain_text(runs);
            let trimmed = raw.trim();
            let slug = if !trimmed.is_empty() {
                slug_tracker.next_slug(trimmed)
            } else {
                String::new()
            };
            write!(w, "<h{lvl}")?;
            if !slug.is_empty() {
                w.write_all(b" id=\"")?;
                write_escaped_html(w, &slug)?;
                w.write_all(b"\"")?;
            }
            write_block_attrs(w, *alignment, *direction)?;
            write_indent_style(w, *indent_level)?;
            w.write_all(b">")?;
            write_runs_html(w, runs, base_dir, known_slugs, image_cache)?;
            writeln!(w, "</h{lvl}>")
        }
        DocBlock::Paragraph {
            runs,
            alignment,
            direction,
            indent_level,
            ..
        } => {
            w.write_all(b"<p")?;
            write_block_attrs(w, *alignment, *direction)?;
            write_indent_style(w, *indent_level)?;
            w.write_all(b">")?;
            write_runs_html(w, runs, base_dir, known_slugs, image_cache)?;
            w.write_all(b"</p>\n")
        }
        DocBlock::BlockQuote {
            blocks,
            direction,
            indent_level,
            ..
        } => {
            w.write_all(b"<blockquote")?;
            if *direction == TextDirection::Rtl {
                w.write_all(b" dir=\"rtl\"")?;
            }
            write_indent_style(w, *indent_level)?;
            w.write_all(b">\n")?;
            for inner in blocks {
                write_block_html(w, inner, base_dir, slug_tracker, known_slugs, image_cache)?;
            }
            w.write_all(b"</blockquote>\n")
        }
        DocBlock::CodeBlock { info, code, .. } => {
            if is_mermaid_code_info(info)
                && !code.trim().is_empty()
                && let Some((png_bytes, _, _)) = mermaid::render_mermaid_to_png_bytes(code, 2.0)
            {
                let b64 = encode_base64(&png_bytes);
                return writeln!(
                    w,
                    "<figure class=\"diagram mermaid-diagram\"><img src=\"data:image/png;base64,{b64}\" alt=\"Mermaid diagram\"></figure>"
                );
            }
            if is_math_code_info(info) {
                let trimmed = code.trim();
                if trimmed.is_empty() {
                    return Ok(());
                }
                if let Some((png_bytes, _, _)) = math::render_math_to_png_bytes(
                    trimmed,
                    18.0,
                    true,
                    [20, 24, 32, 255],
                    [255, 255, 255, 255],
                ) {
                    let b64 = encode_base64(&png_bytes);
                    w.write_all(
                        b"<figure class=\"diagram math-block\"><img src=\"data:image/png;base64,",
                    )?;
                    w.write_all(b64.as_bytes())?;
                    w.write_all(b"\" alt=\"")?;
                    write_escaped_html(w, trimmed)?;
                    return w.write_all(b"\"></figure>\n");
                }
            }
            let lang = info.split_whitespace().next().unwrap_or("");
            if lang.is_empty() {
                w.write_all(b"<pre><code>")?;
                write_escaped_html(w, code)?;
                w.write_all(b"</code></pre>\n")
            } else {
                w.write_all(b"<pre><code class=\"language-")?;
                write_escaped_html(w, lang)?;
                w.write_all(b"\">")?;
                write_escaped_html(w, code)?;
                w.write_all(b"</code></pre>\n")
            }
        }
        DocBlock::List {
            ordered,
            start,
            items,
            direction,
            indent_level,
            ..
        } => {
            let tag = if *ordered { "ol" } else { "ul" };
            write!(w, "<{tag}")?;
            if *ordered && *start != 1 {
                write!(w, " start=\"{start}\"")?;
            }
            if *direction == TextDirection::Rtl {
                w.write_all(b" dir=\"rtl\"")?;
            }
            write_indent_style(w, *indent_level)?;
            w.write_all(b">\n")?;
            for item in items {
                if item.checked.is_some() {
                    w.write_all(b"<li class=\"task-item\">\n")?;
                } else {
                    w.write_all(b"<li>\n")?;
                }
                if let Some(checked) = item.checked {
                    let chk = if checked { " checked" } else { "" };
                    writeln!(w, "<input type=\"checkbox\" disabled{chk}> ")?;
                }
                for b in &item.blocks {
                    write_block_html(w, b, base_dir, slug_tracker, known_slugs, image_cache)?;
                }
                w.write_all(b"</li>\n")?;
            }
            writeln!(w, "</{tag}>")
        }
        DocBlock::Table {
            alignments,
            header,
            rows,
            ..
        } => {
            w.write_all(b"<div class=\"table-wrap\"><table>\n")?;
            if !header.is_empty() {
                w.write_all(b"<thead><tr>\n")?;
                for (c, cell) in header.iter().enumerate() {
                    let align = table_align_attr(alignments.get(c).copied().unwrap_or_default());
                    write!(w, "<th{align}>")?;
                    write_runs_html(w, cell, base_dir, known_slugs, image_cache)?;
                    w.write_all(b"</th>")?;
                }
                w.write_all(b"\n</tr></thead>\n")?;
            }
            w.write_all(b"<tbody>\n")?;
            for row in rows {
                w.write_all(b"<tr>")?;
                for (c, cell) in row.iter().enumerate() {
                    let align = table_align_attr(alignments.get(c).copied().unwrap_or_default());
                    write!(w, "<td{align}>")?;
                    write_runs_html(w, cell, base_dir, known_slugs, image_cache)?;
                    w.write_all(b"</td>")?;
                }
                w.write_all(b"</tr>\n")?;
            }
            w.write_all(b"</tbody>\n</table></div>\n")
        }
        DocBlock::ThematicBreak { .. } => w.write_all(b"<hr>\n"),
        DocBlock::FootnoteDefinition { name, blocks, .. } => {
            let fn_slug = heading_slug(name);
            let id_val = if fn_slug.is_empty() {
                format!("fn-{}", name.trim().to_ascii_lowercase())
            } else {
                format!("fn-{fn_slug}")
            };
            w.write_all(b"<section class=\"footnote-def\" id=\"")?;
            write_escaped_html(w, &id_val)?;
            w.write_all(b"\">\n<span class=\"footnote-label\">[^")?;
            write_escaped_html(w, name)?;
            w.write_all(b"]:</span>\n")?;
            for inner in blocks {
                write_block_html(w, inner, base_dir, slug_tracker, known_slugs, image_cache)?;
            }
            w.write_all(b"</section>\n")
        }
    }
}

fn table_align_attr(align: TableAlign) -> &'static str {
    match align {
        TableAlign::Left => " style=\"text-align: left\"",
        TableAlign::Center => " style=\"text-align: center\"",
        TableAlign::Right => " style=\"text-align: right\"",
        TableAlign::None => "",
    }
}

fn write_runs_html<W: Write>(
    w: &mut W,
    runs: &[InlineRun],
    base_dir: Option<&Path>,
    known_slugs: &HashSet<String>,
    image_cache: &mut FxHashMap<String, String>,
) -> io::Result<()> {
    for run in runs {
        if let Some(ref img_url) = run.image_url {
            if let Some(src) = resolve_safe_html_image_src(img_url, base_dir, image_cache) {
                w.write_all(b"<img src=\"")?;
                write_escaped_html(w, &src)?;
                w.write_all(b"\" alt=\"")?;
                write_escaped_html(w, &run.text)?;
                w.write_all(b"\">")?;
            }
            continue;
        }

        if run.is_footnote_ref {
            let name = run.text.trim_matches(|c| matches!(c, '[' | ']' | '^'));
            let slug = heading_slug(name);
            let href = if slug.is_empty() {
                format!("#fn-{}", name.trim().to_ascii_lowercase())
            } else {
                format!("#fn-{slug}")
            };
            w.write_all(b"<sup><a href=\"")?;
            write_escaped_html(w, &href)?;
            w.write_all(b"\" class=\"footnote-ref\">")?;
            write_escaped_html(w, &run.text)?;
            w.write_all(b"</a></sup>")?;
            continue;
        }

        let link_info = run
            .link_url
            .as_deref()
            .and_then(|raw_url| classify_html_href(raw_url, known_slugs));
        if let Some((ref href, external)) = link_info {
            w.write_all(b"<a href=\"")?;
            write_escaped_html(w, href)?;
            if external {
                w.write_all(b"\" target=\"_blank\" rel=\"noopener noreferrer\">")?;
            } else {
                w.write_all(b"\">")?;
            }
        }
        if run.strikethrough {
            w.write_all(b"<del>")?;
        }
        if run.underline {
            w.write_all(b"<u>")?;
        }
        if run.italic {
            w.write_all(b"<em>")?;
        }
        if run.bold {
            w.write_all(b"<strong>")?;
        }

        if run.is_math {
            let plain = math::math_to_plain_text(&run.text);
            w.write_all(b"<span class=\"math-inline\" title=\"")?;
            write_escaped_html(w, &run.text)?;
            w.write_all(b"\">")?;
            write_escaped_html(w, &plain)?;
            w.write_all(b"</span>")?;
        } else if run.code {
            w.write_all(b"<code>")?;
            write_escaped_html(w, &run.text)?;
            w.write_all(b"</code>")?;
        } else {
            write_escaped_html(w, &run.text)?;
        }

        if run.bold {
            w.write_all(b"</strong>")?;
        }
        if run.italic {
            w.write_all(b"</em>")?;
        }
        if run.underline {
            w.write_all(b"</u>")?;
        }
        if run.strikethrough {
            w.write_all(b"</del>")?;
        }
        if link_info.is_some() {
            w.write_all(b"</a>")?;
        }
    }
    Ok(())
}

fn strip_control_chars(s: &str) -> String {
    if !s.chars().any(|c| c.is_control()) {
        return s.to_string();
    }
    s.chars().filter(|c| !c.is_control()).collect()
}

fn is_unsafe_scheme(url: &str) -> bool {
    let trimmed = strip_control_chars(url.trim().trim_start_matches('<').trim_end_matches('>').trim());
    let lower = trimmed.to_ascii_lowercase();
    lower.is_empty()
        || lower.starts_with("javascript:")
        || lower.starts_with("vbscript:")
        || lower.starts_with("data:")
        || lower.starts_with("file:")
}

fn is_safe_raster_data_uri(trimmed: &str) -> bool {
    let clean = strip_control_chars(trimmed);
    let lower = clean.to_ascii_lowercase();
    lower.starts_with("data:image/png;base64,")
        || lower.starts_with("data:image/jpeg;base64,")
        || lower.starts_with("data:image/jpg;base64,")
        || lower.starts_with("data:image/gif;base64,")
        || lower.starts_with("data:image/webp;base64,")
}

fn classify_html_href(raw_url: &str, known_slugs: &HashSet<String>) -> Option<(String, bool)> {
    let trimmed_raw = raw_url
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    if trimmed_raw.chars().any(|c| c.is_control()) {
        return None;
    }
    let trimmed = trimmed_raw;
    if trimmed.is_empty() || is_unsafe_scheme(trimmed) {
        return None;
    }
    if let Some(anchor) = trimmed.strip_prefix('#') {
        let decoded = anchor.replace("%20", " ");
        let lower = decoded.trim().to_ascii_lowercase();
        let slug = if known_slugs.contains(&lower) {
            lower
        } else {
            heading_slug(&decoded)
        };
        if slug.is_empty() {
            return None;
        }
        return Some((format!("#{slug}"), false));
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        return Some((trimmed.to_string(), true));
    }
    if lower.starts_with("www.") {
        return Some((format!("https://{trimmed}"), true));
    }

    if trimmed.starts_with(r"\\") || trimmed.starts_with("//") {
        return None;
    }

    // Reject unknown explicit schemes (`data:`, `custom:`, etc.) while allowing relative links.
    if let Some(colon) = trimmed.find(':') {
        let before = &trimmed[..colon];
        if !before.contains('/') && !before.contains('\\') && !before.contains('.') {
            return None;
        }
    }

    Some((trimmed.to_string(), false))
}

fn resolve_safe_html_image_src(
    raw_url: &str,
    base_dir: Option<&Path>,
    cache: &mut FxHashMap<String, String>,
) -> Option<String> {
    let trimmed = raw_url
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    if trimmed.is_empty() || trimmed.chars().any(|c| c.is_control()) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(trimmed.to_string());
    }
    if lower.starts_with("data:") {
        return is_safe_raster_data_uri(trimmed).then(|| trimmed.to_string());
    }
    if let Some(cached) = cache.get(trimmed) {
        return (!cached.is_empty()).then(|| cached.clone());
    }
    if let Some((mime, bytes)) = crate::export::load_safe_export_web_image(trimmed, base_dir, 2048)
    {
        let uri = format!("data:{mime};base64,{}", encode_base64(&bytes));
        cache.insert(trimmed.to_string(), uri.clone());
        return Some(uri);
    }
    cache.insert(trimmed.to_string(), String::new());
    // Reject UNC, file:, or absolute local paths if image load failed; keep safe relative URLs as-is
    if trimmed.starts_with(r"\\")
        || trimmed.starts_with("//")
        || lower.starts_with("file:")
        || Path::new(trimmed).is_absolute()
    {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn write_escaped_html<W: Write>(w: &mut W, text: &str) -> io::Result<()> {
    let bytes = text.as_bytes();
    let mut last = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        let esc: Option<&[u8]> = match b {
            b'&' => Some(b"&amp;"),
            b'<' => Some(b"&lt;"),
            b'>' => Some(b"&gt;"),
            b'"' => Some(b"&quot;"),
            b'\'' => Some(b"&#39;"),
            _ => None,
        };
        if let Some(seq) = esc {
            if i > last {
                w.write_all(&bytes[last..i])?;
            }
            w.write_all(seq)?;
            last = i + 1;
        }
    }
    if last < bytes.len() {
        w.write_all(&bytes[last..])?;
    }
    Ok(())
}

const HTML_STYLESHEET: &str = r#"
:root {
  --bg: #f6f8fb;
  --paper: #ffffff;
  --text: #1b2230;
  --muted: #5a6578;
  --border: #dfe4ec;
  --accent: #1d5fc4;
  --accent-soft: rgba(29, 95, 196, 0.10);
  --code-bg: #f1f4f9;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #11151d;
    --paper: #181d27;
    --text: #e6ebf4;
    --muted: #98a4b8;
    --border: #2a3342;
    --accent: #6aa3ff;
    --accent-soft: rgba(106, 163, 255, 0.14);
    --code-bg: #1f2633;
  }
}
* { box-sizing: border-box; }
html { scroll-behavior: smooth; }
body {
  margin: 0;
  background: var(--bg);
  color: var(--text);
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
  line-height: 1.65;
  font-size: 16px;
}
.layout {
  max-width: 1240px;
  margin: 0 auto;
  padding: 24px 20px;
  display: flex;
  align-items: flex-start;
  gap: 24px;
}
.outline {
  width: 260px;
  flex-shrink: 0;
  position: sticky;
  top: 20px;
  max-height: calc(100vh - 40px);
  overflow-y: auto;
  background: var(--paper);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 12px 14px;
}
.outline-title {
  font-weight: 700;
  font-size: 13px;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--muted);
  cursor: pointer;
}
.outline-list {
  list-style: none;
  margin: 10px 0 0;
  padding: 0;
}
.outline-item { margin: 2px 0; }
.outline-item a {
  display: block;
  padding: 4px 8px;
  border-radius: 6px;
  text-decoration: none;
  color: var(--text);
  font-size: 13.5px;
}
.outline-item a:hover {
  background: var(--accent-soft);
  color: var(--accent);
}
.outline-item.lvl-2 { padding-left: 12px; }
.outline-item.lvl-3 { padding-left: 24px; }
.outline-item.lvl-4 { padding-left: 36px; }
.outline-item.lvl-5 { padding-left: 48px; }
.outline-item.lvl-6 { padding-left: 60px; }
.document {
  flex: 1;
  min-width: 0;
  max-width: 860px;
  background: var(--paper);
  border: 1px solid var(--border);
  border-radius: 12px;
  padding: 40px 48px;
}
h1, h2, h3, h4, h5, h6 {
  margin-top: 1.5em;
  margin-bottom: 0.5em;
  line-height: 1.28;
  scroll-margin-top: 16px;
}
h1:first-child, h2:first-child, p:first-child { margin-top: 0; }
h1, h2 { padding-bottom: 0.28em; border-bottom: 1px solid var(--border); }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
code {
  font-family: "Cascadia Mono", Consolas, "Courier New", monospace;
  font-size: 0.9em;
  background: var(--code-bg);
  padding: 0.14em 0.36em;
  border-radius: 4px;
}
pre {
  background: var(--code-bg);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 14px 16px;
  overflow-x: auto;
}
pre code { background: transparent; padding: 0; font-size: 0.88em; }
blockquote {
  margin: 1em 0;
  padding: 4px 0 4px 16px;
  border-inline-start: 4px solid var(--accent);
  color: var(--muted);
}
.table-wrap { overflow-x: auto; margin: 1em 0; }
table { width: 100%; border-collapse: collapse; }
th, td { border: 1px solid var(--border); padding: 8px 12px; text-align: left; }
th { background: var(--code-bg); font-weight: 600; }
.task-item { list-style: none; margin-left: -1.2em; }
.diagram { margin: 1.2em 0; text-align: center; }
.diagram img, .document img { max-width: 100%; height: auto; border-radius: 6px; }
hr { border: 0; border-top: 1px solid var(--border); margin: 1.8em 0; }
.align-center { text-align: center; }
.align-right { text-align: right; }
.align-left { text-align: left; }
.align-justify { text-align: justify; }
.footnote-def {
  margin-top: 1em;
  padding-top: 0.6em;
  border-top: 1px dashed var(--border);
  font-size: 0.92em;
  color: var(--muted);
}
.footnote-label { font-weight: 600; color: var(--accent); }
.math-inline { font-family: "Cambria Math", "Times New Roman", serif; }
@media (max-width: 900px) {
  .layout { flex-direction: column; padding: 14px; }
  .outline { width: 100%; position: static; max-height: none; }
  .document { width: 100%; padding: 24px 20px; }
}
"#;
