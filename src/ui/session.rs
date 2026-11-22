use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use ropey::Rope;

use crate::ui::settings::{
    escape_json_str, extract_bool_field, extract_f32_field, extract_str_field,
    extract_usize_field, find_json_value_slice,
};

static SESSION_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Persisted metadata for a single tab across restarts or crash-safe auto-recovery.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTabEntry {
    pub file_path: Option<PathBuf>,
    pub recovery_file: Option<PathBuf>,
    pub dirty: bool,
    pub cursor_char: usize,
    pub scroll_y: f32,
    pub untitled_num: usize,
}

/// Complete open-tabs session state stored next to `settings.json`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionState {
    pub active_tab: usize,
    pub tabs: Vec<SessionTabEntry>,
}

/// Returns the session manifest path adjacent to `settings_path` (`session.json`).
pub fn session_path(settings_path: &Path) -> PathBuf {
    match settings_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join("session.json"),
        _ => PathBuf::from("session.json"),
    }
}

/// Returns the directory used for crash-safe unsaved buffer snapshots (`recovery/`).
pub fn recovery_dir(settings_path: &Path) -> PathBuf {
    match settings_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join("recovery"),
        _ => PathBuf::from("recovery"),
    }
}

fn atomic_replace_from_temp(tmp_path: &Path, target_path: &Path) -> io::Result<()> {
    match fs::rename(tmp_path, target_path) {
        Ok(()) => Ok(()),
        Err(first_err) if target_path.exists() => {
            let parent = target_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let name = target_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("session");
            let seq = SESSION_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let bak = parent.join(format!(".{name}.{}.{seq}.bak", std::process::id()));
            if fs::rename(target_path, &bak).is_ok() {
                match fs::rename(tmp_path, target_path) {
                    Ok(()) => {
                        let _ = fs::remove_file(&bak);
                        Ok(())
                    }
                    Err(e) => {
                        let _ = fs::rename(&bak, target_path);
                        Err(e)
                    }
                }
            } else {
                Err(first_err)
            }
        }
        Err(err) => Err(err),
    }
}

fn create_atomic_temp_path(target_path: &Path) -> io::Result<(PathBuf, fs::File)> {
    let parent = target_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("data");
    let seq = SESSION_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{name}.{}.{seq}.tmp", std::process::id()));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    Ok((tmp, file))
}

/// Atomically writes `bytes` to `target_path` via temp-file + flush + rename.
pub fn atomic_write_bytes(target_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (tmp_path, file) = create_atomic_temp_path(target_path)?;
    let write_res: io::Result<()> = (|| {
        let mut writer = BufWriter::with_capacity(64 * 1024, file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_data()?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    if let Err(err) = atomic_replace_from_temp(&tmp_path, target_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

/// Streams a `Rope` buffer directly to a crash-recovery file in `recovery_dir` without materializing a full `String`.
pub fn write_recovery_snapshot(
    rec_dir: &Path,
    slot_idx: usize,
    rope: &Rope,
) -> io::Result<PathBuf> {
    let target = rec_dir.join(format!("tab_{slot_idx}.md"));
    let (tmp_path, file) = create_atomic_temp_path(&target)?;
    let write_res: io::Result<()> = (|| {
        let mut writer = BufWriter::with_capacity(64 * 1024, file);
        for chunk in rope.chunks() {
            writer.write_all(chunk.as_bytes())?;
        }
        writer.flush()?;
        writer.get_ref().sync_data()?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    if let Err(err) = atomic_replace_from_temp(&tmp_path, &target) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(target)
}

/// Deletes stale recovery files in `rec_dir` not listed in `keep_paths`.
pub fn cleanup_unused_recovery_files(rec_dir: &Path, keep_paths: &[PathBuf]) {
    let Ok(entries) = fs::read_dir(rec_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let keep = keep_paths.iter().any(|kp| {
            #[cfg(windows)]
            {
                kp.to_string_lossy()
                    .eq_ignore_ascii_case(&p.to_string_lossy())
            }
            #[cfg(not(windows))]
            {
                kp == &p
            }
        });
        if !keep {
            let _ = fs::remove_file(&p);
        }
    }
}

impl SessionState {
    pub fn to_json_string(&self) -> String {
        let mut out = String::from("{\n");
        let _ = writeln!(out, "  \"active_tab\": {},", self.active_tab);
        out.push_str("  \"tabs\": [");
        for (i, tab) in self.tabs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("\n    {");
            match &tab.file_path {
                Some(p) => {
                    let _ = write!(
                        out,
                        "\n      \"file_path\": \"{}\",",
                        escape_json_str(&p.to_string_lossy())
                    );
                }
                None => out.push_str("\n      \"file_path\": null,"),
            }
            match &tab.recovery_file {
                Some(p) => {
                    let _ = write!(
                        out,
                        "\n      \"recovery_file\": \"{}\",",
                        escape_json_str(&p.to_string_lossy())
                    );
                }
                None => out.push_str("\n      \"recovery_file\": null,"),
            }
            let _ = writeln!(out, "\n      \"dirty\": {},", tab.dirty);
            let _ = writeln!(out, "      \"cursor_char\": {},", tab.cursor_char);
            let _ = writeln!(out, "      \"scroll_y\": {:.2},", tab.scroll_y.max(0.0));
            let _ = writeln!(out, "      \"untitled_num\": {}", tab.untitled_num);
            out.push_str("    }");
        }
        if !self.tabs.is_empty() {
            out.push_str("\n  ");
        }
        out.push_str("]\n}\n");
        out
    }

    pub fn from_json_str(raw: &str) -> Option<Self> {
        let active_tab = extract_usize_field(raw, "active_tab").unwrap_or(0);
        let tabs_slice = find_json_value_slice(raw, "tabs")?.strip_prefix('[')?;
        let objects = split_top_level_json_objects(tabs_slice);
        let mut tabs = Vec::with_capacity(objects.len());
        for obj in objects {
            let file_path = extract_str_field(obj, "file_path")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from);
            let recovery_file = extract_str_field(obj, "recovery_file")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from);
            let dirty = extract_bool_field(obj, "dirty").unwrap_or(false);
            let cursor_char = extract_usize_field(obj, "cursor_char").unwrap_or(0);
            let scroll_y = extract_f32_field(obj, "scroll_y").unwrap_or(0.0).max(0.0);
            let untitled_num = extract_usize_field(obj, "untitled_num").unwrap_or(0);
            if file_path.is_some() || recovery_file.is_some() {
                tabs.push(SessionTabEntry {
                    file_path,
                    recovery_file,
                    dirty,
                    cursor_char,
                    scroll_y,
                    untitled_num,
                });
            }
        }
        Some(Self { active_tab, tabs })
    }
}

fn split_top_level_json_objects(array_body: &str) -> Vec<&str> {
    let mut objs = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut obj_start: Option<usize> = None;

    for (idx, ch) in array_body.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    obj_start = Some(idx);
                }
                depth += 1;
            }
            '}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(start) = obj_start.take() {
                            objs.push(&array_body[start..=idx]);
                        }
                    }
                }
            }
            ']' if depth == 0 => break,
            _ => {}
        }
    }
    objs
}

/// Atomically persists `state` as UTF-8 JSON to `path`.
pub fn save_session(path: &Path, state: &SessionState) -> io::Result<()> {
    atomic_write_bytes(path, state.to_json_string().as_bytes())
}

/// Loads `SessionState` from `path`, returning `None` if missing or unreadable.
pub fn load_session(path: &Path) -> Option<SessionState> {
    let raw = fs::read_to_string(path).ok()?;
    SessionState::from_json_str(&raw)
}

/// Snapshot descriptor for one tab in a session persistence job.
#[derive(Debug, Clone)]
pub struct SessionSaveTab {
    pub slot_idx: usize,
    pub file_path: Option<PathBuf>,
    pub rope: Option<Rope>,
    pub dirty: bool,
    pub cursor_char: usize,
    pub scroll_y: f32,
    pub untitled_num: usize,
}

/// Complete session + recovery snapshot batch written either synchronously or on a worker thread.
#[derive(Debug, Clone)]
pub struct SessionSaveJob {
    pub session_path: PathBuf,
    pub recovery_dir: PathBuf,
    pub active_tab: usize,
    pub tabs: Vec<SessionSaveTab>,
}

/// Executes a `SessionSaveJob`, writing recovery snapshots, `session.json`, and cleaning stale files.
pub fn write_session_save_job(job: &SessionSaveJob) {
    let mut keep_paths = Vec::new();
    let mut entries = Vec::with_capacity(job.tabs.len());
    let mut new_active = 0usize;

    for (idx, tab) in job.tabs.iter().enumerate() {
        let recovery_file = match tab.rope {
            Some(ref rope) => match write_recovery_snapshot(&job.recovery_dir, tab.slot_idx, rope) {
                Ok(p) => {
                    keep_paths.push(p.clone());
                    Some(p)
                }
                Err(_) => None,
            },
            None => None,
        };
        if tab.file_path.is_none() && recovery_file.is_none() {
            continue;
        }
        if idx == job.active_tab {
            new_active = entries.len();
        }
        entries.push(SessionTabEntry {
            file_path: tab.file_path.clone(),
            recovery_file,
            dirty: tab.dirty,
            cursor_char: tab.cursor_char,
            scroll_y: tab.scroll_y,
            untitled_num: tab.untitled_num,
        });
    }

    let state = SessionState {
        active_tab: if entries.is_empty() {
            0
        } else {
            new_active.min(entries.len() - 1)
        },
        tabs: entries,
    };
    let _ = save_session(&job.session_path, &state);
    cleanup_unused_recovery_files(&job.recovery_dir, &keep_paths);
}

/// Coalescing background session/crash-recovery writer that avoids UI-thread disk sync hitches.
#[derive(Debug, Default)]
pub struct BackgroundSessionSaver {
    tx: Option<Sender<SessionSaveJob>>,
    handle: Option<JoinHandle<()>>,
}

impl BackgroundSessionSaver {
    /// Queues `job` for asynchronous writing, lazily spawning `"md-session-saver"` on first use.
    pub fn enqueue(&mut self, job: SessionSaveJob) {
        if let Some(ref tx) = self.tx {
            if tx.send(job.clone()).is_ok() {
                return;
            }
        }
        let (tx, rx) = mpsc::channel::<SessionSaveJob>();
        if tx.send(job).is_err() {
            return;
        }
        if let Ok(handle) = thread::Builder::new()
            .name("md-session-saver".to_string())
            .spawn(move || {
                while let Ok(mut latest) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        latest = newer;
                    }
                    write_session_save_job(&latest);
                }
            })
        {
            self.tx = Some(tx);
            self.handle = Some(handle);
        }
    }
}

impl Drop for BackgroundSessionSaver {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn session_json_roundtrip_handles_windows_paths_dirty_recovery_and_corrupt_input() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let sess_path = session_path(&settings_path);
        let rec_dir = recovery_dir(&settings_path);

        let rope = Rope::from_str("# Unsaved Draft\n\nRecovered body with {braces} and \"quotes\".\n");
        let rec_file = write_recovery_snapshot(&rec_dir, 1, &rope).unwrap();
        let stale_file = rec_dir.join("tab_99.md");
        fs::write(&stale_file, "old").unwrap();

        let state = SessionState {
            active_tab: 1,
            tabs: vec![
                SessionTabEntry {
                    file_path: Some(PathBuf::from(r"C:\Docs\notes {v1}\readme.md")),
                    recovery_file: None,
                    dirty: false,
                    cursor_char: 42,
                    scroll_y: 128.5,
                    untitled_num: 0,
                },
                SessionTabEntry {
                    file_path: None,
                    recovery_file: Some(rec_file.clone()),
                    dirty: true,
                    cursor_char: 15,
                    scroll_y: 0.0,
                    untitled_num: 2,
                },
            ],
        };
        save_session(&sess_path, &state).unwrap();
        cleanup_unused_recovery_files(&rec_dir, std::slice::from_ref(&rec_file));
        assert!(rec_file.exists());
        assert!(!stale_file.exists());

        let loaded = load_session(&sess_path).expect("session must parse");
        assert_eq!(loaded.active_tab, 1);
        assert_eq!(loaded.tabs.len(), 2);
        assert_eq!(
            loaded.tabs[0].file_path.as_deref(),
            Some(Path::new(r"C:\Docs\notes {v1}\readme.md"))
        );
        assert!(!loaded.tabs[0].dirty);
        assert_eq!(loaded.tabs[0].cursor_char, 42);
        assert!((loaded.tabs[0].scroll_y - 128.5).abs() < 0.01);
        assert_eq!(loaded.tabs[1].recovery_file.as_deref(), Some(rec_file.as_path()));
        assert!(loaded.tabs[1].dirty);
        assert_eq!(loaded.tabs[1].untitled_num, 2);

        assert!(SessionState::from_json_str("{corrupt invalid json").is_none());
    }

    #[test]
    fn background_session_saver_writes_recovery_and_manifest_cleanly() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let sess_path = session_path(&settings_path);
        let rec_dir = recovery_dir(&settings_path);

        {
            let mut saver = BackgroundSessionSaver::default();
            saver.enqueue(SessionSaveJob {
                session_path: sess_path.clone(),
                recovery_dir: rec_dir.clone(),
                active_tab: 0,
                tabs: vec![SessionSaveTab {
                    slot_idx: 0,
                    file_path: None,
                    rope: Some(Rope::from_str("# First Draft")),
                    dirty: true,
                    cursor_char: 5,
                    scroll_y: 0.0,
                    untitled_num: 1,
                }],
            });
            // Coalesce/overwrite with newer snapshot
            saver.enqueue(SessionSaveJob {
                session_path: sess_path.clone(),
                recovery_dir: rec_dir.clone(),
                active_tab: 0,
                tabs: vec![SessionSaveTab {
                    slot_idx: 0,
                    file_path: None,
                    rope: Some(Rope::from_str("# Final Background Draft")),
                    dirty: true,
                    cursor_char: 12,
                    scroll_y: 44.0,
                    untitled_num: 1,
                }],
            });
        } // Drop joins worker cleanly before assertions

        let loaded = load_session(&sess_path).expect("background session.json must exist");
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.tabs[0].cursor_char, 12);
        let rec_path = loaded.tabs[0].recovery_file.as_ref().unwrap();
        assert_eq!(
            fs::read_to_string(rec_path).unwrap(),
            "# Final Background Draft"
        );
    }
}
