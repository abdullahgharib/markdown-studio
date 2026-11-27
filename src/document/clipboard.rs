//! Smart clipboard reading (Win32 `CF_HTML` & `CF_UNICODETEXT`) and rich-text conversion
//! from Web, Microsoft Word, Google Docs, and Excel/Sheets HTML or TSV into clean GFM Markdown.

use std::borrow::Cow;
use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::document::ast::build_table_markdown;
use crate::export::ir::TableAlign;

const MAX_CLIPBOARD_IMAGE_PIXELS: u64 = 64_000_000;
const MAX_CLIPBOARD_HTML_BYTES: usize = 16 * 1024 * 1024;
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_HTML_TAG_STACK: usize = 128;
static IMAGE_PASTE_SEQ: AtomicU64 = AtomicU64::new(1);

/// Extracts the HTML fragment from a Windows `CF_HTML` clipboard payload, honoring
/// `StartFragment` / `EndFragment` UTF-8 byte offsets with marker and raw HTML fallbacks.
pub fn extract_cf_html_fragment(raw: &str) -> String {
    let mut start_frag: Option<usize> = None;
    let mut end_frag: Option<usize> = None;
    let mut start_html: Option<usize> = None;
    let mut end_html: Option<usize> = None;

    for line in raw.lines().take(16) {
        let trimmed = line.trim();
        if trimmed.starts_with('<') {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("StartFragment:") {
            start_frag = val.trim().parse::<usize>().ok();
        } else if let Some(val) = trimmed.strip_prefix("EndFragment:") {
            end_frag = val.trim().parse::<usize>().ok();
        } else if let Some(val) = trimmed.strip_prefix("StartHTML:") {
            start_html = val.trim().parse::<usize>().ok();
        } else if let Some(val) = trimmed.strip_prefix("EndHTML:") {
            end_html = val.trim().parse::<usize>().ok();
        }
    }

    if let (Some(s), Some(e)) = (start_frag, end_frag)
        && s < e && e <= raw.len() && raw.is_char_boundary(s) && raw.is_char_boundary(e) {
            return raw[s..e].to_string();
        }

    if let Some(start_marker) = raw.find("<!--StartFragment-->") {
        let body_start = start_marker + "<!--StartFragment-->".len();
        if let Some(rel_end) = raw[body_start..].find("<!--EndFragment-->") {
            return raw[body_start..body_start + rel_end].to_string();
        }
    }

    if let (Some(s), Some(e)) = (start_html, end_html)
        && s < e && e <= raw.len() && raw.is_char_boundary(s) && raw.is_char_boundary(e) {
            return raw[s..e].to_string();
        }

    raw.to_string()
}

/// Checks whether a candidate OS `CF_HTML` snippet actually corresponds to the pasted `plain_text`,
/// guarding against stale multi-format clipboards or synthetic `Event::Paste` payloads.
pub fn html_matches_plain_text(plain_text: &str, html: &str) -> bool {
    let Some(token) = first_meaningful_token(plain_text) else {
        return true;
    };
    let visible = extract_plain_preview_from_html(html);
    let token_lower = token.to_ascii_lowercase();
    visible.to_ascii_lowercase().contains(&token_lower)
}

fn first_meaningful_token(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut start: Option<usize> = None;
    let mut fallback: Option<&str> = None;
    for (idx, ch) in text.char_indices() {
        if ch.is_alphanumeric() {
            if start.is_none() {
                start = Some(idx);
            }
            let s = start.unwrap();
            let next = idx + ch.len_utf8();
            if next - s >= 16 {
                return Some(&text[s..next]);
            }
        } else if let Some(s) = start.take() {
            let tok = &bytes[s..idx];
            if tok.len() >= 2 {
                return Some(&text[s..idx]);
            }
            if fallback.is_none() {
                fallback = Some(&text[s..idx]);
            }
        }
    }
    if let Some(s) = start {
        let tok = &text[s..];
        if tok.len() >= 2 || fallback.is_none() {
            return Some(tok);
        }
    }
    fallback
}

fn extract_plain_preview_from_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(4096));
    let bytes = html.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && out.len() < 32_768 {
        if bytes[i] == b'<' {
            if html[i..].starts_with("<!--") {
                if let Some(end) = html[i + 4..].find("-->") {
                    i = i + 4 + end + 3;
                    continue;
                }
                break;
            }
            let peek = &bytes[i..bytes.len().min(i + 12)];
            for raw_tag in ["<script", "<style", "<head", "<title", "<xml"] {
                if peek.len() >= raw_tag.len()
                    && peek[..raw_tag.len()].eq_ignore_ascii_case(raw_tag.as_bytes())
                    && peek
                        .get(raw_tag.len())
                        .is_none_or(|b| *b == b'>' || b.is_ascii_whitespace())
                {
                    let close = format!("</{}", &raw_tag[1..]);
                    if let Some(pos) = find_ascii_ci(&html[i..], &close) {
                        if let Some(gt) = html[i + pos..].find('>') {
                            i = i + pos + gt + 1;
                        } else {
                            i = bytes.len();
                        }
                        break;
                    }
                }
            }
            if let Some(gt) = html[i..].find('>') {
                i += gt + 1;
                out.push(' ');
                continue;
            }
            break;
        }
        let start = i;
        while i < bytes.len() && bytes[i] != b'<' {
            i += 1;
        }
        let decoded = decode_html_entities(&html[start..i]);
        out.push_str(&decoded);
    }
    out
}

#[cfg(target_os = "windows")]
mod win_clip {
    use core::ffi::c_void;
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;

    use super::{dib_to_png_bytes, extract_cf_html_fragment, html_matches_plain_text};

    const CF_DIB: u32 = 8;
    const CF_UNICODETEXT: u32 = 13;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(h_wnd_new_owner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn RegisterClipboardFormatW(lpsz_format: *const u16) -> u32;
        fn GetClipboardData(u_format: u32) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalLock(h_mem: *mut c_void) -> *const u8;
        fn GlobalUnlock(h_mem: *mut c_void) -> i32;
        fn GlobalSize(h_mem: *mut c_void) -> usize;
    }

    struct ClipboardGuard;
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            // SAFETY: Constructed only after a successful `OpenClipboard` call.
            unsafe {
                CloseClipboard();
            }
        }
    }

    struct GlobalLockGuard(*mut c_void);
    impl Drop for GlobalLockGuard {
        fn drop(&mut self) {
            // SAFETY: Constructed only after a non-null `GlobalLock` return on the same handle.
            unsafe {
                GlobalUnlock(self.0);
            }
        }
    }

    fn open_clipboard_guard() -> Option<ClipboardGuard> {
        // SAFETY: Passing null hwnd opens the clipboard for the current task.
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            Some(ClipboardGuard)
        } else {
            None
        }
    }

    pub fn read_html() -> Option<String> {
        let _clip = open_clipboard_guard()?;
        let fmt_name: Vec<u16> = OsStr::new("HTML Format")
            .encode_wide()
            .chain(once(0))
            .collect();
        // SAFETY: `fmt_name` is a NUL-terminated UTF-16 string.
        let cf_html = unsafe { RegisterClipboardFormatW(fmt_name.as_ptr()) };
        if cf_html == 0 {
            return None;
        }
        // SAFETY: Clipboard is open and guarded by `_clip`.
        let handle = unsafe { GetClipboardData(cf_html) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: Handle came from `GetClipboardData` while clipboard is open.
        let size = unsafe { GlobalSize(handle) };
        if size == 0 || size > super::MAX_CLIPBOARD_HTML_BYTES {
            return None;
        }
        // SAFETY: Locking non-zero global clipboard handle.
        let ptr = unsafe { GlobalLock(handle) };
        if ptr.is_null() {
            return None;
        }
        let _lock = GlobalLockGuard(handle);
        // SAFETY: `GlobalLock` succeeded and `size` is the locked allocation length.
        let raw_bytes = unsafe { std::slice::from_raw_parts(ptr, size) };
        let used_len = raw_bytes.iter().position(|&b| b == 0).unwrap_or(raw_bytes.len());
        let raw_str = String::from_utf8_lossy(&raw_bytes[..used_len]).into_owned();
        if raw_str.trim().is_empty() {
            None
        } else {
            Some(extract_cf_html_fragment(&raw_str))
        }
    }

    pub fn read_html_matching(plain_text: &str) -> Option<String> {
        let html = read_html()?;
        if html_matches_plain_text(plain_text, &html) {
            Some(html)
        } else {
            None
        }
    }

    pub fn read_plain_text() -> Option<String> {
        let _clip = open_clipboard_guard()?;
        // SAFETY: Clipboard is open and guarded by `_clip`.
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: Handle came from `GetClipboardData(CF_UNICODETEXT)` while clipboard is open.
        let size_bytes = unsafe { GlobalSize(handle) };
        let u16_count = size_bytes / 2;
        if u16_count == 0 {
            return None;
        }
        // SAFETY: Locking non-zero global clipboard handle.
        let ptr = unsafe { GlobalLock(handle) } as *const u16;
        if ptr.is_null() {
            return None;
        }
        let _lock = GlobalLockGuard(handle);
        // SAFETY: `GlobalLock` succeeded for `size_bytes`, aligned UTF-16 `CF_UNICODETEXT` buffer.
        let u16_slice = unsafe { std::slice::from_raw_parts(ptr, u16_count) };
        let used_len = u16_slice.iter().position(|&c| c == 0).unwrap_or(u16_slice.len());
        let text = String::from_utf16_lossy(&u16_slice[..used_len]);
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        }
    }

    fn read_global_clipboard_bytes(format: u32, max_bytes: usize) -> Option<Vec<u8>> {
        // SAFETY: Caller holds an open `ClipboardGuard`.
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: Handle came from `GetClipboardData` while clipboard is open.
        let size = unsafe { GlobalSize(handle) };
        if size == 0 || size > max_bytes {
            return None;
        }
        // SAFETY: Locking non-zero global clipboard handle.
        let ptr = unsafe { GlobalLock(handle) };
        if ptr.is_null() {
            return None;
        }
        let _lock = GlobalLockGuard(handle);
        // SAFETY: `GlobalLock` succeeded and `size` is the locked allocation length.
        let raw_bytes = unsafe { std::slice::from_raw_parts(ptr, size) };
        Some(raw_bytes.to_vec())
    }

    pub fn read_image_png() -> Option<Vec<u8>> {
        let _clip = open_clipboard_guard()?;
        let png_name: Vec<u16> = OsStr::new("PNG").encode_wide().chain(once(0)).collect();
        // SAFETY: `png_name` is a NUL-terminated UTF-16 string.
        let cf_png = unsafe { RegisterClipboardFormatW(png_name.as_ptr()) };
        if cf_png != 0
            && let Some(bytes) =
                read_global_clipboard_bytes(cf_png, super::MAX_CLIPBOARD_IMAGE_BYTES)
            && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            && let Ok((w, h)) = image::ImageReader::with_format(
                std::io::Cursor::new(&bytes),
                image::ImageFormat::Png,
            )
            .into_dimensions()
            && (w as u64).saturating_mul(h as u64) <= super::MAX_CLIPBOARD_IMAGE_PIXELS
        {
            return Some(bytes);
        }
        let dib = read_global_clipboard_bytes(CF_DIB, super::MAX_CLIPBOARD_IMAGE_BYTES)?;
        dib_to_png_bytes(&dib)
    }
}

#[inline]
fn extract_masked_u8(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let shifted = (pixel & mask) >> shift;
    let max_val = mask >> shift;
    if max_val == 0 {
        0
    } else if max_val == 255 {
        shifted as u8
    } else {
        ((shifted as u64 * 255 + (max_val as u64 / 2)) / max_val as u64).min(255) as u8
    }
}

/// Converts a Windows `CF_DIB` (`BITMAPINFOHEADER` / V4 / V5) byte payload (24-bit or 32-bit
/// `BI_RGB` / `BI_BITFIELDS`, bottom-up or top-down) into encoded PNG bytes.
pub fn dib_to_png_bytes(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let read_u32 = |off: usize| -> u32 {
        u32::from_le_bytes([dib[off], dib[off + 1], dib[off + 2], dib[off + 3]])
    };
    let read_i32 = |off: usize| -> i32 {
        i32::from_le_bytes([dib[off], dib[off + 1], dib[off + 2], dib[off + 3]])
    };
    let read_u16 = |off: usize| -> u16 { u16::from_le_bytes([dib[off], dib[off + 1]]) };

    let bi_size = read_u32(0) as usize;
    if bi_size < 40 || bi_size > dib.len() {
        return None;
    }
    let width_raw = read_i32(4);
    let height_raw = read_i32(8);
    if width_raw <= 0 || height_raw == 0 || height_raw == i32::MIN {
        return None;
    }
    let width = width_raw as u32;
    let top_down = height_raw < 0;
    let height = height_raw.unsigned_abs();
    if (width as u64).saturating_mul(height as u64) > MAX_CLIPBOARD_IMAGE_PIXELS {
        return None;
    }
    let planes = read_u16(12);
    let bpp = read_u16(14);
    let compression = read_u32(16);
    let clr_used = read_u32(32) as usize;
    if planes != 1 || !matches!(bpp, 24 | 32) {
        return None;
    }
    if !matches!(compression, 0 | 3) || (compression == 3 && bpp != 32) {
        return None;
    }

    let (mut r_mask, mut g_mask, mut b_mask, mut a_mask) =
        (0x00FF_0000_u32, 0x0000_FF00_u32, 0x0000_00FF_u32, 0xFF00_0000_u32);
    let mut pixel_offset = bi_size;
    if compression == 3 {
        let mask_base = if bi_size == 40 {
            if dib.len() < 52 {
                return None;
            }
            pixel_offset = 52;
            40
        } else if bi_size >= 52 {
            40
        } else {
            return None;
        };
        r_mask = read_u32(mask_base);
        g_mask = read_u32(mask_base + 4);
        b_mask = read_u32(mask_base + 8);
        a_mask = if bi_size >= 56 {
            read_u32(mask_base + 12)
        } else {
            0
        };
    }
    if clr_used > 0 {
        pixel_offset = pixel_offset.checked_add(clr_used.checked_mul(4)?)?;
    }

    let row_stride = ((width as usize).checked_mul(bpp as usize)?)
        .div_ceil(32)
        .checked_mul(4)?;
    let total_pixel_bytes = row_stride.checked_mul(height as usize)?;
    if pixel_offset.checked_add(total_pixel_bytes)? > dib.len() {
        return None;
    }
    let pixel_data = &dib[pixel_offset..];

    let rgba_len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    let mut rgba = vec![0u8; rgba_len];
    let mut any_non_zero_alpha = false;

    for y in 0..(height as usize) {
        let src_y = if top_down {
            y
        } else {
            (height as usize) - 1 - y
        };
        let src_row = &pixel_data[src_y * row_stride..src_y * row_stride + row_stride];
        let dst_row_off = y * (width as usize) * 4;
        if bpp == 24 {
            for x in 0..(width as usize) {
                let b = src_row[x * 3];
                let g = src_row[x * 3 + 1];
                let r = src_row[x * 3 + 2];
                let o = dst_row_off + x * 4;
                rgba[o] = r;
                rgba[o + 1] = g;
                rgba[o + 2] = b;
                rgba[o + 3] = 255;
            }
            any_non_zero_alpha = true;
        } else if compression == 0 {
            for x in 0..(width as usize) {
                let b = src_row[x * 4];
                let g = src_row[x * 4 + 1];
                let r = src_row[x * 4 + 2];
                let a = src_row[x * 4 + 3];
                if a != 0 {
                    any_non_zero_alpha = true;
                }
                let o = dst_row_off + x * 4;
                rgba[o] = r;
                rgba[o + 1] = g;
                rgba[o + 2] = b;
                rgba[o + 3] = a;
            }
        } else {
            for x in 0..(width as usize) {
                let p = u32::from_le_bytes([
                    src_row[x * 4],
                    src_row[x * 4 + 1],
                    src_row[x * 4 + 2],
                    src_row[x * 4 + 3],
                ]);
                let r = extract_masked_u8(p, r_mask);
                let g = extract_masked_u8(p, g_mask);
                let b = extract_masked_u8(p, b_mask);
                let a = if a_mask != 0 {
                    let av = extract_masked_u8(p, a_mask);
                    if av != 0 {
                        any_non_zero_alpha = true;
                    }
                    av
                } else {
                    any_non_zero_alpha = true;
                    255
                };
                let o = dst_row_off + x * 4;
                rgba[o] = r;
                rgba[o + 1] = g;
                rgba[o + 2] = b;
                rgba[o + 3] = a;
            }
        }
    }

    // Windows GDI BitBlt / PrintScreen sets 32-bit rgbReserved bytes to 0x00 for opaque images.
    if !any_non_zero_alpha {
        for px in rgba.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
    }

    let img = image::RgbaImage::from_raw(width, height, rgba)?;
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// Reads a PNG-encoded bitmap image from the OS clipboard when available.
pub fn read_os_clipboard_image_png() -> Option<Vec<u8>> {
    #[cfg(target_os = "windows")]
    {
        win_clip::read_image_png()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

fn private_pasted_assets_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA")
            && !appdata.is_empty()
        {
            return PathBuf::from(appdata)
                .join("MarkdownStudio")
                .join("pasted_assets");
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(home) = std::env::var_os("HOME")
            && !home.is_empty()
        {
            return PathBuf::from(home)
                .join(".markdown_studio")
                .join("pasted_assets");
        }
    }
    std::env::temp_dir().join("markdown_studio_assets")
}

/// Saves pasted clipboard PNG bytes to `<doc_dir>/assets/pasted-image-<ts>.png` (or private
/// per-user assets dir for untitled buffers) and returns `(saved_file_path, markdown_image_url)`.
pub fn save_pasted_image_png(
    png_bytes: &[u8],
    doc_path: Option<&Path>,
) -> io::Result<(PathBuf, String)> {
    if png_bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Clipboard image payload is empty",
        ));
    }
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let (target_dir, relative_prefix, private_perms) = match doc_path.and_then(Path::parent) {
        Some(parent) if !parent.as_os_str().is_empty() => {
            (parent.join("assets"), Some("assets"), false)
        }
        _ => (private_pasted_assets_dir(), None, true),
    };
    #[cfg(unix)]
    if private_perms {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&target_dir)?;
    } else {
        std::fs::create_dir_all(&target_dir)?;
    }
    #[cfg(not(unix))]
    {
        let _ = private_perms;
        std::fs::create_dir_all(&target_dir)?;
    }

    for attempt in 0..32 {
        let filename = if attempt == 0 {
            format!("pasted-image-{ts_ms}.png")
        } else {
            let seq = IMAGE_PASTE_SEQ.fetch_add(1, Ordering::Relaxed);
            format!("pasted-image-{ts_ms}-{seq}.png")
        };
        let full_path = target_dir.join(&filename);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        if private_perms {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(&full_path) {
            Ok(mut file) => {
                file.write_all(png_bytes)?;
                file.flush()?;
                let url = match relative_prefix {
                    Some(rel) => format!("{rel}/{filename}"),
                    None => full_path.to_string_lossy().replace('\\', "/"),
                };
                return Ok((full_path, url));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Unable to allocate unique pasted image filename",
    ))
}

/// Reads the OS clipboard HTML fragment (`CF_HTML`) when available.
pub fn read_os_clipboard_html() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        win_clip::read_html()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Reads OS clipboard HTML only when it matches `plain_text`, preventing stale multi-format
/// clipboard leakage or interference in synthetic paste events.
pub fn read_os_clipboard_html_matching(plain_text: &str) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        win_clip::read_html_matching(plain_text)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = plain_text;
        None
    }
}

/// Reads verbatim plain Unicode text from the OS clipboard (`Ctrl+Shift+V`).
pub fn read_os_clipboard_plain_text() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        win_clip::read_plain_text()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Converts rich clipboard input (`html` or spreadsheet TSV `plain_text`) to clean GFM Markdown,
/// falling back to `plain_text` verbatim when no rich structure is detected.
pub fn smart_paste_markdown(plain_text: &str, html: Option<&str>) -> String {
    if let Some(h) = html
        && let Some(md) = html_to_markdown(h) {
            return md;
        }
    if let Some(table_md) = tsv_to_markdown_table(plain_text) {
        return table_md;
    }
    plain_text.to_string()
}

/// Detects tab-separated spreadsheet grids (Excel, Google Sheets, LibreOffice Calc) in plain text
/// and converts them into a GFM Markdown table.
pub fn tsv_to_markdown_table(text: &str) -> Option<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let raw_lines: Vec<&str> = normalized.split('\n').collect();
    let start = raw_lines.iter().position(|l| !l.trim().is_empty())?;
    let end = raw_lines.iter().rposition(|l| !l.trim().is_empty())? + 1;
    let lines = &raw_lines[start..end];
    if lines.len() < 2 {
        return None;
    }
    let first_trim = lines[0].trim_start();
    if first_trim.starts_with('|') || first_trim.starts_with("```") || first_trim.starts_with("~~~")
    {
        return None;
    }

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(lines.len());
    let mut expected_cols: Option<usize> = None;
    let mut any_non_empty_cell = false;
    let mut all_first_col_empty = true;

    for line in lines {
        if !line.contains('\t') {
            return None;
        }
        let cols: Vec<String> = line
            .split('\t')
            .map(|c| {
                let cleaned = c.trim().to_string();
                if !cleaned.is_empty() {
                    any_non_empty_cell = true;
                }
                cleaned
            })
            .collect();
        if cols.len() < 2 {
            return None;
        }
        if !cols[0].is_empty() {
            all_first_col_empty = false;
        }
        match expected_cols {
            None => expected_cols = Some(cols.len()),
            Some(c) if c != cols.len() => return None,
            _ => {}
        }
        rows.push(cols);
    }

    let col_count = expected_cols?;
    if !any_non_empty_cell || (all_first_col_empty && col_count < 3) {
        return None;
    }

    let header = rows.remove(0);
    let alignments = vec![TableAlign::None; col_count];
    Some(build_table_markdown(&alignments, &header, &rows))
}

fn find_ascii_ci(haystack: &str, needle_lower: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle_lower.as_bytes();
    if n.is_empty() {
        return Some(0);
    }
    if n.len() > h.len() {
        return None;
    }
    let first = n[0];
    let mut i = 0usize;
    while i + n.len() <= h.len() {
        if h[i].to_ascii_lowercase() == first && h[i..i + n.len()].eq_ignore_ascii_case(n) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn decode_html_entities<'a>(input: &'a str) -> Cow<'a, str> {
    if !input.contains('&') {
        return Cow::Borrowed(input);
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after_amp = &rest[amp + 1..];
        if let Some(semi) = after_amp.find(';').filter(|&s| s <= 16) {
            let entity = &after_amp[..semi];
            if let Some(decoded) = resolve_entity(entity) {
                out.push_str(decoded.as_ref());
                rest = &after_amp[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after_amp;
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn resolve_entity(entity: &str) -> Option<Cow<'static, str>> {
    if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
        let code = u32::from_str_radix(hex, 16).ok()?;
        let ch = char::from_u32(code).filter(|&c| c != '\0')?;
        return Some(Cow::Owned(ch.to_string()));
    }
    if let Some(dec) = entity.strip_prefix('#') {
        let code = dec.parse::<u32>().ok()?;
        let ch = char::from_u32(code).filter(|&c| c != '\0')?;
        return Some(Cow::Owned(ch.to_string()));
    }
    let named = match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{00a0}",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "bull" => "•",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "times" => "×",
        "divide" => "÷",
        "minus" => "−",
        "le" => "≤",
        "ge" => "≥",
        "ne" => "≠",
        "plusmn" => "±",
        "rarr" => "→",
        "larr" => "←",
        _ => return None,
    };
    Some(Cow::Borrowed(named))
}

#[derive(Debug, Clone)]
struct HtmlAttr {
    name: String,
    value: String,
}

#[derive(Debug, Clone)]
enum HtmlToken<'a> {
    Open {
        tag: String,
        attrs: Vec<HtmlAttr>,
        self_closing: bool,
    },
    Close {
        tag: String,
    },
    Text(&'a str),
}

fn is_void_html_tag(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_raw_skip_tag(tag: &str) -> bool {
    matches!(
        tag,
        "script" | "style" | "head" | "title" | "noscript" | "xml" | "svg" | "template"
    )
}

fn find_tag_end(s: &str, start: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = start;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'>' {
            return i;
        }
        i += 1;
    }
    bytes.len()
}

fn parse_html_attrs(raw_attrs: &str) -> (Vec<HtmlAttr>, bool) {
    let mut attrs = Vec::new();
    let trimmed = raw_attrs.trim();
    let (body, self_closing) = if let Some(stripped) = trimmed.strip_suffix('/') {
        (stripped.trim_end(), true)
    } else {
        (trimmed, false)
    };
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let name_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        let name = body[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let q = bytes[i];
                i += 1;
                let val_start = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                let raw_val = &body[val_start..i];
                if i < bytes.len() && bytes[i] == q {
                    i += 1;
                }
                decode_html_entities(raw_val).into_owned()
            } else {
                let val_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                decode_html_entities(&body[val_start..i]).into_owned()
            }
        } else {
            String::new()
        };
        if !name.is_empty() {
            attrs.push(HtmlAttr { name, value });
        }
    }
    (attrs, self_closing)
}

fn attr_value<'a>(attrs: &'a [HtmlAttr], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.value.as_str())
}

fn tokenize_html(html: &str) -> Vec<HtmlToken<'_>> {
    let mut tokens = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if html[i..].starts_with("<!--") {
                if let Some(end) = html[i + 4..].find("-->") {
                    i = i + 4 + end + 3;
                } else {
                    break;
                }
                continue;
            }
            if bytes.get(i + 1).is_some_and(|b| *b == b'!' || *b == b'?') {
                let gt = find_tag_end(html, i + 2);
                i = (gt + 1).min(bytes.len());
                continue;
            }
            let gt = find_tag_end(html, i + 1);
            let inside = html[i + 1..gt].trim();
            i = (gt + 1).min(bytes.len());
            if inside.is_empty() {
                continue;
            }
            if let Some(rest) = inside.strip_prefix('/') {
                let tag = rest
                    .split(|c: char| c.is_ascii_whitespace() || c == '/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !tag.is_empty() {
                    tokens.push(HtmlToken::Close { tag });
                }
                continue;
            }
            let split_pos = inside
                .find(|c: char| c.is_ascii_whitespace() || c == '/')
                .unwrap_or(inside.len());
            let tag = inside[..split_pos].to_ascii_lowercase();
            let raw_attrs = &inside[split_pos..];
            let (attrs, slash_self) = parse_html_attrs(raw_attrs);
            let self_closing = slash_self || is_void_html_tag(&tag);
            if is_raw_skip_tag(&tag) && !self_closing {
                let close_pat = format!("</{tag}");
                if let Some(rel_close) = find_ascii_ci(&html[i..], &close_pat) {
                    let close_gt = find_tag_end(html, i + rel_close + close_pat.len());
                    i = (close_gt + 1).min(bytes.len());
                } else {
                    break;
                }
                continue;
            }
            if !tag.is_empty() {
                tokens.push(HtmlToken::Open {
                    tag,
                    attrs,
                    self_closing,
                });
            }
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b'<' {
                i += 1;
            }
            tokens.push(HtmlToken::Text(&html[start..i]));
        }
    }
    tokens
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InlineCssFlags {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: bool,
    strike: bool,
    text_align: Option<TableAlign>,
}

fn parse_inline_css_flags(style: &str) -> InlineCssFlags {
    let mut flags = InlineCssFlags {
        bold: None,
        italic: None,
        underline: false,
        strike: false,
        text_align: None,
    };
    for decl in style.split(';') {
        let Some((prop, val)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim().to_ascii_lowercase();
        let val = val.trim().to_ascii_lowercase();
        match prop.as_str() {
            "font-weight" => {
                if val.contains("bold")
                    || matches!(val.as_str(), "600" | "700" | "800" | "900")
                    || val
                        .parse::<u32>()
                        .is_ok_and(|w| w >= 600)
                {
                    flags.bold = Some(true);
                } else if val.contains("normal")
                    || val
                        .parse::<u32>()
                        .is_ok_and(|w| w <= 500)
                {
                    flags.bold = Some(false);
                }
            }
            "font-style" => {
                if val.contains("italic") || val.contains("oblique") {
                    flags.italic = Some(true);
                } else if val.contains("normal") {
                    flags.italic = Some(false);
                }
            }
            "text-decoration" | "text-decoration-line" => {
                if val.contains("underline") {
                    flags.underline = true;
                }
                if val.contains("line-through") {
                    flags.strike = true;
                }
            }
            "text-align" => {
                flags.text_align = match val.as_str() {
                    "center" => Some(TableAlign::Center),
                    "right" | "end" => Some(TableAlign::Right),
                    "left" | "start" => Some(TableAlign::Left),
                    _ => None,
                };
            }
            _ => {}
        }
    }
    flags
}

fn strip_ascii_control_and_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

fn sanitize_md_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for ch in url.trim().chars() {
        if ch.is_control() {
            continue;
        }
        match ch {
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            ' ' => out.push_str("%20"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            _ => out.push(ch),
        }
    }
    out
}

fn has_safe_paste_scheme(raw: &str, allow_mailto: bool) -> bool {
    let clean = strip_ascii_control_and_ws(raw.trim());
    let lower = clean.to_ascii_lowercase();
    if lower.is_empty() || lower.starts_with(r"\\") || lower.starts_with("//") {
        return false;
    }
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with('#')
        || (allow_mailto && lower.starts_with("mailto:"))
    {
        return true;
    }
    // Disallow explicit schemes (`javascript:`, `vbscript:`, `data:`, `file:`, `ms-msdt:`, etc.)
    // while allowing safe relative paths (`docs/foo.md`, `assets/pic.png`).
    let before_delim = lower.split(['/', '?', '#']).next().unwrap_or("");
    !before_delim.contains(':')
}

fn is_unsafe_paste_url(url: &str) -> bool {
    !has_safe_paste_scheme(url, true)
}

fn is_unsafe_or_blob_image_src(src: &str) -> bool {
    !has_safe_paste_scheme(src, false)
}

fn extract_code_lang_from_attrs(attrs: &[HtmlAttr]) -> Option<String> {
    for key in ["data-lang", "data-language", "lang"] {
        if let Some(val) = attr_value(attrs, key) {
            let t = val.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    if let Some(class_val) = attr_value(attrs, "class") {
        for cls in class_val.split_whitespace() {
            let lower = cls.to_ascii_lowercase();
            for prefix in ["language-", "lang-", "highlight-source-"] {
                if let Some(rest) = lower.strip_prefix(prefix)
                    && !rest.is_empty() {
                        return Some(rest.to_string());
                    }
            }
        }
    }
    None
}

fn wrap_inline_code_ticks(code: &str) -> String {
    let mut max_ticks = 0usize;
    let mut cur = 0usize;
    for b in code.bytes() {
        if b == b'`' {
            cur += 1;
            max_ticks = max_ticks.max(cur);
        } else {
            cur = 0;
        }
    }
    let ticks = "`".repeat(max_ticks + 1);
    let need_pad = code.starts_with('`') || code.ends_with('`');
    if need_pad {
        format!("{ticks} {code} {ticks}")
    } else {
        format!("{ticks}{code}{ticks}")
    }
}

fn wrap_fenced_code_block(lang: &str, raw_code: &str) -> String {
    let clean = raw_code
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let body = clean.trim_matches('\n');
    let fence = if body.contains("```") { "~~~~" } else { "```" };
    if lang.is_empty() {
        format!("{fence}\n{body}\n{fence}")
    } else {
        format!("{fence}{lang}\n{body}\n{fence}")
    }
}

fn normalize_ws_collapse(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_ws = false;
    for ch in raw.chars() {
        let mapped = if ch == '\u{00a0}' { ' ' } else { ch };
        if mapped.is_ascii_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(mapped);
            prev_ws = false;
        }
    }
    out
}

#[derive(Debug, Clone)]
struct ListContext {
    ordered: bool,
    next_num: usize,
}

#[derive(Debug, Clone)]
struct TableCellData {
    text: String,
    align: TableAlign,
}

#[derive(Debug, Default)]
struct TableCapture {
    rows: Vec<Vec<TableCellData>>,
    active_cell: Option<(String, usize, TableAlign)>,
}

#[derive(Debug, Clone)]
enum TagEffect {
    None,
    Heading,
    ParagraphOrDiv,
    Blockquote,
    List,
    ListItem,
    Pre,
    InlineCode,
    Bold,
    Italic,
    Underline,
    Strike,
    Link { href: String, start_len: usize },
    Table,
    TableSection,
    TableRow,
    TableCell,
}

struct HtmlMdConverter {
    out: String,
    pending_breaks: usize,
    blockquote_depth: usize,
    list_stack: Vec<ListContext>,
    pre_depth: usize,
    pre_lang: String,
    pre_buf: String,
    inline_code_depth: usize,
    inline_code_buf: String,
    in_heading: bool,
    bold_depth: usize,
    italic_depth: usize,
    underline_depth: usize,
    strike_depth: usize,
    link_depth: usize,
    table: Option<TableCapture>,
    tag_stack: Vec<(String, Vec<TagEffect>)>,
    has_rich_structure: bool,
}

impl HtmlMdConverter {
    fn new() -> Self {
        Self {
            out: String::new(),
            pending_breaks: 0,
            blockquote_depth: 0,
            list_stack: Vec::new(),
            pre_depth: 0,
            pre_lang: String::new(),
            pre_buf: String::new(),
            inline_code_depth: 0,
            inline_code_buf: String::new(),
            in_heading: false,
            bold_depth: 0,
            italic_depth: 0,
            underline_depth: 0,
            strike_depth: 0,
            link_depth: 0,
            table: None,
            tag_stack: Vec::new(),
            has_rich_structure: false,
        }
    }

    fn ensure_blockquote_line_prefix(&mut self) {
        if self.blockquote_depth > 0 && (self.out.is_empty() || self.out.ends_with('\n')) {
            for _ in 0..self.blockquote_depth {
                self.out.push_str("> ");
            }
        }
    }

    fn flush_breaks(&mut self) {
        if self.out.is_empty() {
            self.pending_breaks = 0;
            self.ensure_blockquote_line_prefix();
            return;
        }
        let breaks = self.pending_breaks.min(2);
        self.pending_breaks = 0;
        if breaks == 0 {
            self.ensure_blockquote_line_prefix();
            return;
        }
        let trailing_nls = self.out.chars().rev().take_while(|&c| c == '\n').count();
        for i in trailing_nls..breaks {
            self.out.push('\n');
            if self.blockquote_depth > 0 && i + 1 < breaks {
                for _ in 0..self.blockquote_depth {
                    self.out.push_str("> ");
                }
            }
        }
        self.ensure_blockquote_line_prefix();
    }

    fn push_inline_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Some(tbl) = self.table.as_mut() {
            if let Some((ref mut cell, _, _)) = tbl.active_cell {
                cell.push_str(s);
            }
            return;
        }
        if self.inline_code_depth > 0 {
            self.inline_code_buf.push_str(s);
            return;
        }
        self.flush_breaks();
        self.out.push_str(s);
    }

    fn current_buffer_mut(&mut self) -> &mut String {
        if let Some(tbl) = self.table.as_mut()
            && let Some((ref mut cell, _, _)) = tbl.active_cell {
                return cell;
            }
        if self.inline_code_depth > 0 {
            return &mut self.inline_code_buf;
        }
        &mut self.out
    }

    fn open_inline_marker(&mut self, marker: &str) {
        if self.pre_depth > 0 || self.inline_code_depth > 0 {
            return;
        }
        if self.table.is_none() {
            self.flush_breaks();
            if self.out.is_empty() && self.blockquote_depth > 0 {
                for _ in 0..self.blockquote_depth {
                    self.out.push_str("> ");
                }
            }
        }
        self.current_buffer_mut().push_str(marker);
    }

    fn close_inline_marker(&mut self, open_marker: &str, close_marker: &str) {
        if self.pre_depth > 0 || self.inline_code_depth > 0 {
            return;
        }
        let buf = self.current_buffer_mut();
        if buf.ends_with(open_marker) {
            let new_len = buf.len() - open_marker.len();
            buf.truncate(new_len);
            return;
        }
        // Move trailing horizontal spaces outside CommonMark markers (e.g. `**foo **` -> `**foo** `)
        let trimmed_len = buf.trim_end_matches(' ').len();
        if trimmed_len < buf.len() {
            let trailing_spaces = buf.len() - trimmed_len;
            let without_spaces = &buf[..trimmed_len];
            if without_spaces.ends_with(open_marker) {
                let keep_len = trimmed_len - open_marker.len();
                let spaces = " ".repeat(trailing_spaces);
                buf.truncate(keep_len);
                buf.push_str(&spaces);
                return;
            }
            let spaces = " ".repeat(trailing_spaces);
            buf.truncate(trimmed_len);
            buf.push_str(close_marker);
            buf.push_str(&spaces);
        } else {
            buf.push_str(close_marker);
        }
    }

    fn request_breaks(&mut self, n: usize) {
        if self.table.is_some() {
            if let Some(tbl) = self.table.as_mut()
                && let Some((ref mut cell, _, _)) = tbl.active_cell
                    && !cell.ends_with(' ') && !cell.is_empty() {
                        cell.push(' ');
                    }
            return;
        }
        let line_tail = self.out.rsplit('\n').next().unwrap_or(&self.out).trim();
        let after_empty_marker = line_tail == "-"
            || line_tail == "- [ ]"
            || line_tail == "- [x]"
            || line_tail.chars().all(|c| c == '#' || c == '>')
            || (line_tail.ends_with('.')
                && line_tail.len() > 1
                && line_tail[..line_tail.len() - 1]
                    .bytes()
                    .all(|b| b.is_ascii_digit()));
        if !self.out.is_empty() && !after_empty_marker {
            self.pending_breaks = self.pending_breaks.max(n);
        }
    }

    fn handle_open(&mut self, tag: String, attrs: Vec<HtmlAttr>, self_closing: bool) {
        if self.pre_depth > 0 {
            if tag == "code" && self.pre_lang.is_empty() {
                if let Some(lang) = extract_code_lang_from_attrs(&attrs) {
                    self.pre_lang = lang;
                }
            } else if tag == "br"
                || (matches!(tag.as_str(), "div" | "p")
                    && !self.pre_buf.is_empty()
                    && !self.pre_buf.ends_with('\n'))
            {
                self.pre_buf.push('\n');
            }
            if !self_closing {
                self.tag_stack.push((tag, vec![TagEffect::None]));
            }
            return;
        }

        let mut effects = Vec::new();
        let css = attr_value(&attrs, "style")
            .map(parse_inline_css_flags)
            .unwrap_or(InlineCssFlags {
                bold: None,
                italic: None,
                underline: false,
                strike: false,
                text_align: None,
            });

        match tag.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag[1..].parse::<usize>().unwrap_or(1).clamp(1, 6);
                self.has_rich_structure = true;
                self.request_breaks(2);
                self.flush_breaks();
                if self.out.is_empty() && self.blockquote_depth > 0 {
                    for _ in 0..self.blockquote_depth {
                        self.out.push_str("> ");
                    }
                }
                for _ in 0..level {
                    self.out.push('#');
                }
                self.out.push(' ');
                self.in_heading = true;
                effects.push(TagEffect::Heading);
            }
            "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "aside" => {
                if self.table.is_some() {
                    if let Some(tbl) = self.table.as_mut()
                        && let Some((ref mut cell, _, _)) = tbl.active_cell
                            && !cell.is_empty() && !cell.ends_with(' ') {
                                cell.push(' ');
                            }
                } else {
                    let breaks = if tag == "p" && self.list_stack.is_empty() {
                        2
                    } else {
                        1
                    };
                    self.request_breaks(breaks);
                }
                effects.push(TagEffect::ParagraphOrDiv);
            }
            "br" => {
                if self.table.is_some() {
                    if let Some(tbl) = self.table.as_mut()
                        && let Some((ref mut cell, _, _)) = tbl.active_cell
                            && !cell.is_empty() && !cell.ends_with(' ') {
                                cell.push(' ');
                            }
                } else if !self.in_heading && self.inline_code_depth == 0 {
                    self.flush_breaks();
                    self.out.push('\n');
                    self.ensure_blockquote_line_prefix();
                }
            }
            "hr" => {
                self.has_rich_structure = true;
                if self.table.is_none() {
                    self.request_breaks(2);
                    self.flush_breaks();
                    self.out.push_str("---");
                    self.request_breaks(2);
                }
            }
            "blockquote" => {
                self.has_rich_structure = true;
                self.request_breaks(2);
                self.flush_breaks();
                self.blockquote_depth += 1;
                effects.push(TagEffect::Blockquote);
            }
            "ul" | "ol" => {
                self.has_rich_structure = true;
                let breaks = if self.list_stack.is_empty() { 2 } else { 1 };
                self.request_breaks(breaks);
                let start_num = attr_value(&attrs, "start")
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .unwrap_or(1);
                self.list_stack.push(ListContext {
                    ordered: tag == "ol",
                    next_num: start_num,
                });
                effects.push(TagEffect::List);
            }
            "li" => {
                self.has_rich_structure = true;
                self.request_breaks(1);
                self.flush_breaks();
                if self.out.is_empty() && self.blockquote_depth > 0 {
                    for _ in 0..self.blockquote_depth {
                        self.out.push_str("> ");
                    }
                }
                let depth = self.list_stack.len().saturating_sub(1);
                for _ in 0..depth {
                    self.out.push_str("  ");
                }
                if let Some(top) = self.list_stack.last_mut() {
                    if top.ordered {
                        let num = top.next_num;
                        top.next_num += 1;
                        self.out.push_str(&format!("{num}. "));
                    } else {
                        self.out.push_str("- ");
                    }
                } else {
                    self.out.push_str("- ");
                }
                effects.push(TagEffect::ListItem);
            }
            "input" => {
                let is_checkbox = attr_value(&attrs, "type")
                    .is_some_and(|t| t.trim().eq_ignore_ascii_case("checkbox"));
                if is_checkbox && !self.list_stack.is_empty() {
                    self.has_rich_structure = true;
                    let checked = attrs.iter().any(|a| a.name == "checked");
                    let marker = if checked { "[x] " } else { "[ ] " };
                    self.push_inline_str(marker);
                }
            }
            "pre" => {
                self.has_rich_structure = true;
                self.request_breaks(2);
                self.flush_breaks();
                self.pre_depth += 1;
                self.pre_buf.clear();
                self.pre_lang = extract_code_lang_from_attrs(&attrs).unwrap_or_default();
                effects.push(TagEffect::Pre);
            }
            "code" | "kbd" | "samp" | "tt" => {
                self.has_rich_structure = true;
                if self.inline_code_depth == 0 {
                    self.inline_code_buf.clear();
                }
                self.inline_code_depth += 1;
                effects.push(TagEffect::InlineCode);
            }
            "table" => {
                self.has_rich_structure = true;
                if self.table.is_none() {
                    self.request_breaks(2);
                    self.flush_breaks();
                    self.table = Some(TableCapture::default());
                    effects.push(TagEffect::Table);
                }
            }
            "thead" => {
                if self.table.is_some() {
                    effects.push(TagEffect::TableSection);
                }
            }
            "tr" => {
                if let Some(tbl) = self.table.as_mut() {
                    tbl.rows.push(Vec::new());
                    effects.push(TagEffect::TableRow);
                }
            }
            "th" | "td" => {
                if let Some(tbl) = self.table.as_mut() {
                    let colspan = attr_value(&attrs, "colspan")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, 32);
                    let align = attr_value(&attrs, "align")
                        .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
                            "center" => Some(TableAlign::Center),
                            "right" => Some(TableAlign::Right),
                            "left" => Some(TableAlign::Left),
                            _ => None,
                        })
                        .or(css.text_align)
                        .unwrap_or(TableAlign::None);
                    tbl.active_cell = Some((String::new(), colspan, align));
                    effects.push(TagEffect::TableCell);
                }
            }
            "a" => {
                if let Some(href) = attr_value(&attrs, "href") {
                    let href_trim = href.trim();
                    if !is_unsafe_paste_url(href_trim) {
                        self.has_rich_structure = true;
                        if self.table.is_none() {
                            self.flush_breaks();
                        }
                        let start_len = if let Some(tbl) = self.table.as_ref() {
                            tbl.active_cell.as_ref().map(|c| c.0.len()).unwrap_or(0)
                        } else {
                            self.out.len()
                        };
                        let safe_href = sanitize_md_url(href_trim);
                        if !safe_href.is_empty() {
                            self.link_depth += 1;
                            effects.push(TagEffect::Link {
                                href: safe_href,
                                start_len,
                            });
                        }
                    }
                }
            }
            "img" => {
                if let Some(src) = attr_value(&attrs, "src") {
                    let src_trim = src.trim();
                    if !is_unsafe_or_blob_image_src(src_trim) {
                        let safe_src = sanitize_md_url(src_trim);
                        if !safe_src.is_empty() {
                            self.has_rich_structure = true;
                            let alt = attr_value(&attrs, "alt")
                                .map(|a| a.trim().replace(['[', ']'], ""))
                                .unwrap_or_default();
                            self.push_inline_str(&format!("![{alt}]({safe_src})"));
                        }
                    }
                }
            }
            "b" | "strong" => {
                if css.bold != Some(false) {
                    self.has_rich_structure = true;
                    if self.bold_depth == 0 {
                        self.open_inline_marker("**");
                    }
                    self.bold_depth += 1;
                    effects.push(TagEffect::Bold);
                }
            }
            "i" | "em" | "cite" => {
                if css.italic != Some(false) {
                    self.has_rich_structure = true;
                    if self.italic_depth == 0 {
                        self.open_inline_marker("*");
                    }
                    self.italic_depth += 1;
                    effects.push(TagEffect::Italic);
                }
            }
            "u" | "ins" => {
                self.has_rich_structure = true;
                if self.underline_depth == 0 {
                    self.open_inline_marker("<u>");
                }
                self.underline_depth += 1;
                effects.push(TagEffect::Underline);
            }
            "s" | "del" | "strike" => {
                self.has_rich_structure = true;
                if self.strike_depth == 0 {
                    self.open_inline_marker("~~");
                }
                self.strike_depth += 1;
                effects.push(TagEffect::Strike);
            }
            _ => {}
        }

        // Handle inline CSS styles on <span>, <font>, <p>, etc.
        if matches!(tag.as_str(), "span" | "font" | "div" | "p") {
            if css.bold == Some(true) {
                self.has_rich_structure = true;
                if self.bold_depth == 0 {
                    self.open_inline_marker("**");
                }
                self.bold_depth += 1;
                effects.push(TagEffect::Bold);
            }
            if css.italic == Some(true) {
                self.has_rich_structure = true;
                if self.italic_depth == 0 {
                    self.open_inline_marker("*");
                }
                self.italic_depth += 1;
                effects.push(TagEffect::Italic);
            }
            if css.underline && self.link_depth == 0 {
                self.has_rich_structure = true;
                if self.underline_depth == 0 {
                    self.open_inline_marker("<u>");
                }
                self.underline_depth += 1;
                effects.push(TagEffect::Underline);
            }
            if css.strike {
                self.has_rich_structure = true;
                if self.strike_depth == 0 {
                    self.open_inline_marker("~~");
                }
                self.strike_depth += 1;
                effects.push(TagEffect::Strike);
            }
        }

        if !self_closing && self.tag_stack.len() < MAX_HTML_TAG_STACK {
            if effects.is_empty() {
                effects.push(TagEffect::None);
            }
            self.tag_stack.push((tag, effects));
        }
    }

    fn apply_close_effect(&mut self, effect: TagEffect) {
        match effect {
            TagEffect::None => {}
            TagEffect::Heading => {
                self.in_heading = false;
                self.request_breaks(2);
            }
            TagEffect::ParagraphOrDiv => {
                if self.table.is_none() {
                    self.request_breaks(if self.list_stack.is_empty() { 2 } else { 1 });
                }
            }
            TagEffect::Blockquote => {
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.request_breaks(2);
            }
            TagEffect::List => {
                self.list_stack.pop();
                let breaks = if self.list_stack.is_empty() { 2 } else { 1 };
                self.request_breaks(breaks);
            }
            TagEffect::ListItem => {
                self.request_breaks(1);
            }
            TagEffect::Pre => {
                if self.pre_depth > 0 {
                    self.pre_depth -= 1;
                    if self.pre_depth == 0 {
                        let fenced = wrap_fenced_code_block(&self.pre_lang, &self.pre_buf);
                        self.pre_buf.clear();
                        self.pre_lang.clear();
                        self.push_inline_str(&fenced);
                        self.request_breaks(2);
                    }
                }
            }
            TagEffect::InlineCode => {
                if self.inline_code_depth > 0 {
                    self.inline_code_depth -= 1;
                    if self.inline_code_depth == 0 {
                        let snippet = std::mem::take(&mut self.inline_code_buf);
                        let trimmed = snippet.trim();
                        if !trimmed.is_empty() {
                            let wrapped = wrap_inline_code_ticks(trimmed);
                            self.push_inline_str(&wrapped);
                        }
                    }
                }
            }
            TagEffect::Bold => {
                if self.bold_depth > 0 {
                    self.bold_depth -= 1;
                    if self.bold_depth == 0 {
                        self.close_inline_marker("**", "**");
                    }
                }
            }
            TagEffect::Italic => {
                if self.italic_depth > 0 {
                    self.italic_depth -= 1;
                    if self.italic_depth == 0 {
                        self.close_inline_marker("*", "*");
                    }
                }
            }
            TagEffect::Underline => {
                if self.underline_depth > 0 {
                    self.underline_depth -= 1;
                    if self.underline_depth == 0 {
                        self.close_inline_marker("<u>", "</u>");
                    }
                }
            }
            TagEffect::Strike => {
                if self.strike_depth > 0 {
                    self.strike_depth -= 1;
                    if self.strike_depth == 0 {
                        self.close_inline_marker("~~", "~~");
                    }
                }
            }
            TagEffect::Link { href, start_len } => {
                self.link_depth = self.link_depth.saturating_sub(1);
                let target_buf = if let Some(tbl) = self.table.as_mut() {
                    tbl.active_cell.as_mut().map(|c| &mut c.0)
                } else {
                    Some(&mut self.out)
                };
                if let Some(buf) = target_buf
                    && start_len <= buf.len() {
                        let inner = buf[start_len..].trim().replace('\n', " ");
                        buf.truncate(start_len);
                        if !inner.is_empty() {
                            let escaped_inner = if inner.starts_with("![") {
                                inner
                            } else {
                                inner.replace(['[', ']'], "\\$&")
                            };
                            buf.push_str(&format!("[{escaped_inner}]({href})"));
                        } else {
                            buf.push_str(&format!("[{href}]({href})"));
                        }
                    }
            }
            TagEffect::TableSection => {}
            TagEffect::TableRow => {}
            TagEffect::TableCell => {
                if let Some(tbl) = self.table.as_mut()
                    && let Some((raw, colspan, align)) = tbl.active_cell.take() {
                        let cleaned = normalize_ws_collapse(&raw).trim().to_string();
                        let row = match tbl.rows.last_mut() {
                            Some(r) => r,
                            None => {
                                tbl.rows.push(Vec::new());
                                tbl.rows.last_mut().unwrap()
                            }
                        };
                        row.push(TableCellData {
                            text: cleaned,
                            align,
                        });
                        for _ in 1..colspan {
                            row.push(TableCellData {
                                text: String::new(),
                                align,
                            });
                        }
                    }
            }
            TagEffect::Table => {
                if let Some(mut tbl) = self.table.take() {
                    if let Some((raw, _colspan, align)) = tbl.active_cell.take() {
                        let cleaned = normalize_ws_collapse(&raw).trim().to_string();
                        let row = match tbl.rows.last_mut() {
                            Some(r) => r,
                            None => {
                                tbl.rows.push(Vec::new());
                                tbl.rows.last_mut().unwrap()
                            }
                        };
                        row.push(TableCellData {
                            text: cleaned,
                            align,
                        });
                    }
                    tbl.rows.retain(|r| !r.is_empty());
                    if !tbl.rows.is_empty() {
                        let max_cols = tbl.rows.iter().map(|r| r.len()).max().unwrap_or(0);
                        if max_cols > 0 {
                            let mut alignments = vec![TableAlign::None; max_cols];
                            for (c, cell) in tbl.rows[0].iter().enumerate() {
                                if cell.align != TableAlign::None {
                                    alignments[c] = cell.align;
                                }
                            }
                            let mut header: Vec<String> = tbl
                                .rows
                                .remove(0)
                                .into_iter()
                                .map(|c| c.text)
                                .collect();
                            header.resize(max_cols, String::new());
                            let body: Vec<Vec<String>> = tbl
                                .rows
                                .into_iter()
                                .map(|r| {
                                    let mut cells: Vec<String> =
                                        r.into_iter().map(|c| c.text).collect();
                                    cells.resize(max_cols, String::new());
                                    cells
                                })
                                .collect();
                            let md_table = build_table_markdown(&alignments, &header, &body);
                            self.push_inline_str(md_table.trim_end());
                            self.request_breaks(2);
                        }
                    }
                }
            }
        }
    }

    fn handle_close(&mut self, tag: &str) {
        let Some(pos) = self.tag_stack.iter().rposition(|(t, _)| t == tag) else {
            return;
        };
        let mut to_close = self.tag_stack.drain(pos..).collect::<Vec<_>>();
        while let Some((_, effects)) = to_close.pop() {
            for eff in effects.into_iter().rev() {
                self.apply_close_effect(eff);
            }
        }
    }

    fn handle_text(&mut self, raw: &str) {
        let decoded = decode_html_entities(raw);
        if self.pre_depth > 0 {
            let text = decoded.replace('\u{00a0}', " ");
            self.pre_buf.push_str(&text);
            return;
        }

        let collapsed = normalize_ws_collapse(&decoded);
        if collapsed == " " {
            let cur_empty = if let Some(tbl) = self.table.as_ref() {
                tbl.active_cell
                    .as_ref()
                    .is_none_or(|c| c.0.is_empty() || c.0.ends_with(' '))
            } else if self.inline_code_depth > 0 {
                self.inline_code_buf.is_empty() || self.inline_code_buf.ends_with(' ')
            } else {
                self.pending_breaks > 0
                    || self.out.is_empty()
                    || self.out.ends_with('\n')
                    || self.out.ends_with(' ')
            };
            if cur_empty {
                return;
            }
        }
        let s = if self.table.is_none()
            && self.inline_code_depth == 0
            && (self.pending_breaks > 0
                || self.out.is_empty()
                || self.out.ends_with('\n')
                || self.out.ends_with(' '))
        {
            collapsed.trim_start_matches(' ')
        } else {
            collapsed.as_str()
        };
        if s.is_empty() {
            return;
        }
        if self.table.is_some() {
            if let Some(tbl) = self.table.as_mut()
                && let Some((ref mut cell, _, _)) = tbl.active_cell {
                    if cell.is_empty() || (cell.ends_with(' ') && s.starts_with(' ')) {
                        cell.push_str(s.trim_start_matches(' '));
                    } else {
                        cell.push_str(s);
                    }
                }
            return;
        }
        self.push_inline_str(s);
    }

    fn finish(mut self) -> Option<String> {
        while let Some((_, effects)) = self.tag_stack.pop() {
            for eff in effects.into_iter().rev() {
                self.apply_close_effect(eff);
            }
        }
        if !self.has_rich_structure {
            return None;
        }
        let cleaned = self
            .out
            .lines()
            .map(|l| l.trim_end_matches(' '))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        }
    }
}

/// Converts copied HTML (from Web browsers, Word, Google Docs, or Excel) into clean GFM Markdown.
/// Returns `None` when the HTML carries no Markdown-relevant formatting or structure (e.g., plain
/// syntax-colored code spans copied from an IDE), allowing callers to paste plain text verbatim.
pub fn html_to_markdown(html: &str) -> Option<String> {
    let tokens = tokenize_html(html);
    if tokens.is_empty() {
        return None;
    }
    let mut conv = HtmlMdConverter::new();
    for tok in tokens {
        match tok {
            HtmlToken::Open {
                tag,
                attrs,
                self_closing,
            } => conv.handle_open(tag, attrs, self_closing),
            HtmlToken::Close { tag } => conv.handle_close(&tag),
            HtmlToken::Text(raw) => conv.handle_text(raw),
        }
    }
    conv.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_cf_html_fragment_parses_offsets_and_markers() {
        let body = "<p>Hello <b>World</b></p>";
        let prefix = "Version:0.9\r\nStartHTML:00000097\r\nEndHTML:00000180\r\n";
        let start_marker = "<html><body><!--StartFragment-->";
        let end_marker = "<!--EndFragment--></body></html>";
        let header_stub = format!(
            "{prefix}StartFragment:{:08}\r\nEndFragment:{:08}\r\n",
            0, 0
        );
        let start_frag = header_stub.len() + start_marker.len();
        let end_frag = start_frag + body.len();
        let full = format!(
            "{prefix}StartFragment:{start_frag:08}\r\nEndFragment:{end_frag:08}\r\n{start_marker}{body}{end_marker}"
        );
        assert_eq!(extract_cf_html_fragment(&full), body);

        let marker_only = "Version:0.9\n<html><body><!--StartFragment--><em>Hi</em><!--EndFragment--></body></html>";
        assert_eq!(extract_cf_html_fragment(marker_only), "<em>Hi</em>");
    }

    #[test]
    fn converts_website_html_headings_formatting_links_images_lists_and_code() {
        let html = concat!(
            "<style>.x{color:red}</style><script>alert(1)</script>",
            "<h1>Release Notes</h1>",
            "<p>Check <strong>bold</strong>, <em>italic</em>, <u>under</u>, <del>old</del>, and <code>x + 1</code>. ",
            "Visit <a href=\"https://rust-lang.org\">Rust Docs</a> or ",
            "<a href=\"javascript:alert(1)\">Bad Link</a>.</p>",
            "<p><img src=\"https://example.com/logo.png\" alt=\"Logo\">",
            "<img src=\"data:image/png;base64,AAAA\" alt=\"InlineBlob\"></p>",
            "<ul>",
            "<li>First item<ul><li>Nested bullet</li></ul></li>",
            "<li><input type=\"checkbox\" checked> Done task</li>",
            "<li><input type=\"checkbox\"> Todo task</li>",
            "</ul>",
            "<ol start=\"3\"><li>Third</li><li>Fourth</li></ol>",
            "<blockquote><p>Quoted wisdom</p></blockquote>",
            "<pre><code class=\"language-rust\">fn main() {\n    println!(\"hi\");\n}</code></pre>",
            "<hr>"
        );
        let md = html_to_markdown(html).expect("rich HTML converts to Markdown");
        assert!(md.contains("# Release Notes"), "got:\n{md}");
        assert!(
            md.contains("**bold**, *italic*, <u>under</u>, ~~old~~, and `x + 1`"),
            "got:\n{md}"
        );
        assert!(md.contains("[Rust Docs](https://rust-lang.org)"), "got:\n{md}");
        assert!(!md.contains("javascript:"), "unsafe link must be stripped:\n{md}");
        assert!(
            md.contains("![Logo](https://example.com/logo.png)"),
            "got:\n{md}"
        );
        assert!(
            !md.contains("data:image/png"),
            "data: base64 image must be skipped:\n{md}"
        );
        assert!(md.contains("- First item\n  - Nested bullet"), "got:\n{md}");
        assert!(md.contains("- [x] Done task"), "got:\n{md}");
        assert!(md.contains("- [ ] Todo task"), "got:\n{md}");
        assert!(md.contains("3. Third\n4. Fourth"), "got:\n{md}");
        assert!(md.contains("> Quoted wisdom"), "got:\n{md}");
        assert!(
            md.contains("```rust\nfn main() {\n    println!(\"hi\");\n}\n```"),
            "got:\n{md}"
        );
        assert!(md.contains("---"), "got:\n{md}");
    }

    #[test]
    fn converts_word_google_docs_and_excel_html_cleanly() {
        let word_html = concat!(
            "<!--[if gte mso 9]><xml><w:WordDocument><w:View>Normal</w:View></w:WordDocument></xml><![endif]-->",
            "<b style=\"font-weight:normal\">",
            "<p class=\"MsoNormal\">Quarterly <span style=\"font-weight:700;font-style:italic\">Summary</span>",
            "<o:p>&nbsp;</o:p></p>",
            "<p class=\"MsoNormal\"><span style=\"text-decoration: underline line-through\">Amended</span></p>",
            "</b>"
        );
        let word_md = html_to_markdown(word_html).expect("Word/Docs HTML converts");
        assert!(word_md.contains("Quarterly ***Summary***") || word_md.contains("Quarterly ** *Summary* **") || word_md.contains("***Summary***") || word_md.contains("**Summary**"), "got:\n{word_md}");
        assert!(!word_md.starts_with("**Quarterly"), "font-weight:normal wrapper must not bold all text:\n{word_md}");
        assert!(word_md.contains("<u>~~Amended~~</u>") || word_md.contains("~~<u>Amended</u>~~"), "got:\n{word_md}");

        let excel_html = concat!(
            "<table>",
            "<tr><td>Item</td><td align=\"right\">Qty</td><td style=\"text-align:center\">Total</td></tr>",
            "<tr><td>Pen | Blue</td><td>2</td><td>10</td></tr>",
            "</table>"
        );
        let excel_md = html_to_markdown(excel_html).expect("Excel HTML table converts");
        assert!(excel_md.contains("| Item | Qty | Total |"), "got:\n{excel_md}");
        assert!(excel_md.contains("| --- | --: | :-: |"), "got:\n{excel_md}");
        assert!(excel_md.contains("| Pen \\| Blue | 2 | 10 |"), "got:\n{excel_md}");
    }

    #[test]
    fn converts_tsv_grids_and_falls_back_for_plain_ide_code() {
        let tsv = "Name\tScore\tGrade\r\nAlice\t95\tA\r\nBob | Jr\t88\tB\r\n";
        let smart = smart_paste_markdown(tsv, None);
        assert!(smart.contains("| Name | Score | Grade |"), "got:\n{smart}");
        assert!(smart.contains("| Alice | 95 | A |"), "got:\n{smart}");
        assert!(smart.contains("| Bob \\| Jr | 88 | B |"), "got:\n{smart}");

        // Indented two-line code with leading tabs must NOT become a table
        let indented_code = "\tlet a = 1;\n\tlet b = 2;\n";
        assert_eq!(tsv_to_markdown_table(indented_code), None);

        // Plain VS Code colored spans (no bold/italic/heading/code tags) must return None -> plain text
        let vscode_html = "<div style=\"color:#d4d4d4;background-color:#1e1e1e\"><div><span style=\"color:#569cd6\">fn</span> <span style=\"color:#dcdcaa\">demo</span>() {}</div></div>";
        assert_eq!(html_to_markdown(vscode_html), None);
        assert_eq!(
            smart_paste_markdown("fn demo() {}", Some(vscode_html)),
            "fn demo() {}"
        );
    }

    #[test]
    fn html_matches_plain_text_guards_against_stale_clipboard_html() {
        assert!(html_matches_plain_text(
            "Quarterly Summary\nAmended",
            "<p>Quarterly <b>Summary</b></p><p>Amended</p>"
        ));
        assert!(!html_matches_plain_text(
            "Col1\tCol2\n1\t2",
            "<h1>Unrelated Website Article</h1>"
        ));
    }

    fn build_test_dib(
        width: i32,
        height: i32,
        bpp: u16,
        compression: u32,
        pixels: &[u8],
    ) -> Vec<u8> {
        let mut dib = vec![0u8; 40];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&width.to_le_bytes());
        dib[8..12].copy_from_slice(&height.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&bpp.to_le_bytes());
        dib[16..20].copy_from_slice(&compression.to_le_bytes());
        dib.extend_from_slice(pixels);
        dib
    }

    #[test]
    fn dib_to_png_bytes_decodes_24bit_padded_and_32bit_dib_images() {
        // 1) 24-bit 3x2 bottom-up DIB (3 * 3 = 9 bytes -> padded to 12 bytes per row)
        // Bottom row (y=1): blue pixel, green pixel, red pixel + 3 padding bytes
        // Top row (y=0): white, black, yellow + 3 padding bytes
        let row_bottom = [
            255, 0, 0, // B=255 -> Blue
            0, 255, 0, // G=255 -> Green
            0, 0, 255, // R=255 -> Red
            0, 0, 0, // DWORD padding
        ];
        let row_top = [
            255, 255, 255, // White
            0, 0, 0, // Black
            0, 255, 255, // Yellow (R=255,G=255,B=0)
            0, 0, 0, // DWORD padding
        ];
        let mut pixels_24 = Vec::new();
        pixels_24.extend_from_slice(&row_bottom);
        pixels_24.extend_from_slice(&row_top);
        let dib24 = build_test_dib(3, 2, 24, 0, &pixels_24);
        let png24 = dib_to_png_bytes(&dib24).expect("24-bit DIB must encode to PNG");
        let img24 = image::load_from_memory(&png24).unwrap().to_rgba8();
        assert_eq!(img24.dimensions(), (3, 2));
        assert_eq!(img24.get_pixel(0, 0).0, [255, 255, 255, 255]);
        assert_eq!(img24.get_pixel(2, 0).0, [255, 255, 0, 255]);
        assert_eq!(img24.get_pixel(0, 1).0, [0, 0, 255, 255]);
        assert_eq!(img24.get_pixel(2, 1).0, [255, 0, 0, 255]);

        // 2) 32-bit top-down (height = -2) BI_RGB with 0x00 reserved alpha bytes -> promoted to opaque 255
        let pixels_32_opaque = [
            10, 20, 30, 0, 40, 50, 60, 0, // top row (BGRA)
            70, 80, 90, 0, 100, 110, 120, 0, // bottom row
        ];
        let dib32 = build_test_dib(2, -2, 32, 0, &pixels_32_opaque);
        let png32 = dib_to_png_bytes(&dib32).expect("32-bit DIB must encode to PNG");
        let img32 = image::load_from_memory(&png32).unwrap().to_rgba8();
        assert_eq!(img32.dimensions(), (2, 2));
        assert_eq!(img32.get_pixel(0, 0).0, [30, 20, 10, 255]);
        assert_eq!(img32.get_pixel(1, 1).0, [120, 110, 100, 255]);

        // 3) Invalid/truncated DIB safely returns None
        assert!(dib_to_png_bytes(&[0u8; 12]).is_none());
    }

    #[test]
    fn save_pasted_image_png_writes_assets_relative_and_temp_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let doc_path = dir.path().join("notes.md");
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

        let (saved_a, url_a) = save_pasted_image_png(&png, Some(&doc_path)).unwrap();
        let (saved_b, url_b) = save_pasted_image_png(&png, Some(&doc_path)).unwrap();
        assert!(saved_a.exists());
        assert!(saved_b.exists());
        assert_ne!(saved_a, saved_b);
        assert!(url_a.starts_with("assets/pasted-image-"));
        assert!(url_b.starts_with("assets/pasted-image-"));

        let (saved_temp, url_temp) = save_pasted_image_png(&png, None).unwrap();
        assert!(saved_temp.exists());
        assert!(url_temp.contains("pasted-image-"));
        let _ = std::fs::remove_file(&saved_temp);
    }

    #[test]
    fn html_to_markdown_handles_multibyte_utf8_and_strips_dangerous_schemes() {
        let malicious_utf8 = "<p>مرحبا — café <a href=\"javascript:alert(1)\">bad</a> and \
             <a href=\"data:text/html,<script>1</script>\">data</a> and \
             <a href=\"https://example.org/ok\">ok</a></p>";
        let md = html_to_markdown(malicious_utf8).expect("valid sanitized markdown");
        assert!(md.contains("مرحبا — café"));
        assert!(!md.to_ascii_lowercase().contains("javascript:"));
        assert!(!md.to_ascii_lowercase().contains("data:text/html"));
        assert!(md.contains("[ok](https://example.org/ok)"));

        let preview = extract_plain_preview_from_html("<p><b>عربي 🚀</b> test</p>");
        assert!(preview.contains("عربي 🚀"));
    }
}
