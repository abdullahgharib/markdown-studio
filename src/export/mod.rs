pub mod docx;
pub mod html;
pub mod ir;
pub mod json;
pub mod pdf;
pub mod print;

use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread;

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use ropey::Rope;

use crate::document::ast::parse_rope_to_ir;
use crate::export::print::PrintJobOptions;

const MAX_EXPORT_IMAGE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXPORT_IMAGE_PIXELS: u64 = 64_000_000;
const MAX_EXPORT_SVG_UNCOMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const MAX_EXPORT_SVG_EMBEDDED_DATA_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn is_unc_or_network_path(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    let is_non_verbatim_unc = raw.starts_with(r"\\") && !raw.starts_with(r"\\?\");
    let is_verbatim_unc = raw
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case(r"\\?\UNC\"));
    if is_non_verbatim_unc || is_verbatim_unc || raw.starts_with("//") {
        return true;
    }
    matches!(
        path.components().next(),
        Some(std::path::Component::Prefix(p))
            if matches!(
                p.kind(),
                std::path::Prefix::UNC(_, _) | std::path::Prefix::VerbatimUNC(_, _)
            )
    )
}

fn export_svg_fontdb() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTDB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    Arc::clone(FONTDB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    }))
}

fn export_safe_svg_href_resolver() -> resvg::usvg::ImageHrefResolver<'static> {
    resvg::usvg::ImageHrefResolver {
        resolve_string: Box::new(|_, _| None),
        resolve_data: Box::new(|mime, data, _opts| {
            if data.len() > MAX_EXPORT_SVG_EMBEDDED_DATA_BYTES {
                return None;
            }
            let fmt_hint = match mime {
                "image/png" => Some(ImageFormat::Png),
                "image/jpg" | "image/jpeg" => Some(ImageFormat::Jpeg),
                "image/gif" => Some(ImageFormat::Gif),
                "image/webp" => Some(ImageFormat::WebP),
                _ => None,
            };
            let reader = match fmt_hint {
                Some(fmt) => ImageReader::with_format(Cursor::new(data.as_slice()), fmt),
                None => ImageReader::new(Cursor::new(data.as_slice()))
                    .with_guessed_format()
                    .ok()?,
            };
            let actual_fmt = reader.format()?;
            let (w, h) = reader.into_dimensions().ok()?;
            if w == 0 || h == 0 || (w as u64).saturating_mul(h as u64) > MAX_EXPORT_IMAGE_PIXELS {
                return None;
            }
            match actual_fmt {
                ImageFormat::Jpeg => Some(resvg::usvg::ImageKind::JPEG(data)),
                ImageFormat::Png => Some(resvg::usvg::ImageKind::PNG(data)),
                ImageFormat::Gif => Some(resvg::usvg::ImageKind::GIF(data)),
                ImageFormat::WebP => Some(resvg::usvg::ImageKind::WEBP(data)),
                _ => None,
            }
        }),
    }
}

/// Resolves a local image asset referenced during export, strictly confined to `base_dir`
/// and blocking UNC network paths, control chars, `file://` URLs, and directory traversal.
pub(crate) fn resolve_confined_export_path(raw_url: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    let trimmed = raw_url
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    if trimmed.is_empty() || trimmed.chars().any(|c| c.is_control()) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("data:")
        || lower.starts_with("file:")
        || lower.starts_with("javascript:")
        || lower.starts_with("vbscript:")
    {
        return None;
    }
    if trimmed.starts_with(r"\\") || trimmed.starts_with("//") {
        return None;
    }

    let base = base_dir?;
    let canon_base = base.canonicalize().ok()?;
    let decoded = trimmed.replace("%20", " ");

    for candidate in [trimmed, decoded.as_str()] {
        let p = Path::new(candidate);
        if is_unc_or_network_path(p) {
            return None;
        }
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.join(p)
        };
        if let Ok(canon) = joined.canonicalize()
            && canon.starts_with(&canon_base)
            && canon.is_file()
        {
            return Some(canon);
        }
    }
    None
}

fn read_confined_export_bytes(
    raw_url: &str,
    base_dir: Option<&Path>,
    max_bytes: u64,
) -> Option<(PathBuf, Vec<u8>)> {
    let canon = resolve_confined_export_path(raw_url, base_dir)?;
    let file = File::open(&canon).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len().min(max_bytes) as usize);
    file.take(max_bytes + 1).read_to_end(&mut bytes).ok()?;
    if (bytes.len() as u64) > max_bytes || bytes.is_empty() {
        return None;
    }
    Some((canon, bytes))
}

fn looks_like_svg_bytes(bytes: &[u8], path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("svg" | "svgz")) {
        return true;
    }
    let head = &bytes[..bytes.len().min(512)];
    if let Ok(s) = std::str::from_utf8(head) {
        let l = s.trim_start_matches('\u{feff}').trim_start().to_ascii_lowercase();
        return l.starts_with("<svg") || (l.starts_with("<?xml") && l.contains("<svg"));
    }
    false
}

fn rasterize_svg_for_export(bytes: &[u8], max_dim: u32) -> Option<image::RgbaImage> {
    let decompressed_owned;
    let xml_bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::read::GzDecoder::new(bytes)
            .take((MAX_EXPORT_SVG_UNCOMPRESSED_BYTES + 1) as u64);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).ok()?;
        if out.len() > MAX_EXPORT_SVG_UNCOMPRESSED_BYTES {
            return None;
        }
        decompressed_owned = out;
        decompressed_owned.as_slice()
    } else {
        bytes
    };
    if xml_bytes.len() > MAX_EXPORT_SVG_UNCOMPRESSED_BYTES {
        return None;
    }
    let xml_str = std::str::from_utf8(xml_bytes).ok()?;
    let opt = resvg::usvg::Options {
        fontdb: export_svg_fontdb(),
        image_href_resolver: export_safe_svg_href_resolver(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(xml_str, &opt).ok()?;
    let size = tree.size();
    let base_w = size.width().max(1.0);
    let base_h = size.height().max(1.0);
    let mut scale = 2.0_f32;
    let max_d = max_dim.max(1) as f32;
    if base_w * scale > max_d {
        scale = max_d / base_w;
    }
    if base_h * scale > max_d {
        scale = scale.min(max_d / base_h);
    }
    let px_w = (base_w * scale).round().clamp(1.0, max_d) as u32;
    let px_h = (base_h * scale).round().clamp(1.0, max_d) as u32;
    if (px_w as u64).saturating_mul(px_h as u64) > MAX_EXPORT_IMAGE_PIXELS {
        return None;
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px_w, px_h)?;
    let transform =
        resvg::tiny_skia::Transform::from_scale(px_w as f32 / base_w, px_h as f32 / base_h);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // Un-premultiply RGBA for standard RgbaImage consumers
    let mut raw = pixmap.take();
    for px in raw.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a > 0 && a < 255 {
            let af = a as f32 / 255.0;
            px[0] = ((px[0] as f32 / af).round()).clamp(0.0, 255.0) as u8;
            px[1] = ((px[1] as f32 / af).round()).clamp(0.0, 255.0) as u8;
            px[2] = ((px[2] as f32 / af).round()).clamp(0.0, 255.0) as u8;
        }
    }
    image::RgbaImage::from_raw(px_w, px_h, raw)
}

fn decode_bounded_raster(bytes: &[u8], max_dim: u32) -> Option<image::RgbaImage> {
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        let dec = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).ok()?;
        let (w, h) = dec.dimensions();
        if w == 0 || h == 0 || (w as u64).saturating_mul(h as u64) > MAX_EXPORT_IMAGE_PIXELS {
            return None;
        }
    }
    let dim_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let (w, h) = dim_reader.into_dimensions().ok()?;
    if w == 0 || h == 0 || (w as u64).saturating_mul(h as u64) > MAX_EXPORT_IMAGE_PIXELS {
        return None;
    }
    let mut limits = Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    let mut dyn_img = reader.decode().ok()?;
    if dyn_img.width() > max_dim || dyn_img.height() > max_dim {
        dyn_img = dyn_img.resize(max_dim, max_dim, image::imageops::FilterType::Triangle);
    }
    Some(dyn_img.to_rgba8())
}

/// Safely loads and downscales a local image (`<= 64 MiB`, `<= 64 MP`, confined to `base_dir`,
/// supporting PNG/JPEG/GIF/WebP/TIFF/BMP and SVG/SVGZ) for PDF/DOCX export.
pub(crate) fn load_safe_export_rgba(
    raw_url: &str,
    base_dir: Option<&Path>,
    max_dim: u32,
) -> Option<image::RgbaImage> {
    let (canon, bytes) = read_confined_export_bytes(raw_url, base_dir, MAX_EXPORT_IMAGE_FILE_BYTES)?;
    if looks_like_svg_bytes(&bytes, &canon) {
        return rasterize_svg_for_export(&bytes, max_dim);
    }
    decode_bounded_raster(&bytes, max_dim)
}

/// Safely loads and validates a local image for HTML data-URI embedding, confining paths to
/// `base_dir`, rasterizing SVG/SVGZ to PNG, and rejecting non-image payloads.
pub(crate) fn load_safe_export_web_image(
    raw_url: &str,
    base_dir: Option<&Path>,
    max_svg_dim: u32,
) -> Option<(&'static str, Vec<u8>)> {
    let (canon, bytes) =
        read_confined_export_bytes(raw_url, base_dir, 32 * 1024 * 1024)?;
    if looks_like_svg_bytes(&bytes, &canon) {
        let rgba = rasterize_svg_for_export(&bytes, max_svg_dim)?;
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(rgba)
            .write_to(&mut png, ImageFormat::Png)
            .ok()?;
        return Some(("image/png", png.into_inner()));
    }
    let reader = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?;
    let fmt = reader.format()?;
    let (w, h) = reader.into_dimensions().ok()?;
    if w == 0 || h == 0 || (w as u64).saturating_mul(h as u64) > MAX_EXPORT_IMAGE_PIXELS {
        return None;
    }
    match fmt {
        ImageFormat::Png => Some(("image/png", bytes)),
        ImageFormat::Jpeg => Some(("image/jpeg", bytes)),
        ImageFormat::Gif => Some(("image/gif", bytes)),
        ImageFormat::WebP => Some(("image/webp", bytes)),
        ImageFormat::Bmp | ImageFormat::Tiff => {
            let rgba = decode_bounded_raster(&bytes, max_svg_dim)?;
            let mut png = Cursor::new(Vec::new());
            DynamicImage::ImageRgba8(rgba)
                .write_to(&mut png, ImageFormat::Png)
                .ok()?;
            Some(("image/png", png.into_inner()))
        }
        _ => None,
    }
}

/// Encodes arbitrary bytes to standard RFC 4648 Base64 without padding omissions.
pub(crate) fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Supported background export / print job kinds.
#[derive(Debug, Clone)]
pub enum ExportTarget {
    Pdf(PathBuf),
    PdfWithOptions {
        path: PathBuf,
        layout: pdf::PdfLayoutOptions,
    },
    Docx(PathBuf),
    Html(PathBuf),
    Json {
        path: PathBuf,
        source_path: Option<PathBuf>,
    },
    Print { spool_only: bool },
    PrintWithOptions(PrintJobOptions),
}

/// Progress/completion messages streamed back from background worker threads.
#[derive(Debug, Clone)]
pub enum ExportEvent {
    Started { label: String },
    Progress { fraction: f32, stage: String },
    Completed { message: String, output_path: Option<PathBuf> },
    Failed { error: String },
}

struct ExportRequest {
    rope: Rope,
    title: String,
    target: ExportTarget,
    base_dir: Option<PathBuf>,
    cancel: Arc<AtomicBool>,
}

/// Non-blocking export & print pipeline running on a dedicated `std::thread` worker.
pub struct BackgroundExporter {
    tx: Sender<ExportRequest>,
    rx: Receiver<ExportEvent>,
    cancel_flag: Arc<AtomicBool>,
    pub busy: bool,
    pub progress: f32,
    pub status_text: String,
}

impl Default for BackgroundExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundExporter {
    pub fn new() -> Self {
        Self::spawn_with_ctx(None)
    }

    pub fn with_repaint_ctx(ctx: egui::Context) -> Self {
        Self::spawn_with_ctx(Some(ctx))
    }

    fn spawn_with_ctx(repaint_ctx: Option<egui::Context>) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<ExportRequest>();
        let (ev_tx, ev_rx) = mpsc::channel::<ExportEvent>();
        let notify = move || {
            if let Some(ref c) = repaint_ctx {
                c.request_repaint();
            }
        };

        thread::Builder::new()
            .name("md-exporter".to_string())
            .spawn(move || {
                while let Ok(req) = req_rx.recv() {
                    let label = match &req.target {
                        ExportTarget::Pdf(p) | ExportTarget::PdfWithOptions { path: p, .. } => {
                            format!("Exporting PDF to {}", p.display())
                        }
                        ExportTarget::Docx(p) => format!("Exporting DOCX to {}", p.display()),
                        ExportTarget::Html(p) => format!("Exporting HTML to {}", p.display()),
                        ExportTarget::Json { path, .. } => {
                            format!("Exporting JSON to {}", path.display())
                        }
                        ExportTarget::Print { .. } | ExportTarget::PrintWithOptions(_) => {
                            "Preparing print spool PDF...".to_string()
                        }
                    };
                    let _ = ev_tx.send(ExportEvent::Started { label });
                    notify();
                    let _ = ev_tx.send(ExportEvent::Progress {
                        fraction: 0.15,
                        stage: "Parsing Markdown AST...".to_string(),
                    });
                    notify();

                    if req.cancel.load(Ordering::Acquire) {
                        let _ = ev_tx.send(ExportEvent::Failed {
                            error: "Export cancelled".to_string(),
                        });
                        notify();
                        continue;
                    }

                    let blocks = parse_rope_to_ir(&req.rope);
                    let is_json = matches!(req.target, ExportTarget::Json { .. });
                    let rope_for_json = if is_json { Some(req.rope) } else { drop(req.rope); None };

                    let base_dir = req.base_dir.as_deref();
                    let cancel_ref = &req.cancel;
                    let result: Result<(String, Option<PathBuf>), String> = match req.target {
                        ExportTarget::Pdf(path) => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = -1i32;
                            pdf::export_pdf_to_file_with_options_and_base_dir(
                                &blocks,
                                &req.title,
                                &path,
                                pdf::PdfLayoutOptions::default(),
                                base_dir,
                                |f| {
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        let _ = tx_prog.send(ExportEvent::Progress {
                                            fraction: 0.2 + 0.75 * f,
                                            stage: "Compiling vector PDF pages...".to_string(),
                                        });
                                        notify();
                                    }
                                },
                            )
                            .and_then(|()| {
                                if cancel_ref.load(Ordering::Acquire) {
                                    Err("Export cancelled".to_string())
                                } else {
                                    Ok((format!("Saved PDF: {}", path.display()), Some(path)))
                                }
                            })
                        }
                        ExportTarget::PdfWithOptions { path, layout } => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = -1i32;
                            pdf::export_pdf_to_file_with_options_and_base_dir(
                                &blocks,
                                &req.title,
                                &path,
                                layout,
                                base_dir,
                                |f| {
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        let _ = tx_prog.send(ExportEvent::Progress {
                                            fraction: 0.2 + 0.75 * f,
                                            stage: "Compiling vector PDF pages...".to_string(),
                                        });
                                        notify();
                                    }
                                },
                            )
                            .and_then(|()| {
                                if cancel_ref.load(Ordering::Acquire) {
                                    Err("Export cancelled".to_string())
                                } else {
                                    Ok((format!("Saved PDF: {}", path.display()), Some(path)))
                                }
                            })
                        }
                        ExportTarget::Docx(path) => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = -1i32;
                            docx::export_docx_to_file_with_base_dir(
                                &blocks,
                                &path,
                                base_dir,
                                |f| {
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        let _ = tx_prog.send(ExportEvent::Progress {
                                            fraction: 0.2 + 0.75 * f,
                                            stage: "Building OpenXML Word document...".to_string(),
                                        });
                                        notify();
                                    }
                                },
                            )
                            .and_then(|()| {
                                if cancel_ref.load(Ordering::Acquire) {
                                    Err("Export cancelled".to_string())
                                } else {
                                    Ok((format!("Saved Word document: {}", path.display()), Some(path)))
                                }
                            })
                        }
                        ExportTarget::Html(path) => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = -1i32;
                            html::export_html_to_file_with_base_dir(
                                &blocks,
                                &req.title,
                                &path,
                                base_dir,
                                |f| {
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        let _ = tx_prog.send(ExportEvent::Progress {
                                            fraction: 0.2 + 0.75 * f,
                                            stage: "Rendering standalone HTML...".to_string(),
                                        });
                                        notify();
                                    }
                                },
                            )
                            .and_then(|()| {
                                if cancel_ref.load(Ordering::Acquire) {
                                    Err("Export cancelled".to_string())
                                } else {
                                    Ok((format!("Saved HTML: {}", path.display()), Some(path)))
                                }
                            })
                        }
                        ExportTarget::Json { path, source_path } => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = -1i32;
                            let rope_ref = rope_for_json.as_ref().expect("rope kept for JSON");
                            json::export_json_to_file_from_rope(
                                &blocks,
                                &req.title,
                                source_path.as_deref(),
                                rope_ref,
                                &path,
                                |f| {
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        let _ = tx_prog.send(ExportEvent::Progress {
                                            fraction: 0.2 + 0.75 * f,
                                            stage: "Serializing structured JSON...".to_string(),
                                        });
                                        notify();
                                    }
                                },
                            )
                            .map(|()| (format!("Saved JSON: {}", path.display()), Some(path)))
                        }
                        ExportTarget::Print { spool_only } => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = -1i32;
                            match print::create_print_spool_pdf_with_options_and_base_dir(
                                &blocks,
                                &req.title,
                                &PrintJobOptions::default(),
                                base_dir,
                                |f| {
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        let _ = tx_prog.send(ExportEvent::Progress {
                                            fraction: 0.2 + 0.65 * f,
                                            stage: "Rendering print spool PDF...".to_string(),
                                        });
                                        notify();
                                    }
                                },
                            ) {
                                Ok(spool_path) => {
                                    if spool_only {
                                        Ok((
                                            format!("Created spool PDF: {}", spool_path.display()),
                                            Some(spool_path),
                                        ))
                                    } else {
                                        let _ = ev_tx.send(ExportEvent::Progress {
                                            fraction: 0.9,
                                            stage: "Sending to OS print spooler...".to_string(),
                                        });
                                        notify();
                                        print::dispatch_to_os_printer(&spool_path)
                                            .map(|msg| (msg, Some(spool_path)))
                                    }
                                }
                                Err(e) => Err(e),
                            }
                        }
                        ExportTarget::PrintWithOptions(opts) => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = -1i32;
                            match print::create_print_spool_pdf_with_options_and_base_dir(
                                &blocks,
                                &req.title,
                                &opts,
                                base_dir,
                                |f| {
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        let _ = tx_prog.send(ExportEvent::Progress {
                                            fraction: 0.2 + 0.65 * f,
                                            stage: "Rendering print spool PDF...".to_string(),
                                        });
                                        notify();
                                    }
                                },
                            ) {
                                Ok(spool_path) => {
                                    let _ = ev_tx.send(ExportEvent::Progress {
                                        fraction: 0.9,
                                        stage: "Sending print job to printer...".to_string(),
                                    });
                                    notify();
                                    print::dispatch_to_os_printer_with_options(&spool_path, &opts)
                                        .map(|msg| (msg, Some(spool_path)))
                                }
                                Err(e) => Err(e),
                            }
                        }
                    };

                    match result {
                        Ok((message, output_path)) => {
                            let _ = ev_tx.send(ExportEvent::Completed {
                                message,
                                output_path,
                            });
                        }
                        Err(error) => {
                            let _ = ev_tx.send(ExportEvent::Failed { error });
                        }
                    }
                    notify();
                }
            })
            .expect("failed to spawn background exporter thread");

        Self {
            tx: req_tx,
            rx: ev_rx,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            busy: false,
            progress: 0.0,
            status_text: String::new(),
        }
    }

    /// Signals any active background export job to cancel.
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Release);
    }

    /// Enqueues an export or print job using an O(1) cloned `Rope` snapshot.
    pub fn start_job(&mut self, rope: Rope, title: String, target: ExportTarget) {
        self.start_job_with_base_dir(rope, title, target, None);
    }

    /// Enqueues an export or print job with an optional base directory for relative image paths.
    pub fn start_job_with_base_dir(
        &mut self,
        rope: Rope,
        title: String,
        target: ExportTarget,
        base_dir: Option<PathBuf>,
    ) {
        self.busy = true;
        self.progress = 0.05;
        self.status_text = "Starting background job...".to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Arc::clone(&cancel);
        let _ = self.tx.send(ExportRequest {
            rope,
            title,
            target,
            base_dir,
            cancel,
        });
    }

    /// Drains pending worker events without blocking the UI thread.
    pub fn poll(&mut self) -> Vec<ExportEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            match &ev {
                ExportEvent::Started { label } => {
                    self.busy = true;
                    self.progress = 0.1;
                    self.status_text = label.clone();
                }
                ExportEvent::Progress { fraction, stage } => {
                    self.busy = true;
                    self.progress = *fraction;
                    self.status_text = stage.clone();
                }
                ExportEvent::Completed { message, .. } => {
                    self.busy = false;
                    self.progress = 1.0;
                    self.status_text = message.clone();
                }
                ExportEvent::Failed { error } => {
                    self.busy = false;
                    self.progress = 0.0;
                    self.status_text = format!("Export failed: {error}");
                }
            }
            events.push(ev);
        }
        events
    }
}
