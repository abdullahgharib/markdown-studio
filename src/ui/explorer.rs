use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, Margin, RichText, Sense, Stroke, Vec2};

use crate::ui::theme::WordTheme;

const MAX_SCAN_DEPTH: usize = 64;
const MAX_SCAN_ENTRIES: usize = 50_000;
const FAST_SYNC_SCAN_ENTRIES: usize = 256;
const FAST_SYNC_SCAN_BUDGET: Duration = Duration::from_millis(2);

/// File type filter pills for the File Explorer sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExplorerFileFilter {
    #[default]
    All,
    Markdown,
    Text,
    CodeAndData,
}

impl ExplorerFileFilter {
    pub fn label(self) -> &'static str {
        match self {
            ExplorerFileFilter::All => "All",
            ExplorerFileFilter::Markdown => "MD",
            ExplorerFileFilter::Text => "Text",
            ExplorerFileFilter::CodeAndData => "Code",
        }
    }

    pub fn matches_extension(self, ext_lower: &str) -> bool {
        match self {
            ExplorerFileFilter::All => true,
            ExplorerFileFilter::Markdown => {
                matches!(ext_lower, "md" | "markdown" | "mdown" | "mkd" | "mdx")
            }
            ExplorerFileFilter::Text => {
                matches!(ext_lower, "txt" | "log" | "rst" | "org" | "tex" | "adoc")
            }
            ExplorerFileFilter::CodeAndData => matches!(
                ext_lower,
                "json"
                    | "toml"
                    | "yaml"
                    | "yml"
                    | "xml"
                    | "svg"
                    | "html"
                    | "htm"
                    | "css"
                    | "js"
                    | "jsx"
                    | "ts"
                    | "tsx"
                    | "rs"
                    | "py"
                    | "c"
                    | "cpp"
                    | "cc"
                    | "h"
                    | "hpp"
                    | "cs"
                    | "java"
                    | "go"
                    | "rb"
                    | "php"
                    | "sh"
                    | "bat"
                    | "cmd"
                    | "ps1"
                    | "ini"
                    | "cfg"
                    | "conf"
                    | "env"
                    | "csv"
                    | "tsv"
                    | "sql"
            ),
        }
    }
}

/// Action returned from rendering the File Explorer panel.
#[derive(Debug, Clone, PartialEq)]
pub enum ExplorerAction {
    None,
    OpenFile(PathBuf),
    OpenFolderDialog,
    SetRoot(PathBuf),
    ContextMenu {
        path: PathBuf,
        is_dir: bool,
        is_root_empty_area: bool,
        pos: egui::Pos2,
    },
}

/// Cached directory entry metadata for fast tree browsing and search/filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorerEntry {
    pub path: PathBuf,
    pub name: String,
    pub rel_path: String,
    pub name_lower: String,
    pub rel_path_lower: String,
    pub is_dir: bool,
    pub ext_lower: String,
}

type ExplorerFilteredCache = (String, ExplorerFileFilter, bool, u64, Vec<usize>);
type ExplorerScanResult = (PathBuf, bool, u64, Vec<ExplorerEntry>, bool);

#[derive(Debug, Clone)]
struct FlatTreeRow {
    path: PathBuf,
    name: String,
    ext_lower: String,
    is_dir: bool,
    depth: usize,
    expanded: bool,
    empty_hint: bool,
}

/// Stateful, cached VS Code-style File Explorer with search & file type filters.
#[derive(Debug, Default)]
pub struct FileExplorerState {
    pub root_dir: Option<PathBuf>,
    pub search_query: String,
    pub file_filter: ExplorerFileFilter,
    pub show_hidden: bool,
    expanded_dirs: HashSet<PathBuf>,
    dir_cache: HashMap<PathBuf, Vec<ExplorerEntry>>,
    flat_tree_cache: Option<(PathBuf, bool, u64, Vec<FlatTreeRow>)>,
    tree_rev: u64,
    search_cache: Option<(PathBuf, bool, Vec<ExplorerEntry>, bool)>,
    filtered_cache: Option<ExplorerFilteredCache>,
    scan_rx: Option<Receiver<ExplorerScanResult>>,
    scan_in_flight: Option<(PathBuf, bool, u64)>,
    scan_gen: u64,
}

impl FileExplorerState {
    pub fn set_root(&mut self, root: PathBuf) {
        if self.root_dir.as_ref() != Some(&root) {
            self.root_dir = Some(root.clone());
            self.expanded_dirs.clear();
            self.expanded_dirs.insert(root);
            self.refresh();
        }
    }

    pub fn refresh(&mut self) {
        self.scan_gen = self.scan_gen.wrapping_add(1);
        self.tree_rev = self.tree_rev.wrapping_add(1);
        self.dir_cache.clear();
        self.flat_tree_cache = None;
        self.search_cache = None;
        self.filtered_cache = None;
        self.scan_rx = None;
        self.scan_in_flight = None;
    }

    pub fn is_dir_expanded(&self, dir: &Path) -> bool {
        self.expanded_dirs.contains(dir)
    }

    pub fn toggle_dir(&mut self, dir: &Path) {
        if self.expanded_dirs.contains(dir) {
            self.expanded_dirs.remove(dir);
        } else {
            self.expanded_dirs.insert(dir.to_path_buf());
        }
        self.tree_rev = self.tree_rev.wrapping_add(1);
    }

    /// Returns sorted direct children of `dir`, reading from disk once and caching.
    pub fn directory_children(&mut self, dir: &Path) -> &[ExplorerEntry] {
        let root = self.root_dir.clone();
        let show_hidden = self.show_hidden;
        if !self.dir_cache.contains_key(dir) {
            let entries = read_dir_sorted(dir, root.as_deref(), show_hidden);
            self.dir_cache.insert(dir.to_path_buf(), entries);
            self.tree_rev = self.tree_rev.wrapping_add(1);
        }
        self.dir_cache
            .get(dir)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn ensure_flat_tree(&mut self, root: &Path) {
        if self.flat_tree_cache.as_ref().is_some_and(
            |(cached_root, cached_hidden, cached_rev, _)| {
                cached_root == root
                    && *cached_hidden == self.show_hidden
                    && *cached_rev == self.tree_rev
            },
        ) {
            return;
        }
        let mut rows = Vec::new();
        let mut seen_dirs = HashSet::new();
        if let Ok(canon) = fs::canonicalize(root) {
            seen_dirs.insert(canon);
        }
        self.append_flat_dir(root, 0, &mut seen_dirs, &mut rows);
        self.flat_tree_cache = Some((root.to_path_buf(), self.show_hidden, self.tree_rev, rows));
    }

    fn append_flat_dir(
        &mut self,
        dir: &Path,
        depth: usize,
        seen_dirs: &mut HashSet<PathBuf>,
        out: &mut Vec<FlatTreeRow>,
    ) {
        if depth > MAX_SCAN_DEPTH {
            return;
        }
        let count = self.directory_children(dir).len();
        if count == 0 {
            if depth == 0 {
                out.push(FlatTreeRow {
                    path: dir.to_path_buf(),
                    name: "(Empty folder)".to_string(),
                    ext_lower: String::new(),
                    is_dir: false,
                    depth,
                    expanded: false,
                    empty_hint: true,
                });
            }
            return;
        }
        for idx in 0..count {
            let (path, name, ext_lower, is_dir) = {
                let entry = &self.directory_children(dir)[idx];
                (
                    entry.path.clone(),
                    entry.name.clone(),
                    entry.ext_lower.clone(),
                    entry.is_dir,
                )
            };
            let expanded = is_dir && self.is_dir_expanded(&path);
            out.push(FlatTreeRow {
                path: path.clone(),
                name,
                ext_lower,
                is_dir,
                depth,
                expanded,
                empty_hint: false,
            });
            if expanded && let Ok(canon) = fs::canonicalize(&path)
                && seen_dirs.insert(canon)
            {
                self.append_flat_dir(&path, depth + 1, seen_dirs, out);
            }
        }
    }

    fn search_cache_matches(&self, root: &Path) -> bool {
        matches!(
            &self.search_cache,
            Some((cached_root, cached_hidden, _, _))
                if cached_root == root && *cached_hidden == self.show_hidden
        )
    }

    fn poll_background_scan(&mut self) {
        let Some(rx) = self.scan_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok((root, show_hidden, generation, entries, truncated)) => {
                self.scan_rx = None;
                self.scan_in_flight = None;
                if self.root_dir.as_deref() == Some(root.as_path())
                    && self.show_hidden == show_hidden
                    && self.scan_gen == generation
                {
                    self.search_cache = Some((root, show_hidden, entries, truncated));
                    self.filtered_cache = None;
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.scan_rx = None;
                self.scan_in_flight = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Ensures `search_cache` is populated without blocking the UI thread past a tiny budget.
    /// Small/local folders finish immediately; large or slow folders continue on a background thread.
    fn ensure_search_cache_nonblocking(&mut self, ctx: &egui::Context) -> bool {
        self.poll_background_scan();
        let Some(root) = self.root_dir.clone() else {
            return false;
        };
        if self.search_cache_matches(&root) {
            return true;
        }
        if let Some((in_root, in_hidden, in_gen)) = &self.scan_in_flight
            && in_root == &root && *in_hidden == self.show_hidden && *in_gen == self.scan_gen {
                return false;
            }
        match scan_tree_with_budget(
            &root,
            self.show_hidden,
            FAST_SYNC_SCAN_ENTRIES,
            FAST_SYNC_SCAN_BUDGET,
        ) {
            ScanProgress::Complete { entries, truncated } => {
                self.search_cache = Some((root, self.show_hidden, entries, truncated));
                self.filtered_cache = None;
                true
            }
            ScanProgress::Partial {
                entries,
                stack,
                visited_canon,
            } => {
                self.spawn_background_scan(
                    root,
                    self.show_hidden,
                    entries,
                    stack,
                    visited_canon,
                    Some(ctx.clone()),
                );
                false
            }
        }
    }

    fn spawn_background_scan(
        &mut self,
        root: PathBuf,
        show_hidden: bool,
        partial_entries: Vec<ExplorerEntry>,
        remaining_stack: Vec<(PathBuf, usize)>,
        visited_canon: HashSet<PathBuf>,
        repaint_ctx: Option<egui::Context>,
    ) {
        let generation = self.scan_gen;
        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        self.scan_in_flight = Some((root.clone(), show_hidden, generation));
        let _ = thread::Builder::new()
            .name("md-explorer-scan".to_string())
            .spawn(move || {
                let (entries, truncated) = finish_scan_tree(
                    &root,
                    show_hidden,
                    partial_entries,
                    remaining_stack,
                    visited_canon,
                );
                let _ = tx.send((root, show_hidden, generation, entries, truncated));
                if let Some(c) = repaint_ctx {
                    c.request_repaint();
                }
            });
    }

    /// Recomputes cached filtered entry indices only when query/filter/scan generation changed.
    fn ensure_filtered_cache(&mut self) {
        let cache_hit = self.filtered_cache.as_ref().is_some_and(
            |(cached_q, cached_filter, cached_hidden, cached_gen, _)| {
                *cached_filter == self.file_filter
                    && *cached_hidden == self.show_hidden
                    && *cached_gen == self.scan_gen
                    && self.search_query.trim().eq_ignore_ascii_case(cached_q)
            },
        );
        if cache_hit {
            return;
        }
        let Some((_, _, all_entries, _)) = self.search_cache.as_ref() else {
            return;
        };
        let query = self.search_query.trim().to_ascii_lowercase();
        let filter = self.file_filter;
        let mut indices = Vec::new();
        for (idx, entry) in all_entries.iter().enumerate() {
            if entry.is_dir {
                // When filtering by file type, list matching files directly.
                if filter != ExplorerFileFilter::All || query.is_empty() {
                    continue;
                }
                if entry.name_lower.contains(&query) || entry.rel_path_lower.contains(&query) {
                    indices.push(idx);
                }
            } else {
                if !filter.matches_extension(&entry.ext_lower) {
                    continue;
                }
                if query.is_empty()
                    || entry.name_lower.contains(&query)
                    || entry.rel_path_lower.contains(&query)
                {
                    indices.push(idx);
                }
            }
        }
        self.filtered_cache = Some((query, filter, self.show_hidden, self.scan_gen, indices));
    }

    /// Returns bounded recursive entries matching `search_query` and `file_filter`.
    /// Only walks disk when cache is invalid (root changed, `show_hidden` toggled, or `refresh()`).
    pub fn visible_search_entries(&mut self) -> (Vec<ExplorerEntry>, bool) {
        let Some(root) = self.root_dir.clone() else {
            return (Vec::new(), false);
        };
        if !self.search_cache_matches(&root) {
            let (entries, truncated) = scan_tree_bounded(&root, self.show_hidden);
            self.search_cache = Some((root, self.show_hidden, entries, truncated));
            self.filtered_cache = None;
        }

        self.ensure_filtered_cache();
        let (_, _, all_entries, truncated) = self
            .search_cache
            .as_ref()
            .expect("search_cache populated above");
        let matches = match self.filtered_cache.as_ref() {
            Some((_, _, _, _, indices)) => {
                indices.iter().map(|&idx| all_entries[idx].clone()).collect()
            }
            None => Vec::new(),
        };

        (matches, *truncated)
    }
}

fn should_skip_entry_name(name: &str, is_dir: bool, show_hidden: bool) -> bool {
    if show_hidden {
        return false;
    }
    if name.starts_with('.') {
        return true;
    }
    is_dir && matches!(name, "target" | "node_modules" | "__pycache__")
}

fn make_rel_path(path: &Path, root: Option<&Path>) -> String {
    if let Some(r) = root
        && let Ok(rel) = path.strip_prefix(r) {
            return rel.to_string_lossy().replace('\\', "/");
        }
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn read_dir_sorted(dir: &Path, root: Option<&Path>, show_hidden: bool) -> Vec<ExplorerEntry> {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or_else(|_| path.is_dir());
        if should_skip_entry_name(&name, is_dir, show_hidden) {
            continue;
        }
        let ext_lower = if is_dir {
            String::new()
        } else {
            path.extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default()
        };
        let rel_path = make_rel_path(&path, root);
        let name_lower = name.to_ascii_lowercase();
        let rel_path_lower = rel_path.to_ascii_lowercase();
        out.push(ExplorerEntry {
            path,
            name,
            rel_path,
            name_lower,
            rel_path_lower,
            is_dir,
            ext_lower,
        });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name_lower.cmp(&b.name_lower))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

enum ScanProgress {
    Complete {
        entries: Vec<ExplorerEntry>,
        truncated: bool,
    },
    Partial {
        entries: Vec<ExplorerEntry>,
        stack: Vec<(PathBuf, usize)>,
        visited_canon: HashSet<PathBuf>,
    },
}

fn scan_tree_with_budget(
    root: &Path,
    show_hidden: bool,
    max_sync_entries: usize,
    max_sync_duration: Duration,
) -> ScanProgress {
    let start = Instant::now();
    let mut results = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut visited_canon: HashSet<PathBuf> = HashSet::new();
    let mut truncated = false;

    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_SCAN_DEPTH {
            continue;
        }
        if let Ok(canon) = fs::canonicalize(&dir)
            && !visited_canon.insert(canon)
        {
            continue;
        }
        let children = read_dir_sorted(&dir, Some(root), show_hidden);
        // Push subdirs in reverse so alphabetical order is preserved in DFS
        let mut subdirs = Vec::new();
        for item in children {
            let is_dir = item.is_dir;
            let p = item.path.clone();
            results.push(item);
            if results.len() >= MAX_SCAN_ENTRIES {
                truncated = true;
                break;
            }
            if is_dir && depth < MAX_SCAN_DEPTH {
                subdirs.push((p, depth + 1));
            }
        }
        if truncated {
            break;
        }
        for sub in subdirs.into_iter().rev() {
            stack.push(sub);
        }
        if !stack.is_empty()
            && (results.len() >= max_sync_entries || start.elapsed() >= max_sync_duration)
        {
            return ScanProgress::Partial {
                entries: results,
                stack,
                visited_canon,
            };
        }
    }

    ScanProgress::Complete {
        entries: results,
        truncated,
    }
}

fn finish_scan_tree(
    root: &Path,
    show_hidden: bool,
    mut results: Vec<ExplorerEntry>,
    mut stack: Vec<(PathBuf, usize)>,
    mut visited_canon: HashSet<PathBuf>,
) -> (Vec<ExplorerEntry>, bool) {
    let mut truncated = results.len() >= MAX_SCAN_ENTRIES;

    while !truncated {
        let Some((dir, depth)) = stack.pop() else {
            break;
        };
        if depth > MAX_SCAN_DEPTH {
            continue;
        }
        if let Ok(canon) = fs::canonicalize(&dir)
            && !visited_canon.insert(canon)
        {
            continue;
        }
        let children = read_dir_sorted(&dir, Some(root), show_hidden);
        let mut subdirs = Vec::new();
        for item in children {
            let is_dir = item.is_dir;
            let p = item.path.clone();
            results.push(item);
            if results.len() >= MAX_SCAN_ENTRIES {
                truncated = true;
                break;
            }
            if is_dir && depth < MAX_SCAN_DEPTH {
                subdirs.push((p, depth + 1));
            }
        }
        if truncated {
            break;
        }
        for sub in subdirs.into_iter().rev() {
            stack.push(sub);
        }
    }

    (results, truncated)
}

fn scan_tree_bounded(root: &Path, show_hidden: bool) -> (Vec<ExplorerEntry>, bool) {
    finish_scan_tree(
        root,
        show_hidden,
        Vec::new(),
        vec![(root.to_path_buf(), 0)],
        HashSet::new(),
    )
}

fn file_badge_label(ext_lower: &str, theme: &WordTheme) -> (&'static str, Color32) {
    match ext_lower {
        "md" | "markdown" | "mdown" | "mkd" | "mdx" => ("MD", theme.accent),
        "txt" | "log" | "rst" | "org" | "tex" | "adoc" => ("TXT", theme.text_muted),
        "json" => ("JSON", theme.warning_amber),
        "html" | "htm" => ("HTML", theme.warning_amber),
        "rs" => ("RS", theme.accent),
        "py" => ("PY", theme.accent),
        "toml" | "yaml" | "yml" => ("CFG", theme.text_muted),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" => ("IMG", theme.success_green),
        "pdf" => ("PDF", theme.warning_amber),
        "docx" => ("DOC", theme.accent),
        _ => ("FILE", theme.text_muted),
    }
}

fn paths_equal_ci(a: &Path, b: &Path) -> bool {
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

/// Renders the VS Code-style File Explorer inside the left sidebar.
pub fn render_explorer_panel(
    ui: &mut egui::Ui,
    state: &mut FileExplorerState,
    active_file: Option<&Path>,
    theme: &WordTheme,
) -> ExplorerAction {
    let mut action = ExplorerAction::None;

    // Folder header bar: current root + Open Folder + Up + Refresh + Hidden toggle
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(4.0, 2.0);
        let folder_label = state
            .root_dir
            .as_ref()
            .and_then(|p| p.file_name().or_else(|| p.as_os_str().into()))
            .map(|n| n.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                state
                    .root_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "No Folder".to_string())
            });
        let tip = state
            .root_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "Open a folder to browse files".to_string());
        ui.label(
            RichText::new(folder_label)
                .strong()
                .size(12.0)
                .color(theme.text_primary),
        )
        .on_hover_text(tip);

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 0.0);
            let folder_btn = ui
                .small_button("Folder")
                .on_hover_text("Open Folder in Explorer");
            if folder_btn.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if folder_btn.clicked() {
                action = ExplorerAction::OpenFolderDialog;
            }

            let refresh_btn = ui
                .small_button("↻")
                .on_hover_text("Refresh File Explorer");
            if refresh_btn.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if refresh_btn.clicked() {
                state.refresh();
            }

            let can_up = state
                .root_dir
                .as_ref()
                .and_then(|p| p.parent())
                .is_some();
            let up_btn = ui
                .add_enabled(can_up, egui::Button::new("↑").small())
                .on_hover_text("Go to Parent Folder");
            if up_btn.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if up_btn.clicked()
                && let Some(parent) = state.root_dir.as_ref().and_then(|p| p.parent()) {
                    action = ExplorerAction::SetRoot(parent.to_path_buf());
                }
        });
    });

    ui.add_space(4.0);

    // Search box
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
        let te_w = (ui.available_width() - 26.0).max(80.0);
        ui.add_sized(
            [te_w, 22.0],
            egui::TextEdit::singleline(&mut state.search_query)
                .hint_text("Search files...")
                .desired_width(te_w),
        );
        let clear_enabled = !state.search_query.is_empty();
        let clr = ui
            .add_enabled(clear_enabled, egui::Button::new("×").small())
            .on_hover_text("Clear search");
        if clr.clicked() {
            state.search_query.clear();
        }
    });

    ui.add_space(4.0);

    // File type filter pills + Hidden toggle
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
        for filter in [
            ExplorerFileFilter::All,
            ExplorerFileFilter::Markdown,
            ExplorerFileFilter::Text,
            ExplorerFileFilter::CodeAndData,
        ] {
            let active = state.file_filter == filter;
            let btn = egui::Button::new(
                RichText::new(filter.label())
                    .size(11.0)
                    .strong()
                    .color(if active {
                        theme.accent
                    } else {
                        theme.text_muted
                    }),
            )
            .fill(if active {
                theme.accent_soft
            } else {
                theme.pill_bg
            })
            .stroke(Stroke::new(
                1.0_f32,
                if active {
                    theme.accent
                } else {
                    theme.border_subtle
                },
            ))
            .corner_radius(CornerRadius::same(8));
            let resp = ui.add(btn);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                state.file_filter = filter;
            }
        }

        let hid_btn = egui::Button::new(
            RichText::new(".*")
                .size(11.0)
                .color(if state.show_hidden {
                    theme.accent
                } else {
                    theme.text_muted
                }),
        )
        .fill(if state.show_hidden {
            theme.accent_soft
        } else {
            theme.pill_bg
        })
        .stroke(Stroke::new(1.0_f32, theme.border_subtle))
        .corner_radius(CornerRadius::same(8));
        let hid_resp = ui.add(hid_btn).on_hover_text("Show Hidden & Build Folders");
        if hid_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if hid_resp.clicked() {
            state.show_hidden = !state.show_hidden;
            state.refresh();
        }
    });

    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);

    let Some(root_dir) = state.root_dir.clone() else {
        ui.label(
            RichText::new("Open a folder or Markdown file to browse files here.")
                .size(12.0)
                .italics()
                .color(theme.text_muted),
        );
        ui.add_space(6.0);
        let open_btn = egui::Button::new(RichText::new("Open Folder...").strong().size(12.0))
            .fill(theme.accent_soft)
            .stroke(Stroke::new(1.0_f32, theme.accent));
        if ui.add(open_btn).clicked() {
            action = ExplorerAction::OpenFolderDialog;
        }
        return action;
    };

    let filtering = !state.search_query.trim().is_empty()
        || state.file_filter != ExplorerFileFilter::All;

    if filtering {
        let mut ready = false;
        let mut is_empty = false;
        let mut match_count = 0usize;
        let mut truncated = false;
        if state.ensure_search_cache_nonblocking(ui.ctx()) {
            state.ensure_filtered_cache();
            ready = true;
            truncated = state
                .search_cache
                .as_ref()
                .map(|(_, _, _, t)| *t)
                .unwrap_or(false);
            match_count = state
                .filtered_cache
                .as_ref()
                .map(|(_, _, _, _, idxs)| idxs.len())
                .unwrap_or(0);
            is_empty = match_count == 0;
        }
        let mut clicked_dir: Option<PathBuf> = None;
        if !ready {
            egui::ScrollArea::vertical()
                .id_salt("file_explorer_tree_scroll")
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Scanning folder…")
                            .size(12.0)
                            .italics()
                            .color(theme.text_muted),
                    );
                });
        } else if is_empty {
            egui::ScrollArea::vertical()
                .id_salt("file_explorer_tree_scroll")
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No matching files found.")
                            .size(12.0)
                            .italics()
                            .color(theme.text_muted),
                    );
                });
        } else {
            egui::ScrollArea::vertical()
                .id_salt("file_explorer_tree_scroll")
                .show_rows(ui, 26.0, match_count, |ui, row_range| {
                    let all_entries = match state.search_cache.as_ref() {
                        Some((_, _, entries, _)) => entries.as_slice(),
                        None => &[][..],
                    };
                    let indices = match state.filtered_cache.as_ref() {
                        Some((_, _, _, _, idxs)) => idxs.as_slice(),
                        None => &[][..],
                    };
                    for &idx in &indices[row_range] {
                        let Some(entry) = all_entries.get(idx) else {
                            continue;
                        };
                        if entry.is_dir {
                            let row_resp = render_explorer_row(
                                ui,
                                0,
                                "▸",
                                &entry.name,
                                Some(&entry.rel_path),
                                None,
                                false,
                                theme,
                            );
                            if row_resp.clicked() {
                                clicked_dir = Some(entry.path.clone());
                            } else if let Some(pos) = secondary_click_pos_for(ui, &row_resp) {
                                action = ExplorerAction::ContextMenu {
                                    path: entry.path.clone(),
                                    is_dir: true,
                                    is_root_empty_area: false,
                                    pos,
                                };
                            }
                        } else {
                            let is_active =
                                active_file.is_some_and(|af| paths_equal_ci(af, &entry.path));
                            let badge = file_badge_label(&entry.ext_lower, theme);
                            let hint = if entry.rel_path != entry.name {
                                Some(entry.rel_path.as_str())
                            } else {
                                None
                            };
                            let row_resp = render_explorer_row(
                                ui,
                                0,
                                "",
                                &entry.name,
                                hint,
                                Some(badge),
                                is_active,
                                theme,
                            )
                            .on_hover_text(entry.path.display().to_string());
                            if row_resp.clicked() {
                                action = ExplorerAction::OpenFile(entry.path.clone());
                            } else if let Some(pos) = secondary_click_pos_for(ui, &row_resp) {
                                action = ExplorerAction::ContextMenu {
                                    path: entry.path.clone(),
                                    is_dir: false,
                                    is_root_empty_area: false,
                                    pos,
                                };
                            }
                        }
                    }
                });
            if truncated {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("(Showing first {MAX_SCAN_ENTRIES} scanned entries)"))
                        .size(11.0)
                        .weak(),
                );
            }
        }
        if let Some(dir_path) = clicked_dir {
            state.search_query.clear();
            state.file_filter = ExplorerFileFilter::All;
            state.expanded_dirs.insert(dir_path);
            state.tree_rev = state.tree_rev.wrapping_add(1);
        }
    } else {
        state.ensure_flat_tree(&root_dir);
        let row_count = state
            .flat_tree_cache
            .as_ref()
            .map(|(_, _, _, r)| r.len())
            .unwrap_or(0);
        let mut toggle_dir: Option<PathBuf> = None;
        let tree_scroll_resp = egui::ScrollArea::vertical()
            .id_salt("file_explorer_tree_scroll")
            .show_rows(ui, 22.0, row_count, |ui, row_range| {
                let rows = match state.flat_tree_cache.as_ref() {
                    Some((_, _, _, r)) => r.as_slice(),
                    None => &[][..],
                };
                for row in &rows[row_range] {
                    if row.empty_hint {
                        ui.label(
                            RichText::new(&row.name)
                                .size(12.0)
                                .italics()
                                .color(theme.text_muted),
                        );
                    } else if row.is_dir {
                        let arrow = if row.expanded { "▼" } else { "▶" };
                        let resp = render_explorer_row(
                            ui,
                            row.depth,
                            arrow,
                            &row.name,
                            None,
                            None,
                            false,
                            theme,
                        )
                        .on_hover_text(row.path.display().to_string());
                        if resp.clicked() {
                            toggle_dir = Some(row.path.clone());
                        } else if let Some(pos) = secondary_click_pos_for(ui, &resp) {
                            action = ExplorerAction::ContextMenu {
                                path: row.path.clone(),
                                is_dir: true,
                                is_root_empty_area: false,
                                pos,
                            };
                        }
                    } else {
                        let is_active =
                            active_file.is_some_and(|af| paths_equal_ci(af, &row.path));
                        let badge = file_badge_label(&row.ext_lower, theme);
                        let resp = render_explorer_row(
                            ui,
                            row.depth,
                            "",
                            &row.name,
                            None,
                            Some(badge),
                            is_active,
                            theme,
                        )
                        .on_hover_text(row.path.display().to_string());
                        if resp.clicked() {
                            action = ExplorerAction::OpenFile(row.path.clone());
                        } else if let Some(pos) = secondary_click_pos_for(ui, &resp) {
                            action = ExplorerAction::ContextMenu {
                                path: row.path.clone(),
                                is_dir: false,
                                is_root_empty_area: false,
                                pos,
                            };
                        }
                    }
                }
            });
        if action == ExplorerAction::None
            && ui.input(|i| i.pointer.secondary_clicked())
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()))
            && tree_scroll_resp.inner_rect.contains(pos)
        {
            action = ExplorerAction::ContextMenu {
                path: root_dir,
                is_dir: true,
                is_root_empty_area: true,
                pos,
            };
        }
        if let Some(dir_path) = toggle_dir {
            state.toggle_dir(&dir_path);
        }
    }

    action
}

fn secondary_click_pos_for(ui: &egui::Ui, resp: &egui::Response) -> Option<egui::Pos2> {
    if resp.secondary_clicked() {
        return Some(
            resp.interact_pointer_pos()
                .or_else(|| ui.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos())))
                .unwrap_or_else(|| resp.rect.center()),
        );
    }
    if ui.input(|i| i.pointer.secondary_clicked())
        && let Some(pos) = ui.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()))
        && resp.rect.contains(pos)
    {
        return Some(pos);
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn render_explorer_row(
    ui: &mut egui::Ui,
    depth: usize,
    prefix: &str,
    name: &str,
    sub_hint: Option<&str>,
    badge: Option<(&'static str, Color32)>,
    is_active: bool,
    theme: &WordTheme,
) -> egui::Response {
    let row_h = if sub_hint.is_some() { 30.0 } else { 22.0 };
    let avail_w = ui.available_width().max(60.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, row_h), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let bg = if is_active {
        theme.accent_soft
    } else if resp.hovered() {
        theme.pill_bg
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(5), bg);
    }
    if is_active {
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 1.0, rect.top() + 3.0),
            egui::pos2(rect.left() + 3.5, rect.bottom() - 3.0),
        );
        ui.painter().rect_filled(bar, 1.5, theme.accent);
    }

    let indent = depth as f32 * 12.0 + 6.0;
    let mut x = rect.left() + indent;

    if !prefix.is_empty() {
        let pre_galley = ui.painter().layout_no_wrap(
            prefix.to_string(),
            egui::FontId::proportional(9.5),
            theme.text_muted,
        );
        let py = rect.center().y - pre_galley.size().y * 0.5;
        ui.painter().galley(egui::pos2(x, py), pre_galley, theme.text_muted);
        x += 13.0;
    } else if let Some((badge_txt, badge_col)) = badge {
        let badge_galley = ui.painter().layout_no_wrap(
            badge_txt.to_string(),
            egui::FontId::monospace(9.5),
            badge_col,
        );
        let bw = badge_galley.size().x + 6.0;
        let bh = 14.0;
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(x, rect.center().y - bh * 0.5),
            Vec2::new(bw, bh),
        );
        ui.painter().rect_filled(
            badge_rect,
            CornerRadius::same(3),
            Color32::from_rgba_unmultiplied(badge_col.r(), badge_col.g(), badge_col.b(), 26),
        );
        ui.painter().galley(
            egui::pos2(x + 3.0, rect.center().y - badge_galley.size().y * 0.5),
            badge_galley,
            badge_col,
        );
        x += bw + 5.0;
    } else {
        x += 4.0;
    }

    let max_text_w = (rect.right() - x - 4.0).max(20.0);
    let name_color = if is_active {
        theme.accent
    } else {
        theme.text_primary
    };
    let name_galley = ui.painter().layout_no_wrap(
        name.to_string(),
        egui::FontId::proportional(12.0),
        name_color,
    );
    let ny = if sub_hint.is_some() {
        rect.top() + 3.0
    } else {
        rect.center().y - name_galley.size().y * 0.5
    };
    ui.painter()
        .with_clip_rect(rect.shrink2(Vec2::new(2.0, 0.0)))
        .galley(egui::pos2(x, ny), name_galley, name_color);

    if let Some(hint) = sub_hint {
        let hint_galley = ui.painter().layout_no_wrap(
            hint.to_string(),
            egui::FontId::proportional(10.0),
            theme.text_muted,
        );
        ui.painter()
            .with_clip_rect(rect.shrink2(Vec2::new(2.0, 0.0)))
            .galley(
                egui::pos2(x, rect.top() + 16.5),
                hint_galley,
                theme.text_muted,
            );
    }

    let _ = (Margin::ZERO, max_text_w);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn explorer_scans_sorts_searches_and_filters_files() {
        let dir = tempdir().unwrap();
        let docs = dir.path().join("docs");
        let target_dir = dir.path().join("target");
        let hidden_dir = dir.path().join(".git");
        fs::create_dir_all(&docs).unwrap();
        fs::create_dir_all(&target_dir).unwrap();
        fs::create_dir_all(&hidden_dir).unwrap();

        fs::write(dir.path().join("README.md"), "# Root").unwrap();
        fs::write(dir.path().join("notes.txt"), "plain").unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        fs::write(docs.join("guide.markdown"), "# Guide").unwrap();
        fs::write(target_dir.join("ignored.md"), "skip").unwrap();
        fs::write(hidden_dir.join("HEAD.txt"), "skip").unwrap();

        let mut st = FileExplorerState::default();
        st.set_root(dir.path().to_path_buf());

        // Direct children: directory `docs` sorts before files; `.git` and `target` skipped
        let root_names: Vec<String> = st
            .directory_children(dir.path())
            .iter()
            .map(|e| e.name.clone())
            .collect();
        assert_eq!(
            root_names,
            vec!["docs", "config.json", "notes.txt", "README.md"]
        );

        // Filter Markdown across tree
        st.file_filter = ExplorerFileFilter::Markdown;
        let (md_hits, truncated) = st.visible_search_entries();
        assert!(!truncated);
        let md_names: Vec<String> = md_hits.iter().map(|e| e.rel_path.clone()).collect();
        assert_eq!(md_names, vec!["README.md", "docs/guide.markdown"]);

        // Filter CodeAndData + search query
        st.file_filter = ExplorerFileFilter::CodeAndData;
        st.search_query = "conf".to_string();
        let (code_hits, _) = st.visible_search_entries();
        assert_eq!(code_hits.len(), 1);
        assert_eq!(code_hits[0].name, "config.json");

        // Filter Text
        st.file_filter = ExplorerFileFilter::Text;
        st.search_query.clear();
        let (txt_hits, _) = st.visible_search_entries();
        assert_eq!(txt_hits.len(), 1);
        assert_eq!(txt_hits[0].name, "notes.txt");

        // Enabling show_hidden exposes target/ignored.md and .git/HEAD.txt
        st.show_hidden = true;
        st.file_filter = ExplorerFileFilter::Markdown;
        st.refresh();
        let (all_md, _) = st.visible_search_entries();
        assert!(all_md.iter().any(|e| e.name == "ignored.md"));
    }

    #[test]
    fn explorer_reuses_filtered_cache_and_handles_background_scan() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("notes");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.path().join("alpha.md"), "# Alpha").unwrap();
        fs::write(sub.join("beta.md"), "# Beta").unwrap();
        fs::write(sub.join("gamma.txt"), "Gamma").unwrap();

        let mut st = FileExplorerState::default();
        st.set_root(dir.path().to_path_buf());
        st.file_filter = ExplorerFileFilter::Markdown;
        st.search_query = "beta".to_string();

        let (hits1, _) = st.visible_search_entries();
        assert_eq!(hits1.len(), 1);
        assert_eq!(hits1[0].name, "beta.md");
        let cached_ptr = st
            .filtered_cache
            .as_ref()
            .map(|c| c.4.as_ptr())
            .expect("filtered_cache populated");

        // Case/whitespace-equivalent query reuses the exact same cached Vec<usize> allocation
        st.search_query = " BETA ".to_string();
        st.ensure_filtered_cache();
        let reused_ptr = st.filtered_cache.as_ref().unwrap().4.as_ptr();
        assert_eq!(cached_ptr, reused_ptr);

        // Changing query or filter invalidates and recomputes filtered_cache
        st.search_query.clear();
        st.file_filter = ExplorerFileFilter::Text;
        let (txt_hits, _) = st.visible_search_entries();
        assert_eq!(txt_hits.len(), 1);
        assert_eq!(txt_hits[0].name, "gamma.txt");

        // Background scan path completes cleanly when budget triggers Partial
        st.refresh();
        st.spawn_background_scan(
            dir.path().to_path_buf(),
            false,
            Vec::new(),
            vec![(dir.path().to_path_buf(), 0)],
            std::collections::HashSet::new(),
            None,
        );
        for _ in 0..100 {
            st.poll_background_scan();
            if st.search_cache.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(st.search_cache.is_some());
        assert!(st.scan_in_flight.is_none());
    }
}
