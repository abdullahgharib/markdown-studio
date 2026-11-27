use std::collections::{HashMap, VecDeque};
use std::io::{Cursor, Read};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use egui::{ColorImage, Id, TextureHandle};
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageReader, Limits};
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::DefaultConnector;

pub(super) const MAX_IMAGE_PIXELS: u64 = 64_000_000;
pub(super) const MAX_IMAGE_TEXTURE_DIM: u32 = 4096;
const MAX_LOCAL_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REMOTE_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SVG_UNCOMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const MAX_SVG_EMBEDDED_DATA_BYTES: usize = 8 * 1024 * 1024;
const MAX_GIF_FRAMES: usize = 120;
const MAX_GIF_TOTAL_BYTES: usize = 48 * 1024 * 1024;
const MAX_GIF_TEXTURE_DIM: u32 = 1280;
const MAX_PENDING_IMAGE_JOBS: usize = 256;
const MAX_IMAGE_CACHE_ENTRIES: usize = 512;
const MAX_IMAGE_VRAM_CACHE_BYTES: usize = 256 * 1024 * 1024;
const LOCAL_STAT_TTL: Duration = Duration::from_secs(2);
const REMOTE_RETRY_TTL: Duration = Duration::from_secs(60);
const ASYNC_IMAGE_BYTES_THRESHOLD: u64 = 256 * 1024;

#[derive(Clone)]
pub(super) struct DecodedFrame {
    pub image: ColorImage,
    pub delay: Duration,
}

#[derive(Clone)]
pub(super) struct DecodedImage {
    pub frames: Vec<DecodedFrame>,
}

#[derive(Clone)]
pub(super) enum ImageLoadState {
    Loaded(TextureHandle),
    Loading,
    Unavailable,
}

pub(super) fn is_remote_http_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

pub(super) fn is_unc_or_network_path(path: &Path) -> bool {
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

pub(super) fn resolve_image_path(
    url: &str,
    base_dir: Option<&Path>,
    doc_path: Option<&Path>,
) -> PathBuf {
    let trimmed = url.trim().trim_start_matches('<').trim_end_matches('>');
    if trimmed.starts_with(r"\\") || trimmed.starts_with("//") {
        return PathBuf::new();
    }
    let decoded = trimmed.replace("%20", " ");
    let direct = PathBuf::from(trimmed);
    if is_unc_or_network_path(&direct) {
        return PathBuf::new();
    }
    if direct.is_absolute() || direct.exists() {
        return direct;
    }
    if let Some(dir) = base_dir.or_else(|| doc_path.and_then(|p| p.parent())) {
        let rel = dir.join(trimmed);
        if !is_unc_or_network_path(&rel) && rel.exists() {
            return rel;
        }
        let rel_decoded = dir.join(&decoded);
        if !is_unc_or_network_path(&rel_decoded) && rel_decoded.exists() {
            return rel_decoded;
        }
    }
    let fallback = PathBuf::from(decoded);
    if is_unc_or_network_path(&fallback) {
        PathBuf::new()
    } else {
        fallback
    }
}

#[inline]
pub(super) fn image_dimensions_within_limit(width: u32, height: u32) -> bool {
    width > 0 && height > 0 && (width as u64).saturating_mul(height as u64) <= MAX_IMAGE_PIXELS
}

static SVG_FONTDB_READY: AtomicBool = AtomicBool::new(false);

fn system_svg_fontdb() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTDB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    Arc::clone(FONTDB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        SVG_FONTDB_READY.store(true, Ordering::Release);
        Arc::new(db)
    }))
}

fn looks_like_svg(bytes: &[u8], hint_name: &str) -> bool {
    let path_part = hint_name
        .split(['?', '#'])
        .next()
        .unwrap_or(hint_name)
        .trim()
        .to_ascii_lowercase();
    if path_part.ends_with(".svg") || path_part.ends_with(".svgz") {
        return true;
    }
    // Gzip header only treated as SVGZ when extension indicates `.svgz`; plain XML/SVG checked via prefix
    let probe_len = bytes.len().min(512);
    if let Ok(head) = std::str::from_utf8(&bytes[..probe_len]) {
        let trimmed = head.trim_start_matches('\u{feff}').trim_start();
        let lower = trimmed.to_ascii_lowercase();
        return lower.starts_with("<svg")
            || (lower.starts_with("<?xml") && lower.contains("<svg"))
            || lower.contains("<svg ");
    }
    false
}

fn bounded_decompress_svgz(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = flate2::read::GzDecoder::new(bytes).take((MAX_SVG_UNCOMPRESSED_BYTES + 1) as u64);
    let mut out = Vec::with_capacity(bytes.len().saturating_mul(2).min(MAX_SVG_UNCOMPRESSED_BYTES));
    decoder.read_to_end(&mut out).ok()?;
    if out.len() > MAX_SVG_UNCOMPRESSED_BYTES {
        return None;
    }
    Some(out)
}

fn safe_svg_image_href_resolver() -> resvg::usvg::ImageHrefResolver<'static> {
    resvg::usvg::ImageHrefResolver {
        resolve_string: Box::new(|_, _| None),
        resolve_data: Box::new(|mime, data, _opts| {
            if data.len() > MAX_SVG_EMBEDDED_DATA_BYTES {
                return None;
            }
            let fmt_hint = match mime {
                "image/png" => Some(image::ImageFormat::Png),
                "image/jpg" | "image/jpeg" => Some(image::ImageFormat::Jpeg),
                "image/gif" => Some(image::ImageFormat::Gif),
                "image/webp" => Some(image::ImageFormat::WebP),
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
            if !image_dimensions_within_limit(w, h) {
                return None;
            }
            match actual_fmt {
                image::ImageFormat::Jpeg => Some(resvg::usvg::ImageKind::JPEG(data)),
                image::ImageFormat::Png => Some(resvg::usvg::ImageKind::PNG(data)),
                image::ImageFormat::Gif => Some(resvg::usvg::ImageKind::GIF(data)),
                image::ImageFormat::WebP => Some(resvg::usvg::ImageKind::WEBP(data)),
                _ => None,
            }
        }),
    }
}

fn decode_svg_bytes(bytes: &[u8]) -> Option<DecodedImage> {
    let decompressed_owned;
    let xml_bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
        decompressed_owned = bounded_decompress_svgz(bytes)?;
        decompressed_owned.as_slice()
    } else {
        bytes
    };
    if xml_bytes.len() > MAX_SVG_UNCOMPRESSED_BYTES {
        return None;
    }
    let xml_str = std::str::from_utf8(xml_bytes).ok()?;
    let opt = resvg::usvg::Options {
        fontdb: system_svg_fontdb(),
        image_href_resolver: safe_svg_image_href_resolver(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(xml_str, &opt).ok()?;
    let size = tree.size();
    let base_w = size.width().max(1.0);
    let base_h = size.height().max(1.0);

    // Rasterize at 2x for crisp HiDPI rendering while respecting texture dimension limits
    let mut scale = 2.0_f32;
    let max_dim = MAX_IMAGE_TEXTURE_DIM as f32;
    if base_w * scale > max_dim {
        scale = max_dim / base_w;
    }
    if base_h * scale > max_dim {
        scale = scale.min(max_dim / base_h);
    }
    let px_w = (base_w * scale).round().clamp(1.0, max_dim) as u32;
    let px_h = (base_h * scale).round().clamp(1.0, max_dim) as u32;
    if !image_dimensions_within_limit(px_w, px_h) {
        return None;
    }

    let mut pixmap = resvg::tiny_skia::Pixmap::new(px_w, px_h)?;
    let transform =
        resvg::tiny_skia::Transform::from_scale(px_w as f32 / base_w, px_h as f32 / base_h);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let color_image =
        ColorImage::from_rgba_premultiplied([px_w as usize, px_h as usize], pixmap.data());
    Some(DecodedImage {
        frames: vec![DecodedFrame {
            image: color_image,
            delay: Duration::ZERO,
        }],
    })
}

fn decode_gif_bytes(bytes: &[u8]) -> Option<DecodedImage> {
    let decoder = GifDecoder::new(Cursor::new(bytes)).ok()?;
    let (init_w, init_h) = decoder.dimensions();
    if !image_dimensions_within_limit(init_w, init_h) {
        return None;
    }
    let mut frames_out: Vec<DecodedFrame> = Vec::new();
    let mut total_bytes: usize = 0;

    for frame_res in decoder.into_frames() {
        if frames_out.len() >= MAX_GIF_FRAMES {
            break;
        }
        let Ok(frame) = frame_res else {
            break;
        };
        let raw_delay = Duration::from(frame.delay());
        let delay = if raw_delay < Duration::from_millis(20) {
            Duration::from_millis(100)
        } else {
            raw_delay
        };
        let rgba = frame.into_buffer();
        let (w, h) = rgba.dimensions();
        if !image_dimensions_within_limit(w, h) {
            return None;
        }
        let rgba = if w > MAX_GIF_TEXTURE_DIM || h > MAX_GIF_TEXTURE_DIM {
            DynamicImage::ImageRgba8(rgba)
                .resize(
                    MAX_GIF_TEXTURE_DIM,
                    MAX_GIF_TEXTURE_DIM,
                    image::imageops::FilterType::Triangle,
                )
                .to_rgba8()
        } else {
            rgba
        };
        let frame_bytes = (rgba.width() as usize)
            .saturating_mul(rgba.height() as usize)
            .saturating_mul(4);
        if !frames_out.is_empty() && total_bytes.saturating_add(frame_bytes) > MAX_GIF_TOTAL_BYTES {
            break;
        }
        total_bytes = total_bytes.saturating_add(frame_bytes);
        let size = [rgba.width() as usize, rgba.height() as usize];
        frames_out.push(DecodedFrame {
            image: ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
            delay,
        });
    }

    if frames_out.is_empty() {
        return None;
    }
    Some(DecodedImage { frames: frames_out })
}

fn decode_static_raster_bytes(bytes: &[u8]) -> Option<DecodedImage> {
    let dim_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let (w, h) = dim_reader.into_dimensions().ok()?;
    if !image_dimensions_within_limit(w, h) {
        return None;
    }
    let mut limits = Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    let mut img_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    img_reader.limits(limits);
    let dyn_img = img_reader.decode().ok()?;
    let dyn_img =
        if dyn_img.width() > MAX_IMAGE_TEXTURE_DIM || dyn_img.height() > MAX_IMAGE_TEXTURE_DIM {
            dyn_img.resize(
                MAX_IMAGE_TEXTURE_DIM,
                MAX_IMAGE_TEXTURE_DIM,
                image::imageops::FilterType::Triangle,
            )
        } else {
            dyn_img
        };
    let rgba = dyn_img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(DecodedImage {
        frames: vec![DecodedFrame {
            image: ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
            delay: Duration::ZERO,
        }],
    })
}

pub(super) fn decode_image_bytes(bytes: &[u8], hint_name: &str) -> Option<DecodedImage> {
    if bytes.is_empty() {
        return None;
    }
    if (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"))
        && let Some(gif) = decode_gif_bytes(bytes) {
            return Some(gif);
        }
    if looks_like_svg(bytes, hint_name)
        && let Some(svg) = decode_svg_bytes(bytes) {
            return Some(svg);
        }
    decode_static_raster_bytes(bytes)
}

#[allow(dead_code)]
pub(super) fn decode_color_image(path: &Path) -> Option<ColorImage> {
    if path.as_os_str().is_empty() || is_unc_or_network_path(path) {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_LOCAL_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let hint = path.to_string_lossy();
    decode_image_bytes(&bytes, &hint).and_then(|d| d.frames.into_iter().next().map(|f| f.image))
}

fn decode_local_image(path: &Path) -> Option<DecodedImage> {
    if path.as_os_str().is_empty() || is_unc_or_network_path(path) {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_LOCAL_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let hint = path.to_string_lossy();
    decode_image_bytes(&bytes, &hint)
}

pub(crate) fn is_public_ipv4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    if v4.is_unspecified()
        || o[0] == 0
        || v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_multicast()
        || v4.is_broadcast()
        || o[0] >= 240
    {
        return false;
    }
    // CGNAT 100.64.0.0/10
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return false;
    }
    // Benchmarking 198.18.0.0/15
    if o[0] == 198 && (o[1] & 0xfe) == 18 {
        return false;
    }
    // IETF protocol assignments 192.0.0.0/24 & TEST-NET-1 192.0.2.0/24
    if o[0] == 192 && o[1] == 0 && (o[2] == 0 || o[2] == 2) {
        return false;
    }
    // TEST-NET-2 198.51.100.0/24 & TEST-NET-3 203.0.113.0/24
    if (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
    {
        return false;
    }
    true
}

pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_public_ipv4(mapped);
            }
            let seg = v6.segments();
            // IPv4-compatible `::/96`
            if seg[..6] == [0, 0, 0, 0, 0, 0] {
                let v4 = Ipv4Addr::new(
                    (seg[6] >> 8) as u8,
                    seg[6] as u8,
                    (seg[7] >> 8) as u8,
                    seg[7] as u8,
                );
                return is_public_ipv4(v4);
            }
            if v6.is_unspecified() || v6.is_loopback() || v6.is_multicast() {
                return false;
            }
            // Unique local fc00::/7
            if (seg[0] & 0xfe00) == 0xfc00 {
                return false;
            }
            // Link-local fe80::/10 or deprecated site-local fec0::/10
            if (seg[0] & 0xffc0) == 0xfe80 || (seg[0] & 0xffc0) == 0xfec0 {
                return false;
            }
            // Documentation 2001:db8::/32
            if seg[0] == 0x2001 && seg[1] == 0x0db8 {
                return false;
            }
            // NAT64 64:ff9b::/96 & 64:ff9b:1::/48
            if seg[0] == 0x0064 && seg[1] == 0xff9b {
                return false;
            }
            true
        }
    }
}

#[derive(Debug, Default)]
struct SafePublicResolver {
    inner: DefaultResolver,
}

impl Resolver for SafePublicResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let resolved = self.inner.resolve(uri, config, timeout)?;
        let mut filtered: ResolvedSocketAddrs = self.empty();
        for &addr in &resolved {
            if is_public_ip(addr.ip()) && filtered.len() < 16 {
                filtered.push(addr);
            }
        }
        if filtered.is_empty() {
            Err(ureq::Error::HostNotFound)
        } else {
            Ok(filtered)
        }
    }
}

fn remote_http_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .max_redirects(0)
            .max_redirects_will_error(false)
            .user_agent("MarkdownStudio")
            .build();
        ureq::Agent::with_parts(config, DefaultConnector::default(), SafePublicResolver::default())
    })
}

fn resolve_redirect_url(current: &str, location: &str) -> Option<String> {
    let loc = location.trim();
    if loc.is_empty() || loc.chars().any(|c| c.is_control()) {
        return None;
    }
    if is_remote_http_url(loc) {
        return Some(loc.to_string());
    }
    let uri: ureq::http::Uri = current.parse().ok()?;
    let scheme = uri.scheme_str()?;
    let authority = uri.authority()?.as_str();
    if loc.starts_with("//") {
        let full = format!("{scheme}:{loc}");
        return is_remote_http_url(&full).then_some(full);
    }
    if loc.starts_with('/') {
        return Some(format!("{scheme}://{authority}{loc}"));
    }
    None
}

fn fetch_remote_image_bytes(url: &str) -> Option<Vec<u8>> {
    if !is_remote_http_url(url) {
        return None;
    }
    if std::env::var_os("MD_STUDIO_DISABLE_REMOTE_IMAGES")
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        return None;
    }
    let mut current_url = url.trim().to_string();
    for _hop in 0..=5 {
        if !is_remote_http_url(&current_url) {
            return None;
        }
        let resp = remote_http_agent().get(&current_url).call().ok()?;
        if resp.status().is_redirection() {
            let loc = resp.headers().get("location")?.to_str().ok()?;
            current_url = resolve_redirect_url(&current_url, loc)?;
            continue;
        }
        return resp
            .into_body()
            .with_config()
            .limit(MAX_REMOTE_IMAGE_BYTES)
            .read_to_vec()
            .ok();
    }
    None
}

fn decode_remote_image(url: &str) -> Option<DecodedImage> {
    let bytes = fetch_remote_image_bytes(url)?;
    decode_image_bytes(&bytes, url)
}

#[derive(Clone)]
enum ImageSource {
    Local(PathBuf),
    Remote(String),
}

struct ImageDecodeJob {
    source: ImageSource,
    tx: Sender<Option<DecodedImage>>,
    repaint_ctx: egui::Context,
}

struct ImageDecodePool {
    queue: Arc<(Mutex<VecDeque<ImageDecodeJob>>, Condvar)>,
}

impl ImageDecodePool {
    fn global() -> &'static Self {
        static POOL: OnceLock<ImageDecodePool> = OnceLock::new();
        POOL.get_or_init(|| {
            let queue: Arc<(Mutex<VecDeque<ImageDecodeJob>>, Condvar)> =
                Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
            let workers = thread::available_parallelism()
                .map(|n| (n.get() / 2).clamp(2, 8))
                .unwrap_or(4);
            for worker_id in 0..workers {
                let q = Arc::clone(&queue);
                let _ = thread::Builder::new()
                    .name(format!("md-img-pool-{worker_id}"))
                    .spawn(move || {
                        let (lock, cvar) = &*q;
                        loop {
                            let job = {
                                let Ok(mut guard) = lock.lock() else {
                                    break;
                                };
                                while guard.is_empty() {
                                    guard = match cvar.wait(guard) {
                                        Ok(g) => g,
                                        Err(_) => return,
                                    };
                                }
                                guard.pop_front()
                            };
                            if let Some(job) = job {
                                let decoded = match &job.source {
                                    ImageSource::Local(path) => decode_local_image(path),
                                    ImageSource::Remote(url) => decode_remote_image(url),
                                };
                                let _ = job.tx.send(decoded);
                                job.repaint_ctx.request_repaint();
                            }
                        }
                    });
            }
            ImageDecodePool { queue }
        })
    }

    fn enqueue(&self, source: ImageSource, repaint_ctx: egui::Context) -> Receiver<Option<DecodedImage>> {
        let (tx, rx) = mpsc::channel();
        let (lock, cvar) = &*self.queue;
        if let Ok(mut guard) = lock.lock() {
            while guard.len() >= MAX_PENDING_IMAGE_JOBS {
                guard.pop_front();
            }
            guard.push_back(ImageDecodeJob {
                source,
                tx,
                repaint_ctx,
            });
            cvar.notify_one();
        }
        rx
    }
}

#[derive(Clone)]
struct ImageCacheEntry {
    is_remote: bool,
    resolved_label: String,
    resolved_path: PathBuf,
    last_stat_check: Option<Instant>,
    mtime: Option<SystemTime>,
    frames: Vec<(TextureHandle, Duration)>,
    anim_start: Instant,
    total_duration: Duration,
    loading_rx: Option<Arc<Mutex<Receiver<Option<DecodedImage>>>>>,
    approx_vram_bytes: usize,
    last_tick: u64,
}

impl Default for ImageCacheEntry {
    fn default() -> Self {
        Self {
            is_remote: false,
            resolved_label: String::new(),
            resolved_path: PathBuf::new(),
            last_stat_check: None,
            mtime: None,
            frames: Vec::new(),
            anim_start: Instant::now(),
            total_duration: Duration::ZERO,
            loading_rx: None,
            approx_vram_bytes: 0,
            last_tick: 0,
        }
    }
}

impl ImageCacheEntry {
    fn current_frame(&self, now: Instant) -> Option<(TextureHandle, Option<Duration>)> {
        if self.frames.is_empty() {
            return None;
        }
        if self.frames.len() == 1 || self.total_duration.is_zero() {
            return Some((self.frames[0].0.clone(), None));
        }
        let elapsed_ns = now.saturating_duration_since(self.anim_start).as_nanos();
        let total_ns = self.total_duration.as_nanos().max(1);
        let mut cursor_ns = elapsed_ns % total_ns;
        for (tex, delay) in &self.frames {
            let d_ns = delay.as_nanos().max(1);
            if cursor_ns < d_ns {
                let remaining = Duration::from_nanos((d_ns - cursor_ns).min(u64::MAX as u128) as u64);
                return Some((tex.clone(), Some(remaining.max(Duration::from_millis(16)))));
            }
            cursor_ns -= d_ns;
        }
        Some((self.frames[0].0.clone(), Some(Duration::from_millis(16))))
    }
}

#[derive(Clone, Default)]
struct ImageTextureCache {
    entries: HashMap<(String, Option<PathBuf>, Option<PathBuf>), ImageCacheEntry>,
    tick: u64,
}

enum ImageCachePoll {
    Miss,
    Ready {
        state: ImageLoadState,
        repaint_after: Option<Duration>,
        need_refresh: bool,
    },
    DecodedFromWorker {
        key_prefix: String,
        decoded: Option<DecodedImage>,
    },
}

fn upload_decoded_frames(
    ctx: &egui::Context,
    key_prefix: &str,
    decoded: DecodedImage,
) -> (Vec<(TextureHandle, Duration)>, Duration, usize) {
    let mut frames = Vec::with_capacity(decoded.frames.len());
    let mut total_duration = Duration::ZERO;
    let mut vram_bytes = 0usize;
    for (idx, frame) in decoded.frames.into_iter().enumerate() {
        let [w, h] = frame.image.size;
        vram_bytes = vram_bytes.saturating_add(w.saturating_mul(h).saturating_mul(4));
        let tex_name = if idx == 0 {
            key_prefix.to_string()
        } else {
            format!("{key_prefix}#frame{idx}")
        };
        let tex = ctx.load_texture(tex_name, frame.image, egui::TextureOptions::LINEAR);
        total_duration += frame.delay;
        frames.push((tex, frame.delay));
    }
    (frames, total_duration, vram_bytes)
}

pub(super) fn load_cached_image_state(
    ctx: &egui::Context,
    url: &str,
    base_dir: Option<&Path>,
    doc_path: Option<&Path>,
    allow_remote_images: bool,
) -> ImageLoadState {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.starts_with("data:") {
        return ImageLoadState::Unavailable;
    }
    let is_remote = is_remote_http_url(trimmed);
    if is_remote && !allow_remote_images {
        return ImageLoadState::Unavailable;
    }
    let key = if is_remote {
        (trimmed.to_string(), None, None)
    } else {
        (
            trimmed.to_string(),
            base_dir.map(Path::to_path_buf),
            doc_path.map(Path::to_path_buf),
        )
    };
    let cache_id = Id::new("md_bounded_img_cache_v3");
    let now = Instant::now();

    // 1) Fast path: poll cache state without holding `ctx.data_mut` during `ctx.load_texture` or `request_repaint_after`
    let poll = ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<ImageTextureCache>(cache_id);
        cache.tick = cache.tick.wrapping_add(1);
        let cur_tick = cache.tick;
        let Some(entry) = cache.entries.get_mut(&key) else {
            return ImageCachePoll::Miss;
        };
        entry.last_tick = cur_tick;
        if let Some(rx_arc) = entry.loading_rx.clone() {
            let recv_res = rx_arc.lock().map(|rx| rx.try_recv());
            match recv_res {
                Ok(Ok(decoded)) => {
                    entry.loading_rx = None;
                    return ImageCachePoll::DecodedFromWorker {
                        key_prefix: entry.resolved_label.clone(),
                        decoded,
                    };
                }
                Ok(Err(mpsc::TryRecvError::Empty)) => {
                    if let Some((tex, repaint_after)) = entry.current_frame(now) {
                        return ImageCachePoll::Ready {
                            state: ImageLoadState::Loaded(tex),
                            repaint_after,
                            need_refresh: false,
                        };
                    }
                    return ImageCachePoll::Ready {
                        state: ImageLoadState::Loading,
                        repaint_after: None,
                        need_refresh: false,
                    };
                }
                Ok(Err(mpsc::TryRecvError::Disconnected)) | Err(_) => {
                    entry.loading_rx = None;
                    entry.last_stat_check = Some(now);
                    let (state, repaint_after) = match entry.current_frame(now) {
                        Some((tex, next)) => (ImageLoadState::Loaded(tex), next),
                        None => (ImageLoadState::Unavailable, None),
                    };
                    return ImageCachePoll::Ready {
                        state,
                        repaint_after,
                        need_refresh: false,
                    };
                }
            }
        }
        let ttl = if entry.is_remote {
            // Loaded remote images stay cached permanently; failed remote fetches retry after REMOTE_RETRY_TTL
            if !entry.frames.is_empty() {
                None
            } else {
                Some(REMOTE_RETRY_TTL)
            }
        } else {
            Some(LOCAL_STAT_TTL)
        };
        let fresh = match ttl {
            None => true,
            Some(limit) => entry
                .last_stat_check
                .is_some_and(|t| now.saturating_duration_since(t) < limit),
        };
        let (state, repaint_after) = match entry.current_frame(now) {
            Some((tex, next)) => (ImageLoadState::Loaded(tex), next),
            None => (ImageLoadState::Unavailable, None),
        };
        ImageCachePoll::Ready {
            state,
            repaint_after,
            need_refresh: !fresh,
        }
    });

    match poll {
        ImageCachePoll::DecodedFromWorker {
            key_prefix,
            decoded,
        } => {
            // Upload texture(s) OUTSIDE `ctx.data_mut` to avoid re-entrant egui Context RwLock deadlock
            let uploaded = decoded.map(|dec| upload_decoded_frames(ctx, &key_prefix, dec));
            let mut result_state = ImageLoadState::Unavailable;
            let mut repaint_after = None;
            ctx.data_mut(|d| {
                let cache = d.get_temp_mut_or_default::<ImageTextureCache>(cache_id);
                if let Some(entry) = cache.entries.get_mut(&key) {
                    entry.last_stat_check = Some(now);
                    if let Some((frames, total_duration, vram_bytes)) = uploaded {
                        entry.frames = frames;
                        entry.anim_start = now;
                        entry.total_duration = total_duration;
                        entry.approx_vram_bytes = vram_bytes;
                        if let Some((tex, next)) = entry.current_frame(now) {
                            result_state = ImageLoadState::Loaded(tex);
                            repaint_after = next;
                        }
                    } else {
                        entry.frames.clear();
                        entry.total_duration = Duration::ZERO;
                        entry.approx_vram_bytes = 0;
                    }
                }
                evict_if_over_budget(cache, &key);
            });
            if let Some(delay) = repaint_after {
                ctx.request_repaint_after(delay);
            }
            return result_state;
        }
        ImageCachePoll::Ready {
            state,
            repaint_after,
            need_refresh: false,
        } => {
            if let Some(delay) = repaint_after {
                ctx.request_repaint_after(delay);
            }
            return state;
        }
        ImageCachePoll::Ready {
            need_refresh: true,
            ..
        }
        | ImageCachePoll::Miss => {}
    }

    // 2) Remote URL miss / retry expiration: always fetch off-thread on ImageDecodePool
    if is_remote {
        let url_owned = trimmed.to_string();
        let rx = ImageDecodePool::global().enqueue(ImageSource::Remote(url_owned.clone()), ctx.clone());
        ctx.data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<ImageTextureCache>(cache_id);
            evict_if_over_budget(cache, &key);
            cache.tick = cache.tick.wrapping_add(1);
            let t = cache.tick;
            cache.entries.insert(
                key,
                ImageCacheEntry {
                    is_remote: true,
                    resolved_label: url_owned,
                    resolved_path: PathBuf::new(),
                    last_stat_check: Some(now),
                    mtime: None,
                    frames: Vec::new(),
                    anim_start: now,
                    total_duration: Duration::ZERO,
                    loading_rx: Some(Arc::new(Mutex::new(rx))),
                    approx_vram_bytes: 0,
                    last_tick: t,
                },
            );
        });
        return ImageLoadState::Loading;
    }

    // 3) Local file miss or TTL expired: resolve path & check mtime once (blocking UNC paths)
    let resolved = resolve_image_path(trimmed, base_dir, doc_path);
    if resolved.as_os_str().is_empty() || is_unc_or_network_path(&resolved) {
        return ImageLoadState::Unavailable;
    }
    let meta = std::fs::metadata(&resolved).ok();
    let mtime = meta.as_ref().and_then(|m| m.modified().ok());
    let file_len = meta.as_ref().map(|m| m.len()).unwrap_or(0);

    enum LocalCheck {
        Unchanged(ImageLoadState, Option<Duration>),
        Changed,
    }
    let unchanged = ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<ImageTextureCache>(cache_id);
        if let Some(entry) = cache.entries.get_mut(&key)
            && !entry.is_remote && entry.resolved_path == resolved && entry.mtime == mtime {
                entry.last_stat_check = Some(now);
                return match entry.current_frame(now) {
                    Some((tex, next)) => LocalCheck::Unchanged(ImageLoadState::Loaded(tex), next),
                    None if entry.loading_rx.is_some() => {
                        LocalCheck::Unchanged(ImageLoadState::Loading, None)
                    }
                    None => LocalCheck::Unchanged(ImageLoadState::Unavailable, None),
                };
            }
        LocalCheck::Changed
    });
    if let LocalCheck::Unchanged(state, repaint_after) = unchanged {
        if let Some(delay) = repaint_after {
            ctx.request_repaint_after(delay);
        }
        return state;
    }

    // 4) Decode small non-GIF/non-SVG local files synchronously; enqueue GIFs, SVGs (to keep fontdb
    //    scan off the UI thread), or large files on worker pool
    let resolved_label = resolved.to_string_lossy().to_string();
    let is_async_only_ext = resolved
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            e.eq_ignore_ascii_case("gif")
                || e.eq_ignore_ascii_case("svgz")
                || (e.eq_ignore_ascii_case("svg") && !SVG_FONTDB_READY.load(Ordering::Acquire))
        });
    let (frames, total_duration, vram_bytes, pending_rx, result_state, repaint_after) =
        if !is_async_only_ext && file_len <= ASYNC_IMAGE_BYTES_THRESHOLD {
            if let Some(decoded) = decode_local_image(&resolved) {
                let (frames, total_duration, vram_bytes) =
                    upload_decoded_frames(ctx, &resolved_label, decoded);
                let first_tex = frames.first().map(|(t, _)| t.clone());
                let state = match first_tex {
                    Some(t) => ImageLoadState::Loaded(t),
                    None => ImageLoadState::Unavailable,
                };
                (frames, total_duration, vram_bytes, None, state, None)
            } else {
                (
                    Vec::new(),
                    Duration::ZERO,
                    0,
                    None,
                    ImageLoadState::Unavailable,
                    None,
                )
            }
        } else {
            let rx = ImageDecodePool::global()
                .enqueue(ImageSource::Local(resolved.clone()), ctx.clone());
            (
                Vec::new(),
                Duration::ZERO,
                0,
                Some(Arc::new(Mutex::new(rx))),
                ImageLoadState::Loading,
                None,
            )
        };

    if let Some(delay) = repaint_after {
        ctx.request_repaint_after(delay);
    }

    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<ImageTextureCache>(cache_id);
        evict_if_over_budget(cache, &key);
        cache.tick = cache.tick.wrapping_add(1);
        let t = cache.tick;
        cache.entries.insert(
            key,
            ImageCacheEntry {
                is_remote: false,
                resolved_label,
                resolved_path: resolved,
                last_stat_check: Some(now),
                mtime,
                frames,
                anim_start: now,
                total_duration,
                loading_rx: pending_rx,
                approx_vram_bytes: vram_bytes,
                last_tick: t,
            },
        );
    });
    result_state
}

fn evict_if_over_budget(
    cache: &mut ImageTextureCache,
    protect_key: &(String, Option<PathBuf>, Option<PathBuf>),
) {
    let mut total_vram: usize = cache.entries.values().map(|e| e.approx_vram_bytes).sum();
    while (total_vram > MAX_IMAGE_VRAM_CACHE_BYTES || cache.entries.len() >= MAX_IMAGE_CACHE_ENTRIES)
        && cache.entries.len() > 1
    {
        let Some(oldest_key) = cache
            .entries
            .iter()
            .filter(|(k, _)| *k != protect_key)
            .min_by_key(|(_, e)| e.last_tick)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        if let Some(removed) = cache.entries.remove(&oldest_key) {
            total_vram = total_vram.saturating_sub(removed.approx_vram_bytes);
        }
    }
}

#[allow(dead_code)]
pub(super) fn load_cached_image_texture(
    ctx: &egui::Context,
    url: &str,
    base_dir: Option<&Path>,
    doc_path: Option<&Path>,
) -> Option<TextureHandle> {
    match load_cached_image_state(ctx, url, base_dir, doc_path, true) {
        ImageLoadState::Loaded(tex) => Some(tex),
        ImageLoadState::Loading | ImageLoadState::Unavailable => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn ssrf_public_ip_filter_blocks_private_loopback_metadata_and_mapped_v6() {
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
        assert!(!is_public_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!is_public_ip(IpAddr::V6(Ipv6Addr::new(
            0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001
        ))));
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))));
    }

    #[test]
    fn svgz_decompression_bomb_exceeding_budget_is_rejected() {
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        let chunk = vec![b' '; 64 * 1024];
        let target_bytes = MAX_SVG_UNCOMPRESSED_BYTES + 256 * 1024;
        let mut written = 0usize;
        while written < target_bytes {
            enc.write_all(&chunk).unwrap();
            written += chunk.len();
        }
        let svgz_bomb = enc.finish().unwrap();
        assert!(
            decode_svg_bytes(&svgz_bomb).is_none(),
            "bounded svgz decoder must reject decompression bomb above MAX_SVG_UNCOMPRESSED_BYTES"
        );
    }
}
