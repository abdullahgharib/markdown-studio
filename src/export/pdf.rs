use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, BufWriter, Cursor};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use lopdf::{Dictionary as LoDictionary, Object as LoObject, ObjectId, StringFormat};
use printpdf::path::PaintMode;
use printpdf::{
    BuiltinFont, Color, ColorBits, ColorSpace, Image, ImageTransform, ImageXObject,
    IndirectFontRef, Line, Mm, PdfDocument, PdfDocumentReference, PdfLayerIndex, PdfLayerReference,
    PdfPageIndex, Point, Px, Rect, Rgb,
};

use crate::document::unicode::{
    shape_bidi_for_display, BlockAlignment, TextDirection,
};
use crate::export::ir::{
    heading_slug, runs_to_plain_text, DocBlock, InlineRun, ListItem, SlugTracker, TableAlign,
};

const DEFAULT_PAGE_W_MM: f32 = 210.0;
const DEFAULT_PAGE_H_MM: f32 = 297.0;
const DEFAULT_MARGIN_MM: f32 = 20.0;
const PT_TO_MM: f32 = 0.352_777_78;

/// Configurable page header and footer templates for PDF export and printing.
/// Supports `{page}`, `{pages}`, `{title}`, and `{date}` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfHeaderFooterOptions {
    pub show_header: bool,
    pub header_template: String,
    pub show_footer: bool,
    pub footer_template: String,
    pub date_text: String,
    pub title_text: String,
}

impl Default for PdfHeaderFooterOptions {
    fn default() -> Self {
        Self {
            show_header: false,
            header_template: "{title}".to_string(),
            show_footer: true,
            footer_template: "Page {page} of {pages}".to_string(),
            date_text: String::new(),
            title_text: String::new(),
        }
    }
}

/// Page geometry and header/footer options for PDF compilation and printing.
#[derive(Debug, Clone)]
pub struct PdfLayoutOptions {
    pub page_w_mm: f32,
    pub page_h_mm: f32,
    pub margin_mm: f32,
    pub header_footer: PdfHeaderFooterOptions,
}

impl Default for PdfLayoutOptions {
    fn default() -> Self {
        Self {
            page_w_mm: DEFAULT_PAGE_W_MM,
            page_h_mm: DEFAULT_PAGE_H_MM,
            margin_mm: DEFAULT_MARGIN_MM,
            header_footer: PdfHeaderFooterOptions::default(),
        }
    }
}

struct FontSet {
    regular: IndirectFontRef,
    bold: IndirectFontRef,
    italic: IndirectFontRef,
    bold_italic: IndirectFontRef,
    mono: IndirectFontRef,
    unicode_enabled: bool,
}

#[derive(Clone, Copy)]
struct ActiveQuoteBar {
    bar_x_mm: f32,
    start_y_mm: f32,
}

#[derive(Debug, Clone)]
struct PdfOutlineEntry {
    level: u8,
    title: String,
    slug: String,
    page_num: usize,
    y_pt: f32,
}

#[derive(Debug, Clone)]
enum PdfLinkTarget {
    Uri(String),
    Anchor(String),
}

#[derive(Debug, Clone)]
struct PdfLinkAnnotation {
    page_num: usize,
    rect_pt: [f32; 4],
    target: PdfLinkTarget,
}

struct PdfCursor {
    doc: PdfDocumentReference,
    layer: PdfLayerReference,
    fonts: FontSet,
    title: String,
    page_w_mm: f32,
    page_h_mm: f32,
    margin_mm: f32,
    content_left_mm: f32,
    content_right_mm: f32,
    top_y_mm: f32,
    bottom_y_mm: f32,
    page_num: usize,
    y_mm: f32,
    base_dir: Option<PathBuf>,
    quote_bars: Vec<ActiveQuoteBar>,
    outline_entries: Vec<PdfOutlineEntry>,
    slug_tracker: SlugTracker,
    anchors: HashMap<String, (usize, f32)>,
    link_annotations: Vec<PdfLinkAnnotation>,
    page_layers: Vec<(PdfPageIndex, PdfLayerIndex)>,
    header_footer: PdfHeaderFooterOptions,
    image_cache: rustc_hash::FxHashMap<String, (u32, u32, Vec<u8>)>,
}

struct CachedPdfFontBytes {
    regular: Option<Vec<u8>>,
    bold: Option<Vec<u8>>,
    italic: Option<Vec<u8>>,
    bold_italic: Option<Vec<u8>>,
    mono: Option<Vec<u8>>,
}

fn read_first_font_bytes(candidates: &[&str]) -> Option<Vec<u8>> {
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}

fn cached_pdf_font_bytes() -> &'static CachedPdfFontBytes {
    static FONT_CACHE: OnceLock<CachedPdfFontBytes> = OnceLock::new();
    FONT_CACHE.get_or_init(|| {
        let regular_candidates = [
            r"C:\Windows\Fonts\arial.ttf",
            r"C:\Windows\Fonts\tahoma.ttf",
            r"C:\Windows\Fonts\segoeui.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/Library/Fonts/Arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        ];
        let bold_candidates = [
            r"C:\Windows\Fonts\arialbd.ttf",
            r"C:\Windows\Fonts\tahomabd.ttf",
            r"C:\Windows\Fonts\segoeuib.ttf",
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            "/Library/Fonts/Arial Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        ];
        let italic_candidates = [
            r"C:\Windows\Fonts\ariali.ttf",
            r"C:\Windows\Fonts\segoeuii.ttf",
            "/System/Library/Fonts/Supplemental/Arial Italic.ttf",
            "/Library/Fonts/Arial Italic.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Oblique.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Italic.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Italic.ttf",
        ];
        let bold_italic_candidates = [
            r"C:\Windows\Fonts\arialbi.ttf",
            r"C:\Windows\Fonts\segoeuiz.ttf",
            "/System/Library/Fonts/Supplemental/Arial Bold Italic.ttf",
            "/Library/Fonts/Arial Bold Italic.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-BoldOblique.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-BoldItalic.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-BoldItalic.ttf",
        ];
        let mono_candidates = [
            r"C:\Windows\Fonts\cour.ttf",
            r"C:\Windows\Fonts\consola.ttf",
            "/System/Library/Fonts/Supplemental/Courier New.ttf",
            "/Library/Fonts/Courier New.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
        ];
        let regular = read_first_font_bytes(&regular_candidates);
        if regular.is_some() {
            CachedPdfFontBytes {
                regular,
                bold: read_first_font_bytes(&bold_candidates),
                italic: read_first_font_bytes(&italic_candidates),
                bold_italic: read_first_font_bytes(&bold_italic_candidates),
                mono: read_first_font_bytes(&mono_candidates),
            }
        } else {
            CachedPdfFontBytes {
                regular: None,
                bold: None,
                italic: None,
                bold_italic: None,
                mono: None,
            }
        }
    })
}

fn add_cached_ttf(doc: &PdfDocumentReference, bytes: Option<&Vec<u8>>) -> Option<IndirectFontRef> {
    bytes.and_then(|b| doc.add_external_font(Cursor::new(b.as_slice())).ok())
}

fn load_font_set(doc: &PdfDocumentReference) -> Result<FontSet, String> {
    let cached = cached_pdf_font_bytes();
    if let Some(regular) = add_cached_ttf(doc, cached.regular.as_ref()) {
        let bold = add_cached_ttf(doc, cached.bold.as_ref()).unwrap_or_else(|| regular.clone());
        let italic = add_cached_ttf(doc, cached.italic.as_ref()).unwrap_or_else(|| regular.clone());
        let bold_italic =
            add_cached_ttf(doc, cached.bold_italic.as_ref()).unwrap_or_else(|| bold.clone());
        let mono = add_cached_ttf(doc, cached.mono.as_ref()).unwrap_or_else(|| regular.clone());
        return Ok(FontSet {
            regular,
            bold,
            italic,
            bold_italic,
            mono,
            unicode_enabled: true,
        });
    }

    let regular = doc
        .add_builtin_font(BuiltinFont::Helvetica)
        .map_err(|e| e.to_string())?;
    let bold = doc
        .add_builtin_font(BuiltinFont::HelveticaBold)
        .map_err(|e| e.to_string())?;
    let italic = doc
        .add_builtin_font(BuiltinFont::HelveticaOblique)
        .map_err(|e| e.to_string())?;
    let bold_italic = doc
        .add_builtin_font(BuiltinFont::HelveticaBoldOblique)
        .map_err(|e| e.to_string())?;
    let mono = doc
        .add_builtin_font(BuiltinFont::Courier)
        .map_err(|e| e.to_string())?;

    Ok(FontSet {
        regular,
        bold,
        italic,
        bold_italic,
        mono,
        unicode_enabled: false,
    })
}

fn expand_header_footer_template(
    template: &str,
    page_num: usize,
    total_pages: usize,
    title_str: &str,
    date_str: &str,
) -> String {
    template
        .replace("{page}", &page_num.to_string())
        .replace("{pages}", &total_pages.to_string())
        .replace("{title}", title_str)
        .replace("{date}", date_str)
}

fn current_iso_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Howard Hinnant civil-from-days algorithm
    let z = secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{year:04}-{m:02}-{d:02}")
}

impl PdfCursor {
    fn new(
        title: &str,
        layout: PdfLayoutOptions,
        base_dir: Option<&Path>,
    ) -> Result<Self, String> {
        let page_w_mm = layout.page_w_mm.max(80.0);
        let page_h_mm = layout.page_h_mm.max(80.0);
        let margin_mm = layout.margin_mm.clamp(8.0, page_w_mm.min(page_h_mm) * 0.25);
        let content_left_mm = margin_mm;
        let content_right_mm = page_w_mm - margin_mm;
        let header_pad = if layout.header_footer.show_header {
            6.0
        } else {
            0.0
        };
        let top_y_mm = page_h_mm - margin_mm - header_pad;
        let bottom_y_mm = margin_mm + 6.0;

        let (doc, page1, layer1) =
            PdfDocument::new(title, Mm(page_w_mm), Mm(page_h_mm), "Layer 1");
        let fonts = load_font_set(&doc)?;

        let layer = doc.get_page(page1).get_layer(layer1);
        Ok(Self {
            doc,
            layer,
            fonts,
            title: title.to_string(),
            page_w_mm,
            page_h_mm,
            margin_mm,
            content_left_mm,
            content_right_mm,
            top_y_mm,
            bottom_y_mm,
            page_num: 1,
            y_mm: top_y_mm,
            base_dir: base_dir.map(Path::to_path_buf),
            quote_bars: Vec::new(),
            outline_entries: Vec::new(),
            slug_tracker: SlugTracker::new(),
            anchors: HashMap::new(),
            link_annotations: Vec::new(),
            page_layers: vec![(page1, layer1)],
            header_footer: layout.header_footer,
            image_cache: rustc_hash::FxHashMap::default(),
        })
    }

    fn draw_quote_bar_on_layer(
        layer: &PdfLayerReference,
        bar_x_mm: f32,
        y_top: f32,
        y_bot: f32,
    ) {
        if y_top > y_bot + 1.0 {
            layer.set_fill_color(Color::Rgb(Rgb::new(0.29, 0.53, 0.85, None)));
            layer.add_rect(
                Rect::new(
                    Mm(bar_x_mm),
                    Mm(y_bot),
                    Mm(bar_x_mm + 1.0),
                    Mm(y_top),
                )
                .with_mode(PaintMode::Fill),
            );
        }
    }

    fn draw_quote_bar_segment(&self, bar_x_mm: f32, y_top: f32, y_bot: f32) {
        Self::draw_quote_bar_on_layer(&self.layer, bar_x_mm, y_top, y_bot);
    }

    fn new_page(&mut self) {
        for bar in &mut self.quote_bars {
            Self::draw_quote_bar_on_layer(
                &self.layer,
                bar.bar_x_mm,
                bar.start_y_mm - 0.5,
                self.bottom_y_mm + 1.0,
            );
            bar.start_y_mm = self.top_y_mm;
        }
        self.page_num += 1;
        let (page, layer_idx) = self.doc.add_page(
            Mm(self.page_w_mm),
            Mm(self.page_h_mm),
            format!("Page {}", self.page_num),
        );
        self.layer = self.doc.get_page(page).get_layer(layer_idx);
        self.page_layers.push((page, layer_idx));
        self.y_mm = self.top_y_mm;
    }

    fn ensure_space(&mut self, needed_mm: f32) {
        if self.y_mm - needed_mm < self.bottom_y_mm {
            self.new_page();
        }
    }

    fn draw_headers_and_footers(&self) {
        if !self.header_footer.show_header && !self.header_footer.show_footer {
            return;
        }
        let total_pages = self.page_layers.len().max(1);
        let title_str = if self.header_footer.title_text.trim().is_empty() {
            self.title.as_str()
        } else {
            self.header_footer.title_text.trim()
        };
        let date_owned;
        let date_str = if self.header_footer.date_text.trim().is_empty() {
            date_owned = current_iso_date();
            date_owned.as_str()
        } else {
            self.header_footer.date_text.trim()
        };
        let muted = Color::Rgb(Rgb::new(0.45, 0.48, 0.54, None));
        let rule_color = Color::Rgb(Rgb::new(0.84, 0.86, 0.89, None));
        let font_pt = 8.5_f32;
        let header_y_mm = self.page_h_mm - (self.margin_mm * 0.65).max(8.0);
        let header_rule_y_mm = self.page_h_mm - self.margin_mm - 1.5;
        let footer_y_mm = (self.margin_mm * 0.55).max(8.0);

        for (idx, &(page_idx, layer_idx)) in self.page_layers.iter().enumerate() {
            let page_num = idx + 1;
            let layer = self.doc.get_page(page_idx).get_layer(layer_idx);
            layer.set_fill_color(muted.clone());

            if self.header_footer.show_header {
                let raw = expand_header_footer_template(
                    &self.header_footer.header_template,
                    page_num,
                    total_pages,
                    title_str,
                    date_str,
                );
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    let prepared = self.prepare_text(trimmed, TextDirection::Ltr);
                    layer.use_text(
                        prepared,
                        font_pt,
                        Mm(self.content_left_mm),
                        Mm(header_y_mm),
                        &self.fonts.regular,
                    );
                    layer.set_outline_color(rule_color.clone());
                    layer.set_outline_thickness(0.4);
                    layer.add_line(Line {
                        points: vec![
                            (Point::new(Mm(self.content_left_mm), Mm(header_rule_y_mm)), false),
                            (Point::new(Mm(self.content_right_mm), Mm(header_rule_y_mm)), false),
                        ],
                        is_closed: false,
                    });
                }
            }

            if self.header_footer.show_footer {
                let raw = expand_header_footer_template(
                    &self.header_footer.footer_template,
                    page_num,
                    total_pages,
                    title_str,
                    date_str,
                );
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    let prepared = self.prepare_text(trimmed, TextDirection::Ltr);
                    let w = approx_text_width_mm(
                        &prepared,
                        font_pt,
                        false,
                        false,
                        self.fonts.unicode_enabled,
                    );
                    let x = (self.content_right_mm - w).max(self.content_left_mm);
                    layer.use_text(
                        prepared,
                        font_pt,
                        Mm(x),
                        Mm(footer_y_mm),
                        &self.fonts.regular,
                    );
                }
            }
        }
    }

    fn font_for_run(&self, run: &InlineRun, force_bold: bool) -> &IndirectFontRef {
        if run.code {
            return &self.fonts.mono;
        }
        let b = run.bold || force_bold;
        match (b, run.italic) {
            (true, true) => &self.fonts.bold_italic,
            (true, false) => &self.fonts.bold,
            (false, true) => &self.fonts.italic,
            (false, false) => &self.fonts.regular,
        }
    }

    fn prepare_text(&self, text: &str, dir: TextDirection) -> String {
        if !self.fonts.unicode_enabled {
            return sanitize_pdf_text(text);
        }
        let clean: String = text.chars().filter(|&c| c != '\r' && c != '\n').collect();
        let has_rtl = dir == TextDirection::Rtl
            || clean.chars().any(|c| {
                ('\u{0590}'..='\u{08FF}').contains(&c) || ('\u{FB50}'..='\u{FEFF}').contains(&c)
            });
        if has_rtl {
            shape_bidi_for_display(&clean, dir)
        } else {
            clean
        }
    }
}

/// Compiles `blocks` into a paginated vector PDF byte buffer using A4 layout.
pub fn render_pdf_bytes(
    blocks: &[DocBlock],
    title: &str,
    on_progress: impl FnMut(f32),
) -> Result<Vec<u8>, String> {
    render_pdf_bytes_with_options(blocks, title, PdfLayoutOptions::default(), on_progress)
}

/// Compiles `blocks` into a paginated vector PDF byte buffer using custom `PdfLayoutOptions`.
pub fn render_pdf_bytes_with_options(
    blocks: &[DocBlock],
    title: &str,
    layout: PdfLayoutOptions,
    on_progress: impl FnMut(f32),
) -> Result<Vec<u8>, String> {
    render_pdf_bytes_with_options_and_base_dir(blocks, title, layout, None, on_progress)
}

struct CompiledPdf {
    bytes: Vec<u8>,
    outline_entries: Vec<PdfOutlineEntry>,
    anchors: HashMap<String, (usize, f32)>,
    link_annotations: Vec<PdfLinkAnnotation>,
}

fn compile_pdf_raw_and_meta(
    blocks: &[DocBlock],
    title: &str,
    layout: PdfLayoutOptions,
    base_dir: Option<&Path>,
    mut on_progress: impl FnMut(f32),
) -> Result<CompiledPdf, String> {
    let mut pdf = PdfCursor::new(title, layout, base_dir)?;
    let total = blocks.len().max(1);

    for (idx, block) in blocks.iter().enumerate() {
        render_block(&mut pdf, block, 0.0, false);
        on_progress((idx + 1) as f32 / total as f32);
    }

    pdf.draw_headers_and_footers();
    pdf.image_cache.clear();
    pdf.image_cache.shrink_to_fit();

    let mut bytes = Vec::new();
    let outline_entries = std::mem::take(&mut pdf.outline_entries);
    let anchors = std::mem::take(&mut pdf.anchors);
    let link_annotations = std::mem::take(&mut pdf.link_annotations);
    pdf.doc
        .save(&mut BufWriter::new(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(CompiledPdf {
        bytes,
        outline_entries,
        anchors,
        link_annotations,
    })
}

/// Compiles `blocks` into a paginated vector PDF byte buffer using custom geometry and optional asset base directory.
pub fn render_pdf_bytes_with_options_and_base_dir(
    blocks: &[DocBlock],
    title: &str,
    layout: PdfLayoutOptions,
    base_dir: Option<&Path>,
    on_progress: impl FnMut(f32),
) -> Result<Vec<u8>, String> {
    let compiled = compile_pdf_raw_and_meta(blocks, title, layout, base_dir, on_progress)?;
    inject_pdf_outlines_and_links(
        compiled.bytes,
        &compiled.outline_entries,
        &compiled.anchors,
        &compiled.link_annotations,
    )
}

fn encode_pdf_outline_title(title: &str) -> LoObject {
    let clean_ascii = title.is_ascii()
        && title
            .bytes()
            .all(|b| b >= 0x20 && b != 0x7F && b != b'(' && b != b')' && b != b'\\');
    if clean_ascii {
        LoObject::String(title.as_bytes().to_vec(), StringFormat::Literal)
    } else {
        let mut utf16 = Vec::with_capacity(2 + title.len() * 2);
        utf16.push(0xFE);
        utf16.push(0xFF);
        for unit in title.encode_utf16() {
            utf16.extend_from_slice(&unit.to_be_bytes());
        }
        LoObject::String(utf16, StringFormat::Hexadecimal)
    }
}

struct OutlineNode {
    entry_idx: usize,
    parent: Option<usize>,
    children: Vec<usize>,
}

fn open_descendant_count(nodes: &[OutlineNode], idx: usize) -> usize {
    let mut total = nodes[idx].children.len();
    for &child_idx in &nodes[idx].children {
        total += open_descendant_count(nodes, child_idx);
    }
    total
}

fn encode_pdf_ascii_or_hex(text: &str) -> LoObject {
    let clean_ascii = text.is_ascii()
        && text
            .bytes()
            .all(|b| b >= 0x20 && b != 0x7F && b != b'(' && b != b')' && b != b'\\');
    if clean_ascii {
        LoObject::String(text.as_bytes().to_vec(), StringFormat::Literal)
    } else {
        LoObject::String(text.as_bytes().to_vec(), StringFormat::Hexadecimal)
    }
}

fn resolve_anchor_target(
    raw: &str,
    anchors: &HashMap<String, (usize, f32)>,
) -> Option<(usize, f32)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let decoded = trimmed.replace("%20", " ");
    let lower = decoded.to_ascii_lowercase();
    if let Some(&pos) = anchors.get(&lower) {
        return Some(pos);
    }
    let slug = heading_slug(&decoded);
    if let Some(&pos) = anchors.get(&slug) {
        return Some(pos);
    }
    let fn_slug = format!("fn-{slug}");
    anchors.get(&fn_slug).copied()
}

fn inject_pdf_outlines_and_links_to_writer<W: std::io::Write>(
    pdf_bytes: Vec<u8>,
    entries: &[PdfOutlineEntry],
    anchors: &HashMap<String, (usize, f32)>,
    link_annotations: &[PdfLinkAnnotation],
    out: &mut W,
) -> Result<(), String> {
    if entries.is_empty() && link_annotations.is_empty() {
        return out.write_all(&pdf_bytes).map_err(|e| e.to_string());
    }

    let mut doc = lopdf::Document::load_mem(&pdf_bytes).map_err(|e| e.to_string())?;
    drop(pdf_bytes);
    let pages: BTreeMap<u32, ObjectId> = doc.get_pages();

    let valid_entries: Vec<&PdfOutlineEntry> = entries
        .iter()
        .filter(|e| pages.contains_key(&(e.page_num as u32)))
        .collect();

    if !valid_entries.is_empty() {
        let mut nodes: Vec<OutlineNode> = Vec::with_capacity(valid_entries.len());
        let mut root_children: Vec<usize> = Vec::new();
        let mut stack: Vec<(usize, u8)> = Vec::new();

        for (entry_idx, entry) in valid_entries.iter().enumerate() {
            let lvl = entry.level.clamp(1, 6);
            while stack.last().is_some_and(|&(_, top_lvl)| top_lvl >= lvl) {
                stack.pop();
            }
            let parent = stack.last().map(|&(p_idx, _)| p_idx);
            let node_idx = nodes.len();
            nodes.push(OutlineNode {
                entry_idx,
                parent,
                children: Vec::new(),
            });
            if let Some(p_idx) = parent {
                nodes[p_idx].children.push(node_idx);
            } else {
                root_children.push(node_idx);
            }
            stack.push((node_idx, lvl));
        }

        if !root_children.is_empty() {
            let catalog_id = doc
                .trailer
                .get(b"Root")
                .and_then(LoObject::as_reference)
                .map_err(|e| e.to_string())?;

            let existing_outlines_id = doc
                .get_object(catalog_id)
                .and_then(LoObject::as_dict)
                .ok()
                .and_then(|d| d.get(b"Outlines").ok())
                .and_then(|o| o.as_reference().ok());
            let outlines_id = existing_outlines_id.unwrap_or_else(|| doc.new_object_id());

            let node_ids: Vec<ObjectId> = (0..nodes.len()).map(|_| doc.new_object_id()).collect();
            let mut sibling_pos: HashMap<usize, usize> = HashMap::with_capacity(nodes.len());
            for (pos, &root_child) in root_children.iter().enumerate() {
                sibling_pos.insert(root_child, pos);
            }
            for node in &nodes {
                for (pos, &child_idx) in node.children.iter().enumerate() {
                    sibling_pos.insert(child_idx, pos);
                }
            }

            for (idx, node) in nodes.iter().enumerate() {
                let entry = valid_entries[node.entry_idx];
                let page_id = pages[&(entry.page_num as u32)];
                let parent_id = node.parent.map(|p| node_ids[p]).unwrap_or(outlines_id);
                let siblings = match node.parent {
                    Some(p) => nodes[p].children.as_slice(),
                    None => root_children.as_slice(),
                };
                let pos = sibling_pos[&idx];

                let mut dict = LoDictionary::new();
                dict.set("Title", encode_pdf_outline_title(&entry.title));
                dict.set("Parent", LoObject::Reference(parent_id));
                if pos > 0 {
                    dict.set("Prev", LoObject::Reference(node_ids[siblings[pos - 1]]));
                }
                if pos + 1 < siblings.len() {
                    dict.set("Next", LoObject::Reference(node_ids[siblings[pos + 1]]));
                }
                if !node.children.is_empty() {
                    dict.set("First", LoObject::Reference(node_ids[node.children[0]]));
                    dict.set(
                        "Last",
                        LoObject::Reference(node_ids[*node.children.last().unwrap()]),
                    );
                    dict.set(
                        "Count",
                        LoObject::Integer(open_descendant_count(&nodes, idx) as i64),
                    );
                }
                dict.set(
                    "Dest",
                    LoObject::Array(vec![
                        LoObject::Reference(page_id),
                        LoObject::Name(b"XYZ".to_vec()),
                        LoObject::Null,
                        LoObject::Real(entry.y_pt),
                        LoObject::Null,
                    ]),
                );
                doc.objects.insert(node_ids[idx], LoObject::Dictionary(dict));
            }

            let mut outlines_dict = LoDictionary::new();
            outlines_dict.set("Type", LoObject::Name(b"Outlines".to_vec()));
            outlines_dict.set("First", LoObject::Reference(node_ids[root_children[0]]));
            outlines_dict.set(
                "Last",
                LoObject::Reference(node_ids[*root_children.last().unwrap()]),
            );
            outlines_dict.set("Count", LoObject::Integer(nodes.len() as i64));
            doc.objects
                .insert(outlines_id, LoObject::Dictionary(outlines_dict));

            if let Ok(catalog_obj) = doc.get_object_mut(catalog_id)
                && let Ok(catalog_dict) = catalog_obj.as_dict_mut() {
                    catalog_dict.set("Outlines", LoObject::Reference(outlines_id));
                    catalog_dict.set("PageMode", LoObject::Name(b"UseOutlines".to_vec()));
                }
        }
    }

    if !link_annotations.is_empty() {
        let mut combined_anchors: HashMap<String, (usize, f32)> = anchors.clone();
        for entry in entries {
            if !entry.slug.is_empty() {
                combined_anchors
                    .entry(entry.slug.clone())
                    .or_insert((entry.page_num, entry.y_pt));
            }
        }

        let mut page_annots: BTreeMap<ObjectId, Vec<ObjectId>> = BTreeMap::new();
        for link in link_annotations {
            let Some(&src_page_id) = pages.get(&(link.page_num as u32)) else {
                continue;
            };
            let mut annot = LoDictionary::new();
            annot.set("Type", LoObject::Name(b"Annot".to_vec()));
            annot.set("Subtype", LoObject::Name(b"Link".to_vec()));
            annot.set(
                "Rect",
                LoObject::Array(vec![
                    LoObject::Real(link.rect_pt[0]),
                    LoObject::Real(link.rect_pt[1]),
                    LoObject::Real(link.rect_pt[2]),
                    LoObject::Real(link.rect_pt[3]),
                ]),
            );
            annot.set(
                "Border",
                LoObject::Array(vec![
                    LoObject::Integer(0),
                    LoObject::Integer(0),
                    LoObject::Integer(0),
                ]),
            );

            match &link.target {
                PdfLinkTarget::Uri(uri) => {
                    let mut action = LoDictionary::new();
                    action.set("Type", LoObject::Name(b"Action".to_vec()));
                    action.set("S", LoObject::Name(b"URI".to_vec()));
                    action.set("URI", encode_pdf_ascii_or_hex(uri));
                    annot.set("A", LoObject::Dictionary(action));
                }
                PdfLinkTarget::Anchor(raw) => {
                    let Some((target_page, target_y_pt)) =
                        resolve_anchor_target(raw, &combined_anchors)
                    else {
                        continue;
                    };
                    let Some(&target_page_id) = pages.get(&(target_page as u32)) else {
                        continue;
                    };
                    annot.set(
                        "Dest",
                        LoObject::Array(vec![
                            LoObject::Reference(target_page_id),
                            LoObject::Name(b"XYZ".to_vec()),
                            LoObject::Null,
                            LoObject::Real(target_y_pt),
                            LoObject::Null,
                        ]),
                    );
                }
            }

            let annot_id = doc.add_object(LoObject::Dictionary(annot));
            page_annots.entry(src_page_id).or_default().push(annot_id);
        }

        for (page_id, new_ids) in page_annots {
            let refs: Vec<LoObject> = new_ids.into_iter().map(LoObject::Reference).collect();
            let existing_ref = doc
                .get_object(page_id)
                .and_then(LoObject::as_dict)
                .ok()
                .and_then(|d| d.get(b"Annots").ok())
                .and_then(|o| o.as_reference().ok());

            if let Some(arr_id) = existing_ref {
                if let Ok(LoObject::Array(arr)) = doc.get_object_mut(arr_id) {
                    arr.extend(refs);
                }
            } else if let Ok(page_dict) = doc.get_object_mut(page_id).and_then(LoObject::as_dict_mut)
            {
                if let Ok(LoObject::Array(arr)) = page_dict.get_mut(b"Annots") {
                    arr.extend(refs);
                } else {
                    page_dict.set("Annots", LoObject::Array(refs));
                }
            }
        }
    }

    doc.save_to(out).map_err(|e| e.to_string())
}

fn inject_pdf_outlines_and_links(
    pdf_bytes: Vec<u8>,
    entries: &[PdfOutlineEntry],
    anchors: &HashMap<String, (usize, f32)>,
    link_annotations: &[PdfLinkAnnotation],
) -> Result<Vec<u8>, String> {
    if entries.is_empty() && link_annotations.is_empty() {
        return Ok(pdf_bytes);
    }
    let approx = pdf_bytes.len() + entries.len() * 128 + link_annotations.len() * 96;
    let mut out = Vec::with_capacity(approx);
    inject_pdf_outlines_and_links_to_writer(pdf_bytes, entries, anchors, link_annotations, &mut out)?;
    Ok(out)
}

/// Writes a paginated vector PDF file to `path`.
pub fn export_pdf_to_file(
    blocks: &[DocBlock],
    title: &str,
    path: impl AsRef<Path>,
    on_progress: impl FnMut(f32),
) -> Result<(), String> {
    export_pdf_to_file_with_options(blocks, title, path, PdfLayoutOptions::default(), on_progress)
}

/// Writes a paginated vector PDF file to `path` using custom `PdfLayoutOptions`.
pub fn export_pdf_to_file_with_options(
    blocks: &[DocBlock],
    title: &str,
    path: impl AsRef<Path>,
    layout: PdfLayoutOptions,
    on_progress: impl FnMut(f32),
) -> Result<(), String> {
    export_pdf_to_file_with_options_and_base_dir(blocks, title, path, layout, None, on_progress)
}

/// Writes a paginated vector PDF file to `path` using custom `PdfLayoutOptions` and optional asset base directory.
pub fn export_pdf_to_file_with_options_and_base_dir(
    blocks: &[DocBlock],
    title: &str,
    path: impl AsRef<Path>,
    layout: PdfLayoutOptions,
    base_dir: Option<&Path>,
    on_progress: impl FnMut(f32),
) -> Result<(), String> {
    let compiled = compile_pdf_raw_and_meta(blocks, title, layout, base_dir, on_progress)?;
    let file = File::create(path.as_ref()).map_err(|e: io::Error| e.to_string())?;
    use std::io::Write;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    inject_pdf_outlines_and_links_to_writer(
        compiled.bytes,
        &compiled.outline_entries,
        &compiled.anchors,
        &compiled.link_annotations,
        &mut writer,
    )?;
    writer.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn aligned_line_x(
    left: f32,
    max_w: f32,
    line_w: f32,
    alignment: BlockAlignment,
    direction: TextDirection,
) -> f32 {
    match alignment {
        BlockAlignment::Center => left + ((max_w - line_w) * 0.5).max(0.0),
        BlockAlignment::Right => (left + max_w - line_w).max(left),
        BlockAlignment::End => {
            if direction == TextDirection::Rtl {
                left
            } else {
                (left + max_w - line_w).max(left)
            }
        }
        BlockAlignment::Start => {
            if direction == TextDirection::Rtl {
                (left + max_w - line_w).max(left)
            } else {
                left
            }
        }
        BlockAlignment::Left | BlockAlignment::Justify => left,
    }
}

fn rgba_to_pdf_rgb_bytes(rgba: &image::RgbaImage) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let rgb_len = (w as usize).checked_mul(h as usize)?.checked_mul(3)?;
    let mut rgb_bytes = Vec::with_capacity(rgb_len);
    for px in rgba.pixels() {
        let a = px[3] as f32 / 255.0;
        let inv = 1.0 - a;
        rgb_bytes.push((px[0] as f32 * a + 255.0 * inv).round() as u8);
        rgb_bytes.push((px[1] as f32 * a + 255.0 * inv).round() as u8);
        rgb_bytes.push((px[2] as f32 * a + 255.0 * inv).round() as u8);
    }
    Some((w, h, rgb_bytes))
}

fn rgb_to_pdf_xobject(w: u32, h: u32, rgb_bytes: Vec<u8>) -> ImageXObject {
    ImageXObject {
        width: Px(w as usize),
        height: Px(h as usize),
        color_space: ColorSpace::Rgb,
        bits_per_component: ColorBits::Bit8,
        interpolate: true,
        image_data: rgb_bytes,
        image_filter: None,
        smask: None,
        clipping_bbox: None,
    }
}

fn rgba_to_pdf_image_xobject(rgba: &image::RgbaImage) -> Option<(ImageXObject, u32, u32)> {
    let (w, h, rgb_bytes) = rgba_to_pdf_rgb_bytes(rgba)?;
    Some((rgb_to_pdf_xobject(w, h, rgb_bytes), w, h))
}

fn png_bytes_to_pdf_image_xobject(png_bytes: &[u8]) -> Option<(ImageXObject, u32, u32)> {
    let dyn_img = image::load_from_memory(png_bytes).ok()?;
    let rgba = dyn_img.to_rgba8();
    rgba_to_pdf_image_xobject(&rgba)
}

#[allow(clippy::too_many_arguments)]
fn embed_pdf_image_xobject(
    pdf: &mut PdfCursor,
    xobj: ImageXObject,
    w_px: u32,
    h_px: u32,
    logical_scale: f32,
    left: f32,
    max_w: f32,
    max_h_mm: f32,
    alignment: BlockAlignment,
    direction: TextDirection,
) {
    let eff_scale = logical_scale.max(1.0);
    let aspect = h_px as f32 / w_px.max(1) as f32;
    let natural_w_mm = (w_px as f32 / eff_scale) * 0.264_583_3;
    let min_w_mm = if eff_scale > 1.0 { 4.0 } else { 12.0 };
    let mut draw_w_mm = natural_w_mm.min(max_w).max(min_w_mm);
    let mut draw_h_mm = draw_w_mm * aspect;
    if draw_h_mm > max_h_mm {
        draw_h_mm = max_h_mm;
        draw_w_mm = (draw_h_mm / aspect.max(0.01)).min(max_w);
    }
    pdf.ensure_space(draw_h_mm + 4.0);
    let img_x = aligned_line_x(left, max_w, draw_w_mm, alignment, direction);
    let px_to_mm_300 = 25.4 / 300.0;
    let sx = draw_w_mm / (w_px as f32 * px_to_mm_300);
    let sy = draw_h_mm / (h_px as f32 * px_to_mm_300);
    Image::from(xobj).add_to_layer(
        pdf.layer.clone(),
        ImageTransform {
            translate_x: Some(Mm(img_x)),
            translate_y: Some(Mm(pdf.y_mm - draw_h_mm)),
            scale_x: Some(sx),
            scale_y: Some(sy),
            dpi: Some(300.0),
            ..Default::default()
        },
    );
    pdf.y_mm -= draw_h_mm + 3.0;
}

fn load_pdf_image_xobject(
    pdf: &mut PdfCursor,
    url: &str,
) -> Option<(ImageXObject, u32, u32)> {
    let trimmed = url.trim().trim_start_matches('<').trim_end_matches('>');
    if let Some((w, h, rgb)) = pdf.image_cache.get(trimmed) {
        return Some((rgb_to_pdf_xobject(*w, *h, rgb.clone()), *w, *h));
    }
    let rgba = crate::export::load_safe_export_rgba(trimmed, pdf.base_dir.as_deref(), 2400)?;
    let (w, h, rgb) = rgba_to_pdf_rgb_bytes(&rgba)?;
    pdf.image_cache
        .insert(trimmed.to_string(), (w, h, rgb.clone()));
    Some((rgb_to_pdf_xobject(w, h, rgb), w, h))
}

#[allow(clippy::too_many_arguments)]
fn render_runs_with_images(
    pdf: &mut PdfCursor,
    runs: &[InlineRun],
    left: f32,
    max_w: f32,
    font_pt: f32,
    line_h: f32,
    force_bold: bool,
    muted: bool,
    alignment: BlockAlignment,
    direction: TextDirection,
) {
    let unicode = pdf.fonts.unicode_enabled;
    let mut pending_text: Vec<InlineRun> = Vec::new();
    let flush_text = |pdf: &mut PdfCursor, buf: &mut Vec<InlineRun>| {
        if buf.is_empty() {
            return;
        }
        let lines = wrap_runs(buf, max_w, font_pt, force_bold, unicode);
        for line in &lines {
            pdf.ensure_space(line_h);
            let line_w = measure_line_width_mm(line, font_pt, force_bold);
            let line_x = aligned_line_x(left, max_w, line_w, alignment, direction);
            draw_styled_line(
                pdf,
                line,
                line_x,
                pdf.y_mm - font_pt * PT_TO_MM,
                font_pt,
                force_bold,
                muted,
                direction,
            );
            pdf.y_mm -= line_h;
        }
        buf.clear();
    };

    for run in runs {
        if let Some(ref img_url) = run.image_url
            && let Some((xobj, w_px, h_px)) = load_pdf_image_xobject(pdf, img_url)
        {
                flush_text(pdf, &mut pending_text);
                let max_h_mm = (pdf.top_y_mm - pdf.bottom_y_mm - 12.0).clamp(40.0, 115.0);
                embed_pdf_image_xobject(
                    pdf,
                    xobj,
                    w_px,
                    h_px,
                    1.0,
                    left,
                    max_w,
                    max_h_mm,
                    alignment,
                    direction,
                );
                continue;
            }
        pending_text.push(run.clone());
    }
    flush_text(pdf, &mut pending_text);
}

fn render_block(pdf: &mut PdfCursor, block: &DocBlock, indent_mm: f32, in_quote: bool) {
    let left = pdf.content_left_mm + indent_mm;
    let max_w = (pdf.content_right_mm - left).max(30.0);

    match block {
        DocBlock::Heading {
            level,
            runs,
            alignment,
            indent_level,
            direction,
            ..
        } => {
            let extra_indent = *indent_level as f32 * 8.0;
            let eff_left = left + extra_indent;
            let eff_max_w = (pdf.content_right_mm - eff_left).max(30.0);
            let font_pt = match level {
                1 => 21.0,
                2 => 17.0,
                3 => 14.0,
                4 => 12.0,
                5 => 11.0,
                _ => 10.5,
            };
            let line_h = font_pt * PT_TO_MM * 1.38;
            let preview_lines = wrap_runs(runs, eff_max_w, font_pt, true, pdf.fonts.unicode_enabled);
            let total_h = preview_lines.len().max(1) as f32 * line_h + 6.0;
            // Keep heading with at least one following line.
            pdf.ensure_space(total_h + 8.0);
            if pdf.y_mm < pdf.top_y_mm - 1.0 {
                pdf.y_mm -= 3.0;
            }
            let raw_title = runs_to_plain_text(runs);
            let title = raw_title.trim();
            if !title.is_empty() && *level <= 6 {
                let y_pt = (pdf.y_mm / PT_TO_MM + 4.0).min(pdf.page_h_mm / PT_TO_MM);
                let slug = pdf.slug_tracker.next_slug(title);
                pdf.outline_entries.push(PdfOutlineEntry {
                    level: *level,
                    title: title.to_string(),
                    slug,
                    page_num: pdf.page_num,
                    y_pt,
                });
            }
            render_runs_with_images(
                pdf,
                runs,
                eff_left,
                eff_max_w,
                font_pt,
                line_h,
                true,
                in_quote,
                *alignment,
                *direction,
            );
            if *level <= 2 {
                pdf.layer
                    .set_outline_color(Color::Rgb(Rgb::new(0.80, 0.83, 0.88, None)));
                pdf.layer.set_outline_thickness(0.6);
                let rule_y = pdf.y_mm + 0.5;
                pdf.layer.add_line(Line {
                    points: vec![
                        (Point::new(Mm(eff_left), Mm(rule_y)), false),
                        (Point::new(Mm(pdf.content_right_mm), Mm(rule_y)), false),
                    ],
                    is_closed: false,
                });
                pdf.y_mm -= 2.5;
            }
            pdf.y_mm -= 2.5;
        }
        DocBlock::Paragraph {
            runs,
            alignment,
            indent_level,
            direction,
            ..
        } => {
            let extra_indent = *indent_level as f32 * 8.0;
            let eff_left = left + extra_indent;
            let eff_max_w = (pdf.content_right_mm - eff_left).max(30.0);
            let font_pt = 10.5;
            let line_h = font_pt * PT_TO_MM * 1.45;
            render_runs_with_images(
                pdf,
                runs,
                eff_left,
                eff_max_w,
                font_pt,
                line_h,
                false,
                in_quote,
                *alignment,
                *direction,
            );
            pdf.y_mm -= 3.0;
        }
        DocBlock::BlockQuote {
            blocks,
            indent_level,
            direction,
            ..
        } => {
            let extra_indent = *indent_level as f32 * 8.0;
            let bar_x = if *direction == TextDirection::Rtl {
                pdf.content_right_mm - 2.0 - extra_indent
            } else {
                left + extra_indent + 1.0
            };
            pdf.quote_bars.push(ActiveQuoteBar {
                bar_x_mm: bar_x,
                start_y_mm: pdf.y_mm,
            });
            for inner in blocks {
                render_block(pdf, inner, indent_mm + extra_indent + 5.5, true);
            }
            if let Some(bar) = pdf.quote_bars.pop() {
                pdf.draw_quote_bar_segment(bar.bar_x_mm, bar.start_y_mm - 0.5, pdf.y_mm + 2.5);
            }
            pdf.y_mm -= 2.0;
        }
        DocBlock::CodeBlock { info, code, .. } => {
            if crate::document::ast::is_mermaid_code_info(info) && !code.trim().is_empty()
                && let Some((png_bytes, _, _)) =
                    crate::document::mermaid::render_mermaid_to_png_bytes(code, 2.0)
                    && let Some((xobj, w_px, h_px)) = png_bytes_to_pdf_image_xobject(&png_bytes) {
                        let max_h_mm =
                            (pdf.top_y_mm - pdf.bottom_y_mm - 12.0).clamp(40.0, 162.0);
                        embed_pdf_image_xobject(
                            pdf,
                            xobj,
                            w_px,
                            h_px,
                            2.0,
                            left,
                            max_w,
                            max_h_mm,
                            BlockAlignment::Center,
                            TextDirection::Ltr,
                        );
                        pdf.y_mm -= 2.0;
                        return;
                    }
            if crate::document::ast::is_math_code_info(info) {
                let trimmed = code.trim();
                if trimmed.is_empty() {
                    pdf.y_mm -= 4.0;
                    return;
                }
                if let Some((png_bytes, _, _)) = crate::document::math::render_math_to_png_bytes(
                    trimmed,
                    18.0,
                    true,
                    [20, 24, 32, 255],
                    [255, 255, 255, 255],
                )
                    && let Some((xobj, w_px, h_px)) = png_bytes_to_pdf_image_xobject(&png_bytes) {
                        let max_h_mm =
                            (pdf.top_y_mm - pdf.bottom_y_mm - 12.0).clamp(40.0, 115.0);
                        embed_pdf_image_xobject(
                            pdf,
                            xobj,
                            w_px,
                            h_px,
                            2.0,
                            left,
                            max_w,
                            max_h_mm,
                            BlockAlignment::Center,
                            TextDirection::Ltr,
                        );
                        pdf.y_mm -= 2.0;
                        return;
                    }
            }

            let font_pt = 9.0;
            let line_h = font_pt * PT_TO_MM * 1.4;
            let pad = 3.0;
            let inner_w = (max_w - 2.0 * pad).max(20.0);
            let fallback_code: std::borrow::Cow<'_, str> =
                if crate::document::ast::is_math_code_info(info) {
                    std::borrow::Cow::Owned(crate::document::math::math_to_plain_text(code))
                } else {
                    std::borrow::Cow::Borrowed(code.as_str())
                };
            let wrapped_lines = wrap_code_lines(&fallback_code, inner_w, font_pt);

            let mut idx = 0;
            while idx < wrapped_lines.len() {
                let info_h = if idx == 0 && !info.is_empty() { 4.5 } else { 0.0 };
                pdf.ensure_space(line_h + 2.0 * pad + info_h);
                let avail_h = (pdf.y_mm - pdf.bottom_y_mm - 2.0 * pad - info_h).max(line_h);
                let fit_count = ((avail_h / line_h).floor() as usize)
                    .max(1)
                    .min(wrapped_lines.len() - idx);
                let chunk = &wrapped_lines[idx..idx + fit_count];
                let box_h = chunk.len() as f32 * line_h + 2.0 * pad + info_h;

                let box_top = pdf.y_mm;
                let box_bottom = box_top - box_h;

                pdf.layer
                    .set_fill_color(Color::Rgb(Rgb::new(0.95, 0.96, 0.98, None)));
                pdf.layer
                    .set_outline_color(Color::Rgb(Rgb::new(0.84, 0.87, 0.91, None)));
                pdf.layer.set_outline_thickness(0.4);
                pdf.layer.add_rect(
                    Rect::new(Mm(left), Mm(box_bottom), Mm(left + max_w), Mm(box_top))
                        .with_mode(PaintMode::FillStroke),
                );

                let mut text_y = box_top - pad - font_pt * PT_TO_MM;
                if idx == 0 && !info.is_empty() {
                    pdf.layer
                        .set_fill_color(Color::Rgb(Rgb::new(0.45, 0.50, 0.58, None)));
                    let info_label = pdf.prepare_text(&info.to_uppercase(), TextDirection::Ltr);
                    pdf.layer.use_text(
                        info_label,
                        7.5,
                        Mm(left + pad),
                        Mm(text_y + 0.5),
                        &pdf.fonts.bold,
                    );
                    text_y -= 4.5;
                }

                pdf.layer
                    .set_fill_color(Color::Rgb(Rgb::new(0.14, 0.16, 0.20, None)));
                for code_line in chunk {
                    let rendered_code = pdf.prepare_text(code_line, TextDirection::Ltr);
                    pdf.layer.use_text(
                        rendered_code,
                        font_pt,
                        Mm(left + pad),
                        Mm(text_y),
                        &pdf.fonts.mono,
                    );
                    text_y -= line_h;
                }

                pdf.y_mm = box_bottom - 3.0;
                idx += fit_count;
                if idx < wrapped_lines.len() {
                    pdf.new_page();
                }
            }
        }
        DocBlock::List {
            ordered,
            start,
            items,
            indent_level,
            ..
        } => {
            let extra_indent = *indent_level as f32 * 8.0;
            render_list(
                pdf,
                *ordered,
                *start,
                items,
                indent_mm + extra_indent,
                in_quote,
            );
            pdf.y_mm -= 2.0;
        }
        DocBlock::Table {
            alignments,
            header,
            rows,
            ..
        } => {
            render_table(pdf, alignments, header, rows, left, max_w);
            pdf.y_mm -= 3.5;
        }
        DocBlock::ThematicBreak { .. } => {
            pdf.ensure_space(6.0);
            let y = pdf.y_mm - 2.5;
            pdf.layer
                .set_outline_color(Color::Rgb(Rgb::new(0.76, 0.79, 0.84, None)));
            pdf.layer.set_outline_thickness(0.7);
            pdf.layer.add_line(Line {
                points: vec![
                    (Point::new(Mm(left), Mm(y)), false),
                    (Point::new(Mm(pdf.content_right_mm), Mm(y)), false),
                ],
                is_closed: false,
            });
            pdf.y_mm -= 6.0;
        }
        DocBlock::FootnoteDefinition { name, blocks, .. } => {
            pdf.ensure_space(8.0);
            let y_pt = (pdf.y_mm / PT_TO_MM + 4.0).min(pdf.page_h_mm / PT_TO_MM);
            let fn_norm = heading_slug(name);
            if !fn_norm.is_empty() {
                pdf.anchors
                    .entry(format!("fn-{fn_norm}"))
                    .or_insert((pdf.page_num, y_pt));
                pdf.anchors
                    .entry(fn_norm)
                    .or_insert((pdf.page_num, y_pt));
            }
            let fn_lower = name.trim().to_ascii_lowercase();
            if !fn_lower.is_empty() {
                pdf.anchors
                    .entry(format!("fn-{fn_lower}"))
                    .or_insert((pdf.page_num, y_pt));
            }
            let label = pdf.prepare_text(&format!("[^{name}]:"), TextDirection::Ltr);
            pdf.layer
                .set_fill_color(Color::Rgb(Rgb::new(0.11, 0.38, 0.76, None)));
            pdf.layer.use_text(
                label,
                9.0,
                Mm(left),
                Mm(pdf.y_mm - 9.0 * PT_TO_MM),
                &pdf.fonts.bold,
            );
            pdf.y_mm -= 4.2;
            for inner in blocks {
                render_block(pdf, inner, indent_mm + 6.0, true);
            }
            pdf.y_mm -= 1.5;
        }
    }
}

fn render_list(
    pdf: &mut PdfCursor,
    ordered: bool,
    start: usize,
    items: &[ListItem],
    indent_mm: f32,
    in_quote: bool,
) {
    let marker_x = pdf.content_left_mm + indent_mm + 2.0;
    let body_indent = indent_mm + 7.0;

    for (idx, item) in items.iter().enumerate() {
        pdf.ensure_space(6.0);
        let marker_y = pdf.y_mm - 3.3;

        match item.checked {
            Some(checked) => {
                // Draw vector checkbox
                let box_size = 2.8;
                pdf.layer
                    .set_outline_color(Color::Rgb(Rgb::new(0.24, 0.36, 0.56, None)));
                pdf.layer.set_outline_thickness(0.6);
                if checked {
                    pdf.layer
                        .set_fill_color(Color::Rgb(Rgb::new(0.22, 0.48, 0.82, None)));
                    pdf.layer.add_rect(
                        Rect::new(
                            Mm(marker_x),
                            Mm(marker_y - 0.3),
                            Mm(marker_x + box_size),
                            Mm(marker_y - 0.3 + box_size),
                        )
                        .with_mode(PaintMode::FillStroke),
                    );
                    pdf.layer
                        .set_outline_color(Color::Rgb(Rgb::new(1.0, 1.0, 1.0, None)));
                    pdf.layer.set_outline_thickness(0.7);
                    pdf.layer.add_line(Line {
                        points: vec![
                            (Point::new(Mm(marker_x + 0.5), Mm(marker_y + 1.0)), false),
                            (Point::new(Mm(marker_x + 1.2), Mm(marker_y + 0.3)), false),
                            (Point::new(Mm(marker_x + 2.3), Mm(marker_y + 2.0)), false),
                        ],
                        is_closed: false,
                    });
                } else {
                    pdf.layer.add_rect(
                        Rect::new(
                            Mm(marker_x),
                            Mm(marker_y - 0.3),
                            Mm(marker_x + box_size),
                            Mm(marker_y - 0.3 + box_size),
                        )
                        .with_mode(PaintMode::Stroke),
                    );
                }
            }
            None if ordered => {
                pdf.layer
                    .set_fill_color(Color::Rgb(Rgb::new(0.18, 0.20, 0.24, None)));
                pdf.layer.use_text(
                    format!("{}.", start + idx),
                    10.5,
                    Mm(marker_x - 1.0),
                    Mm(pdf.y_mm - 10.5 * PT_TO_MM),
                    &pdf.fonts.regular,
                );
            }
            None => {
                // Crisp vector bullet square/dot
                pdf.layer
                    .set_fill_color(Color::Rgb(Rgb::new(0.22, 0.26, 0.32, None)));
                pdf.layer.add_rect(
                    Rect::new(
                        Mm(marker_x + 0.6),
                        Mm(marker_y + 0.4),
                        Mm(marker_x + 1.9),
                        Mm(marker_y + 1.7),
                    )
                    .with_mode(PaintMode::Fill),
                );
            }
        }

        if item.blocks.is_empty() {
            pdf.y_mm -= 5.0;
        } else {
            for b in &item.blocks {
                render_block(pdf, b, body_indent, in_quote);
            }
        }
    }
}

fn render_table(
    pdf: &mut PdfCursor,
    alignments: &[TableAlign],
    header: &[Vec<InlineRun>],
    rows: &[Vec<Vec<InlineRun>>],
    left: f32,
    max_w: f32,
) {
    let cols = header
        .len()
        .max(alignments.len())
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0))
        .max(1);
    let col_w = max_w / cols as f32;
    let font_pt = 9.5;
    let line_h = font_pt * PT_TO_MM * 1.38;
    let pad_x = 2.2;
    let pad_y = 2.0;

    let layout_row =
        |cells: &[Vec<InlineRun>], is_header: bool, unicode: bool| -> (Vec<Vec<StyledLine>>, f32) {
            let mut wrapped_cells = Vec::with_capacity(cols);
            let mut max_lines = 1usize;
            for c in 0..cols {
                let runs = cells.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
                let wrapped = wrap_runs(
                    runs,
                    (col_w - 2.0 * pad_x).max(10.0),
                    font_pt,
                    is_header,
                    unicode,
                );
                max_lines = max_lines.max(wrapped.len().max(1));
                wrapped_cells.push(wrapped);
            }
            let row_h = max_lines as f32 * line_h + 2.0 * pad_y;
            (wrapped_cells, row_h)
        };

    let draw_prepared_row = |pdf: &mut PdfCursor,
                             wrapped_cells: &[Vec<StyledLine>],
                             row_h: f32,
                             is_header: bool,
                             row_idx: usize| {
            pdf.ensure_space(row_h);

            let top_y = pdf.y_mm;
            let bot_y = top_y - row_h;

            // Background fill
            if is_header {
                pdf.layer
                    .set_fill_color(Color::Rgb(Rgb::new(0.90, 0.93, 0.97, None)));
                pdf.layer.add_rect(
                    Rect::new(Mm(left), Mm(bot_y), Mm(left + max_w), Mm(top_y))
                        .with_mode(PaintMode::Fill),
                );
            } else if row_idx % 2 == 1 {
                pdf.layer
                    .set_fill_color(Color::Rgb(Rgb::new(0.97, 0.98, 0.99, None)));
                pdf.layer.add_rect(
                    Rect::new(Mm(left), Mm(bot_y), Mm(left + max_w), Mm(top_y))
                        .with_mode(PaintMode::Fill),
                );
            }

            // Borders and cell text
            pdf.layer
                .set_outline_color(Color::Rgb(Rgb::new(0.75, 0.79, 0.85, None)));
            pdf.layer.set_outline_thickness(0.4);
            for (c, cell_lines) in wrapped_cells.iter().enumerate() {
                let cx = left + c as f32 * col_w;
                pdf.layer.add_rect(
                    Rect::new(Mm(cx), Mm(bot_y), Mm(cx + col_w), Mm(top_y))
                        .with_mode(PaintMode::Stroke),
                );

                let align = alignments.get(c).copied().unwrap_or(TableAlign::None);
                for (li, line_seg) in cell_lines.iter().enumerate() {
                    let text_w = measure_line_width_mm(line_seg, font_pt, is_header);
                    let text_x = match align {
                        TableAlign::Center => cx + ((col_w - text_w) * 0.5).max(pad_x),
                        TableAlign::Right => (cx + col_w - pad_x - text_w).max(cx + pad_x),
                        _ => cx + pad_x,
                    };
                    let text_y = top_y - pad_y - font_pt * PT_TO_MM - li as f32 * line_h;
                    draw_styled_line(
                        pdf,
                        line_seg,
                        text_x,
                        text_y,
                        font_pt,
                        is_header,
                        false,
                        TextDirection::Ltr,
                    );
                }
            }

            pdf.y_mm = bot_y;
        };

    let unicode = pdf.fonts.unicode_enabled;
    let header_layout = if !header.is_empty() {
        let h = layout_row(header, true, unicode);
        draw_prepared_row(pdf, &h.0, h.1, true, 0);
        Some(h)
    } else {
        None
    };

    for (r_idx, row) in rows.iter().enumerate() {
        let (wrapped_row, row_h) = layout_row(row, false, unicode);
        if pdf.y_mm - row_h < pdf.bottom_y_mm {
            pdf.new_page();
            if let Some((ref h_cells, h_height)) = header_layout {
                draw_prepared_row(pdf, h_cells, h_height, true, 0);
            }
        }
        draw_prepared_row(pdf, &wrapped_row, row_h, false, r_idx);
    }
}

#[derive(Clone)]
struct StyledSegment {
    run: InlineRun,
    width_mm: f32,
}

type StyledLine = Vec<StyledSegment>;

fn classify_pdf_link_target(run: &InlineRun) -> Option<PdfLinkTarget> {
    if run.is_footnote_ref {
        let name = run.text.trim_matches(|c| matches!(c, '[' | ']' | '^'));
        let slug = heading_slug(name);
        if !slug.is_empty() {
            return Some(PdfLinkTarget::Anchor(format!("fn-{slug}")));
        }
        return None;
    }
    let raw = run
        .link_url
        .as_deref()?
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(anchor) = raw.strip_prefix('#') {
        if !anchor.trim().is_empty() {
            return Some(PdfLinkTarget::Anchor(anchor.to_string()));
        }
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        Some(PdfLinkTarget::Uri(raw.to_string()))
    } else if lower.starts_with("www.") {
        Some(PdfLinkTarget::Uri(format!("https://{raw}")))
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_styled_line(
    pdf: &mut PdfCursor,
    line: &[StyledSegment],
    start_x: f32,
    baseline_y: f32,
    font_pt: f32,
    force_bold: bool,
    muted: bool,
    direction: TextDirection,
) {
    let mut x = start_x;
    let ordered: Vec<&StyledSegment> = if direction == TextDirection::Rtl {
        line.iter().rev().collect()
    } else {
        line.iter().collect()
    };

    for seg in ordered {
        let base_pt = if seg.run.code {
            (font_pt * 0.9).max(8.0)
        } else {
            font_pt
        };
        let (draw_pt, y_baseline) = if seg.run.is_footnote_ref {
            (
                (base_pt * 0.75).max(7.0),
                baseline_y + base_pt * PT_TO_MM * 0.35,
            )
        } else {
            (base_pt, baseline_y)
        };

        if seg.run.code {
            pdf.layer
                .set_fill_color(Color::Rgb(Rgb::new(0.93, 0.94, 0.96, None)));
            pdf.layer.add_rect(
                Rect::new(
                    Mm(x - 0.3),
                    Mm(baseline_y - 0.8),
                    Mm(x + seg.width_mm + 0.3),
                    Mm(baseline_y + draw_pt * PT_TO_MM * 0.95),
                )
                .with_mode(PaintMode::Fill),
            );
        }

        if seg.run.link_url.is_some() || seg.run.is_footnote_ref {
            pdf.layer
                .set_fill_color(Color::Rgb(Rgb::new(0.11, 0.38, 0.76, None)));
        } else if muted {
            pdf.layer
                .set_fill_color(Color::Rgb(Rgb::new(0.32, 0.36, 0.42, None)));
        } else if seg.run.is_math {
            pdf.layer
                .set_fill_color(Color::Rgb(Rgb::new(0.08, 0.16, 0.30, None)));
        } else {
            pdf.layer
                .set_fill_color(Color::Rgb(Rgb::new(0.10, 0.12, 0.16, None)));
        }

        let prepared = pdf.prepare_text(&seg.run.text, direction);
        let font = pdf.font_for_run(&seg.run, force_bold);
        pdf.layer.use_text(
            prepared,
            draw_pt,
            Mm(x),
            Mm(y_baseline),
            font,
        );

        if seg.run.link_url.is_some() || seg.run.underline {
            let line_color = if seg.run.link_url.is_some() {
                Rgb::new(0.11, 0.38, 0.76, None)
            } else {
                Rgb::new(0.15, 0.17, 0.22, None)
            };
            pdf.layer.set_outline_color(Color::Rgb(line_color));
            pdf.layer.set_outline_thickness(0.35);
            pdf.layer.add_line(Line {
                points: vec![
                    (Point::new(Mm(x), Mm(y_baseline - 0.5)), false),
                    (Point::new(Mm(x + seg.width_mm), Mm(y_baseline - 0.5)), false),
                ],
                is_closed: false,
            });
        }
        if seg.run.strikethrough {
            pdf.layer
                .set_outline_color(Color::Rgb(Rgb::new(0.25, 0.25, 0.25, None)));
            pdf.layer.set_outline_thickness(0.4);
            let strike_y = y_baseline + draw_pt * PT_TO_MM * 0.32;
            pdf.layer.add_line(Line {
                points: vec![
                    (Point::new(Mm(x), Mm(strike_y)), false),
                    (Point::new(Mm(x + seg.width_mm), Mm(strike_y)), false),
                ],
                is_closed: false,
            });
        }

        if seg.width_mm > 0.0
            && let Some(target) = classify_pdf_link_target(&seg.run) {
                let x1_pt = x / PT_TO_MM;
                let y1_pt = ((y_baseline - 1.2) / PT_TO_MM).max(0.0);
                let x2_pt = (x + seg.width_mm) / PT_TO_MM;
                let y2_pt = (y_baseline + draw_pt * PT_TO_MM * 1.05) / PT_TO_MM;
                pdf.link_annotations.push(PdfLinkAnnotation {
                    page_num: pdf.page_num,
                    rect_pt: [x1_pt, y1_pt, x2_pt, y2_pt],
                    target,
                });
            }

        x += seg.width_mm;
    }
}

fn measure_line_width_mm(line: &[StyledSegment], font_pt: f32, force_bold: bool) -> f32 {
    let _ = (font_pt, force_bold);
    line.iter().map(|s| s.width_mm).sum()
}

#[inline]
fn approx_char_width_mm(c: char, font_pt: f32, bold: bool, mono: bool) -> f32 {
    let scale = font_pt * PT_TO_MM;
    if mono {
        return 0.60 * scale;
    }
    let factor = if bold { 1.07 } else { 1.0 };
    let unit = match c {
        'i' | 'l' | 'I' | '!' | '.' | ',' | ':' | ';' | '|' | '\'' => 0.28,
        'f' | 'j' | 'r' | 't' | '(' | ')' | '[' | ']' | '-' | ' ' => 0.34,
        'm' | 'w' | 'M' | 'W' | '@' | '%' => 0.80,
        c if (c as u32) >= 0x2E80 => 0.95,
        c if !c.is_ascii() => 0.52,
        c if c.is_uppercase() => 0.64,
        _ => 0.51,
    };
    unit * scale * factor
}

fn approx_text_width_mm(
    text: &str,
    font_pt: f32,
    bold: bool,
    mono: bool,
    unicode_enabled: bool,
) -> f32 {
    if unicode_enabled {
        text.chars()
            .filter(|&c| c != '\r' && c != '\n')
            .map(|c| approx_char_width_mm(c, font_pt, bold, mono))
            .sum()
    } else {
        let sanitized = sanitize_pdf_text(text);
        sanitized
            .chars()
            .map(|c| approx_char_width_mm(c, font_pt, bold, mono))
            .sum()
    }
}

fn wrap_runs(
    runs: &[InlineRun],
    max_width_mm: f32,
    font_pt: f32,
    force_bold: bool,
    unicode_enabled: bool,
) -> Vec<StyledLine> {
    let mut lines: Vec<StyledLine> = vec![Vec::new()];
    let mut cur_w = 0.0_f32;

    for run in runs {
        let math_plain;
        let mut remaining = if run.is_math {
            math_plain = crate::document::math::math_to_plain_text(&run.text);
            math_plain.as_str()
        } else {
            run.text.as_str()
        };
        while !remaining.is_empty() {
            if let Some(nl_pos) = remaining.find('\n') {
                let before = &remaining[..nl_pos];
                if !before.is_empty() {
                    push_words_wrapped(
                        before,
                        run,
                        max_width_mm,
                        font_pt,
                        force_bold,
                        unicode_enabled,
                        &mut lines,
                        &mut cur_w,
                    );
                }
                lines.push(Vec::new());
                cur_w = 0.0;
                remaining = &remaining[nl_pos + 1..];
            } else {
                push_words_wrapped(
                    remaining,
                    run,
                    max_width_mm,
                    font_pt,
                    force_bold,
                    unicode_enabled,
                    &mut lines,
                    &mut cur_w,
                );
                break;
            }
        }
    }

    if lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn push_words_wrapped(
    text: &str,
    template: &InlineRun,
    max_width_mm: f32,
    font_pt: f32,
    force_bold: bool,
    unicode_enabled: bool,
    lines: &mut Vec<StyledLine>,
    cur_w: &mut f32,
) {
    let draw_pt = if template.code {
        (font_pt * 0.9).max(8.0)
    } else if template.is_footnote_ref {
        (font_pt * 0.75).max(7.0)
    } else {
        font_pt
    };
    let bold = template.bold || force_bold;

    // Split preserving whitespace tokens
    let mut start = 0;
    let bytes = text.as_bytes();
    while start < text.len() {
        let is_ws = bytes[start] == b' ';
        let mut end = start;
        while end < text.len() {
            if text.is_char_boundary(end) {
                let ch_ws = bytes[end] == b' ';
                if ch_ws != is_ws {
                    break;
                }
            }
            end += 1;
        }
        let token = &text[start..end];
        start = end;

        let token_w = approx_text_width_mm(token, draw_pt, bold, template.code, unicode_enabled);
        if *cur_w > 0.0 && *cur_w + token_w > max_width_mm {
            if is_ws {
                lines.push(Vec::new());
                *cur_w = 0.0;
                continue;
            }
            lines.push(Vec::new());
            *cur_w = 0.0;
        }

        // If a single word exceeds max_width_mm, hard-split by char in O(len)
        if token_w > max_width_mm && !is_ws {
            let mut chunk = String::new();
            let mut chunk_w = 0.0_f32;
            let mut chunk_chars = 0usize;
            for ch in token.chars() {
                if ch == '\r' || ch == '\n' {
                    chunk.push(ch);
                    continue;
                }
                let ch_w = approx_char_width_mm(ch, draw_pt, bold, template.code);
                if chunk_chars > 0 && chunk_w + ch_w > max_width_mm {
                    append_seg(lines, template, std::mem::take(&mut chunk), chunk_w);
                    lines.push(Vec::new());
                    *cur_w = 0.0;
                    chunk_w = 0.0;
                    chunk_chars = 0;
                }
                chunk.push(ch);
                chunk_w += ch_w;
                chunk_chars += 1;
            }
            if !chunk.is_empty() {
                append_seg(lines, template, chunk, chunk_w);
                *cur_w += chunk_w;
            }
        } else {
            append_seg(lines, template, token.to_string(), token_w);
            *cur_w += token_w;
        }
    }
}

fn append_seg(lines: &mut [StyledLine], template: &InlineRun, text: String, width_mm: f32) {
    if text.is_empty() {
        return;
    }
    let display_text = if template.image_url.is_some() {
        format!("[Image: {text}]")
    } else {
        text
    };
    if let Some(last_line) = lines.last_mut() {
        if let Some(prev) = last_line.last_mut()
            && prev.run.image_url == template.image_url
                && prev.run.is_footnote_ref == template.is_footnote_ref
                && prev.run.is_math == template.is_math
                && prev.run.bold == template.bold
                && prev.run.italic == template.italic
                && prev.run.underline == template.underline
                && prev.run.strikethrough == template.strikethrough
                && prev.run.code == template.code
                && prev.run.link_url == template.link_url
            {
                prev.run.text.push_str(&display_text);
                prev.width_mm += width_mm;
                return;
            }
        let mut run = template.clone();
        run.text = display_text;
        last_line.push(StyledSegment { run, width_mm });
    }
}

fn wrap_code_lines(code: &str, max_width_mm: f32, font_pt: f32) -> Vec<String> {
    let char_w = 0.60 * font_pt * PT_TO_MM;
    let max_chars = (max_width_mm / char_w).floor().max(16.0) as usize;
    let mut out = Vec::new();
    for raw_line in code.lines() {
        let expanded = raw_line.replace('\t', "    ");
        if expanded.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut cur_chars = 0usize;
        for ch in expanded.chars() {
            cur.push(ch);
            cur_chars += 1;
            if cur_chars >= max_chars {
                out.push(std::mem::take(&mut cur));
                cur_chars = 0;
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Maps common Unicode typography to ASCII/WinAnsi safe equivalents for built-in PDF Type1 fonts.
pub fn sanitize_pdf_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\u{2018}' | '\u{2019}' => out.push('\''),
            '\u{201C}' | '\u{201D}' => out.push('"'),
            '\u{2013}' => out.push('-'),
            '\u{2014}' => out.push_str("--"),
            '\u{2026}' => out.push_str("..."),
            '\u{2022}' => out.push('*'),
            '\u{00A0}' => out.push(' '),
            '\u{2192}' => out.push_str("->"),
            '\u{2190}' => out.push_str("<-"),
            '\u{2264}' => out.push_str("<="),
            '\u{2265}' => out.push_str(">="),
            c if (c as u32) <= 0xFF && c != '\r' && c != '\n' => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_footer_template_expands_tokens_and_iso_date() {
        let expanded = expand_header_footer_template(
            "{title} — Page {page} of {pages} ({date})",
            2,
            5,
            "Architecture",
            "2026-11-22",
        );
        assert_eq!(expanded, "Architecture — Page 2 of 5 (2026-11-22)");

        let date = current_iso_date();
        assert_eq!(date.len(), 10);
        assert_eq!(&date[4..5], "-");
        assert_eq!(&date[7..8], "-");
    }

    #[test]
    fn rgba_to_pdf_rgb_bytes_composites_alpha_and_rejects_empty() {
        let empty = image::RgbaImage::new(0, 0);
        assert!(rgba_to_pdf_rgb_bytes(&empty).is_none());

        let mut img = image::RgbaImage::new(1, 2);
        img.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        img.put_pixel(0, 1, image::Rgba([100, 150, 200, 255]));
        let (w, h, rgb) = rgba_to_pdf_rgb_bytes(&img).expect("valid RGB bytes");
        assert_eq!((w, h), (1, 2));
        assert_eq!(rgb, vec![255, 255, 255, 100, 150, 200]);
    }
}
