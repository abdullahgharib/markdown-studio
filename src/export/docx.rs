use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Cursor, Write};
use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

use docx_rs::{
    AbstractNumbering, AlignmentType, BorderType, BreakType, Docx, Hyperlink, HyperlinkType,
    IndentLevel, Level, LevelJc, LevelOverride, LevelText, LineSpacing as DocxLineSpacing,
    NumberFormat, Numbering, NumberingId, PageMargin, Paragraph, ParagraphBorder,
    ParagraphBorderPosition, ParagraphBorders, Pic, Run, RunFonts, Shading, SpecialIndentType,
    Start, Style, StyleType, Table, TableBorder, TableBorderPosition, TableBorders, TableCell,
    TableCellMargins, TableLayoutType, TableRow, WidthType,
};

use crate::document::unicode::{BlockAlignment, TextDirection};
use crate::export::ir::{
    heading_slug, runs_to_plain_text, DocBlock, InlineRun, ListItem, SlugTracker, TableAlign,
};

struct DocxCtx {
    next_ordered_num_id: usize,
    next_bookmark_id: usize,
    slug_tracker: SlugTracker,
    bookmarks: HashMap<String, String>,
    base_dir: Option<PathBuf>,
    image_cache: FxHashMap<String, (Vec<u8>, u32, u32)>,
}

#[derive(Debug, Clone)]
enum DocxLinkTarget {
    Anchor(String),
    External(String),
}

/// Compiles `blocks` into a native OpenXML `.docx` ZIP archive in memory.
pub fn render_docx_bytes(
    blocks: &[DocBlock],
    on_progress: impl FnMut(f32),
) -> Result<Vec<u8>, String> {
    render_docx_bytes_with_base_dir(blocks, None, on_progress)
}

fn build_docx_document(
    blocks: &[DocBlock],
    base_dir: Option<&Path>,
    mut on_progress: impl FnMut(f32),
) -> Docx {
    let mut docx = Docx::new()
        .default_fonts(
            RunFonts::new()
                .ascii("Segoe UI")
                .hi_ansi("Segoe UI")
                .cs("Tahoma"),
        )
        .default_size(22)
        .default_line_spacing(DocxLineSpacing::new().line(276).after(120))
        .page_margin(
            PageMargin::new()
                .top(1440)
                .right(1440)
                .bottom(1440)
                .left(1440),
        );

    // Register native Heading1..Heading6 paragraph styles with Word outline levels.
    for level in 1..=6u8 {
        let size_hp = heading_size_half_points(level);
        let (before_twips, after_twips) = heading_spacing_twips(level);
        let style = Style::new(format!("Heading{level}"), StyleType::Paragraph)
            .name(format!("Heading {level}"))
            .bold()
            .size(size_hp)
            .color("1F3A5F")
            .outline_lvl((level - 1) as usize)
            .line_spacing(
                DocxLineSpacing::new()
                    .before(before_twips)
                    .after(after_twips)
                    .line(264),
            );
        docx = docx.add_style(style);
    }

    // Register bullet abstract/numbering (ID 1) and ordered abstract numbering (abs ID 2) across all 9 OOXML list levels.
    let mut bullet_abs = AbstractNumbering::new(1);
    let mut ordered_abs = AbstractNumbering::new(2);
    for lvl in 0..9usize {
        let left_twips = ((lvl + 1) * 720) as i32;
        bullet_abs = bullet_abs.add_level(
            Level::new(
                lvl,
                Start::new(1),
                NumberFormat::new("bullet"),
                LevelText::new("\u{2022}"),
                LevelJc::new("left"),
            )
            .indent(
                Some(left_twips),
                Some(SpecialIndentType::Hanging(360)),
                None,
                None,
            ),
        );
        ordered_abs = ordered_abs.add_level(
            Level::new(
                lvl,
                Start::new(1),
                NumberFormat::new("decimal"),
                LevelText::new(format!("%{}.", lvl + 1)),
                LevelJc::new("left"),
            )
            .indent(
                Some(left_twips),
                Some(SpecialIndentType::Hanging(360)),
                None,
                None,
            ),
        );
    }
    docx = docx
        .add_abstract_numbering(bullet_abs)
        .add_numbering(Numbering::new(1, 1))
        .add_abstract_numbering(ordered_abs)
        .add_numbering(Numbering::new(2, 2));

    let bookmarks = collect_docx_bookmarks(blocks);
    let mut ctx = DocxCtx {
        next_ordered_num_id: 3,
        next_bookmark_id: 1,
        slug_tracker: SlugTracker::new(),
        bookmarks,
        base_dir: base_dir.map(Path::to_path_buf),
        image_cache: FxHashMap::default(),
    };

    let total = blocks.len().max(1);
    for (idx, block) in blocks.iter().enumerate() {
        docx = append_block(docx, &mut ctx, block, 0, false);
        on_progress((idx + 1) as f32 / total as f32);
    }
    docx
}

/// Compiles `blocks` into a native OpenXML `.docx` ZIP archive in memory with optional relative image base directory.
pub fn render_docx_bytes_with_base_dir(
    blocks: &[DocBlock],
    base_dir: Option<&Path>,
    on_progress: impl FnMut(f32),
) -> Result<Vec<u8>, String> {
    let docx = build_docx_document(blocks, base_dir, on_progress);
    let mut cursor = Cursor::new(Vec::new());
    docx.build().pack(&mut cursor).map_err(|e| e.to_string())?;
    Ok(cursor.into_inner())
}

fn docx_bookmark_name(raw_slug: &str, used: &mut HashSet<String>) -> String {
    let mut base = String::from("md_");
    for ch in raw_slug.chars() {
        if ch.is_ascii_alphanumeric() {
            base.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' {
            base.push('_');
        }
        if base.len() >= 34 {
            break;
        }
    }
    if base.len() <= 3 {
        base.push_str("sec");
    }
    let mut candidate = base.clone();
    let mut n = 1usize;
    while !used.insert(candidate.clone()) {
        candidate = format!("{base}_{n}");
        n += 1;
    }
    candidate
}

fn collect_docx_bookmarks(blocks: &[DocBlock]) -> HashMap<String, String> {
    let mut bookmarks = HashMap::new();
    let mut slug_tracker = SlugTracker::new();
    let mut used_names = HashSet::new();
    collect_docx_bookmarks_rec(blocks, &mut slug_tracker, &mut used_names, &mut bookmarks);
    bookmarks
}

fn collect_docx_bookmarks_rec(
    blocks: &[DocBlock],
    slug_tracker: &mut SlugTracker,
    used_names: &mut HashSet<String>,
    bookmarks: &mut HashMap<String, String>,
) {
    for block in blocks {
        match block {
            DocBlock::Heading { level, runs, .. } => {
                let raw = runs_to_plain_text(runs);
                let title = raw.trim();
                if !title.is_empty() && *level <= 6 {
                    let slug = slug_tracker.next_slug(title);
                    let bm_name = docx_bookmark_name(&slug, used_names);
                    bookmarks.insert(slug, bm_name);
                }
            }
            DocBlock::FootnoteDefinition { name, blocks, .. } => {
                let fn_slug = heading_slug(name);
                let primary_key = if fn_slug.is_empty() {
                    format!("fn-{}", name.trim().to_ascii_lowercase())
                } else {
                    format!("fn-{fn_slug}")
                };
                if !primary_key.is_empty() {
                    let bm_name = docx_bookmark_name(&primary_key, used_names);
                    bookmarks.insert(primary_key, bm_name.clone());
                    if !fn_slug.is_empty() {
                        bookmarks.entry(fn_slug).or_insert_with(|| bm_name.clone());
                    }
                    let lower_fn = format!("fn-{}", name.trim().to_ascii_lowercase());
                    bookmarks.entry(lower_fn).or_insert(bm_name);
                }
                collect_docx_bookmarks_rec(blocks, slug_tracker, used_names, bookmarks);
            }
            DocBlock::BlockQuote { blocks, .. } => {
                collect_docx_bookmarks_rec(blocks, slug_tracker, used_names, bookmarks);
            }
            DocBlock::List { items, .. } => {
                for item in items {
                    collect_docx_bookmarks_rec(&item.blocks, slug_tracker, used_names, bookmarks);
                }
            }
            _ => {}
        }
    }
}

fn heading_spacing_twips(level: u8) -> (u32, u32) {
    match level {
        1 => (320, 140),
        2 => (260, 110),
        3 => (220, 80),
        _ => (180, 60),
    }
}

/// Writes a `.docx` file to `path`.
pub fn export_docx_to_file(
    blocks: &[DocBlock],
    path: impl AsRef<Path>,
    on_progress: impl FnMut(f32),
) -> Result<(), String> {
    export_docx_to_file_with_base_dir(blocks, path, None, on_progress)
}

/// Writes a `.docx` file to `path` with optional base directory for resolving relative image paths.
pub fn export_docx_to_file_with_base_dir(
    blocks: &[DocBlock],
    path: impl AsRef<Path>,
    base_dir: Option<&Path>,
    on_progress: impl FnMut(f32),
) -> Result<(), String> {
    let docx = build_docx_document(blocks, base_dir, on_progress);
    let file = File::create(path.as_ref()).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    docx.build().pack(&mut writer).map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn heading_size_half_points(level: u8) -> usize {
    match level {
        1 => 42, // 21 pt
        2 => 34, // 17 pt
        3 => 28, // 14 pt
        4 => 24, // 12 pt
        5 => 22, // 11 pt
        _ => 21, // 10.5 pt
    }
}

fn to_docx_block_align(align: BlockAlignment, dir: TextDirection) -> AlignmentType {
    match align {
        BlockAlignment::Start => {
            if dir == TextDirection::Rtl {
                AlignmentType::Right
            } else {
                AlignmentType::Left
            }
        }
        BlockAlignment::End => {
            if dir == TextDirection::Rtl {
                AlignmentType::Left
            } else {
                AlignmentType::Right
            }
        }
        BlockAlignment::Left => AlignmentType::Left,
        BlockAlignment::Center => AlignmentType::Center,
        BlockAlignment::Right => AlignmentType::Right,
        BlockAlignment::Justify => AlignmentType::Both,
    }
}

fn configure_paragraph_layout(
    mut p: Paragraph,
    align: BlockAlignment,
    indent_level: u8,
    dir: TextDirection,
    in_quote: bool,
) -> Paragraph {
    p = p.align(to_docx_block_align(align, dir));
    let base_twips = indent_level as i32 * 720;
    let quote_twips = if in_quote { 540 } else { 0 };
    let total_indent = base_twips + quote_twips;
    if total_indent > 0 {
        p = p.indent(Some(total_indent), None, None, None);
    }
    if in_quote {
        let border_pos = if dir == TextDirection::Rtl {
            ParagraphBorderPosition::Right
        } else {
            ParagraphBorderPosition::Left
        };
        p.property = p
            .property
            .shading(Shading::new().fill("F8FAFC"))
            .set_borders(
                ParagraphBorders::with_empty().set(
                    ParagraphBorder::new(border_pos)
                        .val(BorderType::Single)
                        .size(18)
                        .space(10)
                        .color("1D5FC4"),
                ),
            );
    }
    if dir == TextDirection::Rtl {
        p.property = p.property.bidi(true);
    }
    p
}

fn clean_table_borders() -> TableBorders {
    let border = |pos| {
        TableBorder::new(pos)
            .color("CBD5E1")
            .size(4)
            .border_type(BorderType::Single)
    };
    TableBorders::new()
        .set(border(TableBorderPosition::Top))
        .set(border(TableBorderPosition::Bottom))
        .set(border(TableBorderPosition::Left))
        .set(border(TableBorderPosition::Right))
        .set(border(TableBorderPosition::InsideH))
        .set(border(TableBorderPosition::InsideV))
}

fn centered_scaled_pic_paragraph(
    png_bytes: Vec<u8>,
    pixel_w: u32,
    pixel_h: u32,
    max_w: f32,
    max_h: f32,
    min_dim: f32,
) -> Paragraph {
    let logical_w = (pixel_w as f32 * 0.5).max(1.0);
    let logical_h = (pixel_h as f32 * 0.5).max(1.0);
    let scale = (max_w / logical_w).min(max_h / logical_h).min(1.0);
    let disp_w = (logical_w * scale).round().max(min_dim) as u32;
    let disp_h = (logical_h * scale).round().max(min_dim) as u32;
    let pic = Pic::new_with_dimensions(png_bytes, disp_w, disp_h);
    Paragraph::new()
        .align(AlignmentType::Center)
        .line_spacing(DocxLineSpacing::new().before(80).after(100))
        .add_run(Run::new().add_image(pic))
}

fn has_2d_math_command(latex: &str) -> bool {
    [
        "\\frac",
        "\\tfrac",
        "\\dfrac",
        "\\sqrt",
        "\\begin",
        "\\sum",
        "\\int",
        "\\binom",
        "\\over",
        "\\left",
        "\\prod",
        "\\lim",
    ]
    .iter()
    .any(|kw| latex.contains(kw))
}

fn append_block(
    mut docx: Docx,
    ctx: &mut DocxCtx,
    block: &DocBlock,
    list_depth: usize,
    in_quote: bool,
) -> Docx {
    let base_dir = ctx.base_dir.clone();
    match block {
        DocBlock::Heading {
            level,
            runs,
            alignment,
            indent_level,
            direction,
            ..
        } => {
            let eff_lvl = (*level).clamp(1, 6);
            let size_hp = heading_size_half_points(eff_lvl);
            let (before_twips, after_twips) = heading_spacing_twips(eff_lvl);
            let mut p = Paragraph::new()
                .style(&format!("Heading{eff_lvl}"))
                .outline_lvl((eff_lvl - 1) as usize)
                .keep_next(true)
                .line_spacing(
                    DocxLineSpacing::new()
                        .before(before_twips)
                        .after(after_twips)
                        .line(264),
                );
            p = configure_paragraph_layout(p, *alignment, *indent_level, *direction, in_quote);
            if eff_lvl <= 2 && !in_quote {
                p.property = p.property.set_borders(
                    ParagraphBorders::with_empty().set(
                        ParagraphBorder::new(ParagraphBorderPosition::Bottom)
                            .val(BorderType::Single)
                            .size(6)
                            .space(4)
                            .color("CBD5E1"),
                    ),
                );
            }
            let raw_title = runs_to_plain_text(runs);
            let title = raw_title.trim();
            let bm_name = if !title.is_empty() && *level <= 6 {
                let slug = ctx.slug_tracker.next_slug(title);
                ctx.bookmarks.get(&slug).cloned()
            } else {
                None
            };
            let bm_id = if let Some(ref name) = bm_name {
                let id = ctx.next_bookmark_id;
                ctx.next_bookmark_id += 1;
                p = p.add_bookmark_start(id, name);
                Some(id)
            } else {
                None
            };
            for r in runs {
                p = add_inline_run_to_paragraph(
                    p,
                    r,
                    size_hp,
                    true,
                    Some("1F3A5F"),
                    base_dir.as_deref(),
                    &ctx.bookmarks,
                    &mut ctx.image_cache,
                );
            }
            if let Some(id) = bm_id {
                p = p.add_bookmark_end(id);
            }
            docx = docx.add_paragraph(p);
        }
        DocBlock::Paragraph {
            runs,
            alignment,
            indent_level,
            direction,
            ..
        } => {
            let mut p = configure_paragraph_layout(
                Paragraph::new().line_spacing(DocxLineSpacing::new().line(276).after(120)),
                *alignment,
                *indent_level,
                *direction,
                in_quote,
            );
            let color = if in_quote { Some("4A5568") } else { None };
            for r in runs {
                let mut adjusted = r.clone();
                if in_quote && adjusted.image_url.is_none() {
                    adjusted.italic = true;
                }
                p = add_inline_run_to_paragraph(
                    p,
                    &adjusted,
                    22,
                    false,
                    color,
                    base_dir.as_deref(),
                    &ctx.bookmarks,
                    &mut ctx.image_cache,
                );
            }
            docx = docx.add_paragraph(p);
        }
        DocBlock::BlockQuote { blocks, .. } => {
            for inner in blocks {
                docx = append_block(docx, ctx, inner, list_depth, true);
            }
        }
        DocBlock::CodeBlock { info, code, .. } => {
            if crate::document::ast::is_mermaid_code_info(info) && !code.trim().is_empty()
                && let Some((png_bytes, pw, ph)) =
                    crate::document::mermaid::render_mermaid_to_png_bytes(code, 2.0)
                {
                    let p = centered_scaled_pic_paragraph(png_bytes, pw, ph, 560.0, 420.0, 24.0);
                    return docx.add_paragraph(p);
                }
            if crate::document::ast::is_math_code_info(info) {
                let trimmed = code.trim();
                if trimmed.is_empty() {
                    return docx.add_paragraph(Paragraph::new());
                }
                if let Some((png_bytes, pw, ph)) = crate::document::math::render_math_to_png_bytes(
                    trimmed,
                    18.0,
                    true,
                    [20, 24, 32, 255],
                    [255, 255, 255, 255],
                ) {
                    let p = centered_scaled_pic_paragraph(png_bytes, pw, ph, 540.0, 320.0, 12.0);
                    return docx.add_paragraph(p);
                }
            }

            let mut p = Paragraph::new().line_spacing(DocxLineSpacing::new().line(240).after(0));
            let mono_fonts = RunFonts::new()
                .ascii("Consolas")
                .hi_ansi("Consolas")
                .cs("Consolas");
            if !info.is_empty() {
                p = p.add_run(
                    Run::new()
                        .add_text(info.to_uppercase())
                        .bold()
                        .size(15)
                        .color("5A6577")
                        .fonts(mono_fonts.clone()),
                )
                .add_run(Run::new().add_break(BreakType::TextWrapping));
            }
            let mut lines = code.lines().peekable();
            if lines.peek().is_none() {
                p = p.add_run(
                    Run::new()
                        .add_text(" ")
                        .size(18)
                        .fonts(mono_fonts.clone()),
                );
            }
            while let Some(line) = lines.next() {
                p = p.add_run(
                    Run::new()
                        .add_text(line)
                        .size(18)
                        .color("1F242D")
                        .fonts(mono_fonts.clone()),
                );
                if lines.peek().is_some() {
                    p = p.add_run(Run::new().add_break(BreakType::TextWrapping));
                }
            }
            let cell = TableCell::new()
                .shading(Shading::new().fill("F4F6F9"))
                .width(9360, WidthType::Dxa)
                .add_paragraph(p);
            let tbl = Table::new(vec![TableRow::new(vec![cell])])
                .layout(TableLayoutType::Fixed)
                .set_borders(clean_table_borders())
                .margins(TableCellMargins::new().margin(120, 160, 120, 160));
            docx = docx
                .add_table(tbl)
                .add_paragraph(Paragraph::new().line_spacing(DocxLineSpacing::new().line(160).after(40)));
        }
        DocBlock::List {
            ordered,
            start,
            items,
            indent_level,
            ..
        } => {
            let num_id = if *ordered {
                let id = ctx.next_ordered_num_id;
                ctx.next_ordered_num_id += 1;
                let lvl = list_depth.min(8);
                let num = Numbering::new(id, 2).add_override(LevelOverride::new(lvl).start(*start));
                docx = docx.add_numbering(num);
                id
            } else {
                1
            };
            for item in items {
                docx = append_list_item(docx, ctx, *ordered, num_id, item, list_depth, *indent_level);
            }
        }
        DocBlock::Table {
            alignments,
            header,
            rows,
            ..
        } => {
            let tbl = build_docx_table(
                alignments,
                header,
                rows,
                base_dir.as_deref(),
                &ctx.bookmarks,
                &mut ctx.image_cache,
            );
            docx = docx
                .add_table(tbl)
                .add_paragraph(Paragraph::new().line_spacing(DocxLineSpacing::new().line(160).after(40)));
        }
        DocBlock::ThematicBreak { .. } => {
            let mut p = Paragraph::new().line_spacing(DocxLineSpacing::new().before(120).after(120));
            p.property = p.property.set_borders(
                ParagraphBorders::with_empty().set(
                    ParagraphBorder::new(ParagraphBorderPosition::Bottom)
                        .val(BorderType::Single)
                        .size(6)
                        .space(1)
                        .color("CBD5E1"),
                ),
            );
            docx = docx.add_paragraph(p);
        }
        DocBlock::BlankLines { count, .. } => {
            for _ in 0..*count {
                docx = docx.add_paragraph(
                    Paragraph::new().line_spacing(DocxLineSpacing::new().line(240).before(0).after(0)),
                );
            }
        }
        DocBlock::FootnoteDefinition { name, blocks, .. } => {
            let fn_slug = heading_slug(name);
            let key = if fn_slug.is_empty() {
                format!("fn-{}", name.trim().to_ascii_lowercase())
            } else {
                format!("fn-{fn_slug}")
            };
            let bm_name = ctx.bookmarks.get(&key).cloned();
            let mut label_p = Paragraph::new()
                .indent(Some(360), None, None, None)
                .line_spacing(DocxLineSpacing::new().before(80).after(40));
            let bm_id = if let Some(ref bm) = bm_name {
                let id = ctx.next_bookmark_id;
                ctx.next_bookmark_id += 1;
                label_p = label_p.add_bookmark_start(id, bm);
                Some(id)
            } else {
                None
            };
            label_p = label_p.add_run(
                Run::new()
                    .add_text(format!("[^{name}]:"))
                    .bold()
                    .size(18)
                    .color("1D5FC4"),
            );
            if let Some(id) = bm_id {
                label_p = label_p.add_bookmark_end(id);
            }
            docx = docx.add_paragraph(label_p);
            for inner in blocks {
                docx = append_block(docx, ctx, inner, list_depth + 1, false);
            }
        }
    }
    docx
}

fn append_list_item(
    mut docx: Docx,
    ctx: &mut DocxCtx,
    ordered: bool,
    ordered_num_id: usize,
    item: &ListItem,
    depth: usize,
    block_indent_level: u8,
) -> Docx {
    let base_dir = ctx.base_dir.clone();
    let lvl = depth.min(8);
    let extra_indent = block_indent_level as i32 * 720;
    for (idx, block) in item.blocks.iter().enumerate() {
        match block {
            DocBlock::Paragraph {
                runs,
                alignment,
                direction,
                ..
            } => {
                let mut p = Paragraph::new()
                    .align(to_docx_block_align(*alignment, *direction))
                    .line_spacing(DocxLineSpacing::new().line(264).after(60));
                if *direction == TextDirection::Rtl {
                    p.property = p.property.bidi(true);
                }
                if idx == 0 {
                    if let Some(checked) = item.checked {
                        let indent = ((lvl + 1) * 480) as i32 + extra_indent;
                        p = p.indent(
                            Some(indent),
                            Some(SpecialIndentType::Hanging(320)),
                            None,
                            None,
                        );
                        let box_glyph = if checked { "\u{2611} " } else { "\u{2610} " };
                        p = p.add_run(Run::new().add_text(box_glyph).size(22));
                    } else {
                        let num_id = if ordered { ordered_num_id } else { 1 };
                        p = p.numbering(NumberingId::new(num_id), IndentLevel::new(lvl));
                        if extra_indent > 0 {
                            p = p.indent(Some(extra_indent + ((lvl + 1) * 720) as i32), None, None, None);
                        }
                    }
                } else {
                    let indent = ((lvl + 1) * 720) as i32 + extra_indent;
                    p = p.indent(Some(indent), None, None, None);
                }

                for r in runs {
                    p = add_inline_run_to_paragraph(
                        p,
                        r,
                        22,
                        false,
                        None,
                        base_dir.as_deref(),
                        &ctx.bookmarks,
                        &mut ctx.image_cache,
                    );
                }
                docx = docx.add_paragraph(p);
            }
            DocBlock::List {
                ordered: sub_ord,
                start: sub_start,
                items: sub_items,
                indent_level: sub_indent,
                ..
            } => {
                let sub_num_id = if *sub_ord {
                    let id = ctx.next_ordered_num_id;
                    ctx.next_ordered_num_id += 1;
                    let sub_lvl = (depth + 1).min(8);
                    let num =
                        Numbering::new(id, 2).add_override(LevelOverride::new(sub_lvl).start(*sub_start));
                    docx = docx.add_numbering(num);
                    id
                } else {
                    1
                };
                for sub in sub_items {
                    docx = append_list_item(
                        docx,
                        ctx,
                        *sub_ord,
                        sub_num_id,
                        sub,
                        depth + 1,
                        *sub_indent,
                    );
                }
            }
            other => {
                docx = append_block(docx, ctx, other, depth + 1, false);
            }
        }
    }
    docx
}

fn build_docx_table(
    alignments: &[TableAlign],
    header: &[Vec<InlineRun>],
    rows: &[Vec<Vec<InlineRun>>],
    base_dir: Option<&Path>,
    bookmarks: &HashMap<String, String>,
    image_cache: &mut FxHashMap<String, (Vec<u8>, u32, u32)>,
) -> Table {
    let cols = header
        .len()
        .max(alignments.len())
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0))
        .max(1);
    let cell_w_dxa = (9360 / cols).max(900);
    let mut table_rows = Vec::new();

    if !header.is_empty() {
        let mut cells = Vec::with_capacity(cols);
        for c in 0..cols {
            let runs = header.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
            let align = to_docx_align(alignments.get(c).copied().unwrap_or(TableAlign::None));
            let mut p = Paragraph::new()
                .align(align)
                .line_spacing(DocxLineSpacing::new().line(250).after(20));
            for r in runs {
                p = add_inline_run_to_paragraph(
                    p,
                    r,
                    20,
                    true,
                    Some("1E293B"),
                    base_dir,
                    bookmarks,
                    image_cache,
                );
            }
            cells.push(
                TableCell::new()
                    .width(cell_w_dxa, WidthType::Dxa)
                    .shading(Shading::new().fill("E8EEF6"))
                    .add_paragraph(p),
            );
        }
        table_rows.push(TableRow::new(cells).cant_split());
    }

    for (r_idx, row) in rows.iter().enumerate() {
        let mut cells = Vec::with_capacity(cols);
        for c in 0..cols {
            let runs = row.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
            let align = to_docx_align(alignments.get(c).copied().unwrap_or(TableAlign::None));
            let mut p = Paragraph::new()
                .align(align)
                .line_spacing(DocxLineSpacing::new().line(250).after(20));
            for r in runs {
                p = add_inline_run_to_paragraph(
                    p,
                    r,
                    20,
                    false,
                    None,
                    base_dir,
                    bookmarks,
                    image_cache,
                );
            }
            let mut cell = TableCell::new()
                .width(cell_w_dxa, WidthType::Dxa)
                .add_paragraph(p);
            if r_idx % 2 == 1 {
                cell = cell.shading(Shading::new().fill("F8FAFC"));
            }
            cells.push(cell);
        }
        table_rows.push(TableRow::new(cells).cant_split());
    }

    Table::new(table_rows)
        .set_grid(vec![cell_w_dxa; cols])
        .layout(TableLayoutType::Fixed)
        .set_borders(clean_table_borders())
        .margins(TableCellMargins::new().margin(90, 130, 90, 130))
}

fn to_docx_align(a: TableAlign) -> AlignmentType {
    match a {
        TableAlign::None | TableAlign::Left => AlignmentType::Left,
        TableAlign::Center => AlignmentType::Center,
        TableAlign::Right => AlignmentType::Right,
    }
}

fn load_docx_pic(
    url: &str,
    base_dir: Option<&Path>,
    cache: &mut FxHashMap<String, (Vec<u8>, u32, u32)>,
) -> Option<Pic> {
    let trimmed = url.trim().trim_start_matches('<').trim_end_matches('>');
    if let Some((bytes, disp_w, disp_h)) = cache.get(trimmed) {
        return Some(Pic::new_with_dimensions(bytes.clone(), *disp_w, *disp_h));
    }
    let rgba = crate::export::load_safe_export_rgba(trimmed, base_dir, 1200)?;
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let mut png_bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut png_bytes, image::ImageFormat::Png)
        .ok()?;
    let png_vec = png_bytes.into_inner();
    let disp_w = w.clamp(16, 540);
    let disp_h = ((h as f32) * (disp_w as f32 / w as f32))
        .round()
        .max(16.0) as u32;
    cache.insert(trimmed.to_string(), (png_vec.clone(), disp_w, disp_h));
    Some(Pic::new_with_dimensions(png_vec, disp_w, disp_h))
}

fn classify_docx_link(
    inline: &InlineRun,
    bookmarks: &HashMap<String, String>,
) -> Option<DocxLinkTarget> {
    if inline.is_footnote_ref {
        let name = inline.text.trim_matches(|c| matches!(c, '[' | ']' | '^'));
        let slug = heading_slug(name);
        let key = if slug.is_empty() {
            format!("fn-{}", name.trim().to_ascii_lowercase())
        } else {
            format!("fn-{slug}")
        };
        if let Some(bm) = bookmarks.get(&key) {
            return Some(DocxLinkTarget::Anchor(bm.clone()));
        }
        return None;
    }

    let raw = inline
        .link_url
        .as_deref()?
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    if raw.is_empty() || raw.chars().any(|c| c.is_control()) {
        return None;
    }

    if let Some(anchor) = raw.strip_prefix('#') {
        let decoded = anchor.replace("%20", " ");
        let dec_lower = decoded.trim().to_ascii_lowercase();
        if let Some(bm) = bookmarks.get(&dec_lower) {
            return Some(DocxLinkTarget::Anchor(bm.clone()));
        }
        let slug = heading_slug(&decoded);
        if let Some(bm) = bookmarks.get(&slug) {
            return Some(DocxLinkTarget::Anchor(bm.clone()));
        }
        let fn_key = format!("fn-{slug}");
        if let Some(bm) = bookmarks.get(&fn_key) {
            return Some(DocxLinkTarget::Anchor(bm.clone()));
        }
        return None;
    }

    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        return Some(DocxLinkTarget::External(raw.to_string()));
    }
    if lower.starts_with("www.") {
        return Some(DocxLinkTarget::External(format!("https://{raw}")));
    }

    None
}

fn build_styled_text_run(
    text: &str,
    inline: &InlineRun,
    size_hp: usize,
    force_bold: bool,
    override_color: Option<&str>,
    styled_as_link: bool,
) -> Run {
    let mut run = Run::new().add_text(text);
    let eff_size = if inline.is_footnote_ref {
        size_hp.saturating_sub(4).max(14)
    } else if inline.code {
        size_hp.saturating_sub(2).max(16)
    } else {
        size_hp
    };
    run = run.size(eff_size);
    if inline.bold || force_bold || inline.is_footnote_ref {
        run = run.bold();
    }
    if inline.italic || (inline.is_math && !inline.code) {
        run = run.italic();
    }
    if inline.strikethrough {
        run = run.strike();
    }
    if inline.underline && !styled_as_link {
        run = run.underline("single");
    }
    let has_rtl_script = text
        .chars()
        .any(|c| ('\u{0590}'..='\u{08FF}').contains(&c) || ('\u{FB50}'..='\u{FEFF}').contains(&c));
    if inline.code {
        run = run
            .fonts(
                RunFonts::new()
                    .ascii("Consolas")
                    .hi_ansi("Consolas")
                    .cs("Consolas"),
            )
            .shading(Shading::new().fill("EEF2F6"));
    } else if inline.is_math {
        run = run.fonts(
            RunFonts::new()
                .ascii("Cambria Math")
                .hi_ansi("Cambria Math")
                .cs("Cambria Math"),
        );
    } else if has_rtl_script {
        run = run.fonts(RunFonts::new().cs("Tahoma"));
    }
    if styled_as_link || inline.is_footnote_ref {
        run = run.color("1D5FC4");
        if styled_as_link {
            run = run.underline("single");
        }
    } else if let Some(hex) = override_color {
        run = run.color(hex);
    } else if inline.is_math && !inline.code {
        run = run.color("14284C");
    } else {
        run = run.color("1B2230");
    }
    run
}

#[allow(clippy::too_many_arguments)]
fn add_inline_run_to_paragraph(
    mut p: Paragraph,
    inline: &InlineRun,
    size_hp: usize,
    force_bold: bool,
    override_color: Option<&str>,
    base_dir: Option<&Path>,
    bookmarks: &HashMap<String, String>,
    image_cache: &mut FxHashMap<String, (Vec<u8>, u32, u32)>,
) -> Paragraph {
    let link_target = classify_docx_link(inline, bookmarks);
    let styled_as_link = link_target.is_some();

    if let Some(ref img_url) = inline.image_url {
        if let Some(pic) = load_docx_pic(img_url, base_dir, image_cache) {
            return p.add_run(Run::new().add_image(pic));
        }
        let fallback = format!("[Image: {}]", inline.text);
        let run = build_styled_text_run(
            &fallback,
            inline,
            size_hp,
            force_bold,
            override_color,
            styled_as_link,
        );
        return p.add_run(run);
    }

    if inline.is_math && inline.link_url.is_none() && has_2d_math_command(&inline.text)
        && let Some((png_bytes, w, h)) = crate::document::math::render_math_to_png_bytes(
            &inline.text,
            14.0,
            false,
            [20, 24, 32, 255],
            [255, 255, 255, 255],
        ) {
            let disp_w = (w / 2).clamp(12, 520);
            let scale = disp_w as f32 / (w as f32 * 0.5).max(1.0);
            let disp_h = ((h as f32 * 0.5) * scale).round().max(12.0) as u32;
            let pic = Pic::new_with_dimensions(png_bytes, disp_w, disp_h);
            return p.add_run(Run::new().add_image(pic));
        }

    let resolved_text: std::borrow::Cow<'_, str> = if inline.is_math {
        std::borrow::Cow::Owned(crate::document::math::math_to_plain_text(&inline.text))
    } else {
        std::borrow::Cow::Borrowed(inline.text.as_str())
    };
    let parts: Vec<&str> = resolved_text.split('\n').collect();
    if let Some(target) = link_target {
        let mut link = match target {
            DocxLinkTarget::Anchor(bm) => Hyperlink::new(bm, HyperlinkType::Anchor),
            DocxLinkTarget::External(url) => Hyperlink::new(url, HyperlinkType::External),
        };
        for (i, part) in parts.iter().enumerate() {
            if !part.is_empty() {
                link = link.add_run(build_styled_text_run(
                    part,
                    inline,
                    size_hp,
                    force_bold,
                    override_color,
                    true,
                ));
            }
            if i + 1 < parts.len() {
                link = link.add_run(Run::new().add_break(BreakType::TextWrapping));
            }
        }
        p = p.add_hyperlink(link);
    } else {
        for (i, part) in parts.iter().enumerate() {
            if !part.is_empty() {
                p = p.add_run(build_styled_text_run(
                    part,
                    inline,
                    size_hp,
                    force_bold,
                    override_color,
                    false,
                ));
            }
            if i + 1 < parts.len() {
                p = p.add_run(Run::new().add_break(BreakType::TextWrapping));
            }
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_docx_link_allows_safe_schemes_and_blocks_unsafe_uris() {
        let mut bookmarks = HashMap::new();
        bookmarks.insert("sec".to_string(), "_Ref1".to_string());

        let mut run = InlineRun::plain("click");
        run.link_url = Some("javascript:alert(1)".to_string());
        assert!(classify_docx_link(&run, &bookmarks).is_none());

        run.link_url = Some("file:///C:/Windows/System32/cmd.exe".to_string());
        assert!(classify_docx_link(&run, &bookmarks).is_none());

        run.link_url = Some("https://example.org/docs".to_string());
        assert!(matches!(
            classify_docx_link(&run, &bookmarks),
            Some(DocxLinkTarget::External(_))
        ));

        run.link_url = Some("#sec".to_string());
        assert!(matches!(
            classify_docx_link(&run, &bookmarks),
            Some(DocxLinkTarget::Anchor(_))
        ));
    }
}
