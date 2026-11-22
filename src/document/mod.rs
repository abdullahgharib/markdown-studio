pub mod ast;
pub mod calc;
pub mod clipboard;
pub mod editing;
pub mod math;
pub mod mermaid;
pub mod search;
pub mod unicode;
pub mod viewport;

use std::hash::{Hash, Hasher};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ropey::Rope;
use rustc_hash::FxHasher;

use crate::document::unicode::{
    next_grapheme_char_idx, prev_grapheme_char_idx, snap_to_grapheme_start,
};

const MAX_UNDO_OPS: usize = 500;
const MIN_UNDO_KEEP: usize = 16;
const MAX_UNDO_BYTES: usize = 32 * 1024 * 1024;
const UNDO_COALESCE_WINDOW: Duration = Duration::from_millis(1500);
const MAX_COALESCED_CHARS: usize = 128;
const DIRTY_HASH_MAX_BYTES: usize = 2_000_000;

static NEXT_BUFFER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[inline]
fn next_buffer_id() -> u64 {
    NEXT_BUFFER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[inline]
fn is_single_non_newline_char(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c != '\n' && c != '\r') && chars.next().is_none()
}

/// A reversible, surgical edit or file-rename operation for O(log n) undo/redo.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EditOp {
    Text {
        char_idx: usize,
        deleted: String,
        inserted: String,
        cursor_before: usize,
        cursor_after: usize,
    },
    Batch {
        /// Tuples of `(char_idx, deleted, inserted)` stored in reverse-offset execution order.
        edits: Vec<(usize, String, String)>,
        cursor_before: usize,
        cursor_after: usize,
    },
    Rename {
        old_path: PathBuf,
        new_path: PathBuf,
    },
}

/// Lightweight filesystem snapshot `(mtime, byte_len)` for detecting external edits without reading file bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskFileStamp {
    pub modified: std::time::SystemTime,
    pub len: u64,
}

impl DiskFileStamp {
    pub fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let modified = meta.modified().ok()?;
        Some(Self {
            modified,
            len: meta.len(),
        })
    }
}

/// Outcome returned by `DocumentBuffer::undo_with_outcome` / `redo_with_outcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoRedoOutcome {
    None,
    TextEdited,
    Renamed { from: PathBuf, to: PathBuf },
    RenameFailed(String),
}

fn paths_equivalent_on_fs(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn path_display_filename(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.to_string_lossy().to_string())
}

fn format_history_snippet(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut truncated = false;
    for (count, ch) in s.chars().enumerate() {
        if count >= max_chars {
            truncated = true;
            break;
        }
        match ch {
            '\n' => out.push('↵'),
            '\r' => {}
            '\t' => out.push('→'),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    if truncated {
        out.push('…');
    }
    out
}

impl EditOp {
    fn byte_size(&self) -> usize {
        match self {
            EditOp::Text {
                deleted, inserted, ..
            } => deleted.len() + inserted.len(),
            EditOp::Batch { edits, .. } => {
                edits.iter().map(|(_, d, i)| d.len() + i.len()).sum()
            }
            EditOp::Rename { old_path, new_path } => {
                old_path.as_os_str().len() + new_path.as_os_str().len()
            }
        }
    }

    /// Generates a concise, human-readable description of this operation for history menus.
    fn describe(&self) -> String {
        match self {
            EditOp::Batch { edits, .. } => format!("Batch edit ({} regions)", edits.len()),
            EditOp::Rename { old_path, new_path } => {
                format!(
                    "Renamed {} → {}",
                    path_display_filename(old_path),
                    path_display_filename(new_path)
                )
            }
            EditOp::Text {
                deleted, inserted, ..
            } => {
                let ins_chars = inserted.chars().count();
                let del_chars = deleted.chars().count();
                match (del_chars > 0, ins_chars > 0) {
                    (false, true) => {
                        let lines = inserted.matches('\n').count();
                        if ins_chars > 28 && lines > 0 {
                            format!("Inserted {ins_chars} chars ({} lines)", lines + 1)
                        } else {
                            format!("Typed \"{}\"", format_history_snippet(inserted, 24))
                        }
                    }
                    (true, false) => {
                        if del_chars > 28 {
                            format!("Deleted {del_chars} chars")
                        } else {
                            format!("Deleted \"{}\"", format_history_snippet(deleted, 24))
                        }
                    }
                    (true, true) => {
                        let d_snip = format_history_snippet(deleted, 12);
                        let i_snip = format_history_snippet(inserted, 12);
                        format!("Replaced \"{d_snip}\" → \"{i_snip}\"")
                    }
                    (false, false) => "No-op edit".to_string(),
                }
            }
        }
    }
}

/// High-speed UTF-8 B-tree rope document buffer with O(log n) edits and surgical undo/redo.
#[derive(Debug, Clone)]
pub struct DocumentBuffer {
    buf_id: u64,
    rope: Rope,
    undo_stack: std::collections::VecDeque<EditOp>,
    redo_stack: Vec<EditOp>,
    undo_bytes: usize,
    last_undo_time: Option<Instant>,
    pub cursor_char: usize,
    pub selection_anchor: Option<usize>,
    pub cached_selection: Option<(usize, usize)>,
    pub cached_selection_version: u64,
    pub file_path: Option<PathBuf>,
    pub dirty: bool,
    pub version: u64,
    clean_undo_depth: Option<usize>,
    saved_len_bytes: usize,
    saved_hash: u64,
    cached_word_count: usize,
    cached_line_ending: &'static str,
}

fn count_words_full(rope: &Rope) -> usize {
    let mut count = 0usize;
    let mut in_word = false;
    for chunk in rope.chunks() {
        for ch in chunk.chars() {
            if ch.is_whitespace() {
                in_word = false;
            } else if !in_word {
                in_word = true;
                count += 1;
            }
        }
    }
    count
}

struct StreamingWordCounter {
    count: usize,
    in_word: bool,
    bom_checked: bool,
    bom_buf: [u8; 3],
    bom_len: usize,
    pending: [u8; 4],
    pending_len: usize,
    utf8_ok: bool,
}

impl StreamingWordCounter {
    fn new() -> Self {
        Self {
            count: 0,
            in_word: false,
            bom_checked: false,
            bom_buf: [0; 3],
            bom_len: 0,
            pending: [0; 4],
            pending_len: 0,
            utf8_ok: true,
        }
    }

    #[inline]
    fn feed_str(&mut self, s: &str) {
        if s.is_ascii() {
            for &b in s.as_bytes() {
                if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
                    self.in_word = false;
                } else if !self.in_word {
                    self.in_word = true;
                    self.count += 1;
                }
            }
            return;
        }
        for ch in s.chars() {
            if ch.is_whitespace() {
                self.in_word = false;
            } else if !self.in_word {
                self.in_word = true;
                self.count += 1;
            }
        }
    }

    fn feed_utf8_bytes(&mut self, mut bytes: &[u8]) {
        if !self.utf8_ok || bytes.is_empty() {
            return;
        }
        while self.pending_len > 0 && !bytes.is_empty() {
            let take = (4 - self.pending_len).min(bytes.len());
            let mut buf = self.pending;
            buf[self.pending_len..self.pending_len + take].copy_from_slice(&bytes[..take]);
            let combined_len = self.pending_len + take;
            match std::str::from_utf8(&buf[..combined_len]) {
                Ok(s) => {
                    self.feed_str(s);
                    self.pending_len = 0;
                    bytes = &bytes[take..];
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        if let Ok(s) = std::str::from_utf8(&buf[..valid]) {
                            self.feed_str(s);
                        }
                        if e.error_len().is_some() {
                            self.utf8_ok = false;
                            self.pending_len = 0;
                            return;
                        }
                        let rem = combined_len - valid;
                        self.pending[..rem].copy_from_slice(&buf[valid..combined_len]);
                        self.pending_len = rem;
                        bytes = &bytes[take..];
                    } else if e.error_len().is_some() {
                        self.utf8_ok = false;
                        self.pending_len = 0;
                        return;
                    } else {
                        self.pending = buf;
                        self.pending_len = combined_len;
                        return;
                    }
                }
            }
        }

        if bytes.is_empty() {
            return;
        }

        match std::str::from_utf8(bytes) {
            Ok(s) => self.feed_str(s),
            Err(e) => {
                let valid = e.valid_up_to();
                if let Ok(s) = std::str::from_utf8(&bytes[..valid]) {
                    self.feed_str(s);
                }
                if e.error_len().is_some() {
                    self.utf8_ok = false;
                    return;
                }
                let rem = bytes.len() - valid;
                if rem > 3 {
                    self.utf8_ok = false;
                    return;
                }
                self.pending[..rem].copy_from_slice(&bytes[valid..]);
                self.pending_len = rem;
            }
        }
    }

    fn feed_bytes(&mut self, mut bytes: &[u8]) {
        if !self.utf8_ok || bytes.is_empty() {
            return;
        }
        if !self.bom_checked {
            if self.bom_len == 0 && bytes[0] != 0xEF {
                self.bom_checked = true;
            } else {
                let need = (3 - self.bom_len).min(bytes.len());
                self.bom_buf[self.bom_len..self.bom_len + need].copy_from_slice(&bytes[..need]);
                self.bom_len += need;
                bytes = &bytes[need..];
                if self.bom_len < 3 {
                    return;
                }
                self.bom_checked = true;
                if self.bom_buf != [0xEF, 0xBB, 0xBF] {
                    let prefix = self.bom_buf;
                    self.feed_utf8_bytes(&prefix);
                }
            }
        }
        self.feed_utf8_bytes(bytes);
    }

    fn finish_word_count(mut self) -> Option<usize> {
        if !self.bom_checked && self.bom_len > 0 {
            let rem = self.bom_buf;
            let len = self.bom_len;
            self.bom_checked = true;
            self.feed_utf8_bytes(&rem[..len]);
        }
        if self.utf8_ok && self.pending_len == 0 {
            Some(self.count)
        } else {
            None
        }
    }
}

struct StreamingWordCountReader<R: Read> {
    inner: R,
    counter: StreamingWordCounter,
}

impl<R: Read> StreamingWordCountReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            counter: StreamingWordCounter::new(),
        }
    }
}

impl<R: Read> Read for StreamingWordCountReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.counter.feed_bytes(&buf[..n]);
        }
        Ok(n)
    }
}

fn count_word_starts(mut prev_is_ws: bool, chars: impl Iterator<Item = char>) -> usize {
    let mut count = 0usize;
    for ch in chars {
        let is_ws = ch.is_whitespace();
        if !is_ws && prev_is_ws {
            count += 1;
        }
        prev_is_ws = is_ws;
    }
    count
}

fn detect_first_line_ending(rope: &Rope) -> &'static str {
    if rope.len_lines() == 0 {
        return "\n";
    }
    let line0 = rope.line(0);
    let len = line0.len_chars();
    if (len >= 2 && line0.char(len - 1) == '\n' && line0.char(len - 2) == '\r')
        || (len >= 1 && line0.char(len - 1) == '\r')
    {
        "\r\n"
    } else {
        "\n"
    }
}

fn compute_rope_hash(rope: &Rope) -> u64 {
    let mut hasher = FxHasher::default();
    rope.len_bytes().hash(&mut hasher);
    for chunk in rope.chunks() {
        chunk.as_bytes().hash(&mut hasher);
    }
    hasher.finish()
}

impl Default for DocumentBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentBuffer {
    pub fn new() -> Self {
        let rope = Rope::new();
        let saved_hash = compute_rope_hash(&rope);
        Self {
            buf_id: next_buffer_id(),
            rope,
            undo_stack: std::collections::VecDeque::new(),
            redo_stack: Vec::new(),
            undo_bytes: 0,
            last_undo_time: None,
            cursor_char: 0,
            selection_anchor: None,
            cached_selection: None,
            cached_selection_version: 1,
            file_path: None,
            dirty: false,
            version: 1,
            clean_undo_depth: Some(0),
            saved_len_bytes: 0,
            saved_hash,
            cached_word_count: 0,
            cached_line_ending: "\n",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        let rope = Rope::from_str(text);
        let saved_len_bytes = rope.len_bytes();
        let saved_hash = if saved_len_bytes <= DIRTY_HASH_MAX_BYTES {
            compute_rope_hash(&rope)
        } else {
            0
        };
        let cached_word_count = count_words_full(&rope);
        let cached_line_ending = detect_first_line_ending(&rope);
        Self {
            buf_id: next_buffer_id(),
            rope,
            undo_stack: std::collections::VecDeque::new(),
            redo_stack: Vec::new(),
            undo_bytes: 0,
            last_undo_time: None,
            cursor_char: 0,
            selection_anchor: None,
            cached_selection: None,
            cached_selection_version: 1,
            file_path: None,
            dirty: false,
            version: 1,
            clean_undo_depth: Some(0),
            saved_len_bytes,
            saved_hash,
            cached_word_count,
            cached_line_ending,
        }
    }

    /// Reconstructs a `DocumentBuffer` transferred from a detached tab handoff,
    /// preserving original file association and unsaved (`dirty`) state.
    pub fn from_handoff(text: &str, file_path: Option<PathBuf>, dirty: bool) -> Self {
        let mut buf = Self::from_str(text);
        buf.file_path = file_path;
        if dirty {
            buf.dirty = true;
            buf.clean_undo_depth = None;
            buf.saved_len_bytes = usize::MAX;
            buf.saved_hash = 0;
        }
        buf
    }

    /// Returns `true` when `path` appears to be a readable text file (UTF-8, UTF-16, code, config, CSV, logs, etc.) rather than a binary blob.
    pub fn is_likely_text_file(path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        if !path.is_file() {
            return false;
        }
        let Ok(mut file) = std::fs::File::open(path) else {
            return false;
        };
        let mut sample = [0u8; 8192];
        let Ok(n) = file.read(&mut sample) else {
            return false;
        };
        if n == 0 {
            return true;
        }
        let bytes = &sample[..n];
        if has_binary_magic_prefix(bytes) {
            return false;
        }
        if detect_utf16_bom(bytes).is_some() {
            return true;
        }
        if bytes.contains(&0) {
            return false;
        }
        if is_known_text_extension(path) {
            return true;
        }
        is_valid_utf8_prefix(bytes) || is_printable_text_sample(bytes)
    }

    /// Reads a UTF-8, UTF-16 (LE/BE BOM), or compatible plain-text file (stripping any optional BOM) into the B-tree rope.
    pub fn open_file(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut head = [0u8; 8];
        let mut file = std::fs::File::open(&path)?;
        let head_len = file.read(&mut head)?;
        let utf16_bom = detect_utf16_bom(&head[..head_len]);

        let mut streamed_words: Option<usize> = None;
        let mut rope = if utf16_bom.is_none() {
            let chained = io::Cursor::new(&head[..head_len]).chain(file);
            let buf_reader = BufReader::with_capacity(64 * 1024, chained);
            let mut counting_reader = StreamingWordCountReader::new(buf_reader);
            match Rope::from_reader(&mut counting_reader) {
                Ok(r) => {
                    streamed_words = counting_reader.counter.finish_word_count();
                    r
                }
                Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                    let bytes = std::fs::read(&path)?;
                    if has_binary_magic_prefix(&bytes) || bytes.contains(&0) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "File contains binary data and cannot be opened as text",
                        ));
                    }
                    let decoded = String::from_utf8_lossy(&bytes);
                    Rope::from_str(&decoded)
                }
                Err(err) => return Err(err),
            }
        } else {
            drop(file);
            let bytes = std::fs::read(&path)?;
            let decoded = decode_utf16_bytes(&bytes, utf16_bom.unwrap_or(true));
            Rope::from_str(&decoded)
        };

        if rope.len_chars() > 0 && rope.char(0) == '\u{FEFF}' {
            rope.remove(0..1);
        }
        let saved_len_bytes = rope.len_bytes();
        let saved_hash = if saved_len_bytes <= DIRTY_HASH_MAX_BYTES {
            compute_rope_hash(&rope)
        } else {
            0
        };
        let cached_word_count = streamed_words.unwrap_or_else(|| count_words_full(&rope));
        let cached_line_ending = detect_first_line_ending(&rope);
        Ok(Self {
            buf_id: next_buffer_id(),
            rope,
            undo_stack: std::collections::VecDeque::new(),
            redo_stack: Vec::new(),
            undo_bytes: 0,
            last_undo_time: None,
            cursor_char: 0,
            selection_anchor: None,
            cached_selection: None,
            cached_selection_version: 1,
            file_path: Some(path),
            dirty: false,
            version: 1,
            clean_undo_depth: Some(0),
            saved_len_bytes,
            saved_hash,
            cached_word_count,
            cached_line_ending,
        })
    }

    /// Streams a UTF-8 crash-recovery snapshot from `recovery_path` without materializing a full `String`.
    pub fn open_recovery_file(
        recovery_path: impl AsRef<Path>,
        original_path: Option<PathBuf>,
        dirty: bool,
    ) -> io::Result<Self> {
        let file = std::fs::File::open(recovery_path.as_ref())?;
        let reader = BufReader::with_capacity(64 * 1024, file);
        let mut counting_reader = StreamingWordCountReader::new(reader);
        let mut rope = Rope::from_reader(&mut counting_reader)?;
        let streamed_words = counting_reader.counter.finish_word_count();
        if rope.len_chars() > 0 && rope.char(0) == '\u{FEFF}' {
            rope.remove(0..1);
        }
        let saved_len_bytes = rope.len_bytes();
        let cached_word_count = streamed_words.unwrap_or_else(|| count_words_full(&rope));
        let cached_line_ending = detect_first_line_ending(&rope);
        let saved_hash = if !dirty && saved_len_bytes <= DIRTY_HASH_MAX_BYTES {
            compute_rope_hash(&rope)
        } else {
            0
        };
        Ok(Self {
            buf_id: next_buffer_id(),
            rope,
            undo_stack: std::collections::VecDeque::new(),
            redo_stack: Vec::new(),
            undo_bytes: 0,
            last_undo_time: None,
            cursor_char: 0,
            selection_anchor: None,
            cached_selection: None,
            cached_selection_version: 1,
            file_path: original_path,
            dirty,
            version: 1,
            clean_undo_depth: if dirty { None } else { Some(0) },
            saved_len_bytes: if dirty { usize::MAX } else { saved_len_bytes },
            saved_hash,
            cached_word_count,
            cached_line_ending,
        })
    }

    /// Marks the current buffer content and undo position as saved / unmodified.
    pub fn mark_clean(&mut self) {
        self.clean_undo_depth = Some(self.undo_stack.len());
        self.saved_len_bytes = self.rope.len_bytes();
        self.saved_hash = if self.saved_len_bytes <= DIRTY_HASH_MAX_BYTES {
            compute_rope_hash(&self.rope)
        } else {
            0
        };
        self.dirty = false;
        self.last_undo_time = None;
    }

    fn update_dirty_flag(&mut self) {
        if self.clean_undo_depth == Some(self.undo_stack.len()) {
            self.dirty = false;
            return;
        }
        if self.rope.len_bytes() == self.saved_len_bytes
            && self.rope.len_bytes() <= DIRTY_HASH_MAX_BYTES
            && compute_rope_hash(&self.rope) == self.saved_hash
        {
            self.dirty = false;
            return;
        }
        self.dirty = true;
    }

    /// Saves the document buffer explicitly encoded as UTF-8 bytes and flushes to disk atomically.
    pub fn save_to_file(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document.md");

        static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let tmp_path = parent.join(format!(".{file_name}.{pid}.{seq}.mdstudio_tmp"));

        let write_res: io::Result<()> = (|| {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            let mut writer = BufWriter::with_capacity(64 * 1024, file);
            for chunk in self.rope.chunks() {
                writer.write_all(chunk.as_bytes())?;
            }
            writer.flush()?;
            writer.get_ref().sync_data()?;
            Ok(())
        })();

        if let Err(err) = write_res {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err);
        }

        let rename_res = match std::fs::rename(&tmp_path, &path) {
            Ok(()) => Ok(()),
            Err(first_err) if path.exists() => {
                let bak_path = parent.join(format!(".{file_name}.{pid}.{seq}.mdstudio_bak"));
                if std::fs::rename(&path, &bak_path).is_ok() {
                    match std::fs::rename(&tmp_path, &path) {
                        Ok(()) => {
                            let _ = std::fs::remove_file(&bak_path);
                            Ok(())
                        }
                        Err(err) => {
                            let _ = std::fs::rename(&bak_path, &path);
                            Err(err)
                        }
                    }
                } else {
                    Err(first_err)
                }
            }
            Err(first_err) => Err(first_err),
        };

        if let Err(err) = rename_res {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err);
        }

        self.file_path = Some(path);
        self.mark_clean();
        Ok(())
    }

    #[inline]
    pub fn buf_id(&self) -> u64 {
        self.buf_id
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    /// Returns the active selection range `[start, end)` in char indices if non-empty.
    pub fn selection_range(&self) -> Option<Range<usize>> {
        let anchor = self.selection_anchor?;
        let cursor = self.cursor_char.min(self.len_chars());
        let anchor = anchor.min(self.len_chars());
        if anchor == cursor {
            None
        } else if anchor < cursor {
            Some(anchor..cursor)
        } else {
            Some(cursor..anchor)
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.cached_selection = None;
    }

    pub fn cache_current_selection(&mut self) {
        let len = self.len_chars();
        let cursor = self.cursor_char.min(len);
        if let Some(anchor) = self.selection_anchor {
            let anchor = anchor.min(len);
            if anchor != cursor {
                self.cached_selection = Some((anchor, cursor));
                self.cached_selection_version = self.version;
            }
        }
    }

    pub fn restore_cached_selection(&mut self) -> bool {
        if self.selection_range().is_some() {
            return true;
        }
        if let Some((anchor, cursor)) = self.cached_selection {
            if self.cached_selection_version == self.version {
                let len = self.len_chars();
                let a = anchor.min(len);
                let c = cursor.min(len);
                if a != c {
                    self.selection_anchor = Some(a);
                    self.cursor_char = c;
                    return true;
                }
            }
        }
        false
    }

    /// Breaks any active typing-burst coalescing group so the next edit starts a fresh undo entry.
    pub fn break_undo_coalescing(&mut self) {
        self.last_undo_time = None;
    }

    pub fn set_cursor(&mut self, char_idx: usize, extend_selection: bool) {
        let clamped = char_idx.min(self.len_chars());
        if self.cursor_char != clamped {
            self.last_undo_time = None;
        }
        if extend_selection {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_char);
            }
            self.cursor_char = clamped;
            self.cache_current_selection();
        } else {
            self.selection_anchor = None;
            self.cached_selection = None;
            self.cursor_char = clamped;
        }
    }

    /// Snaps the current cursor position backward to a valid grapheme-cluster start on its line.
    pub fn snap_cursor_to_grapheme(&mut self) {
        let (line_idx, col) = self.char_to_line_col(self.cursor_char);
        let line_str = self.line_text(line_idx);
        let snapped = snap_to_grapheme_start(&line_str, col);
        self.cursor_char = self.line_col_to_char(line_idx, snapped);
    }

    /// Moves the cursor left by one full Unicode grapheme cluster (or end of previous line).
    pub fn move_left_grapheme(&mut self, extend_selection: bool) {
        let (line_idx, col) = self.char_to_line_col(self.cursor_char);
        if col > 0 {
            let line_str = self.line_text(line_idx);
            let prev_col = prev_grapheme_char_idx(&line_str, col);
            let target = self.line_col_to_char(line_idx, prev_col);
            self.set_cursor(target, extend_selection);
        } else if line_idx > 0 {
            let target = self.line_col_to_char(line_idx - 1, usize::MAX);
            self.set_cursor(target, extend_selection);
        } else if !extend_selection {
            self.set_cursor(0, false);
        }
    }

    /// Moves the cursor right by one full Unicode grapheme cluster (or start of next line).
    pub fn move_right_grapheme(&mut self, extend_selection: bool) {
        let (line_idx, col) = self.char_to_line_col(self.cursor_char);
        let line_str = self.line_text(line_idx);
        let line_len = line_str.chars().count();
        if col < line_len {
            let next_col = next_grapheme_char_idx(&line_str, col);
            let target = self.line_col_to_char(line_idx, next_col);
            self.set_cursor(target, extend_selection);
        } else if line_idx + 1 < self.len_lines() {
            let target = self.line_to_char(line_idx + 1);
            self.set_cursor(target, extend_selection);
        } else if !extend_selection {
            self.set_cursor(self.len_chars(), false);
        }
    }

    /// Deletes either the active selection or the single grapheme cluster before the cursor.
    pub fn backspace_grapheme(&mut self) {
        if let Some(sel) = self.selection_range() {
            self.remove(sel);
            return;
        }
        let c = self.cursor_char.min(self.len_chars());
        if c == 0 {
            return;
        }
        let (line_idx, col) = self.char_to_line_col(c);
        if col > 0 {
            let line_str = self.line_text(line_idx);
            let prev_col = prev_grapheme_char_idx(&line_str, col);
            let start = self.line_col_to_char(line_idx, prev_col);
            self.remove(start..c);
        } else if c >= 2 && self.rope.char(c - 1) == '\n' && self.rope.char(c - 2) == '\r' {
            self.remove((c - 2)..c);
        } else {
            self.remove((c - 1)..c);
        }
    }

    /// Deletes either the active selection or the single grapheme cluster after the cursor.
    pub fn delete_grapheme(&mut self) {
        if let Some(sel) = self.selection_range() {
            self.remove(sel);
            return;
        }
        let len = self.len_chars();
        let c = self.cursor_char.min(len);
        if c >= len {
            return;
        }
        let (line_idx, col) = self.char_to_line_col(c);
        let line_str = self.line_text(line_idx);
        let line_len = line_str.chars().count();
        if col < line_len {
            let next_col = next_grapheme_char_idx(&line_str, col);
            let end = self.line_col_to_char(line_idx, next_col);
            self.remove(c..end);
        } else if c + 1 < len && self.rope.char(c) == '\r' && self.rope.char(c + 1) == '\n' {
            self.remove(c..(c + 2));
        } else {
            self.remove(c..(c + 1));
        }
    }

    pub fn selected_text(&self) -> String {
        match self.selection_range() {
            Some(range) => self.rope.slice(range).to_string(),
            None => String::new(),
        }
    }

    fn apply_rope_edit(&mut self, start: usize, end: usize, insert_text: &str) {
        let total = self.rope.len_chars();
        let s = start.min(total);
        let e = end.min(total).max(s);

        let prev_ws = if s == 0 {
            true
        } else {
            self.rope.char(s - 1).is_whitespace()
        };
        let after_ch = if e < total {
            Some(self.rope.char(e))
        } else {
            None
        };
        let old_delta = count_word_starts(prev_ws, self.rope.slice(s..e).chars().chain(after_ch));
        let new_delta = count_word_starts(prev_ws, insert_text.chars().chain(after_ch));

        if s < e {
            self.rope.remove(s..e);
        }
        if !insert_text.is_empty() {
            self.rope.insert(s, insert_text);
        }

        self.cached_word_count = self
            .cached_word_count
            .saturating_add(new_delta)
            .saturating_sub(old_delta);
        self.cached_line_ending = detect_first_line_ending(&self.rope);
    }

    /// Inserts text at `char_idx` in O(log n) time and pushes an undo entry.
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let idx = char_idx.min(self.len_chars());
        let cursor_before = self.cursor_char;
        self.apply_rope_edit(idx, idx, text);
        let inserted_chars = text.chars().count();
        let cursor_after = idx + inserted_chars;
        self.cursor_char = cursor_after;
        self.selection_anchor = None;
        self.push_undo(EditOp::Text {
            char_idx: idx,
            deleted: String::new(),
            inserted: text.to_string(),
            cursor_before,
            cursor_after,
        });
    }

    /// Removes `range` in char coordinates in O(log n) time and pushes an undo entry.
    pub fn remove(&mut self, range: Range<usize>) {
        let len = self.len_chars();
        let start = range.start.min(len);
        let end = range.end.min(len);
        if start >= end {
            return;
        }
        let cursor_before = self.cursor_char;
        let deleted = self.rope.slice(start..end).to_string();
        self.apply_rope_edit(start, end, "");
        self.cursor_char = start;
        self.selection_anchor = None;
        self.push_undo(EditOp::Text {
            char_idx: start,
            deleted,
            inserted: String::new(),
            cursor_before,
            cursor_after: start,
        });
    }

    /// Replaces `range` (in char coordinates) with `text` in O(log n) time as a single undoable edit.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let len = self.len_chars();
        let start = range.start.min(len);
        let end = range.end.min(len);
        if start == end && text.is_empty() {
            return;
        }
        let cursor_before = self.cursor_char;
        let deleted = if start < end {
            self.rope.slice(start..end).to_string()
        } else {
            String::new()
        };
        self.apply_rope_edit(start, end, text);
        let cursor_after = start + text.chars().count();
        self.cursor_char = cursor_after;
        self.selection_anchor = None;
        self.push_undo(EditOp::Text {
            char_idx: start,
            deleted,
            inserted: text.to_string(),
            cursor_before,
            cursor_after,
        });
    }

    /// Atomically replaces multiple disjoint character ranges in reverse order without
    /// allocating a single spanning string, pushing one unified undo entry.
    pub fn replace_ranges_atomic(&mut self, mut edits: Vec<(Range<usize>, String)>) {
        if edits.is_empty() {
            return;
        }
        let len = self.len_chars();
        for (r, _) in &mut edits {
            r.start = r.start.min(len);
            r.end = r.end.min(len).max(r.start);
        }
        edits.sort_by_key(|(r, _)| r.start);

        let mut disjoint: Vec<(Range<usize>, String)> = Vec::with_capacity(edits.len());
        let mut prev_end = 0usize;
        for (r, text) in edits {
            if r.start < prev_end {
                continue;
            }
            prev_end = r.end;
            disjoint.push((r, text));
        }
        if disjoint.is_empty() {
            return;
        }
        if disjoint.len() == 1 {
            let (r, text) = disjoint.into_iter().next().unwrap();
            let target = r.start;
            self.replace_range(r, &text);
            self.cursor_char = target.min(self.len_chars());
            return;
        }

        let cursor_before = self.cursor_char;
        let first_start = disjoint[0].0.start;
        let mut applied = Vec::with_capacity(disjoint.len());
        for (r, text) in disjoint.into_iter().rev() {
            let deleted = if r.start < r.end {
                self.rope.slice(r.start..r.end).to_string()
            } else {
                String::new()
            };
            self.apply_rope_edit(r.start, r.end, &text);
            applied.push((r.start, deleted, text));
        }
        self.cursor_char = first_start.min(self.len_chars());
        self.selection_anchor = None;
        self.push_undo(EditOp::Batch {
            edits: applied,
            cursor_before,
            cursor_after: self.cursor_char,
        });
    }

    /// Records an undoable file-rename step while preserving the buffer's current clean/dirty state.
    pub fn record_file_rename(&mut self, old_path: PathBuf, new_path: PathBuf) {
        let was_clean_at_top = self.clean_undo_depth == Some(self.undo_stack.len());
        self.push_undo(EditOp::Rename { old_path, new_path });
        if was_clean_at_top {
            self.clean_undo_depth = Some(self.undo_stack.len());
            self.dirty = false;
        }
    }

    /// Inserts text at the cursor, replacing any active selection.
    pub fn insert_at_cursor(&mut self, text: &str) {
        if let Some(sel) = self.selection_range() {
            self.replace_range(sel, text);
        } else {
            let c = self.cursor_char;
            self.insert(c, text);
        }
    }

    /// Undoes the latest edit or file rename and returns what changed.
    pub fn undo_with_outcome(&mut self) -> UndoRedoOutcome {
        self.last_undo_time = None;
        let pre_len = self.undo_stack.len();
        let was_clean_at_top = self.clean_undo_depth == Some(pre_len);
        let Some(op) = self.undo_stack.pop_back() else {
            return UndoRedoOutcome::None;
        };
        let op_bytes = op.byte_size();
        match op {
            EditOp::Text {
                char_idx,
                deleted,
                inserted,
                cursor_before,
                cursor_after,
            } => {
                self.undo_bytes = self.undo_bytes.saturating_sub(op_bytes);
                let end = (char_idx + inserted.chars().count()).min(self.len_chars());
                self.apply_rope_edit(char_idx, end, &deleted);
                self.cursor_char = cursor_before.min(self.len_chars());
                self.selection_anchor = None;
                self.cached_selection = None;
                self.redo_stack.push(EditOp::Text {
                    char_idx,
                    deleted,
                    inserted,
                    cursor_before,
                    cursor_after,
                });
                self.update_dirty_flag();
                self.version = self.version.wrapping_add(1);
                UndoRedoOutcome::TextEdited
            }
            EditOp::Batch {
                edits,
                cursor_before,
                cursor_after,
            } => {
                self.undo_bytes = self.undo_bytes.saturating_sub(op_bytes);
                for (char_idx, deleted, inserted) in edits.iter().rev() {
                    let end = (*char_idx + inserted.chars().count()).min(self.len_chars());
                    self.apply_rope_edit(*char_idx, end, deleted);
                }
                self.cursor_char = cursor_before.min(self.len_chars());
                self.selection_anchor = None;
                self.cached_selection = None;
                self.redo_stack.push(EditOp::Batch {
                    edits,
                    cursor_before,
                    cursor_after,
                });
                self.update_dirty_flag();
                self.version = self.version.wrapping_add(1);
                UndoRedoOutcome::TextEdited
            }
            EditOp::Rename { old_path, new_path } => {
                let is_case_only =
                    paths_equivalent_on_fs(&new_path, &old_path) && new_path != old_path;
                if !is_case_only && old_path.exists() {
                    let msg = format!(
                        "Cannot undo rename: \"{}\" already exists",
                        path_display_filename(&old_path)
                    );
                    self.undo_stack.push_back(EditOp::Rename { old_path, new_path });
                    return UndoRedoOutcome::RenameFailed(msg);
                }
                if let Err(e) = std::fs::rename(&new_path, &old_path) {
                    let msg = format!("Undo rename failed: {e}");
                    self.undo_stack.push_back(EditOp::Rename { old_path, new_path });
                    return UndoRedoOutcome::RenameFailed(msg);
                }
                self.undo_bytes = self.undo_bytes.saturating_sub(op_bytes);
                self.file_path = Some(old_path.clone());
                if was_clean_at_top {
                    self.clean_undo_depth = Some(self.undo_stack.len());
                }
                self.redo_stack.push(EditOp::Rename {
                    old_path: old_path.clone(),
                    new_path: new_path.clone(),
                });
                self.update_dirty_flag();
                self.version = self.version.wrapping_add(1);
                UndoRedoOutcome::Renamed {
                    from: new_path,
                    to: old_path,
                }
            }
        }
    }

    /// Redoes the latest undone edit or file rename and returns what changed.
    pub fn redo_with_outcome(&mut self) -> UndoRedoOutcome {
        self.last_undo_time = None;
        let pre_len = self.undo_stack.len();
        let was_clean_at_top = self.clean_undo_depth == Some(pre_len);
        let Some(op) = self.redo_stack.pop() else {
            return UndoRedoOutcome::None;
        };
        let op_bytes = op.byte_size();
        match op {
            EditOp::Text {
                char_idx,
                deleted,
                inserted,
                cursor_before,
                cursor_after,
            } => {
                let end = (char_idx + deleted.chars().count()).min(self.len_chars());
                self.apply_rope_edit(char_idx, end, &inserted);
                self.cursor_char = cursor_after.min(self.len_chars());
                self.selection_anchor = None;
                self.cached_selection = None;
                self.undo_bytes = self.undo_bytes.saturating_add(op_bytes);
                self.undo_stack.push_back(EditOp::Text {
                    char_idx,
                    deleted,
                    inserted,
                    cursor_before,
                    cursor_after,
                });
                self.update_dirty_flag();
                self.version = self.version.wrapping_add(1);
                UndoRedoOutcome::TextEdited
            }
            EditOp::Batch {
                edits,
                cursor_before,
                cursor_after,
            } => {
                for (char_idx, deleted, inserted) in &edits {
                    let end = (*char_idx + deleted.chars().count()).min(self.len_chars());
                    self.apply_rope_edit(*char_idx, end, inserted);
                }
                self.cursor_char = cursor_after.min(self.len_chars());
                self.selection_anchor = None;
                self.cached_selection = None;
                self.undo_bytes = self.undo_bytes.saturating_add(op_bytes);
                self.undo_stack.push_back(EditOp::Batch {
                    edits,
                    cursor_before,
                    cursor_after,
                });
                self.update_dirty_flag();
                self.version = self.version.wrapping_add(1);
                UndoRedoOutcome::TextEdited
            }
            EditOp::Rename { old_path, new_path } => {
                let is_case_only =
                    paths_equivalent_on_fs(&old_path, &new_path) && old_path != new_path;
                if !is_case_only && new_path.exists() {
                    let msg = format!(
                        "Cannot redo rename: \"{}\" already exists",
                        path_display_filename(&new_path)
                    );
                    self.redo_stack.push(EditOp::Rename { old_path, new_path });
                    return UndoRedoOutcome::RenameFailed(msg);
                }
                if let Err(e) = std::fs::rename(&old_path, &new_path) {
                    let msg = format!("Redo rename failed: {e}");
                    self.redo_stack.push(EditOp::Rename { old_path, new_path });
                    return UndoRedoOutcome::RenameFailed(msg);
                }
                self.undo_bytes = self.undo_bytes.saturating_add(op_bytes);
                self.file_path = Some(new_path.clone());
                self.undo_stack.push_back(EditOp::Rename {
                    old_path: old_path.clone(),
                    new_path: new_path.clone(),
                });
                if was_clean_at_top {
                    self.clean_undo_depth = Some(self.undo_stack.len());
                }
                self.update_dirty_flag();
                self.version = self.version.wrapping_add(1);
                UndoRedoOutcome::Renamed {
                    from: old_path,
                    to: new_path,
                }
            }
        }
    }

    pub fn undo(&mut self) -> bool {
        matches!(
            self.undo_with_outcome(),
            UndoRedoOutcome::TextEdited | UndoRedoOutcome::Renamed { .. }
        )
    }

    pub fn redo(&mut self) -> bool {
        matches!(
            self.redo_with_outcome(),
            UndoRedoOutcome::TextEdited | UndoRedoOutcome::Renamed { .. }
        )
    }

    /// Number of undoable edit operations currently on the undo stack.
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// Number of redoable edit operations currently on the redo stack.
    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// Returns up to `max_items` undo descriptions ordered most-recent first (step 1, step 2, ...).
    pub fn undo_history_labels(&self, max_items: usize) -> Vec<String> {
        self.undo_stack
            .iter()
            .rev()
            .take(max_items)
            .map(EditOp::describe)
            .collect()
    }

    /// Returns up to `max_items` redo descriptions ordered next-redo first (step 1, step 2, ...).
    pub fn redo_history_labels(&self, max_items: usize) -> Vec<String> {
        self.redo_stack
            .iter()
            .rev()
            .take(max_items)
            .map(EditOp::describe)
            .collect()
    }

    /// Undoes up to `steps` edits, returning the number of steps actually undone.
    pub fn undo_steps(&mut self, steps: usize) -> usize {
        let mut count = 0usize;
        for _ in 0..steps {
            if !self.undo() {
                break;
            }
            count += 1;
        }
        count
    }

    /// Redoes up to `steps` edits, returning the number of steps actually redone.
    pub fn redo_steps(&mut self, steps: usize) -> usize {
        let mut count = 0usize;
        for _ in 0..steps {
            if !self.redo() {
                break;
            }
            count += 1;
        }
        count
    }

    fn finish_edit(&mut self, undo_time: Option<Instant>) {
        self.last_undo_time = undo_time;
        self.redo_stack.clear();
        self.cached_selection = None;
        self.update_dirty_flag();
        self.version = self.version.wrapping_add(1);
    }

    fn try_coalesce_last_edit(&mut self, op: &EditOp, now: Instant) -> bool {
        let can_coalesce_time = self.clean_undo_depth != Some(self.undo_stack.len())
            && self
                .last_undo_time
                .is_some_and(|t| now.saturating_duration_since(t) <= UNDO_COALESCE_WINDOW);
        if !can_coalesce_time {
            return false;
        }
        let (
            Some(EditOp::Text {
                char_idx: prev_idx,
                deleted: prev_del,
                inserted: prev_ins,
                cursor_after: prev_after,
                ..
            }),
            EditOp::Text {
                char_idx: new_idx,
                deleted: new_del,
                inserted: new_ins,
                cursor_after: new_after,
                ..
            },
        ) = (self.undo_stack.back_mut(), op)
        else {
            return false;
        };

        if prev_del.is_empty() && new_del.is_empty() && is_single_non_newline_char(new_ins) {
            let prev_ins_chars = prev_ins.chars().count();
            let break_word_boundary = prev_ins.chars().last().is_some_and(char::is_whitespace)
                && !new_ins.chars().next().is_some_and(char::is_whitespace);
            if prev_ins_chars < MAX_COALESCED_CHARS
                && *prev_idx + prev_ins_chars == *new_idx
                && !break_word_boundary
            {
                prev_ins.push_str(new_ins);
                *prev_after = *new_after;
                self.undo_bytes = self.undo_bytes.saturating_add(new_ins.len());
                return true;
            }
        }

        if prev_ins.is_empty() && new_ins.is_empty() && is_single_non_newline_char(new_del) {
            let prev_del_chars = prev_del.chars().count();
            if prev_del_chars >= MAX_COALESCED_CHARS {
                return false;
            }
            let bs_word_boundary = prev_del.chars().next().is_some_and(char::is_whitespace)
                && !new_del.chars().next().is_some_and(char::is_whitespace);
            if *new_idx + 1 == *prev_idx && !bs_word_boundary {
                let mut combined = String::with_capacity(new_del.len() + prev_del.len());
                combined.push_str(new_del);
                combined.push_str(prev_del);
                *prev_del = combined;
                *prev_idx = *new_idx;
                *prev_after = *new_after;
                self.undo_bytes = self.undo_bytes.saturating_add(new_del.len());
                return true;
            }
            if *new_idx == *prev_idx {
                prev_del.push_str(new_del);
                self.undo_bytes = self.undo_bytes.saturating_add(new_del.len());
                return true;
            }
        }

        false
    }

    fn push_undo(&mut self, op: EditOp) {
        let now = Instant::now();
        if self
            .clean_undo_depth
            .is_some_and(|d| d > self.undo_stack.len())
        {
            self.clean_undo_depth = None;
        }

        if self.try_coalesce_last_edit(&op, now) {
            self.finish_edit(Some(now));
            return;
        }

        self.undo_bytes = self.undo_bytes.saturating_add(op.byte_size());
        self.undo_stack.push_back(op);
        while self.undo_stack.len() > MAX_UNDO_OPS
            || (self.undo_stack.len() > MIN_UNDO_KEEP && self.undo_bytes > MAX_UNDO_BYTES)
        {
            if let Some(evicted) = self.undo_stack.pop_front() {
                self.undo_bytes = self.undo_bytes.saturating_sub(evicted.byte_size());
                self.clean_undo_depth = self.clean_undo_depth.and_then(|d| d.checked_sub(1));
            } else {
                break;
            }
        }
        let next_time = matches!(self.undo_stack.back(), Some(EditOp::Text { .. })).then_some(now);
        self.finish_edit(next_time);
    }

    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx.min(self.len_chars()))
    }

    pub fn line_to_char(&self, line_idx: usize) -> usize {
        let max_line = self.len_lines().saturating_sub(1);
        self.rope.line_to_char(line_idx.min(max_line))
    }

    /// Returns `(line_0based, col_chars_in_line)` for a char offset.
    pub fn char_to_line_col(&self, char_idx: usize) -> (usize, usize) {
        let c = char_idx.min(self.len_chars());
        let line = self.rope.char_to_line(c);
        let line_start = self.rope.line_to_char(line);
        (line, c.saturating_sub(line_start))
    }

    /// Converts `(line_0based, col_chars)` into a clamped char offset within that line (excluding newline).
    pub fn line_col_to_char(&self, line_idx: usize, col_chars: usize) -> usize {
        let max_line = self.len_lines().saturating_sub(1);
        let line = line_idx.min(max_line);
        let start = self.rope.line_to_char(line);
        let line_len = self.line_len_without_newline(line);
        start + col_chars.min(line_len)
    }

    /// Converts a 1-based `(line, utf8_byte_col)` from `comrak::Sourcepos` into a char offset.
    pub fn sourcepos_to_char(&self, line_1based: usize, col_1based_bytes: usize) -> usize {
        if line_1based == 0 {
            return 0;
        }
        let line_idx = (line_1based - 1).min(self.len_lines().saturating_sub(1));
        let line_start_char = self.rope.line_to_char(line_idx);
        let line_start_byte = self.rope.char_to_byte(line_start_char);
        let line_slice = self.rope.line(line_idx);
        let byte_offset_in_line = col_1based_bytes.saturating_sub(1).min(line_slice.len_bytes());
        self.rope.byte_to_char(line_start_byte + byte_offset_in_line)
    }

    /// Returns the line text without trailing `\r` or `\n`.
    pub fn line_text(&self, line_idx: usize) -> String {
        if line_idx >= self.len_lines() {
            return String::new();
        }
        let mut s = self.rope.line(line_idx).to_string();
        while s.ends_with('\n') || s.ends_with('\r') {
            s.pop();
        }
        s
    }

    pub fn line_len_without_newline(&self, line_idx: usize) -> usize {
        if line_idx >= self.len_lines() {
            return 0;
        }
        let slice = self.rope.line(line_idx);
        let mut len = slice.len_chars();
        if len > 0 && slice.char(len - 1) == '\n' {
            len -= 1;
            if len > 0 && slice.char(len - 1) == '\r' {
                len -= 1;
            }
        }
        len
    }

    /// Returns the char range covering lines `[start_line, end_line_inclusive]` excluding trailing newline on the last line.
    pub fn line_range_chars(&self, start_line: usize, end_line: usize) -> Range<usize> {
        let max_line = self.len_lines().saturating_sub(1);
        let s_line = start_line.min(max_line);
        let e_line = end_line.max(s_line).min(max_line);
        let start = self.rope.line_to_char(s_line);
        let end = self.rope.line_to_char(e_line) + self.line_len_without_newline(e_line);
        start..end
    }

    pub fn as_string(&self) -> String {
        self.rope.to_string()
    }

    /// Returns the document's cached line ending style (`"\r\n"` or `"\n"`) in O(1) time.
    pub fn line_ending(&self) -> &'static str {
        self.cached_line_ending
    }

    /// Returns the cached whitespace-separated word count in O(1) time.
    pub fn word_count(&self) -> usize {
        self.cached_word_count
    }
}

fn has_binary_magic_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xFF\xD8\xFF")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"%PDF-")
        || bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"\x7FELF")
        || bytes.starts_with(b"\x1F\x8B")
        || (bytes.starts_with(b"RIFF") && bytes.len() >= 12 && &bytes[8..12] == b"WEBP")
        || (bytes.starts_with(b"MZ") && bytes.contains(&0))
}

/// Returns `Some(true)` for UTF-16 LE BOM (`FF FE`), `Some(false)` for UTF-16 BE BOM (`FE FF`).
fn detect_utf16_bom(bytes: &[u8]) -> Option<bool> {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        // Avoid misidentifying UTF-32 LE (`FF FE 00 00`)
        if bytes.len() >= 4 && bytes[2] == 0 && bytes[3] == 0 {
            return None;
        }
        Some(true)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some(false)
    } else {
        None
    }
}

fn decode_utf16_bytes(bytes: &[u8], little_endian: bool) -> String {
    let start = if bytes.len() >= 2 && (bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF])) {
        2
    } else {
        0
    };
    let (chunks, rem) = bytes[start..].as_chunks::<2>();
    let units = chunks.iter().map(|c| {
        if little_endian {
            u16::from_le_bytes(*c)
        } else {
            u16::from_be_bytes(*c)
        }
    });
    let mut out = char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect::<String>();
    if !rem.is_empty() {
        out.push(char::REPLACEMENT_CHARACTER);
    }
    out
}

fn is_known_text_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "md" | "markdown"
            | "mdown"
            | "mkd"
            | "txt"
            | "log"
            | "json"
            | "jsonc"
            | "toml"
            | "yaml"
            | "yml"
            | "xml"
            | "svg"
            | "html"
            | "htm"
            | "css"
            | "scss"
            | "less"
            | "js"
            | "mjs"
            | "cjs"
            | "jsx"
            | "ts"
            | "mts"
            | "tsx"
            | "rs"
            | "py"
            | "pyw"
            | "c"
            | "cc"
            | "cpp"
            | "cxx"
            | "h"
            | "hh"
            | "hpp"
            | "cs"
            | "java"
            | "kt"
            | "kts"
            | "go"
            | "rb"
            | "php"
            | "swift"
            | "dart"
            | "lua"
            | "r"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "bat"
            | "cmd"
            | "ps1"
            | "psm1"
            | "ini"
            | "cfg"
            | "conf"
            | "env"
            | "properties"
            | "csv"
            | "tsv"
            | "sql"
            | "tex"
            | "latex"
            | "bib"
            | "org"
            | "rst"
            | "adoc"
            | "asciidoc"
            | "diff"
            | "patch"
            | "gitignore"
            | "gitattributes"
            | "lock"
            | "srt"
            | "vtt"
            | "nfo"
    )
}

fn is_valid_utf8_prefix(bytes: &[u8]) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(_) => true,
        Err(e) => bytes.len() - e.valid_up_to() <= 3 && e.error_len().is_none(),
    }
}

fn is_printable_text_sample(bytes: &[u8]) -> bool {
    let ctrl_count = bytes
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t' | b'\x0C'))
        .count();
    ctrl_count * 50 <= bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rope_insert_remove_undo_redo() {
        let mut buf = DocumentBuffer::from_str("Hello world\nSecond line\n");
        assert_eq!(buf.len_lines(), 3);
        assert_eq!(buf.line_text(0), "Hello world");
        assert_eq!(buf.line_text(1), "Second line");

        buf.insert(6, "rust ");
        assert_eq!(buf.line_text(0), "Hello rust world");

        buf.remove(6..11);
        assert_eq!(buf.line_text(0), "Hello world");

        assert!(buf.undo());
        assert_eq!(buf.line_text(0), "Hello rust world");

        assert!(buf.undo());
        assert_eq!(buf.line_text(0), "Hello world");

        assert!(buf.redo());
        assert_eq!(buf.line_text(0), "Hello rust world");
    }

    #[test]
    fn line_col_and_sourcepos_conversions() {
        let buf = DocumentBuffer::from_str("abc\nαβγ\nxyz");
        assert_eq!(buf.line_col_to_char(1, 2), 6);
        assert_eq!(buf.char_to_line_col(6), (1, 2));
        // On line 2 ("αβγ"), byte col 3 (1-based) is after 'α' (2 bytes) => char offset 4 + 1 = 5.
        assert_eq!(buf.sourcepos_to_char(2, 3), 5);
    }

    #[test]
    fn incremental_word_count_matches_full_scan_across_edits() {
        let mut buf = DocumentBuffer::from_str("hello world");
        assert_eq!(buf.word_count(), count_words_full(buf.rope()));

        // Split word -> +1
        buf.insert(5, " ");
        assert_eq!(buf.word_count(), count_words_full(buf.rope()));

        // Insert inside word -> same
        buf.insert(2, "XYZ");
        assert_eq!(buf.word_count(), count_words_full(buf.rope()));

        // Merge two words -> -1
        buf.remove(8..9);
        assert_eq!(buf.word_count(), count_words_full(buf.rope()));

        // Multi-word paste replacement
        buf.replace_range(0..3, "one two three ");
        assert_eq!(buf.word_count(), count_words_full(buf.rope()));

        // Undo/redo chain
        while buf.undo() {
            assert_eq!(buf.word_count(), count_words_full(buf.rope()));
        }
        while buf.redo() {
            assert_eq!(buf.word_count(), count_words_full(buf.rope()));
        }

        // Clear all
        let len = buf.len_chars();
        buf.remove(0..len);
        assert_eq!(buf.word_count(), 0);
        assert_eq!(buf.word_count(), count_words_full(buf.rope()));
    }

    #[test]
    fn undo_redo_history_labels_and_multi_step_jump() {
        let mut buf = DocumentBuffer::from_str("Start");
        buf.insert(5, " alpha");
        buf.insert(11, " beta");
        buf.replace_range(0..5, "Begin");
        buf.remove(6..12);

        assert_eq!(buf.as_string(), "Begin beta");
        assert_eq!(buf.undo_depth(), 4);
        assert_eq!(buf.redo_depth(), 0);

        let undo_labels = buf.undo_history_labels(10);
        assert_eq!(undo_labels.len(), 4);
        assert!(undo_labels[0].contains("Deleted"));
        assert!(undo_labels[1].contains("Replaced"));
        assert!(undo_labels[2].contains("Typed"));
        assert!(undo_labels[3].contains("Typed"));

        // Jump back 3 steps in one call -> should be at state after step 1 ("Start alpha")
        assert_eq!(buf.undo_steps(3), 3);
        assert_eq!(buf.as_string(), "Start alpha");
        assert_eq!(buf.undo_depth(), 1);
        assert_eq!(buf.redo_depth(), 3);

        let redo_labels = buf.redo_history_labels(10);
        assert_eq!(redo_labels.len(), 3);
        assert!(redo_labels[0].contains("beta"));
        assert!(redo_labels[1].contains("Replaced"));
        assert!(redo_labels[2].contains("Deleted"));

        // Jump forward 2 steps -> should reach "Begin alpha beta"
        assert_eq!(buf.redo_steps(2), 2);
        assert_eq!(buf.as_string(), "Begin alpha beta");
        assert_eq!(buf.undo_depth(), 3);
        assert_eq!(buf.redo_depth(), 1);
        assert!(buf.dirty);

        // Undo remaining 3 steps -> restores initial clean state
        assert_eq!(buf.undo_steps(10), 3);
        assert_eq!(buf.as_string(), "Start");
        assert!(!buf.dirty);
    }

    #[test]
    fn opens_any_text_file_encoding_and_rejects_binary() {
        let dir = std::env::temp_dir().join(format!(
            "markdown_studio_text_detect_{}_{}",
            std::process::id(),
            9911
        ));
        let _ = std::fs::create_dir_all(&dir);

        // 1. Extensionless UTF-8 file
        let extless = dir.join("README");
        std::fs::write(&extless, "Hello extensionless UTF-8 text\nLine 2\n").unwrap();
        assert!(DocumentBuffer::is_likely_text_file(&extless));
        let b1 = DocumentBuffer::open_file(&extless).unwrap();
        assert!(b1.as_string().contains("Hello extensionless"));

        // 2. UTF-16 LE with BOM
        let utf16_path = dir.join("notes.log");
        let mut utf16_bytes = vec![0xFFu8, 0xFE];
        for unit in "Hello UTF16\n".encode_utf16() {
            utf16_bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&utf16_path, &utf16_bytes).unwrap();
        assert!(DocumentBuffer::is_likely_text_file(&utf16_path));
        let b2 = DocumentBuffer::open_file(&utf16_path).unwrap();
        assert_eq!(b2.as_string(), "Hello UTF16\n");

        // 3. Binary file (PNG magic + NUL bytes) must be rejected
        let bin_path = dir.join("fake.bin");
        std::fs::write(&bin_path, b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR").unwrap();
        assert!(!DocumentBuffer::is_likely_text_file(&bin_path));
        assert!(DocumentBuffer::open_file(&bin_path).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_undo_redo_and_history_labels() {
        let dir = std::env::temp_dir().join(format!(
            "markdown_studio_rename_undo_{}_{}",
            std::process::id(),
            9922
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let p1 = dir.join("alpha.md");
        let p2 = dir.join("beta.md");
        std::fs::write(&p1, "# Alpha\n").unwrap();

        let mut buf = DocumentBuffer::open_file(&p1).unwrap();
        assert!(!buf.dirty);

        std::fs::rename(&p1, &p2).unwrap();
        buf.file_path = Some(p2.clone());
        buf.record_file_rename(p1.clone(), p2.clone());
        assert!(!buf.dirty, "renaming a clean buffer must keep dirty == false");
        assert_eq!(buf.undo_history_labels(5), vec!["Renamed alpha.md → beta.md"]);

        assert_eq!(
            buf.undo_with_outcome(),
            UndoRedoOutcome::Renamed {
                from: p2.clone(),
                to: p1.clone()
            }
        );
        assert!(p1.exists() && !p2.exists());
        assert_eq!(buf.file_path.as_deref(), Some(p1.as_path()));
        assert!(!buf.dirty);
        assert_eq!(buf.redo_history_labels(5), vec!["Renamed alpha.md → beta.md"]);

        assert_eq!(
            buf.redo_with_outcome(),
            UndoRedoOutcome::Renamed {
                from: p1.clone(),
                to: p2.clone()
            }
        );
        assert!(!p1.exists() && p2.exists());
        assert_eq!(buf.file_path.as_deref(), Some(p2.as_path()));
        assert!(!buf.dirty);

        // Collision on undo rolls back stack safely without corrupting state
        std::fs::write(&p1, "collision\n").unwrap();
        assert!(matches!(
            buf.undo_with_outcome(),
            UndoRedoOutcome::RenameFailed(_)
        ));
        assert!(p2.exists());
        assert_eq!(buf.file_path.as_deref(), Some(p2.as_path()));
        assert_eq!(buf.undo_depth(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn smart_undo_coalesces_typing_backspace_and_delete_bursts() {
        let mut buf = DocumentBuffer::new();
        // Type "hello world" char by char
        for ch in "hello world".chars() {
            let mut s = [0u8; 4];
            let slice = ch.encode_utf8(&mut s);
            buf.insert_at_cursor(slice);
        }
        assert_eq!(buf.as_string(), "hello world");
        // "hello " and "world" split at whitespace-to-word boundary -> 2 undo steps
        assert_eq!(buf.undo_depth(), 2);

        // Save point splits coalescing even for contiguous chars
        buf.mark_clean();
        buf.insert_at_cursor("!");
        assert_eq!(buf.undo_depth(), 3);
        assert!(buf.dirty);
        assert!(buf.undo());
        assert_eq!(buf.as_string(), "hello world");
        assert!(!buf.dirty);

        // Newline starts a fresh undo group
        buf.insert_at_cursor("\n");
        buf.insert_at_cursor("a");
        buf.insert_at_cursor("b");
        assert_eq!(buf.undo_depth(), 4);
        assert!(buf.undo());
        assert_eq!(buf.as_string(), "hello world\n");

        // Multi-char paste does not coalesce with subsequent single-char typing? Wait:
        // single-char typing DOES break on newline/save/break_undo_coalescing,
        // and multi-char paste itself is never coalesced into an existing burst.
        buf.break_undo_coalescing();
        buf.insert_at_cursor("xy");
        let depth_after_paste = buf.undo_depth();
        buf.insert_at_cursor("zz");
        assert_eq!(buf.undo_depth(), depth_after_paste + 1);

        // Backspace burst coalesces per word
        let mut bs_buf = DocumentBuffer::from_str("foo bar");
        bs_buf.set_cursor(7, false);
        for _ in 0..7 {
            bs_buf.backspace_grapheme();
        }
        assert_eq!(bs_buf.as_string(), "");
        // " bar" and "foo" split at whitespace-to-word boundary -> 2 undo steps
        assert_eq!(bs_buf.undo_depth(), 2);
        assert!(bs_buf.undo());
        assert_eq!(bs_buf.as_string(), "foo");
        assert!(bs_buf.undo());
        assert_eq!(bs_buf.as_string(), "foo bar");

        // Forward-delete burst coalesces
        let mut del_buf = DocumentBuffer::from_str("abcdef");
        del_buf.set_cursor(1, false);
        for _ in 0..3 {
            del_buf.delete_grapheme();
        }
        assert_eq!(del_buf.as_string(), "aef");
        assert_eq!(del_buf.undo_depth(), 1);
        assert!(del_buf.undo());
        assert_eq!(del_buf.as_string(), "abcdef");
        assert_eq!(del_buf.cursor_char, 1);
    }

    #[test]
    fn undo_stack_respects_byte_budget_keeping_minimum_ops() {
        let mut buf = DocumentBuffer::new();
        let big = "x".repeat(2 * 1024 * 1024); // 2 MiB per edit
        for _ in 0..24 {
            buf.insert(0, &big);
        }
        // Budget is 32 MiB (~16 ops of 2 MiB), and MIN_UNDO_KEEP is 16
        assert!(buf.undo_depth() >= MIN_UNDO_KEEP);
        assert!(buf.undo_depth() < 24);
        assert!(buf.undo_bytes <= MAX_UNDO_BYTES);

        while buf.undo() {}
        assert_eq!(buf.undo_bytes, 0);
    }

    #[test]
    fn open_recovery_file_streams_and_preserves_dirty_and_path() {
        let dir = std::env::temp_dir().join(format!(
            "markdown_studio_recovery_open_{}_{}",
            std::process::id(),
            9933
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let rec = dir.join("tab_0.md");
        std::fs::write(&rec, "\u{FEFF}# Recovered\nBody").unwrap();
        let orig = dir.join("notes.md");

        let buf = DocumentBuffer::open_recovery_file(&rec, Some(orig.clone()), true).unwrap();
        assert_eq!(buf.as_string(), "# Recovered\nBody");
        assert_eq!(buf.file_path.as_deref(), Some(orig.as_path()));
        assert!(buf.dirty);
        assert!(buf.clean_undo_depth.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_ranges_atomic_applies_undoes_and_redoes_in_one_step() {
        let orig = "Alpha [ONE] middle [TWO] tail";
        let mut buf = DocumentBuffer::from_str(orig);
        let one_s = orig.find("[ONE]").unwrap();
        let two_s = orig.find("[TWO]").unwrap();
        buf.replace_ranges_atomic(vec![
            (two_s..two_s + 5, "2222222".to_string()),
            (one_s..one_s + 5, "1".to_string()),
        ]);
        assert_eq!(buf.as_string(), "Alpha 1 middle 2222222 tail");
        assert_eq!(buf.undo_depth(), 1);
        assert!(buf.dirty);

        assert!(buf.undo());
        assert_eq!(buf.as_string(), orig);
        assert!(!buf.dirty);

        assert!(buf.redo());
        assert_eq!(buf.as_string(), "Alpha 1 middle 2222222 tail");
        assert!(buf.dirty);
    }

    #[test]
    fn open_file_streaming_word_count_matches_from_str_for_utf8_bom_crlf_and_chunk_seams() {
        let dir = std::env::temp_dir().join(format!(
            "markdown_studio_stream_wc_{}_{}",
            std::process::id(),
            4412
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 1) BOM + space before first word + CRLF + Arabic + Emoji
        let bom_path = dir.join("bom_crlf.md");
        let bom_body = "\u{FEFF} Hello  مرحبا\r\nWorld 🚀 test\x0Bvertical\r\n";
        std::fs::write(&bom_path, bom_body.as_bytes()).unwrap();
        let opened_bom = DocumentBuffer::open_file(&bom_path).unwrap();
        let mem_bom = DocumentBuffer::from_str(bom_body.trim_start_matches('\u{FEFF}'));
        assert_eq!(opened_bom.word_count(), mem_bom.word_count());
        assert_eq!(opened_bom.word_count(), 6);
        assert_eq!(opened_bom.line_ending(), "\r\n");
        assert_eq!(opened_bom.saved_hash, mem_bom.saved_hash);

        // 2) >70 KB file with multi-byte UTF-8 emojis & Arabic crossing 64 KB BufReader seams
        let seam_path = dir.join("large_multibyte.md");
        let mut large = String::with_capacity(75_000);
        // Deliberately push non-aligned UTF-8 byte lengths so multi-byte chars straddle 4K/8K/64K
        large.push_str("X ");
        while large.len() < 72_000 {
            large.push_str("مرحبا😀 ");
        }
        std::fs::write(&seam_path, large.as_bytes()).unwrap();
        let opened_large = DocumentBuffer::open_file(&seam_path).unwrap();
        let mem_large = DocumentBuffer::from_str(&large);
        assert_eq!(opened_large.word_count(), mem_large.word_count());
        assert_eq!(opened_large.line_ending(), "\n");
        assert_eq!(opened_large.saved_hash, mem_large.saved_hash);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
