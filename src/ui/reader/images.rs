use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use egui::{ColorImage, Id, TextureHandle};

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

#[derive(Clone, Default)]
struct ImageCacheEntry {
    resolved_path: PathBuf,
    last_stat_check: Option<Instant>,
    mtime: Option<SystemTime>,
    texture: Option<TextureHandle>,
    loading_rx: Option<Arc<Mutex<Receiver<Option<ColorImage>>>>>,
    last_tick: u64,
}

#[derive(Clone, Default)]
struct ImageTextureCache {
    entries: HashMap<(String, Option<PathBuf>, Option<PathBuf>), ImageCacheEntry>,
    tick: u64,
}

pub(super) const MAX_IMAGE_PIXELS: u64 = 64_000_000;
pub(super) const MAX_IMAGE_TEXTURE_DIM: u32 = 4096;
const IMAGE_DECODE_WORKERS: usize = 2;
const MAX_PENDING_IMAGE_JOBS: usize = 64;

#[inline]
pub(super) fn image_dimensions_within_limit(width: u32, height: u32) -> bool {
    width > 0 && height > 0 && (width as u64).saturating_mul(height as u64) <= MAX_IMAGE_PIXELS
}

pub(super) fn decode_color_image(path: &Path) -> Option<ColorImage> {
    let (w, h) = image::image_dimensions(path).ok()?;
    if !image_dimensions_within_limit(w, h) {
        return None;
    }
    let dyn_img = image::open(path).ok()?;
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
    Some(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

struct ImageDecodeJob {
    path: PathBuf,
    tx: Sender<Option<ColorImage>>,
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
                                let _ = job.tx.send(decode_color_image(&job.path));
                                job.repaint_ctx.request_repaint();
                            }
                        }
                    });
            }
            ImageDecodePool { queue }
        })
    }

    fn enqueue(&self, path: PathBuf, repaint_ctx: egui::Context) -> Receiver<Option<ColorImage>> {
        let (tx, rx) = mpsc::channel();
        let (lock, cvar) = &*self.queue;
        if let Ok(mut guard) = lock.lock() {
            while guard.len() >= MAX_PENDING_IMAGE_JOBS {
                guard.pop_front();
            }
            guard.push_back(ImageDecodeJob {
                path,
                tx,
                repaint_ctx,
            });
            cvar.notify_one();
        }
        rx
    }
}

pub(super) fn load_cached_image_texture(
    ctx: &egui::Context,
    url: &str,
    base_dir: Option<&Path>,
    doc_path: Option<&Path>,
) -> Option<TextureHandle> {
    let trimmed = url.trim();
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("data:")
    {
        return None;
    }
    let key = (
        trimmed.to_string(),
        base_dir.map(Path::to_path_buf),
        doc_path.map(Path::to_path_buf),
    );
    let cache_id = Id::new("md_bounded_img_cache_v2");
    let now = Instant::now();
    const STAT_TTL: Duration = Duration::from_secs(2);
    const ASYNC_IMAGE_BYTES_THRESHOLD: u64 = 256 * 1024;

    // 1) Fast path: return cached texture without filesystem syscalls within STAT_TTL
    let need_stat_check = ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<ImageTextureCache>(cache_id);
        cache.tick = cache.tick.wrapping_add(1);
        let cur_tick = cache.tick;
        let Some(entry) = cache.entries.get_mut(&key) else {
            return Err(());
        };
        entry.last_tick = cur_tick;
        if let Some(rx_arc) = entry.loading_rx.clone() {
            let recv_res = rx_arc.lock().map(|rx| rx.try_recv());
            match recv_res {
                Ok(Ok(img_opt)) => {
                    if let Some(color_image) = img_opt {
                        let key_str = entry.resolved_path.to_string_lossy().to_string();
                        entry.texture = Some(ctx.load_texture(
                            key_str,
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    } else {
                        entry.texture = None;
                    }
                    entry.loading_rx = None;
                }
                Ok(Err(mpsc::TryRecvError::Empty)) => {}
                Ok(Err(mpsc::TryRecvError::Disconnected)) | Err(_) => {
                    // Reset stat timestamp so a dropped/evicted queue job retries when visible again
                    entry.loading_rx = None;
                    entry.last_stat_check = None;
                    entry.mtime = None;
                }
            }
            return Ok((false, entry.texture.clone()));
        }
        let fresh = entry
            .last_stat_check
            .is_some_and(|t| now.saturating_duration_since(t) < STAT_TTL);
        Ok((!fresh, entry.texture.clone()))
    });

    if let Ok((false, tex)) = need_stat_check {
        return tex;
    }

    // 2) Miss or TTL expired: resolve path & check mtime once
    let resolved = resolve_image_path(trimmed, base_dir, doc_path);
    let meta = std::fs::metadata(&resolved).ok();
    let mtime = meta.as_ref().and_then(|m| m.modified().ok());
    let file_len = meta.as_ref().map(|m| m.len()).unwrap_or(0);

    let unchanged_tex = ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<ImageTextureCache>(cache_id);
        if let Some(entry) = cache.entries.get_mut(&key) {
            if entry.resolved_path == resolved && entry.mtime == mtime {
                entry.last_stat_check = Some(now);
                return Some(entry.texture.clone());
            }
        }
        None
    });
    if let Some(tex) = unchanged_tex {
        return tex;
    }

    // 3) Decode small images synchronously (zero frame delay) and large images via bounded worker pool
    let (loaded_tex, pending_rx) = if file_len <= ASYNC_IMAGE_BYTES_THRESHOLD {
        let tex = decode_color_image(&resolved).map(|color_image| {
            let key_str = resolved.to_string_lossy().to_string();
            ctx.load_texture(key_str, color_image, egui::TextureOptions::LINEAR)
        });
        (tex, None)
    } else {
        let rx = ImageDecodePool::global().enqueue(resolved.clone(), ctx.clone());
        (None, Some(Arc::new(Mutex::new(rx))))
    };

    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<ImageTextureCache>(cache_id);
        if cache.entries.len() >= 64 && !cache.entries.contains_key(&key) {
            if let Some(oldest_key) = cache
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_tick)
                .map(|(k, _)| k.clone())
            {
                cache.entries.remove(&oldest_key);
            }
        }
        cache.tick = cache.tick.wrapping_add(1);
        let t = cache.tick;
        cache.entries.insert(
            key,
            ImageCacheEntry {
                resolved_path: resolved,
                last_stat_check: Some(now),
                mtime,
                texture: loaded_tex.clone(),
                loading_rx: pending_rx,
                last_tick: t,
            },
        );
    });
    loaded_tex
}
