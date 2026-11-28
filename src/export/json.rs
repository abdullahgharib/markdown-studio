use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use ropey::Rope;

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

#[derive(Clone, Copy)]
struct DocStats {
    word_count: usize,
    char_count: usize,
    line_count: usize,
}

fn stats_from_str(s: &str) -> DocStats {
    DocStats {
        word_count: s.split_whitespace().count(),
        char_count: s.chars().count(),
        line_count: if s.is_empty() {
            0
        } else {
            s.bytes().filter(|&b| b == b'\n').count() + 1
        },
    }
}

fn stats_from_rope(rope: &Rope) -> DocStats {
    let mut word_count = 0usize;
    let mut in_word = false;
    let mut newline_count = 0usize;
    for chunk in rope.chunks() {
        for ch in chunk.chars() {
            if ch == '\n' {
                newline_count += 1;
            }
            if ch.is_whitespace() {
                in_word = false;
            } else if !in_word {
                in_word = true;
                word_count += 1;
            }
        }
    }
    let char_count = rope.len_chars();
    let line_count = if char_count == 0 {
        0
    } else {
        newline_count + 1
    };
    DocStats {
        word_count,
        char_count,
        line_count,
    }
}

/// Renders a Markdown document's AST, outline, links, and statistics into a deterministic,
/// pretty-printed JSON string without external serialization dependencies.
pub fn render_json_string(
    blocks: &[DocBlock],
    title: &str,
    source_path: Option<&Path>,
    raw_markdown: &str,
) -> String {
    let mut buf = Vec::with_capacity(blocks.len() * 256 + 1024);
    let _ = write_json_stream(
        &mut buf,
        blocks,
        title,
        source_path,
        stats_from_str(raw_markdown),
        |_| {},
    );
    String::from_utf8(buf).unwrap_or_default()
}

/// Exports a structured JSON representation of the document by streaming directly to `path`.
pub fn export_json_to_file(
    blocks: &[DocBlock],
    title: &str,
    source_path: Option<&Path>,
    raw_markdown: &str,
    path: impl AsRef<Path>,
    on_progress: impl FnMut(f32),
) -> Result<(), String> {
    let file = File::create(path.as_ref()).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    write_json_stream(
        &mut writer,
        blocks,
        title,
        source_path,
        stats_from_str(raw_markdown),
        on_progress,
    )
    .map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

/// Exports structured JSON by streaming directly to `path` and computing stats straight from `rope`.
pub fn export_json_to_file_from_rope(
    blocks: &[DocBlock],
    title: &str,
    source_path: Option<&Path>,
    rope: &Rope,
    path: impl AsRef<Path>,
    on_progress: impl FnMut(f32),
) -> Result<(), String> {
    let file = File::create(path.as_ref()).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    write_json_stream(
        &mut writer,
        blocks,
        title,
        source_path,
        stats_from_rope(rope),
        on_progress,
    )
    .map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

fn write_json_stream<W: Write>(
    w: &mut W,
    blocks: &[DocBlock],
    title: &str,
    source_path: Option<&Path>,
    stats: DocStats,
    mut on_progress: impl FnMut(f32),
) -> io::Result<()> {
    let mut outline_slugs = SlugTracker::new();
    let mut outline = Vec::new();
    let mut links = Vec::new();
    collect_metadata(blocks, &mut outline_slugs, &mut outline, &mut links);

    w.write_all(b"{\n")?;
    write_kv_str(w, 1, "format", "markdown-studio-document", true)?;
    write_kv_str(w, 1, "version", crate::APP_VERSION, true)?;
    write_kv_str(w, 1, "title", title, true)?;
    match source_path {
        Some(p) => write_kv_str(w, 1, "source_path", &p.to_string_lossy(), true)?,
        None => {
            write_indent(w, 1)?;
            w.write_all(b"\"source_path\": null,\n")?;
        }
    }

    // Stats object
    write_indent(w, 1)?;
    w.write_all(b"\"stats\": {\n")?;
    write_kv_usize(w, 2, "word_count", stats.word_count, true)?;
    write_kv_usize(w, 2, "line_count", stats.line_count, true)?;
    write_kv_usize(w, 2, "char_count", stats.char_count, true)?;
    write_kv_usize(w, 2, "heading_count", outline.len(), true)?;
    write_kv_usize(w, 2, "link_count", links.len(), true)?;
    write_kv_usize(w, 2, "block_count", blocks.len(), false)?;
    write_indent(w, 1)?;
    w.write_all(b"},\n")?;

    // Outline array
    write_indent(w, 1)?;
    if outline.is_empty() {
        w.write_all(b"\"outline\": [],\n")?;
    } else {
        w.write_all(b"\"outline\": [\n")?;
        for (i, item) in outline.iter().enumerate() {
            write_indent(w, 2)?;
            w.write_all(b"{\n")?;
            write_kv_usize(w, 3, "level", item.level as usize, true)?;
            write_kv_str(w, 3, "title", &item.title, true)?;
            write_kv_str(w, 3, "slug", &item.slug, true)?;
            write_kv_usize(w, 3, "source_line", item.source_line, false)?;
            write_indent(w, 2)?;
            w.write_all(if i + 1 < outline.len() { b"},\n" } else { b"}\n" })?;
        }
        write_indent(w, 1)?;
        w.write_all(b"],\n")?;
    }

    // Links array
    write_indent(w, 1)?;
    if links.is_empty() {
        w.write_all(b"\"links\": [],\n")?;
    } else {
        w.write_all(b"\"links\": [\n")?;
        for (i, link) in links.iter().enumerate() {
            write_indent(w, 2)?;
            w.write_all(b"{\n")?;
            write_kv_str(w, 3, "text", &link.text, true)?;
            write_kv_str(w, 3, "url", &link.url, true)?;
            write_kv_str(w, 3, "kind", link.kind, true)?;
            write_kv_opt_str(w, 3, "target_slug", link.target_slug.as_deref(), true)?;
            write_kv_usize(w, 3, "source_line", link.source_line, false)?;
            write_indent(w, 2)?;
            w.write_all(if i + 1 < links.len() { b"},\n" } else { b"}\n" })?;
        }
        write_indent(w, 1)?;
        w.write_all(b"],\n")?;
    }

    // Blocks array
    let mut render_slugs = SlugTracker::new();
    write_indent(w, 1)?;
    if blocks.is_empty() {
        on_progress(1.0);
        w.write_all(b"\"blocks\": []\n")?;
    } else {
        w.write_all(b"\"blocks\": [\n")?;
        let total = blocks.len();
        for (i, block) in blocks.iter().enumerate() {
            write_block_json(w, 2, block, &mut render_slugs, i + 1 < total)?;
            on_progress((i + 1) as f32 / total as f32);
        }
        write_indent(w, 1)?;
        w.write_all(b"]\n")?;
    }

    w.write_all(b"}\n")
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
            DocBlock::CodeBlock { .. }
            | DocBlock::ThematicBreak { .. }
            | DocBlock::BlankLines { .. } => {}
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
                    if slug.is_empty() { None } else { Some(slug) },
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

fn write_block_json<W: Write>(
    w: &mut W,
    indent: usize,
    block: &DocBlock,
    slug_tracker: &mut SlugTracker,
    trailing_comma: bool,
) -> io::Result<()> {
    write_indent(w, indent)?;
    w.write_all(b"{\n")?;
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
            write_kv_str(w, c, "type", "heading", true)?;
            write_kv_usize(w, c, "level", *level as usize, true)?;
            write_kv_str(w, c, "slug", &slug, true)?;
            write_kv_str(w, c, "alignment", alignment_str(*alignment), true)?;
            write_kv_usize(w, c, "indent_level", *indent_level as usize, true)?;
            write_kv_str(w, c, "direction", direction_str(*direction), true)?;
            write_source_lines(w, c, source_lines.start, source_lines.end, true)?;
            write_kv_str(w, c, "text", &plain, true)?;
            write_runs_field(w, c, "runs", runs, false)?;
        }
        DocBlock::Paragraph {
            runs,
            alignment,
            indent_level,
            direction,
            source_lines,
        } => {
            let plain = runs_to_plain_text(runs);
            write_kv_str(w, c, "type", "paragraph", true)?;
            write_kv_str(w, c, "alignment", alignment_str(*alignment), true)?;
            write_kv_usize(w, c, "indent_level", *indent_level as usize, true)?;
            write_kv_str(w, c, "direction", direction_str(*direction), true)?;
            write_source_lines(w, c, source_lines.start, source_lines.end, true)?;
            write_kv_str(w, c, "text", &plain, true)?;
            write_runs_field(w, c, "runs", runs, false)?;
        }
        DocBlock::BlockQuote {
            blocks,
            indent_level,
            direction,
            source_lines,
        } => {
            write_kv_str(w, c, "type", "blockquote", true)?;
            write_kv_usize(w, c, "indent_level", *indent_level as usize, true)?;
            write_kv_str(w, c, "direction", direction_str(*direction), true)?;
            write_source_lines(w, c, source_lines.start, source_lines.end, true)?;
            write_blocks_field(w, c, "blocks", blocks, slug_tracker, false)?;
        }
        DocBlock::CodeBlock {
            info,
            code,
            source_lines,
        } => {
            write_kv_str(w, c, "type", "code_block", true)?;
            write_kv_str(w, c, "info", info, true)?;
            write_kv_str(w, c, "code", code, true)?;
            write_source_lines(w, c, source_lines.start, source_lines.end, false)?;
        }
        DocBlock::List {
            ordered,
            start,
            items,
            indent_level,
            direction,
            source_lines,
        } => {
            write_kv_str(w, c, "type", "list", true)?;
            write_kv_bool(w, c, "ordered", *ordered, true)?;
            write_kv_usize(w, c, "start", *start, true)?;
            write_kv_usize(w, c, "indent_level", *indent_level as usize, true)?;
            write_kv_str(w, c, "direction", direction_str(*direction), true)?;
            write_source_lines(w, c, source_lines.start, source_lines.end, true)?;
            write_indent(w, c)?;
            if items.is_empty() {
                w.write_all(b"\"items\": []\n")?;
            } else {
                w.write_all(b"\"items\": [\n")?;
                for (i, item) in items.iter().enumerate() {
                    write_indent(w, c + 1)?;
                    w.write_all(b"{\n")?;
                    write_indent(w, c + 2)?;
                    match item.checked {
                        Some(b) => writeln!(w, "\"checked\": {b},")?,
                        None => w.write_all(b"\"checked\": null,\n")?,
                    }
                    write_blocks_field(w, c + 2, "blocks", &item.blocks, slug_tracker, false)?;
                    write_indent(w, c + 1)?;
                    w.write_all(if i + 1 < items.len() { b"},\n" } else { b"}\n" })?;
                }
                write_indent(w, c)?;
                w.write_all(b"]\n")?;
            }
        }
        DocBlock::Table {
            alignments,
            header,
            rows,
            source_lines,
        } => {
            write_kv_str(w, c, "type", "table", true)?;
            write_indent(w, c)?;
            w.write_all(b"\"alignments\": [")?;
            for (i, a) in alignments.iter().enumerate() {
                if i > 0 {
                    w.write_all(b", ")?;
                }
                write!(w, "\"{}\"", table_align_str(*a))?;
            }
            w.write_all(b"],\n")?;
            write_source_lines(w, c, source_lines.start, source_lines.end, true)?;
            write_indent(w, c)?;
            if header.is_empty() {
                w.write_all(b"\"header\": [],\n")?;
            } else {
                w.write_all(b"\"header\": [\n")?;
                for (i, cell) in header.iter().enumerate() {
                    write_runs_array_item(w, c + 1, cell, i + 1 < header.len())?;
                }
                write_indent(w, c)?;
                w.write_all(b"],\n")?;
            }
            write_indent(w, c)?;
            if rows.is_empty() {
                w.write_all(b"\"rows\": []\n")?;
            } else {
                w.write_all(b"\"rows\": [\n")?;
                for (r_idx, row) in rows.iter().enumerate() {
                    write_indent(w, c + 1)?;
                    if row.is_empty() {
                        w.write_all(b"[]")?;
                    } else {
                        w.write_all(b"[\n")?;
                        for (c_idx, cell) in row.iter().enumerate() {
                            write_runs_array_item(w, c + 2, cell, c_idx + 1 < row.len())?;
                        }
                        write_indent(w, c + 1)?;
                        w.write_all(b"]")?;
                    }
                    w.write_all(if r_idx + 1 < rows.len() { b",\n" } else { b"\n" })?;
                }
                write_indent(w, c)?;
                w.write_all(b"]\n")?;
            }
        }
        DocBlock::ThematicBreak { source_lines } => {
            write_kv_str(w, c, "type", "thematic_break", true)?;
            write_source_lines(w, c, source_lines.start, source_lines.end, false)?;
        }
        DocBlock::BlankLines {
            count,
            source_lines,
        } => {
            write_kv_str(w, c, "type", "blank_lines", true)?;
            write_indent(w, c)?;
            writeln!(w, "\"count\": {count},")?;
            write_source_lines(w, c, source_lines.start, source_lines.end, false)?;
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
            write_kv_str(w, c, "type", "footnote_definition", true)?;
            write_kv_str(w, c, "name", name, true)?;
            write_kv_str(w, c, "slug", &slug, true)?;
            write_source_lines(w, c, source_lines.start, source_lines.end, true)?;
            write_blocks_field(w, c, "blocks", blocks, slug_tracker, false)?;
        }
    }

    write_indent(w, indent)?;
    w.write_all(if trailing_comma { b"},\n" } else { b"}\n" })
}

fn write_blocks_field<W: Write>(
    w: &mut W,
    indent: usize,
    key: &str,
    blocks: &[DocBlock],
    slug_tracker: &mut SlugTracker,
    trailing_comma: bool,
) -> io::Result<()> {
    write_indent(w, indent)?;
    let comma = if trailing_comma { "," } else { "" };
    if blocks.is_empty() {
        return writeln!(w, "\"{key}\": []{comma}");
    }
    writeln!(w, "\"{key}\": [")?;
    for (i, b) in blocks.iter().enumerate() {
        write_block_json(w, indent + 1, b, slug_tracker, i + 1 < blocks.len())?;
    }
    write_indent(w, indent)?;
    writeln!(w, "]{comma}")
}

fn write_runs_field<W: Write>(
    w: &mut W,
    indent: usize,
    key: &str,
    runs: &[InlineRun],
    trailing_comma: bool,
) -> io::Result<()> {
    write_indent(w, indent)?;
    let comma = if trailing_comma { "," } else { "" };
    if runs.is_empty() {
        return writeln!(w, "\"{key}\": []{comma}");
    }
    writeln!(w, "\"{key}\": [")?;
    for (i, run) in runs.iter().enumerate() {
        write_run_obj(w, indent + 1, run, i + 1 < runs.len())?;
    }
    write_indent(w, indent)?;
    writeln!(w, "]{comma}")
}

fn write_runs_array_item<W: Write>(
    w: &mut W,
    indent: usize,
    runs: &[InlineRun],
    trailing_comma: bool,
) -> io::Result<()> {
    write_indent(w, indent)?;
    let comma = if trailing_comma { "," } else { "" };
    if runs.is_empty() {
        return writeln!(w, "[]{comma}");
    }
    w.write_all(b"[\n")?;
    for (i, run) in runs.iter().enumerate() {
        write_run_obj(w, indent + 1, run, i + 1 < runs.len())?;
    }
    write_indent(w, indent)?;
    writeln!(w, "]{comma}")
}

fn write_run_obj<W: Write>(
    w: &mut W,
    indent: usize,
    run: &InlineRun,
    trailing_comma: bool,
) -> io::Result<()> {
    write_indent(w, indent)?;
    w.write_all(b"{\n")?;
    let c = indent + 1;
    write_kv_str(w, c, "text", &run.text, true)?;
    write_kv_bool(w, c, "bold", run.bold, true)?;
    write_kv_bool(w, c, "italic", run.italic, true)?;
    write_kv_bool(w, c, "underline", run.underline, true)?;
    write_kv_bool(w, c, "strikethrough", run.strikethrough, true)?;
    write_kv_bool(w, c, "code", run.code, true)?;
    write_kv_bool(w, c, "is_math", run.is_math, true)?;
    write_kv_bool(w, c, "is_footnote_ref", run.is_footnote_ref, true)?;
    write_kv_opt_str(w, c, "link_url", run.link_url.as_deref(), true)?;
    write_kv_opt_str(w, c, "image_url", run.image_url.as_deref(), false)?;
    write_indent(w, indent)?;
    w.write_all(if trailing_comma { b"},\n" } else { b"}\n" })
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

fn write_indent<W: Write>(w: &mut W, indent: usize) -> io::Result<()> {
    const SPACES: &[u8] = b"                ";
    let mut remaining = indent * 2;
    while remaining > 0 {
        let take = remaining.min(SPACES.len());
        w.write_all(&SPACES[..take])?;
        remaining -= take;
    }
    Ok(())
}

fn write_kv_str<W: Write>(
    w: &mut W,
    indent: usize,
    key: &str,
    val: &str,
    comma: bool,
) -> io::Result<()> {
    write_indent(w, indent)?;
    w.write_all(b"\"")?;
    w.write_all(key.as_bytes())?;
    w.write_all(b"\": \"")?;
    write_escaped_json_str(w, val)?;
    w.write_all(if comma { b"\",\n" } else { b"\"\n" })
}

fn write_kv_opt_str<W: Write>(
    w: &mut W,
    indent: usize,
    key: &str,
    val: Option<&str>,
    comma: bool,
) -> io::Result<()> {
    write_indent(w, indent)?;
    match val {
        Some(v) => {
            w.write_all(b"\"")?;
            w.write_all(key.as_bytes())?;
            w.write_all(b"\": \"")?;
            write_escaped_json_str(w, v)?;
            w.write_all(if comma { b"\",\n" } else { b"\"\n" })
        }
        None => {
            write!(w, "\"{key}\": null{}", if comma { ",\n" } else { "\n" })
        }
    }
}

fn write_kv_usize<W: Write>(
    w: &mut W,
    indent: usize,
    key: &str,
    val: usize,
    comma: bool,
) -> io::Result<()> {
    write_indent(w, indent)?;
    writeln!(w, "\"{key}\": {val}{}", if comma { "," } else { "" })
}

fn write_kv_bool<W: Write>(
    w: &mut W,
    indent: usize,
    key: &str,
    val: bool,
    comma: bool,
) -> io::Result<()> {
    write_indent(w, indent)?;
    writeln!(w, "\"{key}\": {val}{}", if comma { "," } else { "" })
}

fn write_source_lines<W: Write>(
    w: &mut W,
    indent: usize,
    start: usize,
    end: usize,
    comma: bool,
) -> io::Result<()> {
    write_indent(w, indent)?;
    writeln!(
        w,
        "\"source_lines\": [{start}, {end}]{}",
        if comma { "," } else { "" }
    )
}

fn write_escaped_json_str<W: Write>(w: &mut W, s: &str) -> io::Result<()> {
    let bytes = s.as_bytes();
    let mut last = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        let esc: Option<&[u8]> = match b {
            b'"' => Some(b"\\\""),
            b'\\' => Some(b"\\\\"),
            b'\n' => Some(b"\\n"),
            b'\r' => Some(b"\\r"),
            b'\t' => Some(b"\\t"),
            0x08 => Some(b"\\b"),
            0x0C => Some(b"\\f"),
            0x00..=0x1F => {
                if i > last {
                    w.write_all(&bytes[last..i])?;
                }
                write!(w, "\\u{:04x}", b as u32)?;
                last = i + 1;
                None
            }
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
