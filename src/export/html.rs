use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::document::ast::{is_math_code_info, is_mermaid_code_info};
use crate::document::math;
use crate::document::mermaid;
use crate::document::unicode::{BlockAlignment, TextDirection};
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
    render_html_internal(blocks, title, base_dir, |_| {})
}

/// Writes a standalone HTML5 file to `path` with progress reporting.
pub fn export_html_to_file_with_base_dir(
    blocks: &[DocBlock],
    title: &str,
    path: impl AsRef<Path>,
    base_dir: Option<&Path>,
    on_progress: impl FnMut(f32),
) -> Result<(), String> {
    let html = render_html_internal(blocks, title, base_dir, on_progress);
    let file = File::create(path.as_ref()).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(html.as_bytes())
        .map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn render_html_internal(
    blocks: &[DocBlock],
    title: &str,
    base_dir: Option<&Path>,
    mut on_progress: impl FnMut(f32),
) -> String {
    let mut collect_slugs = SlugTracker::new();
    let mut outline = Vec::new();
    collect_outline_headings(blocks, &mut collect_slugs, &mut outline);

    let known_slugs: HashSet<String> = outline.iter().map(|h| h.slug.clone()).collect();
    let mut render_slugs = SlugTracker::new();
    let mut body_html = String::with_capacity(blocks.len() * 160 + 512);

    let total = blocks.len().max(1);
    for (idx, block) in blocks.iter().enumerate() {
        render_block_html(
            &mut body_html,
            block,
            base_dir,
            &mut render_slugs,
            &known_slugs,
        );
        on_progress((idx + 1) as f32 / total as f32);
    }

    let safe_title = if title.trim().is_empty() {
        "Markdown Studio Document"
    } else {
        title.trim()
    };

    let mut out = String::with_capacity(body_html.len() + 4096);
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(out, "<title>{}</title>", escape_html(safe_title));
    out.push_str("<style>\n");
    out.push_str(HTML_STYLESHEET);
    out.push_str("\n</style>\n</head>\n<body>\n");
    out.push_str("<div class=\"layout\">\n");

    if !outline.is_empty() {
        out.push_str("<nav class=\"outline\" aria-label=\"Document Outline\">\n");
        out.push_str("  <details open>\n");
        out.push_str("    <summary class=\"outline-title\">Outline</summary>\n");
        out.push_str("    <ul class=\"outline-list\">\n");
        for item in &outline {
            let lvl = item.level.clamp(1, 6);
            let _ = writeln!(
                out,
                "      <li class=\"outline-item lvl-{lvl}\"><a href=\"#{}\">{}</a></li>",
                escape_html_attr(&item.slug),
                escape_html(&item.title),
            );
        }
        out.push_str("    </ul>\n");
        out.push_str("  </details>\n");
        out.push_str("</nav>\n");
    }

    out.push_str("<main class=\"document\" role=\"main\">\n");
    out.push_str(&body_html);
    out.push_str("</main>\n");
    out.push_str("</div>\n</body>\n</html>\n");
    out
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

fn block_attrs(alignment: BlockAlignment, direction: TextDirection) -> String {
    let mut attrs = String::new();
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
        let _ = write!(attrs, " class=\"{cls}\"");
    }
    if direction == TextDirection::Rtl {
        attrs.push_str(" dir=\"rtl\"");
    }
    attrs
}

fn render_block_html(
    out: &mut String,
    block: &DocBlock,
    base_dir: Option<&Path>,
    slug_tracker: &mut SlugTracker,
    known_slugs: &HashSet<String>,
) {
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
            let id_attr = if slug.is_empty() {
                String::new()
            } else {
                format!(" id=\"{}\"", escape_html_attr(&slug))
            };
            let indent_style = if *indent_level > 0 {
                format!(" style=\"margin-inline-start: {}em\"", *indent_level as f32 * 1.5)
            } else {
                String::new()
            };
            let attrs = block_attrs(*alignment, *direction);
            let _ = write!(out, "<h{lvl}{id_attr}{attrs}{indent_style}>");
            render_runs_html(out, runs, base_dir, known_slugs);
            let _ = writeln!(out, "</h{lvl}>");
        }
        DocBlock::Paragraph {
            runs,
            alignment,
            direction,
            indent_level,
            ..
        } => {
            let attrs = block_attrs(*alignment, *direction);
            let indent_style = if *indent_level > 0 {
                format!(" style=\"margin-inline-start: {}em\"", *indent_level as f32 * 1.5)
            } else {
                String::new()
            };
            let _ = write!(out, "<p{attrs}{indent_style}>");
            render_runs_html(out, runs, base_dir, known_slugs);
            out.push_str("</p>\n");
        }
        DocBlock::BlockQuote {
            blocks,
            direction,
            indent_level,
            ..
        } => {
            let dir_attr = if *direction == TextDirection::Rtl {
                " dir=\"rtl\""
            } else {
                ""
            };
            let indent_style = if *indent_level > 0 {
                format!(" style=\"margin-inline-start: {}em\"", *indent_level as f32 * 1.5)
            } else {
                String::new()
            };
            let _ = writeln!(out, "<blockquote{dir_attr}{indent_style}>");
            for inner in blocks {
                render_block_html(out, inner, base_dir, slug_tracker, known_slugs);
            }
            out.push_str("</blockquote>\n");
        }
        DocBlock::CodeBlock { info, code, .. } => {
            if is_mermaid_code_info(info) && !code.trim().is_empty() {
                if let Some((png_bytes, _, _)) = mermaid::render_mermaid_to_png_bytes(code, 2.0) {
                    let b64 = encode_base64(&png_bytes);
                    let _ = writeln!(
                        out,
                        "<figure class=\"diagram mermaid-diagram\"><img src=\"data:image/png;base64,{b64}\" alt=\"Mermaid diagram\"></figure>"
                    );
                    return;
                }
            }
            if is_math_code_info(info) {
                let trimmed = code.trim();
                if trimmed.is_empty() {
                    return;
                }
                if let Some((png_bytes, _, _)) = math::render_math_to_png_bytes(
                    trimmed,
                    18.0,
                    true,
                    [20, 24, 32, 255],
                    [255, 255, 255, 255],
                ) {
                    let b64 = encode_base64(&png_bytes);
                    let _ = writeln!(
                        out,
                        "<figure class=\"diagram math-block\"><img src=\"data:image/png;base64,{b64}\" alt=\"{}\"></figure>",
                        escape_html_attr(trimmed),
                    );
                    return;
                }
            }
            let lang = info.split_whitespace().next().unwrap_or("");
            if lang.is_empty() {
                let _ = writeln!(out, "<pre><code>{}</code></pre>", escape_html(code));
            } else {
                let _ = writeln!(
                    out,
                    "<pre><code class=\"language-{}\">{}</code></pre>",
                    escape_html_attr(lang),
                    escape_html(code),
                );
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
            let start_attr = if *ordered && *start != 1 {
                format!(" start=\"{start}\"")
            } else {
                String::new()
            };
            let dir_attr = if *direction == TextDirection::Rtl {
                " dir=\"rtl\""
            } else {
                ""
            };
            let indent_style = if *indent_level > 0 {
                format!(" style=\"margin-inline-start: {}em\"", *indent_level as f32 * 1.5)
            } else {
                String::new()
            };
            let _ = writeln!(out, "<{tag}{start_attr}{dir_attr}{indent_style}>");
            for item in items {
                let task_cls = if item.checked.is_some() {
                    " class=\"task-item\""
                } else {
                    ""
                };
                let _ = writeln!(out, "<li{task_cls}>");
                if let Some(checked) = item.checked {
                    let chk = if checked { " checked" } else { "" };
                    let _ = writeln!(
                        out,
                        "<input type=\"checkbox\" disabled{chk}> "
                    );
                }
                for b in &item.blocks {
                    render_block_html(out, b, base_dir, slug_tracker, known_slugs);
                }
                out.push_str("</li>\n");
            }
            let _ = writeln!(out, "</{tag}>");
        }
        DocBlock::Table {
            alignments,
            header,
            rows,
            ..
        } => {
            out.push_str("<div class=\"table-wrap\"><table>\n");
            if !header.is_empty() {
                out.push_str("<thead><tr>\n");
                for (c, cell) in header.iter().enumerate() {
                    let align = table_align_attr(alignments.get(c).copied().unwrap_or_default());
                    let _ = write!(out, "<th{align}>");
                    render_runs_html(out, cell, base_dir, known_slugs);
                    out.push_str("</th>");
                }
                out.push_str("\n</tr></thead>\n");
            }
            out.push_str("<tbody>\n");
            for row in rows {
                out.push_str("<tr>");
                for (c, cell) in row.iter().enumerate() {
                    let align = table_align_attr(alignments.get(c).copied().unwrap_or_default());
                    let _ = write!(out, "<td{align}>");
                    render_runs_html(out, cell, base_dir, known_slugs);
                    out.push_str("</td>");
                }
                out.push_str("</tr>\n");
            }
            out.push_str("</tbody>\n</table></div>\n");
        }
        DocBlock::ThematicBreak { .. } => {
            out.push_str("<hr>\n");
        }
        DocBlock::FootnoteDefinition { name, blocks, .. } => {
            let fn_slug = heading_slug(name);
            let id_val = if fn_slug.is_empty() {
                format!("fn-{}", name.trim().to_ascii_lowercase())
            } else {
                format!("fn-{fn_slug}")
            };
            let _ = writeln!(
                out,
                "<section class=\"footnote-def\" id=\"{}\">\n<span class=\"footnote-label\">[^{}]:</span>",
                escape_html_attr(&id_val),
                escape_html(name),
            );
            for inner in blocks {
                render_block_html(out, inner, base_dir, slug_tracker, known_slugs);
            }
            out.push_str("</section>\n");
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

fn render_runs_html(
    out: &mut String,
    runs: &[InlineRun],
    base_dir: Option<&Path>,
    known_slugs: &HashSet<String>,
) {
    for run in runs {
        if let Some(ref img_url) = run.image_url {
            if !is_unsafe_scheme(img_url) {
                let src = resolve_html_image_src(img_url, base_dir);
                let _ = write!(
                    out,
                    "<img src=\"{}\" alt=\"{}\">",
                    escape_html_attr(&src),
                    escape_html_attr(&run.text),
                );
                continue;
            }
        }

        if run.is_footnote_ref {
            let name = run.text.trim_matches(|c| matches!(c, '[' | ']' | '^'));
            let slug = heading_slug(name);
            let href = if slug.is_empty() {
                format!("#fn-{}", name.trim().to_ascii_lowercase())
            } else {
                format!("#fn-{slug}")
            };
            let _ = write!(
                out,
                "<sup><a href=\"{}\" class=\"footnote-ref\">{}</a></sup>",
                escape_html_attr(&href),
                escape_html(&run.text),
            );
            continue;
        }

        let inner_html = if run.is_math {
            let plain = math::math_to_plain_text(&run.text);
            format!(
                "<span class=\"math-inline\" title=\"{}\">{}</span>",
                escape_html_attr(&run.text),
                escape_html(&plain)
            )
        } else {
            let escaped = escape_html(&run.text);
            if run.code {
                format!("<code>{escaped}</code>")
            } else {
                escaped
            }
        };

        let mut wrapped = inner_html;
        if run.bold {
            wrapped = format!("<strong>{wrapped}</strong>");
        }
        if run.italic {
            wrapped = format!("<em>{wrapped}</em>");
        }
        if run.underline {
            wrapped = format!("<u>{wrapped}</u>");
        }
        if run.strikethrough {
            wrapped = format!("<del>{wrapped}</del>");
        }

        if let Some(ref raw_url) = run.link_url {
            if let Some((href, external)) = classify_html_href(raw_url, known_slugs) {
                if external {
                    let _ = write!(
                        out,
                        "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{wrapped}</a>",
                        escape_html_attr(&href),
                    );
                } else {
                    let _ = write!(
                        out,
                        "<a href=\"{}\">{wrapped}</a>",
                        escape_html_attr(&href),
                    );
                }
                continue;
            }
        }

        out.push_str(&wrapped);
    }
}

fn is_unsafe_scheme(url: &str) -> bool {
    let trimmed = url.trim().trim_start_matches('<').trim_end_matches('>').trim();
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("javascript:") || lower.starts_with("vbscript:") || lower.starts_with("data:text/html")
}

fn classify_html_href(raw_url: &str, known_slugs: &HashSet<String>) -> Option<(String, bool)> {
    let trimmed = raw_url
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
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

    // Reject unknown explicit schemes (`data:`, `custom:`, etc.) while allowing relative links.
    if let Some(colon) = trimmed.find(':') {
        let before = &trimmed[..colon];
        if !before.contains('/') && !before.contains('\\') && !before.contains('.') {
            return None;
        }
    }

    Some((trimmed.to_string(), false))
}

fn resolve_html_image_src(raw_url: &str, base_dir: Option<&Path>) -> String {
    let trimmed = raw_url
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("data:") {
        return trimmed.to_string();
    }

    let decoded = trimmed.replace("%20", " ");
    let resolve_candidate = |candidate: &str| -> PathBuf {
        let raw_path = Path::new(candidate);
        if raw_path.is_absolute() || raw_path.exists() {
            return raw_path.to_path_buf();
        }
        if let Some(base) = base_dir {
            let joined = base.join(raw_path);
            if joined.exists() {
                return joined;
            }
        }
        raw_path.to_path_buf()
    };

    let resolved = {
        let primary = resolve_candidate(trimmed);
        if primary.exists() || decoded == trimmed {
            primary
        } else {
            resolve_candidate(&decoded)
        }
    };

    if let Ok(bytes) = fs::read(&resolved) {
        let mime = detect_image_mime(&resolved, &bytes);
        let b64 = encode_base64(&bytes);
        return format!("data:{mime};base64,{b64}");
    }

    trimmed.to_string()
}

fn detect_image_mime(path: &Path, bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "image/png";
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    if bytes.starts_with(b"GIF8") {
        return "image/gif";
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("svg") => "image/svg+xml",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("tif") | Some("tiff") => "image/tiff",
        _ => "image/png",
    }
}

pub(crate) fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_html_attr(text: &str) -> String {
    escape_html(text)
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
