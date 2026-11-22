use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::document::unicode::{BlockAlignment, TextDirection};
use crate::export::ir::{
    heading_slug, runs_to_plain_text, DocBlock, InlineRun, SlugTracker, TableAlign,
};

struct JsonOutlineEntry {
    level: u8,
    title: String,
    slug: String,
    source_line: usize,
}

struct JsonLinkEntry {
    text: String,
    url: String,
    kind: &'static str,
    target_slug: Option<String>,
    source_line: usize,
}

/// Renders a Markdown document's AST, outline, links, and statistics into a deterministic,
/// pretty-printed JSON string without external serialization dependencies.
pub fn render_json_string(
    blocks: &[DocBlock],
    title: &str,
    source_path: Option<&Path>,
    raw_markdown: &str,
) -> String {
    render_json_internal(blocks, title, source_path, raw_markdown, |_| {})
}

/// Exports a structured JSON representation of the document to `path`.
pub fn export_json_to_file(
    blocks: &[DocBlock],
    title: &str,
    source_path: Option<&Path>,
    raw_markdown: &str,
    path: impl AsRef<Path>,
    on_progress: impl FnMut(f32),
) -> Result<(), String> {
    let json = render_json_internal(blocks, title, source_path, raw_markdown, on_progress);
    let file = File::create(path.as_ref()).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(json.as_bytes())
        .map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn render_json_internal(
    blocks: &[DocBlock],
    title: &str,
    source_path: Option<&Path>,
    raw_markdown: &str,
    mut on_progress: impl FnMut(f32),
) -> String {
    let mut outline_slugs = SlugTracker::new();
    let mut outline = Vec::new();
    let mut links = Vec::new();
    collect_metadata(blocks, &mut outline_slugs, &mut outline, &mut links);

    let word_count = raw_markdown.split_whitespace().count();
    let char_count = raw_markdown.chars().count();
    let line_count = if raw_markdown.is_empty() {
        0
    } else {
        raw_markdown.bytes().filter(|&b| b == b'\n').count() + 1
    };

    let mut out = String::with_capacity(blocks.len() * 256 + 1024);
    out.push_str("{\n");
    push_kv_str(&mut out, 1, "format", "markdown-studio-document", true);
    push_kv_str(&mut out, 1, "version", crate::APP_VERSION, true);
    push_kv_str(&mut out, 1, "title", title, true);
    match source_path {
        Some(p) => push_kv_str(&mut out, 1, "source_path", &p.to_string_lossy(), true),
        None => {
            push_indent(&mut out, 1);
            out.push_str("\"source_path\": null,\n");
        }
    }

    // Stats object
    push_indent(&mut out, 1);
    out.push_str("\"stats\": {\n");
    push_kv_usize(&mut out, 2, "word_count", word_count, true);
    push_kv_usize(&mut out, 2, "line_count", line_count, true);
    push_kv_usize(&mut out, 2, "char_count", char_count, true);
    push_kv_usize(&mut out, 2, "heading_count", outline.len(), true);
    push_kv_usize(&mut out, 2, "link_count", links.len(), true);
    push_kv_usize(&mut out, 2, "block_count", blocks.len(), false);
    push_indent(&mut out, 1);
    out.push_str("},\n");

    // Outline array
    push_indent(&mut out, 1);
    if outline.is_empty() {
        out.push_str("\"outline\": [],\n");
    } else {
        out.push_str("\"outline\": [\n");
        for (i, item) in outline.iter().enumerate() {
            push_indent(&mut out, 2);
            out.push_str("{\n");
            push_kv_usize(&mut out, 3, "level", item.level as usize, true);
            push_kv_str(&mut out, 3, "title", &item.title, true);
            push_kv_str(&mut out, 3, "slug", &item.slug, true);
            push_kv_usize(&mut out, 3, "source_line", item.source_line, false);
            push_indent(&mut out, 2);
            if i + 1 < outline.len() {
                out.push_str("},\n");
            } else {
                out.push_str("}\n");
            }
        }
        push_indent(&mut out, 1);
        out.push_str("],\n");
    }

    // Links array
    push_indent(&mut out, 1);
    if links.is_empty() {
        out.push_str("\"links\": [],\n");
    } else {
        out.push_str("\"links\": [\n");
        for (i, link) in links.iter().enumerate() {
            push_indent(&mut out, 2);
            out.push_str("{\n");
            push_kv_str(&mut out, 3, "text", &link.text, true);
            push_kv_str(&mut out, 3, "url", &link.url, true);
            push_kv_str(&mut out, 3, "kind", link.kind, true);
            push_indent(&mut out, 3);
            match &link.target_slug {
                Some(slug) => {
                    let _ = writeln!(out, "\"target_slug\": \"{}\",", escape_json_str(slug));
                }
                None => out.push_str("\"target_slug\": null,\n"),
            }
            push_kv_usize(&mut out, 3, "source_line", link.source_line, false);
            push_indent(&mut out, 2);
            if i + 1 < links.len() {
                out.push_str("},\n");
            } else {
                out.push_str("}\n");
            }
        }
        push_indent(&mut out, 1);
        out.push_str("],\n");
    }

    // Blocks array
    let mut render_slugs = SlugTracker::new();
    push_indent(&mut out, 1);
    if blocks.is_empty() {
        on_progress(1.0);
        out.push_str("\"blocks\": []\n");
    } else {
        out.push_str("\"blocks\": [\n");
        let total = blocks.len();
        for (i, block) in blocks.iter().enumerate() {
            write_block_json(&mut out, 2, block, &mut render_slugs, i + 1 < total);
            on_progress((i + 1) as f32 / total as f32);
        }
        push_indent(&mut out, 1);
        out.push_str("]\n");
    }

    out.push_str("}\n");
    out
}

fn collect_metadata(
    blocks: &[DocBlock],
    slug_tracker: &mut SlugTracker,
    outline: &mut Vec<JsonOutlineEntry>,
    links: &mut Vec<JsonLinkEntry>,
) {
    for block in blocks {
        let line = block.source_lines().start;
        match block {
            DocBlock::Heading {
                level,
                runs,
                source_lines,
                ..
            } => {
                let raw = runs_to_plain_text(runs);
                let title = raw.trim();
                if !title.is_empty() && *level <= 6 {
                    let slug = slug_tracker.next_slug(title);
                    outline.push(JsonOutlineEntry {
                        level: *level,
                        title: title.to_string(),
                        slug,
                        source_line: source_lines.start,
                    });
                }
                collect_links_from_runs(runs, line, links);
            }
            DocBlock::Paragraph { runs, .. } => {
                collect_links_from_runs(runs, line, links);
            }
            DocBlock::Table { header, rows, .. } => {
                for cell in header {
                    collect_links_from_runs(cell, line, links);
                }
                for row in rows {
                    for cell in row {
                        collect_links_from_runs(cell, line, links);
                    }
                }
            }
            DocBlock::BlockQuote { blocks, .. } | DocBlock::FootnoteDefinition { blocks, .. } => {
                collect_metadata(blocks, slug_tracker, outline, links);
            }
            DocBlock::List { items, .. } => {
                for item in items {
                    collect_metadata(&item.blocks, slug_tracker, outline, links);
                }
            }
            DocBlock::CodeBlock { .. } | DocBlock::ThematicBreak { .. } => {}
        }
    }
}

fn collect_links_from_runs(runs: &[InlineRun], source_line: usize, links: &mut Vec<JsonLinkEntry>) {
    for run in runs {
        if run.is_footnote_ref {
            let name = run.text.trim_matches(|c| matches!(c, '[' | ']' | '^'));
            let slug = heading_slug(name);
            let target_slug = if slug.is_empty() {
                format!("fn-{}", name.trim().to_ascii_lowercase())
            } else {
                format!("fn-{slug}")
            };
            links.push(JsonLinkEntry {
                text: run.text.clone(),
                url: format!("#{target_slug}"),
                kind: "footnote",
                target_slug: Some(target_slug),
                source_line,
            });
            continue;
        }
        if let Some(ref raw_url) = run.link_url {
            let trimmed = raw_url.trim();
            let (kind, target_slug) = if let Some(anchor) = trimmed.strip_prefix('#') {
                let decoded = anchor.replace("%20", " ");
                let slug = heading_slug(&decoded);
                (
                    "anchor",
                    if slug.is_empty() {
                        None
                    } else {
                        Some(slug)
                    },
                )
            } else {
                let lower = trimmed.to_ascii_lowercase();
                if lower.starts_with("http://")
                    || lower.starts_with("https://")
                    || lower.starts_with("mailto:")
                    || lower.starts_with("www.")
                {
                    ("external", None)
                } else if trimmed.contains(':') && !trimmed.contains('/') && !trimmed.contains('\\')
                {
                    ("other", None)
                } else {
                    ("relative", None)
                }
            };
            links.push(JsonLinkEntry {
                text: run.text.clone(),
                url: trimmed.to_string(),
                kind,
                target_slug,
                source_line,
            });
        }
    }
}

fn write_block_json(
    out: &mut String,
    indent: usize,
    block: &DocBlock,
    slug_tracker: &mut SlugTracker,
    trailing_comma: bool,
) {
    push_indent(out, indent);
    out.push_str("{\n");
    let c = indent + 1;

    match block {
        DocBlock::Heading {
            level,
            runs,
            alignment,
            indent_level,
            direction,
            source_lines,
        } => {
            let plain = runs_to_plain_text(runs);
            let slug = if !plain.trim().is_empty() && *level <= 6 {
                slug_tracker.next_slug(plain.trim())
            } else {
                String::new()
            };
            push_kv_str(out, c, "type", "heading", true);
            push_kv_usize(out, c, "level", *level as usize, true);
            push_kv_str(out, c, "slug", &slug, true);
            push_kv_str(out, c, "alignment", alignment_str(*alignment), true);
            push_kv_usize(out, c, "indent_level", *indent_level as usize, true);
            push_kv_str(out, c, "direction", direction_str(*direction), true);
            push_source_lines(out, c, source_lines.start, source_lines.end, true);
            push_kv_str(out, c, "text", &plain, true);
            push_runs_field(out, c, "runs", runs, false);
        }
        DocBlock::Paragraph {
            runs,
            alignment,
            indent_level,
            direction,
            source_lines,
        } => {
            let plain = runs_to_plain_text(runs);
            push_kv_str(out, c, "type", "paragraph", true);
            push_kv_str(out, c, "alignment", alignment_str(*alignment), true);
            push_kv_usize(out, c, "indent_level", *indent_level as usize, true);
            push_kv_str(out, c, "direction", direction_str(*direction), true);
            push_source_lines(out, c, source_lines.start, source_lines.end, true);
            push_kv_str(out, c, "text", &plain, true);
            push_runs_field(out, c, "runs", runs, false);
        }
        DocBlock::BlockQuote {
            blocks,
            indent_level,
            direction,
            source_lines,
        } => {
            push_kv_str(out, c, "type", "blockquote", true);
            push_kv_usize(out, c, "indent_level", *indent_level as usize, true);
            push_kv_str(out, c, "direction", direction_str(*direction), true);
            push_source_lines(out, c, source_lines.start, source_lines.end, true);
            push_blocks_field(out, c, "blocks", blocks, slug_tracker, false);
        }
        DocBlock::CodeBlock {
            info,
            code,
            source_lines,
        } => {
            push_kv_str(out, c, "type", "code_block", true);
            push_kv_str(out, c, "info", info, true);
            push_kv_str(out, c, "code", code, true);
            push_source_lines(out, c, source_lines.start, source_lines.end, false);
        }
        DocBlock::List {
            ordered,
            start,
            items,
            indent_level,
            direction,
            source_lines,
        } => {
            push_kv_str(out, c, "type", "list", true);
            push_kv_bool(out, c, "ordered", *ordered, true);
            push_kv_usize(out, c, "start", *start, true);
            push_kv_usize(out, c, "indent_level", *indent_level as usize, true);
            push_kv_str(out, c, "direction", direction_str(*direction), true);
            push_source_lines(out, c, source_lines.start, source_lines.end, true);
            push_indent(out, c);
            if items.is_empty() {
                out.push_str("\"items\": []\n");
            } else {
                out.push_str("\"items\": [\n");
                for (i, item) in items.iter().enumerate() {
                    push_indent(out, c + 1);
                    out.push_str("{\n");
                    push_indent(out, c + 2);
                    match item.checked {
                        Some(b) => {
                            let _ = writeln!(out, "\"checked\": {b},");
                        }
                        None => out.push_str("\"checked\": null,\n"),
                    }
                    push_blocks_field(out, c + 2, "blocks", &item.blocks, slug_tracker, false);
                    push_indent(out, c + 1);
                    if i + 1 < items.len() {
                        out.push_str("},\n");
                    } else {
                        out.push_str("}\n");
                    }
                }
                push_indent(out, c);
                out.push_str("]\n");
            }
        }
        DocBlock::Table {
            alignments,
            header,
            rows,
            source_lines,
        } => {
            push_kv_str(out, c, "type", "table", true);
            push_indent(out, c);
            out.push_str("\"alignments\": [");
            for (i, a) in alignments.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "\"{}\"", table_align_str(*a));
            }
            out.push_str("],\n");
            push_source_lines(out, c, source_lines.start, source_lines.end, true);
            push_indent(out, c);
            if header.is_empty() {
                out.push_str("\"header\": [],\n");
            } else {
                out.push_str("\"header\": [\n");
                for (i, cell) in header.iter().enumerate() {
                    write_runs_array_item(out, c + 1, cell, i + 1 < header.len());
                }
                push_indent(out, c);
                out.push_str("],\n");
            }
            push_indent(out, c);
            if rows.is_empty() {
                out.push_str("\"rows\": []\n");
            } else {
                out.push_str("\"rows\": [\n");
                for (r_idx, row) in rows.iter().enumerate() {
                    push_indent(out, c + 1);
                    if row.is_empty() {
                        out.push_str("[]");
                    } else {
                        out.push_str("[\n");
                        for (c_idx, cell) in row.iter().enumerate() {
                            write_runs_array_item(out, c + 2, cell, c_idx + 1 < row.len());
                        }
                        push_indent(out, c + 1);
                        out.push(']');
                    }
                    if r_idx + 1 < rows.len() {
                        out.push_str(",\n");
                    } else {
                        out.push('\n');
                    }
                }
                push_indent(out, c);
                out.push_str("]\n");
            }
        }
        DocBlock::ThematicBreak { source_lines } => {
            push_kv_str(out, c, "type", "thematic_break", true);
            push_source_lines(out, c, source_lines.start, source_lines.end, false);
        }
        DocBlock::FootnoteDefinition {
            name,
            blocks,
            source_lines,
        } => {
            let fn_slug = heading_slug(name);
            let slug = if fn_slug.is_empty() {
                format!("fn-{}", name.trim().to_ascii_lowercase())
            } else {
                format!("fn-{fn_slug}")
            };
            push_kv_str(out, c, "type", "footnote_definition", true);
            push_kv_str(out, c, "name", name, true);
            push_kv_str(out, c, "slug", &slug, true);
            push_source_lines(out, c, source_lines.start, source_lines.end, true);
            push_blocks_field(out, c, "blocks", blocks, slug_tracker, false);
        }
    }

    push_indent(out, indent);
    if trailing_comma {
        out.push_str("},\n");
    } else {
        out.push_str("}\n");
    }
}

fn push_blocks_field(
    out: &mut String,
    indent: usize,
    key: &str,
    blocks: &[DocBlock],
    slug_tracker: &mut SlugTracker,
    trailing_comma: bool,
) {
    push_indent(out, indent);
    let comma = if trailing_comma { "," } else { "" };
    if blocks.is_empty() {
        let _ = writeln!(out, "\"{key}\": []{comma}");
        return;
    }
    let _ = writeln!(out, "\"{key}\": [");
    for (i, b) in blocks.iter().enumerate() {
        write_block_json(out, indent + 1, b, slug_tracker, i + 1 < blocks.len());
    }
    push_indent(out, indent);
    let _ = writeln!(out, "]{comma}");
}

fn push_runs_field(
    out: &mut String,
    indent: usize,
    key: &str,
    runs: &[InlineRun],
    trailing_comma: bool,
) {
    push_indent(out, indent);
    let comma = if trailing_comma { "," } else { "" };
    if runs.is_empty() {
        let _ = writeln!(out, "\"{key}\": []{comma}");
        return;
    }
    let _ = writeln!(out, "\"{key}\": [");
    for (i, run) in runs.iter().enumerate() {
        write_run_obj(out, indent + 1, run, i + 1 < runs.len());
    }
    push_indent(out, indent);
    let _ = writeln!(out, "]{comma}");
}

fn write_runs_array_item(
    out: &mut String,
    indent: usize,
    runs: &[InlineRun],
    trailing_comma: bool,
) {
    push_indent(out, indent);
    let comma = if trailing_comma { "," } else { "" };
    if runs.is_empty() {
        let _ = writeln!(out, "[]{comma}");
        return;
    }
    out.push_str("[\n");
    for (i, run) in runs.iter().enumerate() {
        write_run_obj(out, indent + 1, run, i + 1 < runs.len());
    }
    push_indent(out, indent);
    let _ = writeln!(out, "]{comma}");
}

fn write_run_obj(out: &mut String, indent: usize, run: &InlineRun, trailing_comma: bool) {
    push_indent(out, indent);
    out.push_str("{\n");
    let c = indent + 1;
    push_kv_str(out, c, "text", &run.text, true);
    push_kv_bool(out, c, "bold", run.bold, true);
    push_kv_bool(out, c, "italic", run.italic, true);
    push_kv_bool(out, c, "underline", run.underline, true);
    push_kv_bool(out, c, "strikethrough", run.strikethrough, true);
    push_kv_bool(out, c, "code", run.code, true);
    push_kv_bool(out, c, "is_math", run.is_math, true);
    push_kv_bool(out, c, "is_footnote_ref", run.is_footnote_ref, true);
    push_kv_opt_str(out, c, "link_url", run.link_url.as_deref(), true);
    push_kv_opt_str(out, c, "image_url", run.image_url.as_deref(), false);
    push_indent(out, indent);
    if trailing_comma {
        out.push_str("},\n");
    } else {
        out.push_str("}\n");
    }
}

fn alignment_str(a: BlockAlignment) -> &'static str {
    match a {
        BlockAlignment::Start => "start",
        BlockAlignment::Left => "left",
        BlockAlignment::Center => "center",
        BlockAlignment::Right => "right",
        BlockAlignment::Justify => "justify",
        BlockAlignment::End => "end",
    }
}

fn direction_str(d: TextDirection) -> &'static str {
    match d {
        TextDirection::Ltr => "ltr",
        TextDirection::Rtl => "rtl",
    }
}

fn table_align_str(a: TableAlign) -> &'static str {
    match a {
        TableAlign::None => "none",
        TableAlign::Left => "left",
        TableAlign::Center => "center",
        TableAlign::Right => "right",
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

fn push_kv_str(out: &mut String, indent: usize, key: &str, val: &str, comma: bool) {
    push_indent(out, indent);
    let c = if comma { "," } else { "" };
    let _ = writeln!(out, "\"{key}\": \"{}\"{c}", escape_json_str(val));
}

fn push_kv_opt_str(out: &mut String, indent: usize, key: &str, val: Option<&str>, comma: bool) {
    push_indent(out, indent);
    let c = if comma { "," } else { "" };
    match val {
        Some(v) => {
            let _ = writeln!(out, "\"{key}\": \"{}\"{c}", escape_json_str(v));
        }
        None => {
            let _ = writeln!(out, "\"{key}\": null{c}");
        }
    }
}

fn push_kv_usize(out: &mut String, indent: usize, key: &str, val: usize, comma: bool) {
    push_indent(out, indent);
    let c = if comma { "," } else { "" };
    let _ = writeln!(out, "\"{key}\": {val}{c}");
}

fn push_kv_bool(out: &mut String, indent: usize, key: &str, val: bool, comma: bool) {
    push_indent(out, indent);
    let c = if comma { "," } else { "" };
    let _ = writeln!(out, "\"{key}\": {val}{c}");
}

fn push_source_lines(
    out: &mut String,
    indent: usize,
    start: usize,
    end: usize,
    comma: bool,
) {
    push_indent(out, indent);
    let c = if comma { "," } else { "" };
    let _ = writeln!(out, "\"source_lines\": [{start}, {end}]{c}");
}

fn escape_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}
