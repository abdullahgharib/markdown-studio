use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustc_hash::FxHasher;

use crate::ui::settings::AppSettings;

static NONCE_COUNTER: AtomicU64 = AtomicU64::new(1);
static TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

const HANDOFF_HEADER: &str = "MDSTUDIO_HANDOFF_V1";

/// Parsed command-line launch configuration for Markdown Studio.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LaunchArgs {
    pub files: Vec<PathBuf>,
    pub new_window: bool,
    pub handoff_file: Option<PathBuf>,
    pub window_pos: Option<(f32, f32)>,
}

impl LaunchArgs {
    /// Parses CLI arguments (excluding argv[0]), supporting `--new-window`/`-n`,
    /// `--handoff <path>`, `--window-pos <x>,<y>`, and positional file paths.
    pub fn from_args<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = Self::default();
        let mut iter = args.into_iter();
        let mut positional_only = false;

        while let Some(arg) = iter.next() {
            let s = arg.as_ref();
            if positional_only {
                if !s.is_empty() {
                    out.files.push(PathBuf::from(s));
                }
                continue;
            }
            if s == "--" {
                positional_only = true;
                continue;
            }
            if s == "--new-window" || s == "-n" {
                out.new_window = true;
                continue;
            }
            if let Some(val) = s.strip_prefix("--handoff=") {
                if !val.is_empty() {
                    out.handoff_file = Some(PathBuf::from(val));
                    out.new_window = true;
                }
                continue;
            }
            if s == "--handoff" {
                if let Some(next) = iter.next() {
                    let val = next.as_ref();
                    if !val.is_empty() {
                        out.handoff_file = Some(PathBuf::from(val));
                        out.new_window = true;
                    }
                }
                continue;
            }
            if let Some(val) = s.strip_prefix("--window-pos=") {
                if let Some(pos) = parse_window_pos(val) {
                    out.window_pos = Some(pos);
                }
                continue;
            }
            if s == "--window-pos" {
                if let Some(next) = iter.next() {
                    if let Some(pos) = parse_window_pos(next.as_ref()) {
                        out.window_pos = Some(pos);
                    }
                }
                continue;
            }
            if !s.is_empty() {
                out.files.push(PathBuf::from(s));
            }
        }

        out
    }
}

fn parse_window_pos(raw: &str) -> Option<(f32, f32)> {
    let (xs, ys) = raw.split_once(',')?;
    let x: f32 = xs.trim().parse().ok()?;
    let y: f32 = ys.trim().parse().ok()?;
    if x.is_finite() && y.is_finite() {
        Some((x, y))
    } else {
        None
    }
}

/// Serialized buffer snapshot used to transfer unsaved or untitled tabs to a new window process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedTabHandoff {
    pub file_path: Option<PathBuf>,
    pub dirty: bool,
    pub content: String,
}

fn escape_ipc_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '\t' => out.push_str("%09"),
            '\r' => out.push_str("%0D"),
            '\n' => out.push_str("%0A"),
            other => out.push(other),
        }
    }
    out
}

fn unescape_ipc_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            match &bytes[i + 1..i + 3] {
                b"25" => {
                    out.push('%');
                    i += 3;
                    continue;
                }
                b"09" => {
                    out.push('\t');
                    i += 3;
                    continue;
                }
                b"0D" | b"0d" => {
                    out.push('\r');
                    i += 3;
                    continue;
                }
                b"0A" | b"0a" => {
                    out.push('\n');
                    i += 3;
                    continue;
                }
                _ => {}
            }
        }
        let rest = &s[i..];
        let ch = rest.chars().next().expect("valid utf-8 boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn create_unique_handoff_file() -> io::Result<(PathBuf, std::fs::File)> {
    let dir = std::env::temp_dir();
    for _ in 0..16 {
        let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = dir.join(format!("mdstudio_handoff_{}_{}.handoff", std::process::id(), seq));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Unable to allocate unique handoff file",
    ))
}

fn write_handoff_header(
    writer: &mut impl Write,
    file_path: Option<&Path>,
    dirty: bool,
) -> io::Result<()> {
    let path_str = file_path
        .map(|p| escape_ipc_field(&p.to_string_lossy()))
        .unwrap_or_default();
    writer.write_all(HANDOFF_HEADER.as_bytes())?;
    writer.write_all(b"\nPATH:")?;
    writer.write_all(path_str.as_bytes())?;
    writer.write_all(b"\nDIRTY:")?;
    writer.write_all(if dirty { b"true" } else { b"false" })?;
    writer.write_all(b"\n---\n")
}

/// Writes a temporary `.handoff` snapshot into `std::env::temp_dir()` for a newly spawned window.
pub fn write_handoff_file(handoff: &DetachedTabHandoff) -> io::Result<PathBuf> {
    let (candidate, file) = create_unique_handoff_file()?;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    write_handoff_header(&mut writer, handoff.file_path.as_deref(), handoff.dirty)?;
    writer.write_all(handoff.content.as_bytes())?;
    writer.flush()?;
    Ok(candidate)
}

/// Streams a `ropey::Rope` directly into a temporary `.handoff` file via a 64 KB `BufWriter`
/// without cloning or materializing the entire buffer into a contiguous `String`.
pub fn write_handoff_file_from_rope(
    file_path: Option<&Path>,
    dirty: bool,
    rope: &ropey::Rope,
) -> io::Result<PathBuf> {
    let (candidate, file) = create_unique_handoff_file()?;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    write_handoff_header(&mut writer, file_path, dirty)?;
    for chunk in rope.chunks() {
        writer.write_all(chunk.as_bytes())?;
    }
    writer.flush()?;
    Ok(candidate)
}

/// Reads and immediately deletes a `.handoff` file created by `write_handoff_file`.
pub fn read_and_remove_handoff_file(path: &Path) -> io::Result<DetachedTabHandoff> {
    let raw = std::fs::read_to_string(path)?;
    let _ = std::fs::remove_file(path);

    let (header, content) = raw.split_once("\n---\n").ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "Missing handoff delimiter")
    })?;
    let mut lines = header.lines();
    if lines.next() != Some(HANDOFF_HEADER) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid handoff header",
        ));
    }
    let path_line = lines
        .next()
        .and_then(|l| l.strip_prefix("PATH:"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing PATH line"))?;
    let dirty_line = lines
        .next()
        .and_then(|l| l.strip_prefix("DIRTY:"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing DIRTY line"))?;

    let file_path = if path_line.is_empty() {
        None
    } else {
        Some(PathBuf::from(unescape_ipc_field(path_line)))
    };
    let dirty = dirty_line.trim().eq_ignore_ascii_case("true");

    Ok(DetachedTabHandoff {
        file_path,
        dirty,
        content: content.to_string(),
    })
}

/// Messages forwarded from external launches or sibling windows to a Markdown Studio window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceMessage {
    OpenFile(PathBuf),
    AdoptHandoff(PathBuf),
}

/// Live window metadata published by each open instance for Chrome-style cross-window tab drops.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerWindowInfo {
    pub port: u16,
    pub nonce: String,
    pub outer_rect: egui::Rect,
    pub minimized: bool,
}

/// Returns the default per-user `instance.lock` path alongside `settings.json`.
pub fn default_instance_lock_path() -> PathBuf {
    let mut path = AppSettings::default_path();
    path.set_file_name("instance.lock");
    path
}

fn generate_nonce() -> String {
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();

    let mut fx = FxHasher::default();
    now_nanos.hash(&mut fx);
    pid.hash(&mut fx);
    seq.hash(&mut fx);

    let mut std_h = DefaultHasher::new();
    seq.hash(&mut std_h);
    now_nanos.hash(&mut std_h);

    let combined = fx.finish() ^ std_h.finish().rotate_left(17);
    format!("{combined:016x}")
}

fn write_file_atomic(target: &Path, body: &[u8]) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        "{}.tmp.{}.{}",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("ipc"),
        std::process::id(),
        seq
    );
    let tmp_path = target.with_file_name(tmp_name);
    std::fs::write(&tmp_path, body)?;
    match std::fs::rename(&tmp_path, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = std::fs::remove_file(&tmp_path);
            std::fs::write(target, body)
        }
    }
}

fn write_lock_file_atomic(lock_path: &Path, port: u16, nonce: &str) -> io::Result<()> {
    let body = format!("{port}\n{nonce}\n");
    write_file_atomic(lock_path, body.as_bytes())
}

fn peer_windows_dir(lock_path: &Path) -> PathBuf {
    let parent = lock_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    parent.join("windows")
}

fn parse_peer_window_info(raw: &str) -> Option<PeerWindowInfo> {
    let mut lines = raw.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let nonce = lines.next()?.trim().to_string();
    if nonce.is_empty() {
        return None;
    }
    let coords: Vec<f32> = lines
        .next()?
        .split(',')
        .filter_map(|s| s.trim().parse::<f32>().ok())
        .collect();
    if coords.len() != 4 || coords.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let minimized = lines.next().is_some_and(|s| s.trim() == "1");
    let outer_rect = egui::Rect::from_min_max(
        egui::pos2(coords[0], coords[1]),
        egui::pos2(coords[2], coords[3]),
    );
    Some(PeerWindowInfo {
        port,
        nonce,
        outer_rect,
        minimized,
    })
}

fn read_lock_file(lock_path: &Path) -> io::Result<(u16, String)> {
    let raw = std::fs::read_to_string(lock_path)?;
    let mut lines = raw.lines();
    let port_str = lines
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing IPC port"))?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid IPC port"))?;
    let nonce = lines
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing IPC nonce"))?
        .to_string();
    Ok((port, nonce))
}

/// Loopback IPC server allowing newly launched processes to forward file opens as tabs
/// into the active running window and enabling cross-window tab transfers.
pub struct SingleInstanceServer {
    lock_path: PathBuf,
    win_info_path: PathBuf,
    port: u16,
    nonce: String,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl SingleInstanceServer {
    /// Starts the localhost IPC listener, writes `lock_path`, and returns `(server, receiver)`.
    pub fn start(
        lock_path: PathBuf,
        repaint_ctx: Option<egui::Context>,
    ) -> io::Result<(Self, mpsc::Receiver<InstanceMessage>)> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let nonce = generate_nonce();
        write_lock_file_atomic(&lock_path, port, &nonce)?;
        let win_info_path =
            peer_windows_dir(&lock_path).join(format!("win_{}_{}.info", std::process::id(), port));

        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let nonce_bg = nonce.clone();
        let (tx, rx) = mpsc::channel::<InstanceMessage>();

        let join = std::thread::Builder::new()
            .name("mdstudio-ipc".to_string())
            .spawn(move || {
                for stream_res in listener.incoming() {
                    if stop_bg.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(mut stream) = stream_res else {
                        break;
                    };
                    if stop_bg.load(Ordering::Acquire) {
                        break;
                    }
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));

                    let reader_stream = match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let mut reader = BufReader::new(reader_stream);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        let Ok(n) = reader.read_line(&mut line) else {
                            break;
                        };
                        if n == 0 {
                            break;
                        }
                        let trimmed = line.trim_end_matches(['\r', '\n']);
                        let mut parts = trimmed.splitn(3, '\t');
                        let tok = parts.next().unwrap_or("");
                        let cmd = parts.next().unwrap_or("");
                        let arg = parts.next().unwrap_or("");
                        if tok == nonce_bg && !arg.is_empty() && matches!(cmd, "OPEN" | "HANDOFF")
                        {
                            let path = PathBuf::from(unescape_ipc_field(arg));
                            let msg = if cmd == "HANDOFF" {
                                InstanceMessage::AdoptHandoff(path)
                            } else {
                                InstanceMessage::OpenFile(path)
                            };
                            let _ = tx.send(msg);
                            if let Some(ctx) = &repaint_ctx {
                                ctx.request_repaint();
                            }
                            if stream.write_all(b"OK\n").is_err() {
                                break;
                            }
                            let _ = stream.flush();
                        } else {
                            let _ = stream.write_all(b"ERR\n");
                            break;
                        }
                    }
                }
            })?;

        Ok((
            Self {
                lock_path,
                win_info_path,
                port,
                nonce,
                stop,
                join: Some(join),
            },
            rx,
        ))
    }

    /// Convenience wrapper using `default_instance_lock_path()`.
    pub fn start_default(
        repaint_ctx: Option<egui::Context>,
    ) -> io::Result<(Self, mpsc::Receiver<InstanceMessage>)> {
        Self::start(default_instance_lock_path(), repaint_ctx)
    }

    /// Claims ownership of `lock_path` so whichever window was most recently focused
    /// receives subsequent Explorer/CLI file opens.
    pub fn claim_active_target(&self) {
        let _ = write_lock_file_atomic(&self.lock_path, self.port, &self.nonce);
    }

    /// Publishes this window's outer screen rectangle and minimized state for sibling drag targets.
    pub fn update_window_bounds(&self, outer_rect: egui::Rect, minimized: bool) {
        let body = format!(
            "{}\n{}\n{:.1},{:.1},{:.1},{:.1}\n{}\n",
            self.port,
            self.nonce,
            outer_rect.left(),
            outer_rect.top(),
            outer_rect.right(),
            outer_rect.bottom(),
            if minimized { "1" } else { "0" }
        );
        let _ = write_file_atomic(&self.win_info_path, body.as_bytes());
    }

    /// Lists non-minimized sibling Markdown Studio windows registered under the same IPC directory.
    pub fn list_peer_windows(&self) -> Vec<PeerWindowInfo> {
        let dir = peer_windows_dir(&self.lock_path);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut peers = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("info") {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Some(info) = parse_peer_window_info(&raw) else {
                continue;
            };
            if info.nonce == self.nonce || info.minimized {
                continue;
            }
            peers.push(info);
        }
        peers
    }
}

impl Drop for SingleInstanceServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(80));
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
        if let Ok((_, file_nonce)) = read_lock_file(&self.lock_path) {
            if file_nonce == self.nonce {
                let _ = std::fs::remove_file(&self.lock_path);
            }
        }
        let _ = std::fs::remove_file(&self.win_info_path);
    }
}

fn send_ipc_command_to_port(port: u16, nonce: &str, cmd: &str, field: &str) -> io::Result<bool> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(250)) else {
        return Ok(false);
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));

    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let encoded = escape_ipc_field(field);
    let msg = format!("{nonce}\t{cmd}\t{encoded}\n");
    if stream.write_all(msg.as_bytes()).is_err() || stream.flush().is_err() {
        return Ok(false);
    }
    let mut resp = String::new();
    Ok(reader.read_line(&mut resp).is_ok() && resp.trim() == "OK")
}

/// Forwards an already-saved file path to a specific peer window so it opens as a new tab there.
pub fn try_send_open_to_peer(peer: &PeerWindowInfo, path: &Path) -> io::Result<bool> {
    send_ipc_command_to_port(peer.port, &peer.nonce, "OPEN", &path.to_string_lossy())
}

/// Forwards a temporary `.handoff` snapshot to a specific peer window so it adopts the tab.
pub fn try_send_handoff_to_peer(peer: &PeerWindowInfo, handoff_path: &Path) -> io::Result<bool> {
    send_ipc_command_to_port(
        peer.port,
        &peer.nonce,
        "HANDOFF",
        &handoff_path.to_string_lossy(),
    )
}

/// Attempts to forward `files` to a currently active Markdown Studio instance described
/// by `lock_path`. Returns `Ok(true)` only if all files were acknowledged by the host.
pub fn try_forward_files_to_running_instance(
    lock_path: &Path,
    files: &[PathBuf],
) -> io::Result<bool> {
    if files.is_empty() {
        return Ok(false);
    }
    let Ok((port, nonce)) = read_lock_file(lock_path) else {
        return Ok(false);
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(250)) else {
        return Ok(false);
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));

    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let cwd = std::env::current_dir().ok();

    for path in files {
        let resolved = if path.is_relative() {
            if let Some(dir) = &cwd {
                dir.join(path)
            } else {
                path.clone()
            }
        } else {
            path.clone()
        };
        let encoded = escape_ipc_field(&resolved.to_string_lossy());
        let msg = format!("{nonce}\tOPEN\t{encoded}\n");
        if stream.write_all(msg.as_bytes()).is_err() || stream.flush().is_err() {
            return Ok(false);
        }
        let mut resp = String::new();
        if reader.read_line(&mut resp).is_err() || resp.trim() != "OK" {
            return Ok(false);
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn launch_args_parses_flags_window_pos_handoff_and_files() {
        let args = LaunchArgs::from_args([
            "--new-window",
            "--window-pos",
            "140.5,280",
            "--handoff",
            "C:\\tmp\\doc.handoff",
            "notes.md",
            "C:\\docs\\readme.md",
        ]);
        assert!(args.new_window);
        assert_eq!(args.window_pos, Some((140.5, 280.0)));
        assert_eq!(args.handoff_file, Some(PathBuf::from("C:\\tmp\\doc.handoff")));
        assert_eq!(
            args.files,
            vec![PathBuf::from("notes.md"), PathBuf::from("C:\\docs\\readme.md")]
        );

        let implicit = LaunchArgs::from_args(["-n", "--window-pos=10,20", "one.md"]);
        assert!(implicit.new_window);
        assert_eq!(implicit.window_pos, Some((10.0, 20.0)));
        assert_eq!(implicit.files, vec![PathBuf::from("one.md")]);

        let handoff_implies_new = LaunchArgs::from_args(["--handoff=tab.handoff"]);
        assert!(handoff_implies_new.new_window);
        assert_eq!(
            handoff_implies_new.handoff_file,
            Some(PathBuf::from("tab.handoff"))
        );
    }

    #[test]
    fn handoff_file_roundtrips_unicode_dirty_state_and_self_deletes() {
        let original = DetachedTabHandoff {
            file_path: Some(PathBuf::from("D:\\مجلد عربي\\notes % 100%\tname.md")),
            dirty: true,
            content: "# Title\r\n\r\n---\r\nLine two — مرحبا 🚀".to_string(),
        };
        let path = write_handoff_file(&original).expect("write handoff file");
        assert!(path.exists());

        let loaded = read_and_remove_handoff_file(&path).expect("read and delete handoff file");
        assert_eq!(loaded, original);
        assert!(!path.exists(), "handoff file should be deleted after read");
    }

    #[test]
    fn single_instance_forwards_files_recovers_from_stale_lock_and_switches_active_target() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let lock_path = tmp.path().join("instance.lock");

        // 1. Stale lockfile from crashed instance must fail gracefully with Ok(false).
        std::fs::write(&lock_path, "65001\nstale_nonce_0000\n").expect("write stale lock");
        let forwarded_stale =
            try_forward_files_to_running_instance(&lock_path, &[PathBuf::from("a.md")])
                .expect("stale check");
        assert!(!forwarded_stale);

        // 2. Start first active instance and forward multiple files (including Unicode/spaces/percent).
        let (srv1, rx1) = SingleInstanceServer::start(lock_path.clone(), None).expect("start srv1");
        let f1 = tmp.path().join("first doc.md");
        let f2 = tmp.path().join("ثاني % 2.md");
        let ok = try_forward_files_to_running_instance(&lock_path, &[f1.clone(), f2.clone()])
            .expect("forward to srv1");
        assert!(ok);
        assert_eq!(rx1.recv_timeout(Duration::from_secs(1)).unwrap(), InstanceMessage::OpenFile(f1.clone()));
        assert_eq!(rx1.recv_timeout(Duration::from_secs(1)).unwrap(), InstanceMessage::OpenFile(f2));

        // 3. Start second window (takes active target), verify forward goes to srv2.
        let (srv2, rx2) = SingleInstanceServer::start(lock_path.clone(), None).expect("start srv2");
        let f3 = tmp.path().join("three.md");
        assert!(try_forward_files_to_running_instance(&lock_path, std::slice::from_ref(&f3)).unwrap());
        assert_eq!(rx2.recv_timeout(Duration::from_secs(1)).unwrap(), InstanceMessage::OpenFile(f3));

        // 4. Focus switches back to first window -> claim_active_target redirects new opens to srv1.
        srv1.claim_active_target();
        let f4 = tmp.path().join("four.md");
        assert!(try_forward_files_to_running_instance(&lock_path, std::slice::from_ref(&f4)).unwrap());
        assert_eq!(rx1.recv_timeout(Duration::from_secs(1)).unwrap(), InstanceMessage::OpenFile(f4));

        // 5. Dropping srv2 must NOT delete srv1's lockfile because nonce differs.
        drop(srv2);
        assert!(lock_path.exists());
        assert!(try_forward_files_to_running_instance(&lock_path, std::slice::from_ref(&f1)).unwrap());
        assert_eq!(rx1.recv_timeout(Duration::from_secs(1)).unwrap(), InstanceMessage::OpenFile(f1));

        // 6. Dropping owning srv1 exits cleanly without hanging and deletes lockfile.
        let t0 = Instant::now();
        drop(srv1);
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert!(!lock_path.exists());
    }

    #[test]
    fn peer_window_registry_discovers_bounds_and_transfers_open_and_handoff() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let lock_path = tmp.path().join("instance.lock");

        let (srv1, rx1) = SingleInstanceServer::start(lock_path.clone(), None).expect("start srv1");
        let (srv2, _rx2) = SingleInstanceServer::start(lock_path.clone(), None).expect("start srv2");

        let rect1 = egui::Rect::from_min_max(egui::pos2(50.0, 60.0), egui::pos2(850.0, 660.0));
        let rect2 = egui::Rect::from_min_max(egui::pos2(900.0, 60.0), egui::pos2(1700.0, 660.0));
        srv1.update_window_bounds(rect1, false);
        srv2.update_window_bounds(rect2, false);

        // Each window discovers only non-minimized sibling peers (never itself)
        let peers_for_2 = srv2.list_peer_windows();
        assert_eq!(peers_for_2.len(), 1);
        assert_eq!(peers_for_2[0].outer_rect, rect1);

        // Direct OPEN and HANDOFF forwarding over peer IPC
        let moved_md = tmp.path().join("moved.md");
        assert!(try_send_open_to_peer(&peers_for_2[0], &moved_md).unwrap());
        assert_eq!(
            rx1.recv_timeout(Duration::from_secs(1)).unwrap(),
            InstanceMessage::OpenFile(moved_md)
        );

        let handoff_path = tmp.path().join("tab.handoff");
        assert!(try_send_handoff_to_peer(&peers_for_2[0], &handoff_path).unwrap());
        assert_eq!(
            rx1.recv_timeout(Duration::from_secs(1)).unwrap(),
            InstanceMessage::AdoptHandoff(handoff_path)
        );

        // Minimized peer is hidden from drop candidates
        srv1.update_window_bounds(rect1, true);
        assert!(srv2.list_peer_windows().is_empty());

        let win1_info = srv1.win_info_path.clone();
        drop(srv1);
        assert!(!win1_info.exists(), "dropping server removes its window info file");
        drop(srv2);
    }

    #[test]
    fn write_handoff_file_from_rope_streams_and_roundtrips_with_metadata() {
        let mut rope = ropey::Rope::from_str("# Handoff Header\n");
        for i in 0..200 {
            rope.insert(rope.len_chars(), &format!("Line {i}: مرحبا Rust 🚀\n"));
        }
        let orig_path = PathBuf::from(r"D:\notes\draft%.md");
        let handoff_path =
            write_handoff_file_from_rope(Some(&orig_path), true, &rope).expect("write handoff");
        assert!(handoff_path.exists());

        let loaded = read_and_remove_handoff_file(&handoff_path).expect("read handoff");
        assert!(!handoff_path.exists(), "reading handoff removes temp file");
        assert_eq!(loaded.file_path.as_deref(), Some(orig_path.as_path()));
        assert!(loaded.dirty);
        assert_eq!(loaded.content, rope.to_string());
    }
}
