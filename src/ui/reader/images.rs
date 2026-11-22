use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use egui::{ColorImage, Id, TextureHandle};
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, DynamicImage, ImageReader};

pub(super) const MAX_IMAGE_PIXELS: u64 = 64_000_000;
pub(super) const MAX_IMAGE_TEXTURE_DIM: u32 = 4096;
const MAX_LOCAL_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REMOTE_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_GIF_FRAMES: usize = 120;
const MAX_GIF_TOTAL_BYTES: usize = 48 * 1024 * 1024;
const MAX_GIF_TEXTURE_DIM: u32 = 1280;
const IMAGE_DECODE_WORKERS: usize = 4;
const MAX_PENDING_IMAGE_JOBS: usize = 64;
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

pub(super) fn resolve_image_path(
    url: &str,
    base_dir: Option<&Path>,
    doc_path: Option<&Path>,
) -> PathBuf {
    let trimmed = url.trim().trim_start_matches('<').trim_end_matches('>');
    let decoded = trimmed.replace("%20", " ");
    let direct = PathBuf::from(trimmed);
    if direct.is_absolute() || direct.exists() {
        return direct;
    }
    if let Some(dir) = base_dir.or_else(|| doc_path.and_then(|p| p.parent())) {
        let rel = dir.join(trimmed);
        if rel.exists() {
            return rel;
        }
        let rel_decoded = dir.join(&decoded);
        if rel_decoded.exists() {
            return rel_decoded;
        }
    }
    PathBuf::from(decoded)
}

#[inline]
pub(super) fn image_dimensions_within_limit(width: u32, height: u32) -> bool {
    width > 0 && height > 0 && (width as u64).saturating_mul(height as u64) <= MAX_IMAGE_PIXELS
}

fn system_svg_fontdb() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTDB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    Arc::clone(FONTDB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
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

fn decode_svg_bytes(bytes: &[u8]) -> Option<DecodedImage> {
    let opt = resvg::usvg::Options {
        fontdb: system_svg_fontdb(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(bytes, &opt).ok()?;
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
    let img_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
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
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_LOCAL_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let hint = path.to_string_lossy();
    decode_image_bytes(&bytes, &hint).and_then(|d| d.frames.into_iter().next().map(|f| f.image))
}

fn decode_local_image(path: &Path) -> Option<DecodedImage> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_LOCAL_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let hint = path.to_string_lossy();
    decode_image_bytes(&bytes, &hint)
}

fn remote_http_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .max_redirects(5)
            .user_agent("MarkdownStudio")
            .build();
        ureq::Agent::new_with_config(config)
    })
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
    let resp = remote_http_agent().get(url.trim()).call().ok()?;
    resp.into_body()
        .with_config()
        .limit(MAX_REMOTE_IMAGE_BYTES)
        .read_to_vec()
        .ok()
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
            for worker_id in 0..IMAGE_DECODE_WORKERS {
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
) -> (Vec<(TextureHandle, Duration)>, Duration) {
    let mut frames = Vec::with_capacity(decoded.frames.len());
    let mut total_duration = Duration::ZERO;
    for (idx, frame) in decoded.frames.into_iter().enumerate() {
        let tex_name = if idx == 0 {
            key_prefix.to_string()
        } else {
            format!("{key_prefix}#frame{idx}")
        };
        let tex = ctx.load_texture(tex_name, frame.image, egui::TextureOptions::LINEAR);
        total_duration += frame.delay;
        frames.push((tex, frame.delay));
    }
    (frames, total_duration)
}

pub(super) fn load_cached_image_state(
    ctx: &egui::Context,
    url: &str,
    base_dir: Option<&Path>,
    doc_path: Option<&Path>,
) -> ImageLoadState {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.starts_with("data:") {
        return ImageLoadState::Unavailable;
    }
    let is_remote = is_remote_http_url(trimmed);
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
                    entry.last_stat_check = None;
                    entry.mtime = None;
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
                    if let Some((frames, total_duration)) = uploaded {
                        entry.frames = frames;
                        entry.anim_start = now;
                        entry.total_duration = total_duration;
                        if let Some((tex, next)) = entry.current_frame(now) {
                            result_state = ImageLoadState::Loaded(tex);
                            repaint_after = next;
                        }
                    } else {
                        entry.frames.clear();
                        entry.total_duration = Duration::ZERO;
                    }
                }
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
            evict_oldest_if_full(cache, &key);
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
                    last_tick: t,
                },
            );
        });
        return ImageLoadState::Loading;
    }

    // 3) Local file miss or TTL expired: resolve path & check mtime once
    let resolved = resolve_image_path(trimmed, base_dir, doc_path);
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

    // 4) Decode small non-GIF local files synchronously; enqueue GIFs or large files on worker pool
    let resolved_label = resolved.to_string_lossy().to_string();
    let is_gif_path = resolved
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"));
    let (frames, total_duration, pending_rx, result_state, repaint_after) =
        if !is_gif_path && file_len <= ASYNC_IMAGE_BYTES_THRESHOLD {
            if let Some(decoded) = decode_local_image(&resolved) {
                let (frames, total_duration) = upload_decoded_frames(ctx, &resolved_label, decoded);
                let first_tex = frames.first().map(|(t, _)| t.clone());
                let state = match first_tex {
                    Some(t) => ImageLoadState::Loaded(t),
                    None => ImageLoadState::Unavailable,
                };
                (frames, total_duration, None, state, None)
            } else {
                (
                    Vec::new(),
                    Duration::ZERO,
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
        evict_oldest_if_full(cache, &key);
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
                last_tick: t,
            },
        );
    });
    result_state
}

fn evict_oldest_if_full(
    cache: &mut ImageTextureCache,
    incoming_key: &(String, Option<PathBuf>, Option<PathBuf>),
) {
    if cache.entries.len() >= 64 && !cache.entries.contains_key(incoming_key)
        && let Some(oldest_key) = cache
            .entries
            .iter()
            .min_by_key(|(_, e)| e.last_tick)
            .map(|(k, _)| k.clone())
        {
            cache.entries.remove(&oldest_key);
        }
}

#[allow(dead_code)]
pub(super) fn load_cached_image_texture(
    ctx: &egui::Context,
    url: &str,
    base_dir: Option<&Path>,
    doc_path: Option<&Path>,
) -> Option<TextureHandle> {
    match load_cached_image_state(ctx, url, base_dir, doc_path) {
        ImageLoadState::Loaded(tex) => Some(tex),
        ImageLoadState::Loading | ImageLoadState::Unavailable => None,
    }
}
