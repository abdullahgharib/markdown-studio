pub mod autoscroll;
pub mod dialogs;
pub mod editor;
pub mod explorer;
pub mod instance;
pub mod os_integration;
pub mod reader;
pub mod session;
pub mod settings;
pub mod tabs;
pub mod theme;
pub mod toolbar;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{CornerRadius, FontData, FontDefinitions, FontFamily, Key, Margin, RichText, Sense, Stroke, Vec2};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::document::ast::{build_table_markdown, extract_tables, is_math_code_info, parse_to_ir};
use crate::document::calc;
use crate::document::editing::{
    apply_table_edit, indent_lines, insert_code_block, insert_horizontal_rule, insert_image,
    insert_link, insert_math_block, insert_math_template_block, insert_table, outdent_lines,
    parse_code_fence_line, query_active_format, set_block_alignment, set_heading, table_at_line,
    toggle_bold, toggle_inline_code, toggle_inline_math, toggle_italic, toggle_line_prefix,
    toggle_strikethrough, toggle_underline, ActiveFormatState,
};
use crate::document::unicode::{detect_paragraph_direction, shape_bidi_for_display, TextDirection};
use crate::document::viewport::{BackgroundAstParser, ViewportBlockIndex};
use crate::document::{DiskFileStamp, DocumentBuffer, UndoRedoOutcome};
use crate::export::ir::{runs_to_plain_text, DocBlock};
use crate::export::{BackgroundExporter, ExportTarget};
use crate::ui::autoscroll::{AutoScrollState, AutoScrollTarget};
use crate::ui::dialogs::{
    render_status_bar, ExternalDiffChoice, ExternalDiffModal, FindReplaceBar, InsertLinkModal,
    PendingDiscardAction, PrintDialogModal, StatusBarGoalState, TableEditorModal,
    UnsavedChangesModal, UnsavedPromptChoice,
};
use crate::ui::editor::{render_virtual_editor, EditorAction, VirtualEditorState};
use crate::ui::explorer::{render_explorer_panel, ExplorerAction, FileExplorerState};
use crate::ui::os_integration::OsIntegrationModal;
use crate::ui::reader::{
    activate_wysiwyg_at_line, activate_wysiwyg_code_block_range,
    activate_wysiwyg_preserving_selection, classify_link_url, heading_slug, render_virtual_reader,
    LinkTarget, ReaderAction, VirtualReaderState,
};
use crate::ui::settings::AppSettings;
use crate::ui::tabs::{
    render_tab_bar, InactiveTabState, OutlineItem, TabBarAction, TabBarState, TabSlot,
    TabVisualInfo,
};
use crate::ui::theme::{ThemeMode, WordTheme};
use crate::ui::toolbar::{
    render_toolbar, vivid_button, CalcToolbarInfo, ReadingToolbarInfo, ToolbarAction,
    ToolbarButtonKind, ToolbarState, ViewMode,
};

#[cfg(target_os = "windows")]
fn open_with_os(target: &str) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut core::ffi::c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> isize;
    }

    let op: Vec<u16> = OsStr::new("open").encode_wide().chain(once(0)).collect();
    let file: Vec<u16> = OsStr::new(target).encode_wide().chain(once(0)).collect();
    // SAFETY: `op` and `file` are NUL-terminated UTF-16 slices live across the synchronous call.
    let ret = unsafe {
        ShellExecuteW(
            null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            null(),
            null(),
            1,
        )
    };
    if ret > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW failed with code {ret}"))
    }
}

#[cfg(not(target_os = "windows"))]
fn open_with_os(target: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(target_os = "macos"))]
    let cmd = "xdg-open";
    std::process::Command::new(cmd)
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarTab {
    Explorer,
    #[default]
    Outline,
}

pub struct MarkdownApp {
    pub buffer: DocumentBuffer,
    pub doc_id: u64,
    pub tabs: Vec<TabSlot>,
    pub active_tab: usize,
    next_doc_id: u64,
    next_untitled_num: usize,
    pub view_mode: ViewMode,
    pub show_markup: bool,
    pub theme: WordTheme,
    pub settings: AppSettings,
    pub settings_path: PathBuf,
    pub ast_parser: BackgroundAstParser,
    pub block_index: ViewportBlockIndex,
    pub last_parsed_version: u64,
    last_parsed_doc_id: u64,
    last_parsed_buf_id: u64,
    last_requested_parse: (u64, u64, i32),
    pub exporter: BackgroundExporter,
    pub toolbar_state: ToolbarState,
    pub editor_state: VirtualEditorState,
    pub reader_state: VirtualReaderState,
    pub explorer_state: FileExplorerState,
    pub sidebar_tab: SidebarTab,
    pub table_modal: TableEditorModal,
    pub insert_link_modal: InsertLinkModal,
    pub find_bar: FindReplaceBar,
    pub unsaved_modal: UnsavedChangesModal,
    pub print_modal: PrintDialogModal,
    pub os_modal: OsIntegrationModal,
    pub external_diff_modal: ExternalDiffModal,
    pub goal_state: StatusBarGoalState,
    pub allow_close_now: bool,
    pub outline_open: bool,
    pub focus_mode: bool,
    pub split_ratio: f32,
    pub last_split_top_row: usize,
    pub autoscroll: AutoScrollState,
    pub last_opened_link: Option<String>,
    pub open_external_links_with_os: bool,
    pending_anchor_after_open: Option<String>,
    outline_cache_key: (u64, u64),
    outline_items: Vec<OutlineItem>,
    pub outline_filter: String,
    pub collapsed_outline_lines: FxHashSet<usize>,
    word_count_cache: (u64, u64, usize),
    active_format_cache: ((u64, u64, usize, Option<usize>), ActiveFormatState),
    pending_zoom_save_at: Option<f64>,
    loaded_font_groups: u8,
    pending_font_groups: u8,
    font_scan_version: (u64, u64, u64),
    font_scanned_buf_key: (u64, u64),
    font_rxs: Vec<mpsc::Receiver<u8>>,
    os_sync_rx: Option<mpsc::Receiver<PathBuf>>,
    last_window_title_key: Option<(u64, bool, usize, Option<PathBuf>)>,
    last_applied_zoom: f32,
    instance_server: Option<instance::SingleInstanceServer>,
    instance_rx: Option<mpsc::Receiver<instance::InstanceMessage>>,
    was_window_focused: bool,
    last_published_window_bounds: Option<(egui::Rect, bool)>,
    cached_peer_windows: Option<Vec<instance::PeerWindowInfo>>,
    pub test_viewport_origin: Option<egui::Pos2>,
    tab_bar_state: TabBarState,
    spawn_new_window_with_os: bool,
    pub last_detached_window_args: Vec<String>,
    pub disk_stamp: Option<DiskFileStamp>,
    pub external_change_detected: bool,
    last_disk_check: Instant,
    last_recovery_save: Instant,
    last_recovery_snapshot_key: Vec<(u64, u64, bool)>,
    session_saver: session::BackgroundSessionSaver,
}

impl MarkdownApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_file: Option<PathBuf>) -> Self {
        let mut launch = instance::LaunchArgs::default();
        if let Some(path) = initial_file {
            launch.files.push(path);
        }
        Self::new_with_launch(cc, launch)
    }

    /// Launches a native desktop window from parsed CLI args, restoring any detached tab
    /// handoff, opening initial files as tabs, and starting single-instance IPC.
    pub fn new_with_launch(
        cc: &eframe::CreationContext<'_>,
        launch: instance::LaunchArgs,
    ) -> Self {
        let settings_path = AppSettings::default_path();
        if let Some(legacy_path) = AppSettings::legacy_default_path() {
            AppSettings::migrate_legacy_settings_if_needed(&settings_path, &legacy_path);
        }

        let handoff = launch
            .handoff_file
            .as_deref()
            .and_then(|p| instance::read_and_remove_handoff_file(p).ok());

        let (initial_buffer, extra_files): (DocumentBuffer, &[PathBuf]) =
            if let Some(h) = handoff {
                (
                    DocumentBuffer::from_handoff(&h.content, h.file_path, h.dirty),
                    &launch.files,
                )
            } else if let Some(first) = launch.files.first() {
                (
                    DocumentBuffer::open_file(first).unwrap_or_else(|_| DocumentBuffer::new()),
                    &launch.files[1..],
                )
            } else {
                (DocumentBuffer::new(), &[])
            };

        let mut app = Self::new_with_buffer_and_settings_path(
            &cc.egui_ctx,
            initial_buffer,
            settings_path,
        );
        app.open_external_links_with_os = true;
        app.spawn_new_window_with_os = true;
        for extra in extra_files {
            app.open_specific_file(extra);
        }

        if launch.handoff_file.is_none() && launch.files.is_empty() && !launch.new_window {
            let _ = app.restore_session_from_disk();
        }

        if let Ok((server, rx)) =
            instance::SingleInstanceServer::start_default(Some(cc.egui_ctx.clone()))
        {
            app.instance_server = Some(server);
            app.instance_rx = Some(rx);
        }

        if app.settings.auto_sync_exe_path {
            let exe_path = app.settings.effective_exe_path();
            let (tx, rx) = mpsc::channel();
            let repaint_ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                if let Ok(true) = os_integration::sync_exe_path_if_registered(&exe_path) {
                    let _ = tx.send(exe_path);
                    repaint_ctx.request_repaint();
                }
            });
            app.os_sync_rx = Some(rx);
        }
        app
    }

    pub fn new_headless(ctx: &egui::Context, initial_file: Option<PathBuf>) -> Self {
        Self::new_with_settings_path(ctx, initial_file, AppSettings::default_path())
    }

    pub fn new_with_settings_path(
        ctx: &egui::Context,
        initial_file: Option<PathBuf>,
        settings_path: PathBuf,
    ) -> Self {
        let buffer = if let Some(path) = initial_file {
            DocumentBuffer::open_file(&path).unwrap_or_else(|_| DocumentBuffer::new())
        } else {
            DocumentBuffer::new()
        };
        Self::new_with_buffer_and_settings_path(ctx, buffer, settings_path)
    }

    fn new_with_buffer_and_settings_path(
        ctx: &egui::Context,
        mut buffer: DocumentBuffer,
        settings_path: PathBuf,
    ) -> Self {
        configure_fonts(ctx);
        // App manages zoom exclusively via settings.zoom_factor (Ctrl+=/-/+0, Ctrl+Scroll, Pinch).
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let settings = AppSettings::load_from(&settings_path);
        let theme = WordTheme::from_mode(settings.theme_mode);
        theme.apply_to_ctx(ctx);

        let initial_buf_id = buffer.buf_id();
        let initial_blocks = if buffer.is_empty() {
            Vec::new()
        } else if buffer.len_bytes() < 32_000 {
            parse_to_ir(&buffer.as_string())
        } else {
            Vec::new()
        };
        let initial_index_version = if buffer.len_bytes() < 32_000 {
            buffer.version
        } else {
            0
        };
        let doc_id = 1u64;
        let block_index = ViewportBlockIndex::from_blocks_with_doc(
            initial_blocks,
            720.0,
            initial_index_version,
            doc_id,
        );
        let ast_parser = BackgroundAstParser::with_repaint_ctx(ctx.clone());
        let last_requested_parse = if buffer.len_bytes() >= 32_000 {
            ast_parser.request_parse_for_doc(
                buffer.rope().clone(),
                720.0,
                buffer.version,
                doc_id,
            );
            (doc_id, buffer.version, 720)
        } else {
            (0, 0, 0)
        };
        let outline_items = build_outline_items(&block_index);
        let initial_word_count = buffer.word_count();

        let env_opt = |primary: &str, legacy: &str| {
            std::env::var(primary)
                .ok()
                .or_else(|| std::env::var(legacy).ok())
        };

        let mut view_mode = settings.view_mode;
        if let Some(v) = env_opt("MD_STUDIO_VIEW", "MD_WORD_VIEW") {
            view_mode = match v.trim().to_ascii_lowercase().as_str() {
                "wysiwyg" => ViewMode::Wysiwyg,
                "reader" => ViewMode::Reader,
                "source" => ViewMode::Source,
                "split" => ViewMode::Split,
                _ => view_mode,
            };
        }
        let mut show_markup = settings.show_markup;
        if let Some(m) = env_opt("MD_STUDIO_SHOW_MARKUP", "MD_WORD_SHOW_MARKUP") {
            show_markup = matches!(
                m.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            );
        }
        let mut reader_state = VirtualReaderState::with_base_dir(
            buffer
                .file_path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf),
        );
        reader_state.page_width = settings.page_width;
        reader_state.line_spacing = settings.line_spacing;
        reader_state.auto_calc_math = settings.auto_calc_math;
        reader_state.auto_calc_tables = settings.auto_calc_tables;
        if view_mode == ViewMode::Wysiwyg {
            if let Some(line_idx) = env_opt("MD_STUDIO_EDIT_LINE", "MD_WORD_EDIT_LINE")
                .and_then(|s| s.trim().parse::<usize>().ok())
            {
                activate_wysiwyg_at_line(
                    &mut buffer,
                    &block_index,
                    line_idx,
                    show_markup,
                    &mut reader_state,
                );
            }
        }

        let initial_untitled_num = if buffer.file_path.is_some() { 0 } else { 1 };
        let next_untitled_num = if initial_untitled_num == 1 { 2 } else { 1 };
        let mut explorer_state = FileExplorerState::default();
        if let Some(ref saved_root) = settings.explorer_root {
            if saved_root.is_dir() {
                explorer_state.set_root(saved_root.clone());
            }
        }
        if explorer_state.root_dir.is_none() {
            if let Some(parent) = buffer.file_path.as_deref().and_then(Path::parent) {
                if !parent.as_os_str().is_empty() {
                    explorer_state.set_root(parent.to_path_buf());
                }
            }
        }

        Self {
            doc_id,
            tabs: vec![TabSlot {
                id: doc_id,
                untitled_num: initial_untitled_num,
                state: None,
            }],
            active_tab: 0,
            next_doc_id: doc_id + 1,
            next_untitled_num,
            last_parsed_version: buffer.version,
            last_parsed_doc_id: doc_id,
            last_parsed_buf_id: initial_buf_id,
            last_requested_parse,
            outline_cache_key: (doc_id, block_index.version),
            outline_items,
            outline_filter: String::new(),
            collapsed_outline_lines: FxHashSet::default(),
            word_count_cache: (doc_id, buffer.version, initial_word_count),
            active_format_cache: ((0, 0, usize::MAX, None), ActiveFormatState::default()),
            pending_zoom_save_at: None,
            loaded_font_groups: 0,
            pending_font_groups: 0,
            font_scan_version: (0, 0, 0),
            font_scanned_buf_key: (0, 0),
            font_rxs: Vec::new(),
            os_sync_rx: None,
            last_window_title_key: None,
            last_applied_zoom: 1.0,
            instance_server: None,
            instance_rx: None,
            was_window_focused: false,
            last_published_window_bounds: None,
            cached_peer_windows: None,
            test_viewport_origin: None,
            tab_bar_state: TabBarState::default(),
            spawn_new_window_with_os: false,
            last_detached_window_args: Vec::new(),
            disk_stamp: buffer
                .file_path
                .as_deref()
                .and_then(DiskFileStamp::from_path),
            external_change_detected: false,
            last_disk_check: Instant::now(),
            last_recovery_save: Instant::now(),
            last_recovery_snapshot_key: Vec::new(),
            session_saver: session::BackgroundSessionSaver::default(),
            buffer,
            view_mode,
            show_markup,
            theme,
            outline_open: settings.outline_open,
            focus_mode: false,
            split_ratio: settings.split_ratio,
            last_split_top_row: 0,
            autoscroll: AutoScrollState::default(),
            last_opened_link: None,
            open_external_links_with_os: false,
            pending_anchor_after_open: None,
            settings,
            settings_path,
            ast_parser,
            block_index,
            exporter: BackgroundExporter::with_repaint_ctx(ctx.clone()),
            toolbar_state: ToolbarState::default(),
            editor_state: {
                let mut st = VirtualEditorState::default();
                st.request_focus = matches!(view_mode, ViewMode::Source | ViewMode::Split);
                st
            },
            reader_state,
            explorer_state,
            sidebar_tab: SidebarTab::Outline,
            table_modal: TableEditorModal::default(),
            insert_link_modal: InsertLinkModal::default(),
            find_bar: FindReplaceBar::default(),
            unsaved_modal: UnsavedChangesModal::default(),
            print_modal: PrintDialogModal::default(),
            os_modal: OsIntegrationModal::default(),
            external_diff_modal: ExternalDiffModal::default(),
            goal_state: StatusBarGoalState::default(),
            allow_close_now: false,
        }
    }

    fn persist_settings(&self) {
        let _ = self.settings.save_to(&self.settings_path);
    }

    fn current_base_dir(&self) -> Option<PathBuf> {
        self.buffer
            .file_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }

    fn format_image_insert_path(&self, path: &Path) -> String {
        if let Some(doc_dir) = self.buffer.file_path.as_deref().and_then(Path::parent) {
            if let Ok(rel) = path.strip_prefix(doc_dir) {
                return rel.to_string_lossy().replace('\\', "/");
            }
        }
        path.to_string_lossy().to_string()
    }

    fn ast_needs_sync(&self) -> bool {
        self.last_parsed_doc_id != self.doc_id
            || self.last_parsed_buf_id != self.buffer.buf_id()
            || self.last_parsed_version != self.buffer.version
            || self.block_index.doc_id != self.doc_id
    }

    fn sync_ast_if_needed(&mut self) {
        if self.ast_needs_sync() {
            self.last_parsed_doc_id = self.doc_id;
            self.last_parsed_buf_id = self.buffer.buf_id();
            self.last_parsed_version = self.buffer.version;
            if self.buffer.len_bytes() < 32_000 {
                let blocks = parse_to_ir(&self.buffer.as_string());
                self.block_index = ViewportBlockIndex::from_blocks_with_doc(
                    blocks,
                    720.0,
                    self.buffer.version,
                    self.doc_id,
                );
                self.last_requested_parse = (self.doc_id, self.buffer.version, 720);
                self.outline_cache_key = (0, 0);
            } else {
                let req_key = (self.doc_id, self.buffer.version, 720);
                if self.last_requested_parse != req_key {
                    self.last_requested_parse = req_key;
                    // Non-blocking background parse for large files using O(1) Rope clone.
                    self.ast_parser.request_parse_for_doc(
                        self.buffer.rope().clone(),
                        720.0,
                        self.buffer.version,
                        self.doc_id,
                    );
                }
            }
        }
        if let Some(new_index) = self.ast_parser.poll_latest() {
            if new_index.doc_id == self.doc_id {
                self.block_index = new_index;
                self.outline_cache_key = (0, 0);
            }
        }
        let outline_key = (self.doc_id, self.block_index.version);
        if self.outline_cache_key != outline_key {
            self.outline_items = build_outline_items(&self.block_index);
            self.outline_cache_key = outline_key;
        }
    }

    fn alloc_doc_id(&mut self) -> u64 {
        let id = self.next_doc_id.max(1);
        self.next_doc_id = id.wrapping_add(1).max(1);
        id
    }

    fn alloc_untitled_num(&mut self) -> usize {
        let n = self.next_untitled_num.max(1);
        self.next_untitled_num = n + 1;
        n
    }

    fn untitled_display_name(num: usize) -> String {
        if num <= 1 {
            "Untitled.md".to_string()
        } else {
            format!("Untitled-{num}.md")
        }
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_tab_index(&self) -> usize {
        self.active_tab
    }

    fn tab_is_dirty(&self, idx: usize) -> bool {
        if idx == self.active_tab {
            self.buffer.dirty
        } else {
            self.tabs
                .get(idx)
                .and_then(|t| t.state.as_ref())
                .is_some_and(|s| s.buffer.dirty)
        }
    }

    pub fn has_any_dirty_tab(&self) -> bool {
        (0..self.tabs.len()).any(|i| self.tab_is_dirty(i))
    }

    fn first_dirty_tab_index(&self) -> Option<usize> {
        (0..self.tabs.len()).find(|&i| self.tab_is_dirty(i))
    }

    fn tab_display_name(&self, idx: usize) -> String {
        let file_path = if idx == self.active_tab {
            self.buffer.file_path.as_ref()
        } else {
            self.tabs
                .get(idx)
                .and_then(|t| t.state.as_ref())
                .and_then(|s| s.buffer.file_path.as_ref())
        };
        if let Some(path) = file_path {
            if let Some(name) = path.file_name() {
                return name.to_string_lossy().to_string();
            }
        }
        let num = self.tabs.get(idx).map(|t| t.untitled_num).unwrap_or(1);
        Self::untitled_display_name(num)
    }

    fn doc_title(&self) -> String {
        self.buffer
            .file_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled Document".to_string())
    }

    fn doc_display_name(&self) -> String {
        self.tab_display_name(self.active_tab)
    }

    fn new_reader_state_for_buffer(&self, buf: &DocumentBuffer) -> VirtualReaderState {
        let mut rs = VirtualReaderState::with_base_dir(
            buf.file_path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf),
        );
        rs.page_width = self.settings.page_width;
        rs.line_spacing = self.settings.line_spacing;
        rs.auto_calc_math = self.settings.auto_calc_math;
        rs.auto_calc_tables = self.settings.auto_calc_tables;
        rs
    }

    fn swap_active_with_state(&mut self, incoming: InactiveTabState) -> InactiveTabState {
        self.autoscroll.cancel();
        let doc_id = std::mem::replace(&mut self.doc_id, incoming.doc_id);
        let buffer = std::mem::replace(&mut self.buffer, incoming.buffer);
        let block_index = std::mem::replace(&mut self.block_index, incoming.block_index);
        let last_parsed_version =
            std::mem::replace(&mut self.last_parsed_version, incoming.last_parsed_version);
        let last_parsed_doc_id =
            std::mem::replace(&mut self.last_parsed_doc_id, incoming.last_parsed_doc_id);
        let last_parsed_buf_id =
            std::mem::replace(&mut self.last_parsed_buf_id, incoming.last_parsed_buf_id);
        let editor_state = std::mem::replace(&mut self.editor_state, incoming.editor_state);
        let reader_state = std::mem::replace(&mut self.reader_state, incoming.reader_state);
        let last_split_top_row =
            std::mem::replace(&mut self.last_split_top_row, incoming.last_split_top_row);
        let outline_items = std::mem::replace(&mut self.outline_items, incoming.outline_items);
        let outline_cache_key =
            std::mem::replace(&mut self.outline_cache_key, incoming.outline_cache_key);
        let collapsed_outline_lines = std::mem::replace(
            &mut self.collapsed_outline_lines,
            incoming.collapsed_outline_lines,
        );
        let word_count_cache =
            std::mem::replace(&mut self.word_count_cache, incoming.word_count_cache);
        let active_format_cache =
            std::mem::replace(&mut self.active_format_cache, incoming.active_format_cache);
        let font_scan_version =
            std::mem::replace(&mut self.font_scan_version, incoming.font_scan_version);
        let font_scanned_buf_key =
            std::mem::replace(&mut self.font_scanned_buf_key, incoming.font_scanned_buf_key);
        let disk_stamp = std::mem::replace(&mut self.disk_stamp, incoming.disk_stamp);
        let external_change_detected = std::mem::replace(
            &mut self.external_change_detected,
            incoming.external_change_detected,
        );

        self.reader_state.page_width = self.settings.page_width;
        self.reader_state.line_spacing = self.settings.line_spacing;
        self.reader_state.auto_calc_math = self.settings.auto_calc_math;
        self.reader_state.auto_calc_tables = self.settings.auto_calc_tables;
        self.reader_state.base_dir = self.current_base_dir();
        self.last_requested_parse = (0, 0, 0);
        self.find_bar.last_buf_version = u64::MAX;

        InactiveTabState {
            doc_id,
            buffer,
            block_index,
            last_parsed_version,
            last_parsed_doc_id,
            last_parsed_buf_id,
            editor_state,
            reader_state,
            last_split_top_row,
            outline_items,
            outline_cache_key,
            collapsed_outline_lines,
            word_count_cache,
            active_format_cache,
            font_scan_version,
            font_scanned_buf_key,
            disk_stamp,
            external_change_detected,
        }
    }

    fn apply_incoming_tab_state(&mut self, incoming: InactiveTabState) {
        self.autoscroll.cancel();
        self.doc_id = incoming.doc_id;
        self.buffer = incoming.buffer;
        self.block_index = incoming.block_index;
        self.last_parsed_version = incoming.last_parsed_version;
        self.last_parsed_doc_id = incoming.last_parsed_doc_id;
        self.last_parsed_buf_id = incoming.last_parsed_buf_id;
        self.editor_state = incoming.editor_state;
        self.reader_state = incoming.reader_state;
        self.last_split_top_row = incoming.last_split_top_row;
        self.outline_items = incoming.outline_items;
        self.outline_cache_key = incoming.outline_cache_key;
        self.collapsed_outline_lines = incoming.collapsed_outline_lines;
        self.word_count_cache = incoming.word_count_cache;
        self.active_format_cache = incoming.active_format_cache;
        self.font_scan_version = incoming.font_scan_version;
        self.font_scanned_buf_key = incoming.font_scanned_buf_key;
        self.disk_stamp = incoming.disk_stamp;
        self.external_change_detected = incoming.external_change_detected;
        self.reader_state.page_width = self.settings.page_width;
        self.reader_state.line_spacing = self.settings.line_spacing;
        self.reader_state.auto_calc_math = self.settings.auto_calc_math;
        self.reader_state.auto_calc_tables = self.settings.auto_calc_tables;
        self.reader_state.base_dir = self.current_base_dir();
        self.last_requested_parse = (0, 0, 0);
        self.find_bar.last_buf_version = u64::MAX;
    }

    pub fn switch_to_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() || idx == self.active_tab {
            return;
        }
        let Some(incoming) = self.tabs[idx].state.take() else {
            return;
        };
        self.toolbar_state.renaming_file = false;
        self.reader_state.image_lightbox = None;
        let old_idx = self.active_tab;
        let outgoing = self.swap_active_with_state(*incoming);
        self.tabs[old_idx].state = Some(Box::new(outgoing));
        self.active_tab = idx;
        self.check_external_file_changes();
        self.sync_ast_if_needed();
        self.refocus_active_editor();
    }

    fn open_buffer_in_new_tab(&mut self, new_buf: DocumentBuffer, untitled_num: usize) {
        let new_id = self.alloc_doc_id();
        let word_cnt = new_buf.word_count();
        let new_stamp = new_buf
            .file_path
            .as_deref()
            .and_then(DiskFileStamp::from_path);
        let mut ed_state = VirtualEditorState::default();
        ed_state.request_focus = matches!(self.view_mode, ViewMode::Source | ViewMode::Split);
        let rd_state = self.new_reader_state_for_buffer(&new_buf);
        let fresh = InactiveTabState {
            doc_id: new_id,
            buffer: new_buf,
            block_index: ViewportBlockIndex::empty_for_doc(new_id),
            last_parsed_version: 0,
            last_parsed_doc_id: 0,
            last_parsed_buf_id: 0,
            editor_state: ed_state,
            reader_state: rd_state,
            last_split_top_row: 0,
            outline_items: Vec::new(),
            outline_cache_key: (0, 0),
            collapsed_outline_lines: FxHashSet::default(),
            word_count_cache: (new_id, 0, word_cnt),
            active_format_cache: ((0, 0, usize::MAX, None), ActiveFormatState::default()),
            font_scan_version: (0, 0, 0),
            font_scanned_buf_key: (0, 0),
            disk_stamp: new_stamp,
            external_change_detected: false,
        };
        let old_idx = self.active_tab;
        let outgoing = self.swap_active_with_state(fresh);
        self.tabs[old_idx].state = Some(Box::new(outgoing));
        self.tabs.push(TabSlot {
            id: new_id,
            untitled_num,
            state: None,
        });
        self.active_tab = self.tabs.len() - 1;
        self.sync_ast_if_needed();
    }

    pub fn new_tab(&mut self, ctx: &egui::Context) {
        self.pending_anchor_after_open = None;
        let untitled_num = self.alloc_untitled_num();
        self.open_buffer_in_new_tab(DocumentBuffer::new(), untitled_num);
        self.refocus_active_editor();
        self.exporter.status_text = format!(
            "Opened {}",
            Self::untitled_display_name(untitled_num)
        );
        ctx.request_repaint();
    }

    pub fn request_close_tab(&mut self, ctx: &egui::Context, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if self.tab_is_dirty(idx) {
            self.switch_to_tab(idx);
            self.unsaved_modal.open(PendingDiscardAction::CloseTab(idx));
            ctx.request_repaint();
        } else {
            self.close_tab_discard(ctx, idx);
        }
    }

    pub fn close_tab_discard(&mut self, ctx: &egui::Context, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            self.execute_new_file(ctx);
            self.tabs[0].id = self.doc_id;
            self.tabs[0].untitled_num = 1;
            self.tabs[0].state = None;
            return;
        }
        self.tabs.remove(idx);
        if idx == self.active_tab {
            let new_active = idx.min(self.tabs.len() - 1);
            self.active_tab = new_active;
            if let Some(incoming) = self.tabs[new_active].state.take() {
                self.apply_incoming_tab_state(*incoming);
            }
            self.sync_ast_if_needed();
        } else if idx < self.active_tab {
            self.active_tab -= 1;
        }
        self.refocus_active_editor();
        ctx.request_repaint();
    }

    /// Detaches tab `idx` into either a hovered sibling Markdown Studio window (moving the
    /// tab across instances just like Chrome) or a new standalone window (`--new-window`),
    /// preserving unsaved/untitled buffers via a temp `.handoff` file.
    pub fn detach_tab_to_new_window(&mut self, ctx: &egui::Context, idx: usize) -> bool {
        if idx >= self.tabs.len() {
            return false;
        }
        let display_title = self.tab_display_name(idx);
        let (file_path, dirty, handoff_rope) = if idx == self.active_tab {
            let fp = self.buffer.file_path.clone();
            let d = self.buffer.dirty;
            let r = if fp.is_none() || d {
                Some(self.buffer.rope().clone())
            } else {
                None
            };
            (fp, d, r)
        } else {
            let Some(st) = self.tabs[idx].state.as_ref() else {
                return false;
            };
            let fp = st.buffer.file_path.clone();
            let d = st.buffer.dirty;
            let r = if fp.is_none() || d {
                Some(st.buffer.rope().clone())
            } else {
                None
            };
            (fp, d, r)
        };

        let drop_screen_pos = self.pointer_screen_pos(ctx, self.tab_bar_state.last_drop_pos);
        if let (Some(pos), Some(peers)) = (drop_screen_pos, self.cached_peer_windows.as_ref()) {
            if let Some(peer) = peers.iter().find(|p| p.outer_rect.contains(pos)) {
                let (sent, temp_handoff) = if let (Some(ref path), false) = (&file_path, dirty) {
                    (
                        matches!(instance::try_send_open_to_peer(peer, path), Ok(true)),
                        None,
                    )
                } else {
                    let empty = ropey::Rope::new();
                    let rope_ref = handoff_rope.as_ref().unwrap_or(&empty);
                    match instance::write_handoff_file_from_rope(
                        file_path.as_deref(),
                        dirty,
                        rope_ref,
                    ) {
                        Ok(hp) => {
                            let ok =
                                matches!(instance::try_send_handoff_to_peer(peer, &hp), Ok(true));
                            if !ok {
                                let _ = std::fs::remove_file(&hp);
                            }
                            (ok, Some(hp))
                        }
                        Err(_) => (false, None),
                    }
                };
                if sent {
                    let _ = temp_handoff;
                    if self.tabs.len() > 1 {
                        self.close_tab_discard(ctx, idx);
                    } else {
                        self.allow_close_now = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    self.exporter.status_text = format!("Moved {display_title} to other window");
                    return true;
                }
            }
        }

        if self.tabs.len() <= 1 {
            return false;
        }

        let mut args = vec!["--new-window".to_string()];
        if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
            let rel = self.tab_bar_state.last_drop_pos;
            let target_x = outer.left() + rel.x.max(24.0) - 80.0;
            let target_y = outer.top() + rel.y.max(24.0) - 20.0;
            args.push("--window-pos".to_string());
            args.push(format!("{target_x:.0},{target_y:.0}"));
        }

        let mut created_handoff: Option<PathBuf> = None;
        if let (Some(ref path), false) = (&file_path, dirty) {
            args.push(path.to_string_lossy().to_string());
        } else {
            let empty = ropey::Rope::new();
            let rope_ref = handoff_rope.as_ref().unwrap_or(&empty);
            match instance::write_handoff_file_from_rope(file_path.as_deref(), dirty, rope_ref) {
                Ok(handoff_path) => {
                    args.push("--handoff".to_string());
                    args.push(handoff_path.to_string_lossy().to_string());
                    created_handoff = Some(handoff_path);
                }
                Err(e) => {
                    self.exporter.status_text = format!("Failed to detach tab: {e}");
                    return false;
                }
            }
        }

        self.last_detached_window_args = args.clone();
        if self.spawn_new_window_with_os {
            let exe = std::env::current_exe()
                .unwrap_or_else(|_| self.settings.effective_exe_path());
            if let Err(e) = std::process::Command::new(&exe).args(&args).spawn() {
                if let Some(hp) = created_handoff {
                    let _ = std::fs::remove_file(hp);
                }
                self.exporter.status_text = format!("Failed to open new window: {e}");
                return false;
            }
        }

        self.close_tab_discard(ctx, idx);
        self.exporter.status_text = format!("Opened {display_title} in a new window");
        true
    }

    /// Renames the active document on disk (or saves an untitled document under the new name),
    /// updating the open buffer path, recent files list, and File Explorer cache.
    pub fn rename_active_file(&mut self, raw_name: &str) -> bool {
        let trimmed = raw_name.trim();
        if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
            self.exporter.status_text = "Filename cannot be empty".to_string();
            return false;
        }
        let has_invalid = trimmed.chars().any(|c| {
            matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') || c < '\u{20}'
        });
        if has_invalid {
            self.exporter.status_text = "Invalid characters in filename".to_string();
            return false;
        }

        let final_name = if trimmed.contains('.') {
            trimmed.to_string()
        } else if let Some(ext) = self
            .buffer
            .file_path
            .as_deref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .filter(|e| !e.is_empty())
        {
            format!("{trimmed}.{ext}")
        } else if self.buffer.file_path.is_none() {
            format!("{trimmed}.md")
        } else {
            trimmed.to_string()
        };

        if let Some(old_path) = self.buffer.file_path.clone() {
            let new_path = old_path.with_file_name(&final_name);
            if new_path == old_path {
                return true;
            }
            let is_case_only = paths_match(&old_path, &new_path) && old_path != new_path;
            if !is_case_only && new_path.exists() {
                self.exporter.status_text =
                    format!("Cannot rename: \"{final_name}\" already exists");
                return false;
            }
            match std::fs::rename(&old_path, &new_path) {
                Ok(()) => {
                    self.buffer.file_path = Some(new_path.clone());
                    self.buffer
                        .record_file_rename(old_path.clone(), new_path.clone());
                    self.settings
                        .recent_files
                        .retain(|p| !paths_match(p, &old_path));
                    self.settings.record_recent_file(&new_path);
                    self.persist_settings();
                    self.refresh_disk_stamp();
                    self.reader_state.base_dir = self.current_base_dir();
                    self.explorer_state.refresh();
                    self.exporter.status_text = format!("Renamed to {final_name}");
                    true
                }
                Err(e) => {
                    self.exporter.status_text = format!("Rename failed: {e}");
                    false
                }
            }
        } else if let Some(dir) = self
            .explorer_state
            .root_dir
            .clone()
            .filter(|d| d.is_dir())
        {
            let target = dir.join(&final_name);
            if target.exists() {
                self.exporter.status_text =
                    format!("Cannot rename: \"{final_name}\" already exists");
                return false;
            }
            self.save_untitled_to_target(target, &final_name)
        } else if self.open_external_links_with_os {
            if let Some(target) = rfd::FileDialog::new()
                .add_filter("Markdown", &["md", "markdown"])
                .set_file_name(&final_name)
                .save_file()
            {
                self.save_untitled_to_target(target, &final_name)
            } else {
                false
            }
        } else {
            self.exporter.status_text = "Save the file first to choose a folder".to_string();
            false
        }
    }

    fn save_untitled_to_target(&mut self, target: PathBuf, display_name: &str) -> bool {
        match self.buffer.save_to_file(&target) {
            Ok(()) => {
                self.tabs[self.active_tab].untitled_num = 0;
                self.refresh_disk_stamp();
                self.reader_state.base_dir = self.current_base_dir();
                self.settings.record_recent_file(&target);
                self.persist_settings();
                self.explorer_state.refresh();
                self.exporter.status_text = format!("Saved as {display_name}");
                true
            }
            Err(e) => {
                self.exporter.status_text = format!("Save failed: {e}");
                false
            }
        }
    }

    fn apply_rename_sync(&mut self, from: &Path, to: &Path, action_word: &str) {
        self.settings
            .recent_files
            .retain(|p| !paths_match(p, from));
        self.settings.record_recent_file(to);
        self.persist_settings();
        self.refresh_disk_stamp();
        self.reader_state.base_dir = self.current_base_dir();
        self.explorer_state.refresh();
        let display_name = to
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| to.to_string_lossy().to_string());
        self.exporter.status_text = format!("{action_word} rename → {display_name}");
    }

    fn perform_undo_steps(&mut self, steps: usize) {
        let mut text_changed = false;
        for _ in 0..steps {
            match self.buffer.undo_with_outcome() {
                UndoRedoOutcome::None => break,
                UndoRedoOutcome::TextEdited => text_changed = true,
                UndoRedoOutcome::Renamed { from, to } => {
                    self.apply_rename_sync(&from, &to, "Undid");
                }
                UndoRedoOutcome::RenameFailed(msg) => {
                    self.exporter.status_text = msg;
                    break;
                }
            }
        }
        if text_changed {
            self.editor_state.scroll_to_cursor = true;
        }
    }

    fn perform_redo_steps(&mut self, steps: usize) {
        let mut text_changed = false;
        for _ in 0..steps {
            match self.buffer.redo_with_outcome() {
                UndoRedoOutcome::None => break,
                UndoRedoOutcome::TextEdited => text_changed = true,
                UndoRedoOutcome::Renamed { from, to } => {
                    self.apply_rename_sync(&from, &to, "Redid");
                }
                UndoRedoOutcome::RenameFailed(msg) => {
                    self.exporter.status_text = msg;
                    break;
                }
            }
        }
        if text_changed {
            self.editor_state.scroll_to_cursor = true;
        }
    }

    /// Moves a tab from `from` to `to` while preserving `active_tab` tracking.
    pub fn reorder_tab(&mut self, from: usize, to: usize) -> bool {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return false;
        }
        let slot = self.tabs.remove(from);
        self.tabs.insert(to, slot);
        if self.active_tab == from {
            self.active_tab = to;
        } else if from < self.active_tab && self.active_tab <= to {
            self.active_tab -= 1;
        } else if to <= self.active_tab && self.active_tab < from {
            self.active_tab += 1;
        }
        true
    }

    fn find_open_tab_by_path(&self, path: &Path) -> Option<usize> {
        for idx in 0..self.tabs.len() {
            let tab_path = if idx == self.active_tab {
                self.buffer.file_path.as_deref()
            } else {
                self.tabs[idx]
                    .state
                    .as_ref()
                    .and_then(|s| s.buffer.file_path.as_deref())
            };
            if let Some(existing) = tab_path {
                if paths_match(existing, path) {
                    return Some(idx);
                }
            }
        }
        None
    }

    fn reset_state_for_new_buffer(&mut self) {
        self.doc_id = self.alloc_doc_id();
        self.autoscroll.cancel();
        self.editor_state.reset_for_new_document();
        self.reader_state.active_block_lines = None;
        self.reader_state.active_code_lang_draft.clear();
        self.reader_state.scroll_to_y = Some(0.0);
        self.reader_state.smooth_scroll_target_y = None;
        self.reader_state.highlight_block_lines = None;
        self.reader_state.base_dir = self.current_base_dir();
        self.last_split_top_row = 0;
        self.find_bar.reset_for_new_document();
        self.last_parsed_version = 0;
        self.last_parsed_doc_id = 0;
        self.last_parsed_buf_id = 0;
        self.font_scanned_buf_key = (0, 0);
        self.last_requested_parse = (0, 0, 0);
        self.block_index = ViewportBlockIndex::empty_for_doc(self.doc_id);
        self.outline_items.clear();
        self.outline_cache_key = (0, 0);
        self.collapsed_outline_lines.clear();
        self.refresh_disk_stamp();
        self.toolbar_state.renaming_file = false;
    }

    fn refresh_disk_stamp(&mut self) {
        self.disk_stamp = self
            .buffer
            .file_path
            .as_deref()
            .and_then(DiskFileStamp::from_path);
        self.external_change_detected = false;
        self.last_disk_check = Instant::now();
    }

    /// Checks the active tab's backing file `(mtime, len)` for external edits.
    /// Clean buffers auto-reload seamlessly; dirty buffers raise the non-blocking reload banner.
    pub fn check_external_file_changes(&mut self) {
        self.last_disk_check = Instant::now();
        let Some(path) = self.buffer.file_path.as_deref() else {
            self.disk_stamp = None;
            self.external_change_detected = false;
            return;
        };
        let Some(current_stamp) = DiskFileStamp::from_path(path) else {
            return;
        };
        if self.disk_stamp == Some(current_stamp) {
            return;
        }
        if !self.buffer.dirty {
            if self.reload_active_file_from_disk() {
                self.exporter.status_text = "Auto-reloaded updated file from disk".to_string();
            }
        } else {
            self.external_change_detected = true;
        }
    }

    /// Reloads the active file from disk while preserving clamped cursor and scroll positions.
    pub fn reload_active_file_from_disk(&mut self) -> bool {
        let Some(path) = self.buffer.file_path.clone() else {
            return false;
        };
        let Ok(mut reloaded) = DocumentBuffer::open_file(&path) else {
            self.exporter.status_text = format!("Failed to reload {}", path.display());
            return false;
        };
        let prev_cursor = self.buffer.cursor_char;
        let prev_scroll_y = self.reader_state.last_scroll_offset_y;
        let max_chars = reloaded.len_chars();
        reloaded.set_cursor(prev_cursor.min(max_chars), false);

        self.buffer = reloaded;
        self.last_parsed_version = 0;
        self.last_parsed_doc_id = 0;
        self.last_parsed_buf_id = 0;
        self.font_scanned_buf_key = (0, 0);
        self.last_requested_parse = (0, 0, 0);
        self.block_index = ViewportBlockIndex::empty_for_doc(self.doc_id);
        self.outline_cache_key = (0, 0);
        self.editor_state.reset_for_new_document();
        self.editor_state.scroll_to_cursor = false;
        self.reader_state.active_block_lines = None;
        self.reader_state.scroll_to_y = Some(prev_scroll_y.max(0.0));
        self.reader_state.smooth_scroll_target_y = None;
        self.find_bar.last_buf_version = u64::MAX;
        self.refresh_disk_stamp();
        self.sync_ast_if_needed();
        true
    }

    fn current_session_snapshot_key(&self) -> Vec<(u64, u64, bool)> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                if i == self.active_tab {
                    (self.doc_id, self.buffer.version, self.buffer.dirty)
                } else if let Some(ref st) = slot.state {
                    (st.doc_id, st.buffer.version, st.buffer.dirty)
                } else {
                    (slot.id, 0, false)
                }
            })
            .collect()
    }

    fn build_session_save_job(&self, include_dirty_recovery: bool) -> session::SessionSaveJob {
        let sess_path = session::session_path(&self.settings_path);
        let rec_dir = session::recovery_dir(&self.settings_path);
        let mut save_tabs = Vec::with_capacity(self.tabs.len());
        let mut new_active = 0usize;

        let is_single_empty_untitled = self.tabs.len() == 1
            && self.buffer.file_path.is_none()
            && self.buffer.is_empty()
            && !self.buffer.dirty;

        if !is_single_empty_untitled {
            for (i, slot) in self.tabs.iter().enumerate() {
                let (buf, scroll_y) = if i == self.active_tab {
                    (&self.buffer, self.reader_state.last_scroll_offset_y)
                } else if let Some(ref st) = slot.state {
                    (&st.buffer, st.reader_state.last_scroll_offset_y)
                } else {
                    continue;
                };

                if !include_dirty_recovery && buf.file_path.is_none() {
                    continue;
                }
                let needs_recovery = include_dirty_recovery
                    && (buf.dirty || (buf.file_path.is_none() && !buf.is_empty()));
                if buf.file_path.is_none() && !needs_recovery {
                    continue;
                }

                if i == self.active_tab {
                    new_active = save_tabs.len();
                }
                save_tabs.push(session::SessionSaveTab {
                    slot_idx: i,
                    file_path: buf.file_path.clone(),
                    rope: needs_recovery.then(|| buf.rope().clone()),
                    dirty: include_dirty_recovery && (buf.dirty || buf.file_path.is_none()),
                    cursor_char: buf.cursor_char,
                    scroll_y,
                    untitled_num: slot.untitled_num,
                });
            }
        }

        session::SessionSaveJob {
            session_path: sess_path,
            recovery_dir: rec_dir,
            active_tab: new_active,
            tabs: save_tabs,
        }
    }

    fn save_session_internal(&mut self, include_dirty_recovery: bool) {
        let job = self.build_session_save_job(include_dirty_recovery);
        session::write_session_save_job(&job);
        self.last_recovery_snapshot_key = self.current_session_snapshot_key();
        self.last_recovery_save = Instant::now();
    }

    /// Saves open tabs and crash-safe recovery snapshots for dirty/unsaved buffers immediately.
    pub fn save_session_and_recovery_now(&mut self) {
        self.save_session_internal(true);
    }

    /// Restores open tabs and any unsaved crash-recovery snapshots from `session.json`.
    pub fn restore_session_from_disk(&mut self) -> bool {
        let sess_path = session::session_path(&self.settings_path);
        let Some(sess) = session::load_session(&sess_path) else {
            return false;
        };
        if sess.tabs.is_empty() {
            return false;
        }

        let mut restored_tabs: Vec<(usize, InactiveTabState)> = Vec::new();
        let mut recovered_unsaved = false;

        for entry in sess.tabs {
            let buf_opt = if entry.dirty || entry.file_path.is_none() {
                if let Some(ref rec_path) = entry.recovery_file {
                    let dirty_flag = entry.dirty || entry.file_path.is_none();
                    if let Ok(mut b) = DocumentBuffer::open_recovery_file(
                        rec_path,
                        entry.file_path.clone(),
                        dirty_flag,
                    ) {
                        recovered_unsaved = true;
                        b.set_cursor(entry.cursor_char.min(b.len_chars()), false);
                        Some(b)
                    } else if let Some(ref fp) = entry.file_path {
                        DocumentBuffer::open_file(fp).ok().map(|mut b| {
                            b.set_cursor(entry.cursor_char.min(b.len_chars()), false);
                            b
                        })
                    } else {
                        None
                    }
                } else if let Some(ref fp) = entry.file_path {
                    DocumentBuffer::open_file(fp).ok().map(|mut b| {
                        b.set_cursor(entry.cursor_char.min(b.len_chars()), false);
                        b
                    })
                } else {
                    None
                }
            } else if let Some(ref fp) = entry.file_path {
                DocumentBuffer::open_file(fp).ok().map(|mut b| {
                    b.set_cursor(entry.cursor_char.min(b.len_chars()), false);
                    b
                })
            } else {
                None
            };

            let Some(buf) = buf_opt else {
                continue;
            };
            let new_id = self.alloc_doc_id();
            let word_cnt = buf.word_count();
            let disk_stamp = buf.file_path.as_deref().and_then(DiskFileStamp::from_path);
            let untitled_num = if buf.file_path.is_some() {
                0
            } else if entry.untitled_num > 0 {
                self.next_untitled_num = self.next_untitled_num.max(entry.untitled_num + 1);
                entry.untitled_num
            } else {
                self.alloc_untitled_num()
            };
            let mut ed_state = VirtualEditorState::default();
            ed_state.request_focus = matches!(self.view_mode, ViewMode::Source | ViewMode::Split);
            let mut rd_state = self.new_reader_state_for_buffer(&buf);
            if entry.scroll_y > 1.0 {
                rd_state.scroll_to_y = Some(entry.scroll_y);
                rd_state.last_scroll_offset_y = entry.scroll_y;
            }
            let st = InactiveTabState {
                doc_id: new_id,
                buffer: buf,
                block_index: ViewportBlockIndex::empty_for_doc(new_id),
                last_parsed_version: 0,
                last_parsed_doc_id: 0,
                last_parsed_buf_id: 0,
                editor_state: ed_state,
                reader_state: rd_state,
                last_split_top_row: 0,
                outline_items: Vec::new(),
                outline_cache_key: (0, 0),
                collapsed_outline_lines: FxHashSet::default(),
                word_count_cache: (new_id, 0, word_cnt),
                active_format_cache: ((0, 0, usize::MAX, None), ActiveFormatState::default()),
                font_scan_version: (0, 0, 0),
                font_scanned_buf_key: (0, 0),
                disk_stamp,
                external_change_detected: false,
            };
            restored_tabs.push((untitled_num, st));
        }

        if restored_tabs.is_empty() {
            return false;
        }

        let target_active = sess.active_tab.min(restored_tabs.len() - 1);
        let mut slots = Vec::with_capacity(restored_tabs.len());
        let mut active_state_opt = None;
        for (i, (untitled_num, st)) in restored_tabs.into_iter().enumerate() {
            let id = st.doc_id;
            if i == target_active {
                active_state_opt = Some(st);
                slots.push(TabSlot {
                    id,
                    untitled_num,
                    state: None,
                });
            } else {
                slots.push(TabSlot {
                    id,
                    untitled_num,
                    state: Some(Box::new(st)),
                });
            }
        }

        self.tabs = slots;
        self.active_tab = target_active;
        if let Some(active_state) = active_state_opt {
            self.apply_incoming_tab_state(active_state);
        }
        if self.explorer_state.root_dir.is_none() {
            if let Some(dir) = self.current_base_dir() {
                self.explorer_state.set_root(dir);
            }
        }
        self.sync_ast_if_needed();
        self.last_recovery_snapshot_key = self.current_session_snapshot_key();
        self.last_recovery_save = Instant::now();
        self.exporter.status_text = if recovered_unsaved {
            "Recovered unsaved work from previous session".to_string()
        } else {
            format!("Restored session ({} tab(s))", self.tabs.len())
        };
        true
    }

    fn execute_new_file(&mut self, ctx: &egui::Context) {
        self.pending_anchor_after_open = None;
        self.buffer = DocumentBuffer::new();
        self.reset_state_for_new_buffer();
        self.tabs[self.active_tab].id = self.doc_id;
        self.refocus_active_editor();
        self.exporter.status_text = "Created new document".to_string();
        ctx.request_repaint();
    }

    pub fn open_specific_file(&mut self, path: &Path) {
        self.reader_state.image_lightbox = None;
        if let Some(existing_idx) = self.find_open_tab_by_path(path) {
            self.switch_to_tab(existing_idx);
            self.exporter.status_text = format!("Switched to {}", path.display());
            if let Some(slug) = self.pending_anchor_after_open.take() {
                self.jump_to_anchor(None, &slug);
            }
            return;
        }
        match DocumentBuffer::open_file(path) {
            Ok(new_buf) => {
                let reuse_initial_blank = self.tabs.len() == 1
                    && self.buffer.file_path.is_none()
                    && self.buffer.is_empty()
                    && !self.buffer.dirty;
                if reuse_initial_blank {
                    self.buffer = new_buf;
                    self.reset_state_for_new_buffer();
                    self.tabs[self.active_tab].id = self.doc_id;
                    self.tabs[self.active_tab].untitled_num = 0;
                } else {
                    self.open_buffer_in_new_tab(new_buf, 0);
                }
                if self.explorer_state.root_dir.is_none() {
                    if let Some(parent) = path.parent() {
                        if !parent.as_os_str().is_empty() {
                            self.explorer_state.set_root(parent.to_path_buf());
                            self.settings.explorer_root = Some(parent.to_path_buf());
                        }
                    }
                }
                self.settings.record_recent_file(path);
                self.persist_settings();
                self.sync_ast_if_needed();
                self.refocus_active_editor();
                self.exporter.status_text = format!("Opened {}", path.display());
                if let Some(slug) = self.pending_anchor_after_open.take() {
                    self.jump_to_anchor(None, &slug);
                }
            }
            Err(e) => {
                self.pending_anchor_after_open = None;
                self.exporter.status_text = format!("Open failed: {e}");
            }
        }
    }

    fn execute_open_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "Markdown & Text",
                &["md", "markdown", "mdown", "mkd", "txt", "log", "rst", "org"],
            )
            .add_filter(
                "All Text & Code Files",
                &[
                    "md", "markdown", "txt", "json", "toml", "yaml", "yml", "xml", "svg",
                    "html", "htm", "css", "js", "ts", "rs", "py", "c", "cpp", "h", "hpp",
                    "cs", "java", "go", "rb", "php", "sh", "bat", "cmd", "ps1", "ini",
                    "cfg", "conf", "env", "csv", "tsv", "sql", "tex", "log",
                ],
            )
            .add_filter("All Files", &["*"])
            .pick_file()
        {
            self.open_specific_file(&path);
        }
    }

    fn save_current_document(&mut self) -> bool {
        if let Some(path) = self.buffer.file_path.clone() {
            match self.buffer.save_to_file(&path) {
                Ok(()) => {
                    self.settings.record_recent_file(&path);
                    self.persist_settings();
                    self.refresh_disk_stamp();
                    self.exporter.status_text = format!("Saved {}", path.display());
                    true
                }
                Err(e) => {
                    self.exporter.status_text = format!("Save failed: {e}");
                    false
                }
            }
        } else {
            let default_name = self.doc_display_name();
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Markdown", &["md", "markdown"])
                .set_file_name(&default_name)
                .save_file()
            {
                match self.buffer.save_to_file(&path) {
                    Ok(()) => {
                        self.settings.record_recent_file(&path);
                        self.persist_settings();
                        self.refresh_disk_stamp();
                        self.reader_state.base_dir = self.current_base_dir();
                        self.exporter.status_text = format!("Saved {}", path.display());
                        true
                    }
                    Err(e) => {
                        self.exporter.status_text = format!("Save failed: {e}");
                        false
                    }
                }
            } else {
                false
            }
        }
    }

    fn apply_pending_discard_action(
        &mut self,
        ctx: &egui::Context,
        pending: PendingDiscardAction,
    ) {
        match pending {
            PendingDiscardAction::NewFile => self.execute_new_file(ctx),
            PendingDiscardAction::OpenFile => self.execute_open_file(),
            PendingDiscardAction::OpenSpecificFile(path) => self.open_specific_file(&path),
            PendingDiscardAction::CloseTab(idx) => self.close_tab_discard(ctx, idx),
            PendingDiscardAction::CloseWindow => {
                if self.pending_zoom_save_at.take().is_some() {
                    self.persist_settings();
                }
                self.save_session_internal(false);
                self.allow_close_now = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) -> ToolbarAction {
        if self.toolbar_state.renaming_file
            || self.table_modal.open
            || self.unsaved_modal.is_open()
            || self.print_modal.open
            || self.insert_link_modal.open
            || self.os_modal.open
            || self.external_diff_modal.open
        {
            return ToolbarAction::None;
        }
        let read_only = self.view_mode == ViewMode::Reader;
        let find_focused = self.find_bar.has_input_focus(ctx);
        ctx.input_mut(|i| {
            let cmd_only = egui::Modifiers::COMMAND;
            let cmd_shift = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
            if i.consume_key(cmd_only, Key::Num1) {
                ToolbarAction::SetViewMode(ViewMode::Wysiwyg)
            } else if i.consume_key(cmd_only, Key::Num2) {
                ToolbarAction::SetViewMode(ViewMode::Split)
            } else if i.consume_key(cmd_only, Key::Num3) {
                ToolbarAction::SetViewMode(ViewMode::Reader)
            } else if i.consume_key(cmd_only, Key::Num4) {
                ToolbarAction::SetViewMode(ViewMode::Source)
            } else if i.consume_key(cmd_shift, Key::Tab) || i.consume_key(cmd_only, Key::PageUp) {
                ToolbarAction::PrevTab
            } else if i.consume_key(cmd_only, Key::Tab) || i.consume_key(cmd_only, Key::PageDown) {
                ToolbarAction::NextTab
            } else if i.consume_key(cmd_only, Key::T) {
                ToolbarAction::NewTab
            } else if i.consume_key(cmd_only, Key::W) {
                ToolbarAction::CloseTab
            } else if !read_only && !find_focused && i.consume_key(cmd_only, Key::B) {
                ToolbarAction::ToggleBold
            } else if !read_only && !find_focused && i.consume_key(cmd_only, Key::I) {
                ToolbarAction::ToggleItalic
            } else if !read_only && !find_focused && i.consume_key(cmd_only, Key::U) {
                ToolbarAction::ToggleUnderline
            } else if !read_only && !find_focused && i.consume_key(cmd_only, Key::K) {
                ToolbarAction::OpenLinkDialog
            } else if i.consume_key(cmd_only, Key::Equals)
                || i.consume_key(cmd_only, Key::Plus)
                || i.consume_key(cmd_shift, Key::Equals)
                || i.consume_key(cmd_shift, Key::Plus)
            {
                ToolbarAction::ZoomIn
            } else if i.consume_key(cmd_only, Key::Minus) {
                ToolbarAction::ZoomOut
            } else if i.consume_key(cmd_only, Key::Num0) {
                ToolbarAction::ZoomReset
            } else if i.consume_key(cmd_shift, Key::S) {
                ToolbarAction::SaveFileAs
            } else if i.consume_key(cmd_only, Key::S) {
                ToolbarAction::SaveFile
            } else if i.consume_key(cmd_only, Key::O) {
                ToolbarAction::OpenFile
            } else if !read_only && !find_focused && i.consume_key(cmd_only, Key::N) {
                ToolbarAction::NewFile
            } else if i.consume_key(cmd_only, Key::P) {
                ToolbarAction::PrintDoc
            } else if !read_only
                && !find_focused
                && (i.consume_key(cmd_only, Key::Y) || i.consume_key(cmd_shift, Key::Z))
            {
                ToolbarAction::Redo
            } else if !read_only && !find_focused && i.consume_key(cmd_only, Key::Z) {
                ToolbarAction::Undo
            } else if i.consume_key(cmd_only, Key::H) {
                ToolbarAction::OpenReplaceBar
            } else if i.consume_key(cmd_only, Key::F) {
                ToolbarAction::FocusFindBar
            } else if i.consume_key(egui::Modifiers::NONE, Key::F11) {
                ToolbarAction::ToggleFocusMode
            } else {
                ToolbarAction::None
            }
        })
    }

    fn activate_inserted_fenced_block(&mut self) {
        if self.view_mode != ViewMode::Wysiwyg {
            self.reader_state.active_block_lines = None;
            self.editor_state.scroll_to_cursor = true;
            self.refocus_active_editor();
            return;
        }
        self.sync_ast_if_needed();
        let body_line = self.buffer.char_to_line(self.buffer.cursor_char);
        let open_line = body_line.saturating_sub(1);
        let Some((_, fence_char, fence_len, _)) =
            parse_code_fence_line(&self.buffer.line_text(open_line))
        else {
            self.reader_state.active_block_lines = None;
            self.refocus_active_editor();
            return;
        };
        let mut close_line = body_line;
        while close_line < self.buffer.len_lines() {
            let lt = self.buffer.line_text(close_line);
            let trimmed = lt.trim();
            let is_close = if fence_char == '$' {
                trimmed == "$$"
            } else {
                let count = trimmed.bytes().take_while(|&b| b == fence_char as u8).count();
                count >= fence_len && trimmed[count..].trim().is_empty()
            };
            if is_close {
                break;
            }
            close_line += 1;
        }
        if close_line < self.buffer.len_lines() {
            activate_wysiwyg_code_block_range(
                &mut self.buffer,
                open_line..(close_line + 1),
                self.show_markup,
                &mut self.reader_state,
            );
        } else {
            self.reader_state.active_block_lines = None;
            self.refocus_active_editor();
        }
    }

    fn refocus_active_editor(&mut self) {
        match self.view_mode {
            ViewMode::Wysiwyg => {
                if self.reader_state.active_block_lines.is_some() {
                    self.reader_state.focus_inline_editor = true;
                }
            }
            ViewMode::Source | ViewMode::Split => {
                self.editor_state.request_focus = true;
            }
            ViewMode::Reader => {}
        }
    }

    pub fn dispatch_action(&mut self, ctx: &egui::Context, action: ToolbarAction) {
        let read_only = self.view_mode == ViewMode::Reader;
        let is_mutating = matches!(
            action,
            ToolbarAction::NewFile
                | ToolbarAction::Undo
                | ToolbarAction::Redo
                | ToolbarAction::UndoSteps(_)
                | ToolbarAction::RedoSteps(_)
                | ToolbarAction::ToggleBold
                | ToolbarAction::ToggleItalic
                | ToolbarAction::ToggleUnderline
                | ToolbarAction::ToggleStrike
                | ToolbarAction::ToggleInlineCode
                | ToolbarAction::SetHeading(_)
                | ToolbarAction::ToggleBlock(_)
                | ToolbarAction::SetAlignment(_)
                | ToolbarAction::Indent
                | ToolbarAction::Outdent
                | ToolbarAction::InsertCodeBlock
                | ToolbarAction::InsertHorizontalRule
                | ToolbarAction::InsertMath
                | ToolbarAction::InsertInlineMath
                | ToolbarAction::InsertMathSnippet(_)
                | ToolbarAction::InsertMathTemplate(_)
                | ToolbarAction::CalculateMath
                | ToolbarAction::InsertMermaid
                | ToolbarAction::InsertImage
                | ToolbarAction::OpenLinkDialog
                | ToolbarAction::InsertTable { .. }
                | ToolbarAction::OpenTableEditor
                | ToolbarAction::InsertTableFormula(_)
                | ToolbarAction::CalculateTables
                | ToolbarAction::RenameFile(_)
        );
        if read_only && is_mutating {
            self.exporter.status_text = "Reader mode is read-only".to_string();
            return;
        }

        let mutation_for_wysiwyg = matches!(
            action,
            ToolbarAction::Undo
                | ToolbarAction::Redo
                | ToolbarAction::UndoSteps(_)
                | ToolbarAction::RedoSteps(_)
                | ToolbarAction::ToggleBold
                | ToolbarAction::ToggleItalic
                | ToolbarAction::ToggleUnderline
                | ToolbarAction::ToggleStrike
                | ToolbarAction::ToggleInlineCode
                | ToolbarAction::SetHeading(_)
                | ToolbarAction::ToggleBlock(_)
                | ToolbarAction::SetAlignment(_)
                | ToolbarAction::Indent
                | ToolbarAction::Outdent
                | ToolbarAction::InsertInlineMath
                | ToolbarAction::InsertMathSnippet(_)
                | ToolbarAction::CalculateMath
        );

        match action {
            ToolbarAction::None => {}
            ToolbarAction::NewFile => {
                self.new_tab(ctx);
            }
            ToolbarAction::NewTab => {
                self.new_tab(ctx);
            }
            ToolbarAction::CloseTab => {
                self.request_close_tab(ctx, self.active_tab);
            }
            ToolbarAction::NextTab => {
                if self.tabs.len() > 1 {
                    let next = (self.active_tab + 1) % self.tabs.len();
                    self.switch_to_tab(next);
                    ctx.request_repaint();
                }
            }
            ToolbarAction::PrevTab => {
                if self.tabs.len() > 1 {
                    let prev = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
                    self.switch_to_tab(prev);
                    ctx.request_repaint();
                }
            }
            ToolbarAction::OpenFile => {
                self.execute_open_file();
            }
            ToolbarAction::OpenRecentFile(path) => {
                self.open_specific_file(&path);
            }
            ToolbarAction::SaveFile => {
                let _ = self.save_current_document();
            }
            ToolbarAction::SaveFileAs => {
                let default_name = self.doc_display_name();
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Markdown", &["md", "markdown"])
                    .set_file_name(&default_name)
                    .save_file()
                {
                    match self.buffer.save_to_file(&path) {
                        Ok(()) => {
                            self.settings.record_recent_file(&path);
                            self.persist_settings();
                            self.refresh_disk_stamp();
                            self.reader_state.base_dir = self.current_base_dir();
                            self.exporter.status_text = format!("Saved {}", path.display());
                        }
                        Err(e) => {
                            self.exporter.status_text = format!("Save failed: {e}");
                        }
                    }
                }
            }
            ToolbarAction::RenameFile(new_name) => {
                self.rename_active_file(&new_name);
            }
            ToolbarAction::ExportPdf => {
                let default_name = format!("{}.pdf", self.doc_title());
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PDF Document", &["pdf"])
                    .set_file_name(&default_name)
                    .save_file()
                {
                    let layout = self.print_modal.options.layout_options();
                    self.exporter.start_job_with_base_dir(
                        self.buffer.rope().clone(),
                        self.doc_title(),
                        ExportTarget::PdfWithOptions { path, layout },
                        self.current_base_dir(),
                    );
                }
            }
            ToolbarAction::ExportDocx => {
                let default_name = format!("{}.docx", self.doc_title());
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Word Document", &["docx"])
                    .set_file_name(&default_name)
                    .save_file()
                {
                    self.exporter.start_job_with_base_dir(
                        self.buffer.rope().clone(),
                        self.doc_title(),
                        ExportTarget::Docx(path),
                        self.current_base_dir(),
                    );
                }
            }
            ToolbarAction::ExportHtml => {
                let default_name = format!("{}.html", self.doc_title());
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("HTML Document", &["html", "htm"])
                    .set_file_name(&default_name)
                    .save_file()
                {
                    self.exporter.start_job_with_base_dir(
                        self.buffer.rope().clone(),
                        self.doc_title(),
                        ExportTarget::Html(path),
                        self.current_base_dir(),
                    );
                }
            }
            ToolbarAction::ExportJson => {
                let default_name = format!("{}.json", self.doc_title());
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("JSON Document", &["json"])
                    .set_file_name(&default_name)
                    .save_file()
                {
                    self.exporter.start_job_with_base_dir(
                        self.buffer.rope().clone(),
                        self.doc_title(),
                        ExportTarget::Json {
                            path,
                            source_path: self.buffer.file_path.clone(),
                        },
                        self.current_base_dir(),
                    );
                }
            }
            ToolbarAction::PrintDoc => {
                self.print_modal.open_for_with_ctx(&self.doc_title(), ctx.clone());
            }
            ToolbarAction::Undo => {
                self.perform_undo_steps(1);
                self.refocus_active_editor();
            }
            ToolbarAction::Redo => {
                self.perform_redo_steps(1);
                self.refocus_active_editor();
            }
            ToolbarAction::UndoSteps(steps) => {
                self.perform_undo_steps(steps);
                self.refocus_active_editor();
            }
            ToolbarAction::RedoSteps(steps) => {
                self.perform_redo_steps(steps);
                self.refocus_active_editor();
            }
            ToolbarAction::ToggleBold => {
                self.buffer.restore_cached_selection();
                toggle_bold(&mut self.buffer);
                self.refocus_active_editor();
            }
            ToolbarAction::ToggleItalic => {
                self.buffer.restore_cached_selection();
                toggle_italic(&mut self.buffer);
                self.refocus_active_editor();
            }
            ToolbarAction::ToggleUnderline => {
                self.buffer.restore_cached_selection();
                toggle_underline(&mut self.buffer);
                self.refocus_active_editor();
            }
            ToolbarAction::ToggleStrike => {
                self.buffer.restore_cached_selection();
                toggle_strikethrough(&mut self.buffer);
                self.refocus_active_editor();
            }
            ToolbarAction::ToggleInlineCode => {
                self.buffer.restore_cached_selection();
                toggle_inline_code(&mut self.buffer);
                self.refocus_active_editor();
            }
            ToolbarAction::SetHeading(lvl) => {
                self.buffer.restore_cached_selection();
                set_heading(&mut self.buffer, lvl);
                self.refocus_active_editor();
            }
            ToolbarAction::ToggleBlock(kind) => {
                self.buffer.restore_cached_selection();
                toggle_line_prefix(&mut self.buffer, kind);
                self.refocus_active_editor();
            }
            ToolbarAction::SetAlignment(align) => {
                self.buffer.restore_cached_selection();
                set_block_alignment(&mut self.buffer, align);
                self.refocus_active_editor();
            }
            ToolbarAction::Indent => {
                self.buffer.restore_cached_selection();
                indent_lines(&mut self.buffer);
                self.refocus_active_editor();
            }
            ToolbarAction::Outdent => {
                self.buffer.restore_cached_selection();
                outdent_lines(&mut self.buffer);
                self.refocus_active_editor();
            }
            ToolbarAction::InsertCodeBlock => {
                insert_code_block(&mut self.buffer, "rust");
                self.activate_inserted_fenced_block();
            }
            ToolbarAction::InsertHorizontalRule => {
                insert_horizontal_rule(&mut self.buffer);
                self.reader_state.active_block_lines = None;
                self.editor_state.scroll_to_cursor = true;
                self.refocus_active_editor();
            }
            ToolbarAction::InsertMath => {
                if insert_math_block(&mut self.buffer) {
                    self.activate_inserted_fenced_block();
                } else {
                    self.editor_state.scroll_to_cursor = true;
                    self.refocus_active_editor();
                }
            }
            ToolbarAction::InsertInlineMath => {
                self.buffer.restore_cached_selection();
                toggle_inline_math(&mut self.buffer);
                self.refocus_active_editor();
            }
            ToolbarAction::InsertMathSnippet(snippet) => {
                self.insert_math_snippet_at_cursor(snippet);
            }
            ToolbarAction::InsertMathTemplate(tex) => {
                insert_math_template_block(&mut self.buffer, tex);
                self.activate_inserted_fenced_block();
            }
            ToolbarAction::CalculateMath => {
                self.calculate_math_at_cursor();
            }
            ToolbarAction::ToggleAutoCalcMath => {
                self.settings.auto_calc_math = !self.settings.auto_calc_math;
                self.persist_settings();
                self.exporter.status_text = if self.settings.auto_calc_math {
                    "Math Auto-Calc: ON".to_string()
                } else {
                    "Math Auto-Calc: OFF".to_string()
                };
            }
            ToolbarAction::InsertMermaid => {
                crate::document::editing::insert_mermaid_block(&mut self.buffer, None);
                self.activate_inserted_fenced_block();
            }
            ToolbarAction::InsertImage => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "gif", "tiff", "tif"])
                    .pick_file()
                {
                    let alt = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "image".to_string());
                    let url = self.format_image_insert_path(&path);
                    insert_image(&mut self.buffer, &alt, &url);
                    self.reader_state.active_block_lines = None;
                    self.editor_state.scroll_to_cursor = true;
                    self.refocus_active_editor();
                    self.exporter.status_text = format!("Inserted image {}", path.display());
                }
            }
            ToolbarAction::OpenLinkDialog => {
                self.buffer.restore_cached_selection();
                let prefill = self.buffer.selected_text();
                self.insert_link_modal.open_with(prefill);
            }
            ToolbarAction::InsertTable { rows, cols } => {
                insert_table(&mut self.buffer, rows, cols);
                self.reader_state.active_block_lines = None;
                self.editor_state.scroll_to_cursor = true;
                self.refocus_active_editor();
            }
            ToolbarAction::OpenTableEditor => {
                let line = self.buffer.char_to_line(self.buffer.cursor_char);
                if let Some(table) = table_at_line(&self.buffer, line) {
                    self.table_modal.open_with(table);
                } else {
                    self.exporter.status_text =
                        "Place cursor inside a GFM table to edit it".to_string();
                }
            }
            ToolbarAction::InsertTableFormula(formula) => {
                self.buffer.restore_cached_selection();
                if let Some(sel) = self.buffer.selection_range() {
                    let start = sel.start;
                    self.buffer.replace_range(sel, formula);
                    self.buffer
                        .set_cursor(start + formula.chars().count(), false);
                } else {
                    let pos = self.buffer.cursor_char;
                    self.buffer.insert(pos, formula);
                    self.buffer
                        .set_cursor(pos + formula.chars().count(), false);
                }
                self.editor_state.scroll_to_cursor = true;
                self.refocus_active_editor();
                self.exporter.status_text = format!("Inserted table formula {formula}");
            }
            ToolbarAction::CalculateTables => {
                self.recalculate_tables_at_cursor_or_all();
            }
            ToolbarAction::ToggleAutoCalcTables => {
                self.settings.auto_calc_tables = !self.settings.auto_calc_tables;
                self.persist_settings();
                self.exporter.status_text = if self.settings.auto_calc_tables {
                    "Table Auto-Calc: ON".to_string()
                } else {
                    "Table Auto-Calc: OFF".to_string()
                };
            }
            ToolbarAction::ToggleShowMarkup => {
                self.show_markup = !self.show_markup;
                self.settings.show_markup = self.show_markup;
                let _ = self.settings.save_to(&self.settings_path);
            }
            ToolbarAction::ToggleFindBar => {
                if self.find_bar.open {
                    self.find_bar.open = false;
                    self.refocus_active_editor();
                } else {
                    self.editor_state.request_focus = false;
                    self.reader_state.focus_inline_editor = false;
                    self.find_bar.open_find(&self.buffer);
                }
            }
            ToolbarAction::FocusFindBar => {
                self.editor_state.request_focus = false;
                self.reader_state.focus_inline_editor = false;
                self.find_bar.focus_or_open_find(&self.buffer);
            }
            ToolbarAction::OpenReplaceBar => {
                self.editor_state.request_focus = false;
                self.reader_state.focus_inline_editor = false;
                if read_only {
                    self.find_bar.open_find(&self.buffer);
                } else {
                    self.find_bar.open_replace(&self.buffer);
                }
            }
            ToolbarAction::ToggleOutline => {
                if self.outline_open && self.sidebar_tab == SidebarTab::Outline {
                    self.outline_open = false;
                } else {
                    self.sidebar_tab = SidebarTab::Outline;
                    self.outline_open = true;
                }
                self.settings.outline_open = self.outline_open;
                let _ = self.settings.save_to(&self.settings_path);
            }
            ToolbarAction::ToggleExplorer => {
                if self.outline_open && self.sidebar_tab == SidebarTab::Explorer {
                    self.outline_open = false;
                } else {
                    self.sidebar_tab = SidebarTab::Explorer;
                    self.outline_open = true;
                    if self.explorer_state.root_dir.is_none() {
                        if let Some(parent) = self.current_base_dir() {
                            self.explorer_state.set_root(parent);
                        }
                    }
                }
                self.settings.outline_open = self.outline_open;
                let _ = self.settings.save_to(&self.settings_path);
            }
            ToolbarAction::ToggleTheme => {
                let next_mode = match self.theme.mode {
                    ThemeMode::Light => ThemeMode::Sepia,
                    ThemeMode::Sepia => ThemeMode::Dark,
                    ThemeMode::Dark => ThemeMode::Light,
                };
                self.theme = WordTheme::from_mode(next_mode);
                self.theme.apply_to_ctx(ctx);
                self.settings.dark_mode = next_mode == ThemeMode::Dark;
                self.settings.theme_mode = next_mode;
                self.persist_settings();
            }
            ToolbarAction::SetThemeMode(mode) => {
                self.theme = WordTheme::from_mode(mode);
                self.theme.apply_to_ctx(ctx);
                self.settings.theme_mode = mode;
                self.settings.dark_mode = mode == ThemeMode::Dark;
                self.persist_settings();
                ctx.request_repaint();
            }
            ToolbarAction::SetPageWidth(pw) => {
                self.settings.page_width = pw;
                self.reader_state.page_width = pw;
                self.persist_settings();
                ctx.request_repaint();
            }
            ToolbarAction::SetLineSpacing(ls) => {
                self.settings.line_spacing = ls;
                self.reader_state.line_spacing = ls;
                self.persist_settings();
                ctx.request_repaint();
            }
            ToolbarAction::ToggleFocusMode => {
                self.focus_mode = !self.focus_mode;
                ctx.request_repaint();
            }
            ToolbarAction::ZoomIn => {
                let next = ((self.settings.zoom_factor + 0.1) * 100.0).round() / 100.0;
                self.settings.zoom_factor = next.clamp(0.6, 2.0);
                self.persist_settings();
            }
            ToolbarAction::ZoomOut => {
                let next = ((self.settings.zoom_factor - 0.1) * 100.0).round() / 100.0;
                self.settings.zoom_factor = next.clamp(0.6, 2.0);
                self.persist_settings();
            }
            ToolbarAction::ZoomReset => {
                self.settings.zoom_factor = 1.0;
                self.persist_settings();
            }
            ToolbarAction::SetViewMode(mode) => {
                self.autoscroll.cancel();
                self.reader_state.smooth_scroll_target_y = None;
                self.view_mode = mode;
                self.settings.view_mode = mode;
                self.persist_settings();
                if matches!(mode, ViewMode::Reader | ViewMode::Source) {
                    self.reader_state.active_block_lines = None;
                }
                if mode == ViewMode::Reader {
                    self.table_modal.open = false;
                    self.insert_link_modal.open = false;
                } else if matches!(mode, ViewMode::Source | ViewMode::Split) {
                    self.editor_state.request_focus = true;
                }
                if matches!(mode, ViewMode::Split | ViewMode::Reader | ViewMode::Wysiwyg) {
                    self.sync_ast_if_needed();
                }
                ctx.request_repaint();
            }
            ToolbarAction::OpenOsIntegrationModal => {
                self.os_modal.open_with_settings(&self.settings);
            }
        }

        if mutation_for_wysiwyg && self.view_mode == ViewMode::Wysiwyg {
            self.sync_ast_if_needed();
            let target_line = self.buffer.char_to_line(self.buffer.cursor_char);
            let needs_reactivate = self.reader_state.active_block_lines.as_ref().is_none_or(|r| {
                !r.contains(&target_line) || !block_index_contains_range(&self.block_index, r)
            });
            if needs_reactivate {
                let saved_anchor = self.buffer.selection_anchor;
                let saved_cursor = self.buffer.cursor_char;
                let saved_cached = self.buffer.cached_selection;
                activate_wysiwyg_at_line(
                    &mut self.buffer,
                    &self.block_index,
                    target_line,
                    self.show_markup,
                    &mut self.reader_state,
                );
                self.buffer.selection_anchor = saved_anchor;
                self.buffer.cursor_char = saved_cursor;
                self.buffer.cached_selection = saved_cached;
                self.reader_state.sync_cursor_from_buf = true;
                self.reader_state.focus_inline_editor = true;
            }
            ctx.request_repaint();
        }
    }

    fn render_left_sidebar(&mut self, ctx: &egui::Context) {
        if !self.outline_open {
            return;
        }
        let mut pending_open: Option<PathBuf> = None;
        let mut pending_root: Option<PathBuf> = None;
        let mut pick_folder = false;

        egui::SidePanel::left("document_outline_panel")
            .resizable(true)
            .default_width(245.0)
            .min_width(180.0)
            .max_width(380.0)
            .frame(
                egui::Frame::new()
                    .fill(self.theme.ribbon_bg)
                    .stroke(Stroke::new(1.0_f32, self.theme.border))
                    .inner_margin(Margin::symmetric(10, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
                    for (tab, label) in [
                        (SidebarTab::Explorer, "Explorer"),
                        (SidebarTab::Outline, "Outline"),
                    ] {
                        let active = self.sidebar_tab == tab;
                        let btn = egui::Button::new(
                            RichText::new(label)
                                .size(11.5)
                                .strong()
                                .color(if active {
                                    self.theme.accent
                                } else {
                                    self.theme.text_muted
                                }),
                        )
                        .fill(if active {
                            self.theme.accent_soft
                        } else {
                            egui::Color32::TRANSPARENT
                        })
                        .stroke(Stroke::new(
                            1.0_f32,
                            if active {
                                self.theme.accent
                            } else {
                                self.theme.border_subtle
                            },
                        ))
                        .corner_radius(CornerRadius::same(6));
                        let r = ui.add(btn);
                        if r.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if r.clicked() {
                            self.sidebar_tab = tab;
                            if tab == SidebarTab::Explorer
                                && self.explorer_state.root_dir.is_none()
                            {
                                if let Some(parent) = self.current_base_dir() {
                                    self.explorer_state.set_root(parent);
                                }
                            }
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            self.outline_open = false;
                            self.settings.outline_open = false;
                            self.persist_settings();
                        }
                    });
                });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);

                match self.sidebar_tab {
                    SidebarTab::Explorer => {
                        match render_explorer_panel(
                            ui,
                            &mut self.explorer_state,
                            self.buffer.file_path.as_deref(),
                            &self.theme,
                        ) {
                            ExplorerAction::None => {}
                            ExplorerAction::OpenFile(path) => pending_open = Some(path),
                            ExplorerAction::SetRoot(root) => pending_root = Some(root),
                            ExplorerAction::OpenFolderDialog => pick_folder = true,
                        }
                    }
                    SidebarTab::Outline => {
                        self.render_outline_contents(ui);
                    }
                }
            });

        if pick_folder {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                pending_root = Some(dir);
            }
        }
        if let Some(root) = pending_root {
            self.explorer_state.set_root(root.clone());
            self.settings.explorer_root = Some(root);
            self.persist_settings();
            ctx.request_repaint();
        }
        if let Some(path) = pending_open {
            self.open_specific_file(&path);
            ctx.request_repaint();
        }
    }

    fn render_outline_contents(&mut self, ui: &mut egui::Ui) {
        if self.outline_items.is_empty() {
            ui.label(
                RichText::new("Add # headings to build an outline.")
                    .size(12.0)
                    .italics()
                    .color(self.theme.text_muted),
            );
            return;
        }

        let has_any_children = self
            .outline_items
            .windows(2)
            .any(|w| w[1].level > w[0].level);
        let filter_empty = self.outline_filter.trim().is_empty();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
            let extra_w = if filter_empty { 0.0 } else { 22.0 }
                + if has_any_children && filter_empty {
                    24.0
                } else {
                    0.0
                };
            let edit_w = (ui.available_width() - extra_w - 4.0).max(80.0);
            ui.add_sized(
                Vec2::new(edit_w, 24.0),
                egui::TextEdit::singleline(&mut self.outline_filter)
                    .hint_text("Filter headings...")
                    .margin(Margin::symmetric(6, 3)),
            );
            if !filter_empty {
                let clr = ui.small_button("×").on_hover_text("Clear outline filter");
                if clr.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if clr.clicked() {
                    self.outline_filter.clear();
                }
            }
            if has_any_children && filter_empty {
                let all_collapsed = (0..self.outline_items.len().saturating_sub(1))
                    .filter(|&i| self.outline_items[i + 1].level > self.outline_items[i].level)
                    .all(|i| {
                        self.collapsed_outline_lines
                            .contains(&self.outline_items[i].source_line)
                    });
                let (lbl, tip) = if all_collapsed {
                    ("+", "Expand all outline sections")
                } else {
                    ("−", "Collapse all outline sections")
                };
                let tog = ui.small_button(lbl).on_hover_text(tip);
                if tog.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if tog.clicked() {
                    if all_collapsed {
                        self.collapsed_outline_lines.clear();
                    } else {
                        for i in 0..self.outline_items.len().saturating_sub(1) {
                            if self.outline_items[i + 1].level > self.outline_items[i].level {
                                self.collapsed_outline_lines
                                    .insert(self.outline_items[i].source_line);
                            }
                        }
                    }
                }
            }
        });
        ui.add_space(6.0);

        let visible = visible_outline_indices(
            &self.outline_items,
            &self.outline_filter,
            &self.collapsed_outline_lines,
        );
        if visible.is_empty() {
            ui.label(
                RichText::new("No matching headings.")
                    .size(12.0)
                    .italics()
                    .color(self.theme.text_muted),
            );
            return;
        }

        let cur_line = self.buffer.char_to_line(self.buffer.cursor_char);
        let use_scroll_spy = matches!(self.view_mode, ViewMode::Reader)
            || (self.view_mode == ViewMode::Wysiwyg
                && self.reader_state.active_block_lines.is_none());
        let probe_y = self.reader_state.last_scroll_offset_y + 72.0;
        let active_idx = find_active_outline_index(
            &self.outline_items,
            use_scroll_spy,
            probe_y,
            cur_line,
            &self.block_index,
        );

        let show_chevrons = self.outline_filter.trim().is_empty() && has_any_children;
        let mut toggled_collapse_line: Option<usize> = None;
        let mut clicked_target: Option<(usize, f32)> = None;

        let scroll = egui::ScrollArea::vertical().id_salt("outline_scroll");
        if visible.len() > 200 {
            scroll.show_rows(ui, 24.0, visible.len(), |ui, row_range| {
                for &(item_idx, has_children, is_collapsed) in &visible[row_range] {
                    let item = &self.outline_items[item_idx];
                    let (toggled, clicked) = render_outline_row(
                        ui,
                        item,
                        active_idx == Some(item_idx),
                        has_children,
                        is_collapsed,
                        show_chevrons,
                        &self.theme,
                    );
                    if toggled {
                        toggled_collapse_line = Some(item.source_line);
                    }
                    if clicked {
                        clicked_target = Some((item.source_line, item.y_start));
                    }
                }
            });
        } else {
            scroll.show(ui, |ui| {
                for &(item_idx, has_children, is_collapsed) in &visible {
                    let item = &self.outline_items[item_idx];
                    let (toggled, clicked) = render_outline_row(
                        ui,
                        item,
                        active_idx == Some(item_idx),
                        has_children,
                        is_collapsed,
                        show_chevrons,
                        &self.theme,
                    );
                    if toggled {
                        toggled_collapse_line = Some(item.source_line);
                    }
                    if clicked {
                        clicked_target = Some((item.source_line, item.y_start));
                    }
                }
            });
        }

        if let Some(line) = toggled_collapse_line {
            if !self.collapsed_outline_lines.remove(&line) {
                self.collapsed_outline_lines.insert(line);
            }
        }
        if let Some((source_line, fallback_y)) = clicked_target {
            self.reader_state.active_block_lines = None;
            let c = self.buffer.line_to_char(source_line);
            self.buffer.set_cursor(c, false);
            self.editor_state.scroll_to_cursor = true;
            let resolved = self.block_index.block_for_source_line(source_line);
            let target_y = resolved
                .map(|idx| (self.block_index.block_y_start(idx) - 16.0).max(0.0))
                .unwrap_or(fallback_y);
            self.reader_state.smooth_scroll_to(target_y);
            if let Some(b) = resolved.and_then(|idx| self.block_index.blocks.get(idx)) {
                let now = ui.input(|i| i.time);
                self.reader_state.highlight_block_lines =
                    Some((b.block.source_lines().clone(), now + 1.4));
            }
        }
    }
}

impl eframe::App for MarkdownApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_ui(ctx);

        static SCREENSHOT_OUT: OnceLock<Option<PathBuf>> = OnceLock::new();
        let out_opt = SCREENSHOT_OUT.get_or_init(|| {
            std::env::var_os("MD_STUDIO_SCREENSHOT_OUT")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        });
        if let Some(out_path) = out_opt {
            if let Some(img) = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            }) {
                if let Some(parent) = out_path.parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                }
                let _ = image::save_buffer_with_format(
                    out_path,
                    img.as_raw(),
                    img.width() as u32,
                    img.height() as u32,
                    image::ColorType::Rgba8,
                    image::ImageFormat::Png,
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                static FRAME_COUNT: std::sync::atomic::AtomicU32 =
                    std::sync::atomic::AtomicU32::new(0);
                let frame = FRAME_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if frame == 8 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::UserData::default(),
                    ));
                }
                if frame < 20 {
                    ctx.request_repaint();
                }
            }
        }
    }
}

impl MarkdownApp {
    /// Saves PNG bytes into `assets/` (or temp dir for untitled buffers) and inserts `![pasted-image](<url>)`.
    /// Returns `false` in read-only `ViewMode::Reader` or when writing fails.
    pub fn paste_image_png_bytes(&mut self, png_bytes: &[u8]) -> bool {
        if self.view_mode == ViewMode::Reader || png_bytes.is_empty() {
            return false;
        }
        match crate::document::clipboard::save_pasted_image_png(
            png_bytes,
            self.buffer.file_path.as_deref(),
        ) {
            Ok((_saved_path, url)) => {
                insert_image(&mut self.buffer, "pasted-image", &url);
                self.reader_state.active_block_lines = None;
                self.editor_state.scroll_to_cursor = true;
                self.sync_ast_if_needed();
                self.refocus_active_editor();
                self.exporter.status_text = format!("Pasted image -> {url}");
                true
            }
            Err(err) => {
                self.exporter.status_text = format!("Failed to paste image: {err}");
                false
            }
        }
    }

    fn sync_after_find_jump(&mut self) {
        self.sync_ast_if_needed();
        self.editor_state.scroll_to_cursor = true;
        let target_line = self.buffer.char_to_line(self.buffer.cursor_char);
        if self.view_mode == ViewMode::Wysiwyg
            && !activate_wysiwyg_preserving_selection(
                &self.buffer,
                &self.block_index,
                target_line,
                self.show_markup,
                &mut self.reader_state,
            )
        {
            self.reader_state.active_block_lines = None;
        }
        if let Some(blk_idx) = self.block_index.block_for_source_line(target_line) {
            if blk_idx < self.block_index.blocks.len() {
                self.reader_state.scroll_to_y =
                    Some((self.block_index.block_y_start(blk_idx) - 36.0).max(0.0));
            }
        }
    }

    /// Attaches a custom `InstanceMessage` receiver (primarily for headless UI tests).
    pub fn attach_instance_receiver(&mut self, rx: mpsc::Receiver<instance::InstanceMessage>) {
        self.instance_rx = Some(rx);
    }

    /// Attaches a paired `SingleInstanceServer` + receiver for headless cross-window tests.
    pub fn attach_instance_server(
        &mut self,
        server: instance::SingleInstanceServer,
        rx: mpsc::Receiver<instance::InstanceMessage>,
    ) {
        self.instance_server = Some(server);
        self.instance_rx = Some(rx);
    }

    fn pointer_screen_pos(&self, ctx: &egui::Context, local_pos: egui::Pos2) -> Option<egui::Pos2> {
        let origin = self.test_viewport_origin.or_else(|| {
            ctx.input(|i| {
                i.viewport()
                    .inner_rect
                    .map(|r| r.min)
                    .or_else(|| i.viewport().outer_rect.map(|r| r.min))
            })
        })?;
        Some(origin + local_pos.to_vec2())
    }

    fn adopt_handoff_file(&mut self, handoff_path: &Path) {
        let Ok(handoff) = instance::read_and_remove_handoff_file(handoff_path) else {
            return;
        };
        if let (Some(ref path), false) = (&handoff.file_path, handoff.dirty) {
            self.open_specific_file(path);
            return;
        }
        let display_name = handoff
            .file_path
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled.md".to_string());
        let new_buf = DocumentBuffer::from_handoff(
            &handoff.content,
            handoff.file_path.clone(),
            handoff.dirty,
        );
        let reuse_initial_blank = self.tabs.len() == 1
            && self.buffer.file_path.is_none()
            && self.buffer.is_empty()
            && !self.buffer.dirty;
        if reuse_initial_blank {
            self.buffer = new_buf;
            self.reset_state_for_new_buffer();
            self.tabs[self.active_tab].id = self.doc_id;
            self.tabs[self.active_tab].untitled_num =
                if self.buffer.file_path.is_some() { 0 } else { 1 };
            self.sync_ast_if_needed();
            self.refocus_active_editor();
        } else {
            let untitled_num = if new_buf.file_path.is_some() {
                0
            } else {
                self.alloc_untitled_num()
            };
            self.open_buffer_in_new_tab(new_buf, untitled_num);
            self.refocus_active_editor();
        }
        if self.explorer_state.root_dir.is_none() {
            if let Some(parent) = self.buffer.file_path.as_deref().and_then(Path::parent) {
                if !parent.as_os_str().is_empty() {
                    self.explorer_state.set_root(parent.to_path_buf());
                }
            }
        }
        self.exporter.status_text = format!("Moved {display_name} into this window");
    }

    pub fn update_ui(&mut self, ctx: &egui::Context) {
        let mut incoming_msgs = Vec::new();
        if let Some(rx) = &self.instance_rx {
            while let Ok(msg) = rx.try_recv() {
                incoming_msgs.push(msg);
            }
        }
        if !incoming_msgs.is_empty() {
            for msg in incoming_msgs {
                match msg {
                    instance::InstanceMessage::OpenFile(path) => self.open_specific_file(&path),
                    instance::InstanceMessage::AdoptHandoff(path) => self.adopt_handoff_file(&path),
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }

        if let Some(server) = &self.instance_server {
            let (outer_rect_opt, minimized) = ctx.input(|i| {
                let vp = i.viewport();
                (vp.outer_rect, vp.minimized.unwrap_or(false))
            });
            if let Some(outer_rect) = outer_rect_opt {
                let changed = match self.last_published_window_bounds {
                    Some((prev_rect, prev_min)) => {
                        prev_min != minimized
                            || (prev_rect.min - outer_rect.min).length_sq() >= 4.0
                            || (prev_rect.max - outer_rect.max).length_sq() >= 4.0
                    }
                    None => true,
                };
                if changed {
                    server.update_window_bounds(outer_rect, minimized);
                    self.last_published_window_bounds = Some((outer_rect, minimized));
                }
            }
        }

        let mut gained_focus = false;
        if let Some(is_focused) = ctx.input(|i| i.viewport().focused) {
            if is_focused && !self.was_window_focused {
                gained_focus = true;
                if let Some(server) = &self.instance_server {
                    server.claim_active_target();
                }
            }
            self.was_window_focused = is_focused;
        }
        if gained_focus || self.last_disk_check.elapsed() >= Duration::from_secs(2) {
            self.check_external_file_changes();
        }
        if self.last_recovery_save.elapsed() >= Duration::from_secs(20) {
            let has_unsaved_or_open = self.tabs.iter().enumerate().any(|(i, slot)| {
                if i == self.active_tab {
                    self.buffer.dirty || (self.buffer.file_path.is_none() && !self.buffer.is_empty())
                } else if let Some(ref st) = slot.state {
                    st.buffer.dirty || (st.buffer.file_path.is_none() && !st.buffer.is_empty())
                } else {
                    false
                }
            });
            if has_unsaved_or_open {
                let key = self.current_session_snapshot_key();
                if key != self.last_recovery_snapshot_key {
                    let job = self.build_session_save_job(true);
                    self.session_saver.enqueue(job);
                    self.last_recovery_snapshot_key = key;
                }
            }
            self.last_recovery_save = Instant::now();
        }

        if let Some(rx) = &self.os_sync_rx {
            match rx.try_recv() {
                Ok(exe_path) => {
                    self.exporter.status_text =
                        format!("Synced Windows menu path -> {}", exe_path.display());
                    self.os_sync_rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.os_sync_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        if (self.settings.zoom_factor - self.last_applied_zoom).abs() > 0.001 {
            ctx.set_zoom_factor(self.settings.zoom_factor);
            self.last_applied_zoom = self.settings.zoom_factor;
        }

        let active_untitled = self
            .tabs
            .get(self.active_tab)
            .map(|s| s.untitled_num)
            .unwrap_or(0);
        let title_unchanged = matches!(
            &self.last_window_title_key,
            Some((id, dirty, untitled, path))
                if *id == self.doc_id
                    && *dirty == self.buffer.dirty
                    && *untitled == active_untitled
                    && path.as_deref() == self.buffer.file_path.as_deref()
        );
        if !title_unchanged {
            let dirty_mark = if self.buffer.dirty { "● " } else { "" };
            let title = format!(
                "{}{} — Markdown Studio v{}",
                dirty_mark,
                self.doc_display_name(),
                crate::APP_VERSION
            );
            self.last_window_title_key = Some((
                self.doc_id,
                self.buffer.dirty,
                active_untitled,
                self.buffer.file_path.clone(),
            ));
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            if self.pending_zoom_save_at.take().is_some() {
                self.persist_settings();
            }
            if !self.allow_close_now && self.has_any_dirty_tab() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if !self.unsaved_modal.is_open() {
                    if let Some(dirty_idx) = self.first_dirty_tab_index() {
                        self.switch_to_tab(dirty_idx);
                    }
                    self.unsaved_modal.open(PendingDiscardAction::CloseWindow);
                }
            } else if !self.allow_close_now {
                self.save_session_internal(false);
            }
        }

        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        for dropped in dropped_files {
            if let Some(path) = dropped.path {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_default();
                let is_raster_image = matches!(
                    ext.as_str(),
                    "png" | "jpg" | "jpeg" | "gif" | "tiff" | "tif"
                );
                if is_raster_image {
                    if self.view_mode != ViewMode::Reader {
                        let alt = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "image".to_string());
                        let url = self.format_image_insert_path(&path);
                        insert_image(&mut self.buffer, &alt, &url);
                        self.reader_state.active_block_lines = None;
                        self.editor_state.scroll_to_cursor = true;
                        self.sync_ast_if_needed();
                        self.refocus_active_editor();
                        self.exporter.status_text = format!("Inserted image {}", path.display());
                    }
                } else if DocumentBuffer::is_likely_text_file(&path) {
                    self.open_specific_file(&path);
                } else {
                    self.exporter.status_text =
                        format!("Unsupported binary file: {}", path.display());
                }
            }
        }

        if !self.font_rxs.is_empty() {
            let mut newly_loaded = 0u8;
            self.font_rxs.retain_mut(|rx| match rx.try_recv() {
                Ok(bits) => {
                    newly_loaded |= bits;
                    false
                }
                Err(mpsc::TryRecvError::Empty) => true,
                Err(mpsc::TryRecvError::Disconnected) => false,
            });
            if newly_loaded != 0 {
                self.pending_font_groups &= !newly_loaded;
                self.loaded_font_groups |= newly_loaded;
                ctx.set_fonts(build_fonts_with_loaded_groups(self.loaded_font_groups));
                ctx.request_repaint();
            }
        }
        let doc_buf_key = (self.doc_id, self.buffer.buf_id());
        let font_ver = (self.doc_id, self.buffer.buf_id(), self.buffer.version);
        if self.font_scan_version != font_ver {
            self.font_scan_version = font_ver;
            let already_covered = self.loaded_font_groups | self.pending_font_groups;
            let missing = if self.font_scanned_buf_key != doc_buf_key {
                self.font_scanned_buf_key = doc_buf_key;
                detect_needed_font_groups(self.buffer.rope(), already_covered)
            } else {
                detect_needed_font_groups_around_cursor(&self.buffer, already_covered)
            };
            if missing != 0 {
                let cached_bits = cached_font_groups_mask(missing);
                if cached_bits != 0 {
                    self.loaded_font_groups |= cached_bits;
                    ctx.set_fonts(build_fonts_with_loaded_groups(self.loaded_font_groups));
                    ctx.request_repaint();
                }
                let uncached = missing & !cached_bits;
                if uncached != 0 {
                    self.pending_font_groups |= uncached;
                    self.font_rxs.push(spawn_lazy_font_loader(uncached, ctx.clone()));
                }
            }
        }

        let events = self.exporter.poll();
        if !events.is_empty() {
            ctx.request_repaint();
        } else if self.exporter.busy || self.print_modal.loading_printers {
            // Workers request immediate wakeup via Context; keep a gentle fallback timer.
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        let modal_before_toolbar = self.table_modal.open
            || self.unsaved_modal.is_open()
            || self.print_modal.open
            || self.insert_link_modal.open
            || self.os_modal.open
            || self.external_diff_modal.open;
        if !modal_before_toolbar {
            let mut zoom_delta = 0.0_f32;
            let mut pinch_factor = 1.0_f32;
            let mut consumed_wheel = false;
            ctx.input_mut(|i| {
                let mod_ctrl = i.modifiers.ctrl || i.modifiers.command || i.modifiers.mac_cmd;
                i.events.retain(|ev| match ev {
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                    } if (mod_ctrl || modifiers.ctrl || modifiers.command || modifiers.mac_cmd)
                        && delta.y.abs() > 0.0 =>
                    {
                        let step = match unit {
                            egui::MouseWheelUnit::Line => delta.y.clamp(-1.0, 1.0) * 0.10,
                            egui::MouseWheelUnit::Point => (delta.y / 400.0).clamp(-0.15, 0.15),
                            egui::MouseWheelUnit::Page => delta.y.signum() * 0.15,
                        };
                        zoom_delta += step;
                        consumed_wheel = true;
                        false
                    }
                    egui::Event::Zoom(factor) if (*factor - 1.0).abs() > 0.0001 => {
                        pinch_factor *= *factor;
                        false
                    }
                    _ => true,
                });
                if consumed_wheel {
                    i.smooth_scroll_delta = Vec2::ZERO;
                    i.raw_scroll_delta = Vec2::ZERO;
                }
            });
            if zoom_delta.abs() > 0.0001 || (pinch_factor - 1.0).abs() > 0.0001 {
                let mut new_zoom = self.settings.zoom_factor;
                if zoom_delta.abs() > 0.0001 {
                    new_zoom = ((new_zoom + zoom_delta) * 100.0).round() / 100.0;
                }
                if (pinch_factor - 1.0).abs() > 0.0001 {
                    new_zoom = ((new_zoom * pinch_factor) * 100.0).round() / 100.0;
                }
                new_zoom = new_zoom.clamp(0.6, 2.0);
                if (new_zoom - self.settings.zoom_factor).abs() > 0.001 {
                    self.settings.zoom_factor = new_zoom;
                    ctx.set_zoom_factor(new_zoom);
                    self.last_applied_zoom = new_zoom;
                    self.pending_zoom_save_at = Some(ctx.input(|i| i.time) + 0.35);
                    ctx.request_repaint();
                }
            }
        }
        let now = ctx.input(|i| i.time);
        if let Some(save_at) = self.pending_zoom_save_at {
            if now >= save_at {
                self.pending_zoom_save_at = None;
                self.persist_settings();
            } else {
                ctx.request_repaint_after(Duration::from_millis(120));
            }
        }

        let paste_image_hotkey = !modal_before_toolbar
            && self.view_mode != ViewMode::Reader
            && !self.toolbar_state.renaming_file
            && !self.find_bar.has_input_focus(ctx)
            && ctx.input(|i| {
                let has_paste_ev = i
                    .events
                    .iter()
                    .any(|ev| matches!(ev, egui::Event::Paste(_)));
                let cmd_v = i.events.iter().any(|ev| {
                    matches!(
                        ev,
                        egui::Event::Key {
                            key: Key::V,
                            pressed: true,
                            modifiers,
                            ..
                        } if (modifiers.command || modifiers.ctrl || modifiers.mac_cmd)
                            && !modifiers.shift
                            && !modifiers.alt
                    )
                });
                !has_paste_ev && cmd_v
            });
        if paste_image_hotkey
            && crate::document::clipboard::read_os_clipboard_plain_text().is_none()
        {
            if let Some(png_bytes) = crate::document::clipboard::read_os_clipboard_image_png() {
                if self.paste_image_png_bytes(&png_bytes) {
                    ctx.input_mut(|i| {
                        i.events.retain(|ev| {
                            !matches!(
                                ev,
                                egui::Event::Key {
                                    key: Key::V,
                                    pressed: true,
                                    modifiers,
                                    ..
                                } if (modifiers.command || modifiers.ctrl || modifiers.mac_cmd)
                                    && !modifiers.shift
                            )
                        });
                    });
                }
            }
        }

        let shortcut_action = self.handle_shortcuts(ctx);
        if shortcut_action != ToolbarAction::None {
            self.dispatch_action(ctx, shortcut_action);
        }

        self.sync_ast_if_needed();

        let active_fmt_snapshot = if self.view_mode == ViewMode::Reader {
            ActiveFormatState::default()
        } else {
            let fmt_key = (
                self.doc_id,
                self.buffer.version,
                self.buffer.cursor_char,
                self.buffer.selection_anchor,
            );
            if self.active_format_cache.0 != fmt_key {
                self.active_format_cache = (fmt_key, query_active_format(&self.buffer));
            }
            self.active_format_cache.1.clone()
        };

        let reading_progress = if self.view_mode != ViewMode::Source
            && self.reader_state.last_max_scroll_y > 20.0
        {
            Some(
                (self.reader_state.last_scroll_offset_y / self.reader_state.last_max_scroll_y)
                    .clamp(0.0, 1.0),
            )
        } else {
            None
        };
        let reading_info = ReadingToolbarInfo {
            page_width: self.settings.page_width,
            line_spacing: self.settings.line_spacing,
            focus_mode: self.focus_mode,
            reading_progress,
        };
        let calc_info = CalcToolbarInfo {
            auto_calc_math: self.settings.auto_calc_math,
            auto_calc_tables: self.settings.auto_calc_tables,
        };

        if !self.focus_mode {
            // Top Word-style Two-Tier Header & Formatting Ribbon (or Reading Comfort Bar)
            egui::TopBottomPanel::top("word_toolbar_panel")
                .frame(egui::Frame::new().fill(self.theme.ribbon_bg))
                .show(ctx, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    let explorer_active =
                        self.outline_open && self.sidebar_tab == SidebarTab::Explorer;
                    let outline_active =
                        self.outline_open && self.sidebar_tab == SidebarTab::Outline;
                    let action = render_toolbar(
                        ui,
                        &self.buffer,
                        self.view_mode,
                        explorer_active,
                        outline_active,
                        self.show_markup,
                        &self.settings.recent_files,
                        self.settings.zoom_factor,
                        reading_info,
                        calc_info,
                        &active_fmt_snapshot,
                        &self.theme,
                        self.exporter.busy,
                        &mut self.toolbar_state,
                    );
                    if action != ToolbarAction::None {
                        self.dispatch_action(ctx, action);
                        self.sync_ast_if_needed();
                    }
                    let read_only = self.view_mode == ViewMode::Reader;
                    let find_was_open = self.find_bar.open;
                    let find_jumped =
                        self.find_bar
                            .show(ui, &mut self.buffer, &self.theme, read_only);
                    if find_was_open && !self.find_bar.open {
                        self.refocus_active_editor();
                    }
                    if find_jumped {
                        self.sync_after_find_jump();
                    }
                });
            let active_tab = self.active_tab;
            let active_buf = &self.buffer;
            let tabs_meta: Vec<TabVisualInfo<'_>> = self
                .tabs
                .iter()
                .enumerate()
                .map(|(i, slot)| {
                    let (full_path, dirty) = if i == active_tab {
                        (active_buf.file_path.as_deref(), active_buf.dirty)
                    } else if let Some(ref st) = slot.state {
                        (st.buffer.file_path.as_deref(), st.buffer.dirty)
                    } else {
                        (None, false)
                    };
                    let title = full_path
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| Self::untitled_display_name(slot.untitled_num));
                    TabVisualInfo {
                        id: slot.id,
                        title,
                        full_path,
                        dirty,
                    }
                })
                .collect();
            let over_peer_window = if self.tab_bar_state.dragging_tab_idx.is_some() {
                if self.cached_peer_windows.is_none() {
                    if let Some(server) = &self.instance_server {
                        self.cached_peer_windows = Some(server.list_peer_windows());
                    }
                }
                let local_pos = ctx
                    .input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()))
                    .unwrap_or(self.tab_bar_state.last_pointer_pos);
                let screen_pos = self.pointer_screen_pos(ctx, local_pos);
                match (screen_pos, self.cached_peer_windows.as_ref()) {
                    (Some(pos), Some(peers)) => peers.iter().any(|p| p.outer_rect.contains(pos)),
                    _ => false,
                }
            } else {
                false
            };

            let tab_action = egui::TopBottomPanel::top("document_tab_bar")
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| {
                    render_tab_bar(
                        ui,
                        &tabs_meta,
                        self.active_tab,
                        &self.theme,
                        &mut self.tab_bar_state,
                        over_peer_window,
                    )
                })
                .inner;
            match tab_action {
                TabBarAction::None => {}
                TabBarAction::Select(idx) => {
                    self.switch_to_tab(idx);
                    ctx.request_repaint();
                }
                TabBarAction::Close(idx) => {
                    self.request_close_tab(ctx, idx);
                    ctx.request_repaint();
                }
                TabBarAction::NewTab => {
                    self.new_tab(ctx);
                }
                TabBarAction::DetachToNewWindow(idx) => {
                    self.detach_tab_to_new_window(ctx, idx);
                    ctx.request_repaint();
                }
                TabBarAction::Reorder { from, to } => {
                    self.reorder_tab(from, to);
                    ctx.request_repaint();
                }
            }
            if self.tab_bar_state.dragging_tab_idx.is_none() {
                self.cached_peer_windows = None;
            }
        } else {
            self.tab_bar_state.clear();
            self.cached_peer_windows = None;
            if self.find_bar.open {
                egui::TopBottomPanel::top("focus_find_panel")
                    .frame(egui::Frame::new().fill(self.theme.ribbon_bg))
                    .show(ctx, |ui| {
                        let read_only = self.view_mode == ViewMode::Reader;
                        let find_was_open = self.find_bar.open;
                        let find_jumped =
                            self.find_bar
                                .show(ui, &mut self.buffer, &self.theme, read_only);
                        if find_was_open && !self.find_bar.open {
                            self.refocus_active_editor();
                        }
                        if find_jumped {
                            self.sync_after_find_jump();
                        }
                    });
            }
        }

        let modal_open_now = self.table_modal.open
            || self.unsaved_modal.is_open()
            || self.print_modal.open
            || self.insert_link_modal.open
            || self.os_modal.open
            || self.external_diff_modal.open;
        if self.reader_state.image_lightbox.is_some()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            self.reader_state.image_lightbox = None;
            ctx.request_repaint();
        } else if !modal_open_now && !self.autoscroll.active && self.find_bar.open {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
                self.find_bar.open = false;
                self.refocus_active_editor();
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, Key::F3)) {
                if self.find_bar.find_prev(&mut self.buffer) {
                    self.sync_after_find_jump();
                }
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F3))
                && self.find_bar.find_next(&mut self.buffer)
            {
                self.sync_after_find_jump();
            }
        } else if !modal_open_now
            && !self.autoscroll.active
            && self.focus_mode
            && self.reader_state.active_block_lines.is_none()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            self.focus_mode = false;
            ctx.request_repaint();
        }

        if self.word_count_cache.0 != self.doc_id || self.word_count_cache.1 != self.buffer.version
        {
            self.word_count_cache = (self.doc_id, self.buffer.version, self.buffer.word_count());
        }

        if !self.focus_mode {
            // Bottom Status & Background Progress Bar
            let mut goals_changed = false;
            egui::TopBottomPanel::bottom("status_bar_panel")
                .frame(egui::Frame::new().fill(self.theme.ribbon_bg))
                .show(ctx, |ui| {
                    goals_changed = render_status_bar(
                        ui,
                        &self.buffer,
                        self.view_mode,
                        self.buffer.word_count(),
                        self.settings.zoom_factor,
                        reading_progress,
                        &mut self.settings.word_goal,
                        &mut self.settings.char_goal,
                        &mut self.goal_state,
                        &self.exporter,
                        &self.theme,
                    );
                });
            if goals_changed {
                self.persist_settings();
            }
        } else {
            let mut focus_action = ToolbarAction::None;
            let theme_mode = self.theme.mode;
            let outline_open = self.outline_open;
            let zoom_pct = format!("{}%", (self.settings.zoom_factor * 100.0).round() as usize);
            egui::Area::new(egui::Id::new("focus_mode_controls"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 12.0))
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(self.theme.paper_bg)
                        .stroke(Stroke::new(1.0_f32, self.theme.border))
                        .corner_radius(CornerRadius::same(16))
                        .inner_margin(Margin::symmetric(10, 5))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
                                for (tm, lbl) in [
                                    (ThemeMode::Light, "Light"),
                                    (ThemeMode::Sepia, "Sepia"),
                                    (ThemeMode::Dark, "Dark"),
                                ] {
                                    let active = theme_mode == tm;
                                    let tip = format!("Switch to {lbl} theme");
                                    if vivid_button(
                                        ui,
                                        RichText::new(lbl).size(11.5).color(if active {
                                            self.theme.accent
                                        } else {
                                            self.theme.text_muted
                                        }),
                                        Vec2::ZERO,
                                        10,
                                        active,
                                        true,
                                        ToolbarButtonKind::Subtle,
                                        &tip,
                                        &self.theme,
                                    )
                                    .clicked()
                                    {
                                        focus_action = ToolbarAction::SetThemeMode(tm);
                                    }
                                }
                                ui.separator();
                                if vivid_button(
                                    ui,
                                    RichText::new("Outline").size(11.5).color(if outline_open {
                                        self.theme.accent
                                    } else {
                                        self.theme.text_primary
                                    }),
                                    Vec2::ZERO,
                                    10,
                                    outline_open,
                                    true,
                                    ToolbarButtonKind::Subtle,
                                    "Toggle Document Outline panel",
                                    &self.theme,
                                )
                                .clicked()
                                {
                                    focus_action = ToolbarAction::ToggleOutline;
                                }
                                ui.label(
                                    RichText::new(zoom_pct)
                                        .size(11.5)
                                        .color(self.theme.text_muted),
                                );
                                if vivid_button(
                                    ui,
                                    RichText::new("Exit Focus (F11)")
                                        .size(11.5)
                                        .strong()
                                        .color(self.theme.accent),
                                    Vec2::ZERO,
                                    10,
                                    true,
                                    true,
                                    ToolbarButtonKind::AccentSoft,
                                    "Exit Focus Mode (F11 / Esc)",
                                    &self.theme,
                                )
                                .clicked()
                                {
                                    focus_action = ToolbarAction::ToggleFocusMode;
                                }
                            });
                        });
                });
            if focus_action != ToolbarAction::None {
                self.dispatch_action(ctx, focus_action);
            }
        }

        if self.external_change_detected {
            let banner_fill = if self.theme.dark {
                egui::Color32::from_rgb(66, 48, 18)
            } else {
                egui::Color32::from_rgb(255, 245, 214)
            };
            let mut reload_now = false;
            let mut dismiss_banner = false;
            let mut open_diff = false;
            egui::TopBottomPanel::top("external_change_banner")
                .frame(
                    egui::Frame::new()
                        .fill(banner_fill)
                        .stroke(Stroke::new(1.0_f32, self.theme.warning_amber))
                        .inner_margin(Margin::symmetric(12, 5)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);
                        ui.label(
                            RichText::new("⚠ This file was modified outside Markdown Studio.")
                                .size(12.5)
                                .strong()
                                .color(self.theme.warning_amber),
                        );
                        if vivid_button(
                            ui,
                            RichText::new("Compare Diff")
                                .size(12.0)
                                .color(self.theme.text_primary),
                            Vec2::ZERO,
                            6,
                            false,
                            true,
                            ToolbarButtonKind::Secondary,
                            "Preview line-by-line differences between local edits and disk file",
                            &self.theme,
                        )
                        .clicked()
                        {
                            open_diff = true;
                        }
                        if vivid_button(
                            ui,
                            RichText::new("Reload from Disk")
                                .size(12.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                            Vec2::ZERO,
                            6,
                            true,
                            true,
                            ToolbarButtonKind::Primary,
                            "Discard unsaved changes and load updated file from disk",
                            &self.theme,
                        )
                        .clicked()
                        {
                            reload_now = true;
                        }
                        if vivid_button(
                            ui,
                            RichText::new("Keep Local Changes")
                                .size(12.0)
                                .color(self.theme.text_primary),
                            Vec2::ZERO,
                            6,
                            false,
                            true,
                            ToolbarButtonKind::Secondary,
                            "Ignore external disk change and keep editing current buffer",
                            &self.theme,
                        )
                        .clicked()
                        {
                            dismiss_banner = true;
                        }
                    });
                });
            if open_diff {
                if let Some(path) = self.buffer.file_path.as_deref() {
                    self.external_diff_modal.open_for(&self.buffer, path);
                }
            }
            if reload_now {
                self.external_diff_modal.open = false;
                if self.reload_active_file_from_disk() {
                    self.exporter.status_text = "Reloaded file from disk".to_string();
                }
            } else if dismiss_banner {
                self.external_diff_modal.open = false;
                self.refresh_disk_stamp();
                self.exporter.status_text = "Kept local unsaved edits".to_string();
            }
        }

        self.render_left_sidebar(ctx);

        // Propagate live Find & Replace highlight ranges into editor & reader states in O(1) Arc clone
        self.reader_state.modal_open = modal_open_now;
        self.reader_state.search_whole_word = self.find_bar.whole_word;
        self.reader_state.base_dir = self.current_base_dir();
        self.reader_state.auto_calc_math = self.settings.auto_calc_math;
        self.reader_state.auto_calc_tables = self.settings.auto_calc_tables;
        if self.find_bar.open {
            self.find_bar
                .ensure_matches_fresh_with_ctx(&self.buffer, Some(ctx));
            let active_m = self.find_bar.active_match_range();
            self.editor_state.search_matches = Arc::clone(&self.find_bar.matches);
            self.editor_state.active_search_match = active_m.clone();
            self.reader_state.search_matches = Arc::clone(&self.find_bar.matches);
            self.reader_state.active_search_match = active_m;
        } else {
            if !self.editor_state.search_matches.is_empty() {
                self.editor_state.search_matches = Arc::default();
                self.editor_state.active_search_match = None;
            }
            if !self.reader_state.search_matches.is_empty() {
                self.reader_state.search_matches = Arc::default();
                self.reader_state.active_search_match = None;
            }
        }

        let opt_pane_rect = |r: egui::Rect| r.is_positive().then_some(r);
        let (editor_pane_rect, reader_pane_rect) = match self.view_mode {
            ViewMode::Source => (opt_pane_rect(self.editor_state.last_pane_rect), None),
            ViewMode::Reader | ViewMode::Wysiwyg => {
                (None, opt_pane_rect(self.reader_state.last_pane_rect))
            }
            ViewMode::Split => (
                opt_pane_rect(self.editor_state.last_pane_rect),
                opt_pane_rect(self.reader_state.last_pane_rect),
            ),
        };
        let autoscroll_scroll = self.autoscroll.update_and_paint(
            ctx,
            &self.theme,
            modal_open_now,
            editor_pane_rect,
            reader_pane_rect,
        );
        let suppress_autoscroll =
            self.autoscroll.active || self.autoscroll.suppress_pane_clicks_this_frame;
        self.editor_state.autoscroll_active = suppress_autoscroll;
        self.reader_state.autoscroll_active = suppress_autoscroll;
        if let Some((target, dy)) = autoscroll_scroll {
            match target {
                AutoScrollTarget::Editor => self.editor_state.pending_scroll_delta_y += dy,
                AutoScrollTarget::Reader => self.reader_state.pending_scroll_delta_y += dy,
            }
        }

        // Main Document Workspace
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(self.theme.desk_bg))
            .show(ctx, |ui| {
                let block_shortcuts = modal_open_now;
                match self.view_mode {
                    ViewMode::Source => {
                        let ed_action = render_virtual_editor(
                            ui,
                            &mut self.buffer,
                            &self.theme,
                            &mut self.editor_state,
                            block_shortcuts,
                        );
                        if let EditorAction::OpenLink(url) = ed_action {
                            self.open_link_target(ctx, &url);
                        }
                    }
                    ViewMode::Reader => {
                        let action = render_virtual_reader(
                            ui,
                            &mut self.buffer,
                            &mut self.block_index,
                            &self.theme,
                            false,
                            false,
                            true,
                            &mut self.reader_state,
                        );
                        self.handle_reader_action(ctx, action);
                    }
                    ViewMode::Wysiwyg => {
                        let action = render_virtual_reader(
                            ui,
                            &mut self.buffer,
                            &mut self.block_index,
                            &self.theme,
                            true,
                            self.show_markup,
                            false,
                            &mut self.reader_state,
                        );
                        self.handle_reader_action(ctx, action);
                    }
                    ViewMode::Split => {
                        let avail = ui.available_rect_before_wrap();
                        let total_w = avail.width().max(380.0);
                        let splitter_w = 6.0_f32;
                        let left_w = ((total_w - splitter_w) * self.split_ratio)
                            .clamp(180.0, total_w - 180.0 - splitter_w);
                        let right_w = (total_w - left_w - splitter_w).max(180.0);

                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::ZERO;
                            ui.allocate_ui_with_layout(
                                egui::vec2(left_w, avail.height()),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                                    let ed_action = render_virtual_editor(
                                        ui,
                                        &mut self.buffer,
                                        &self.theme,
                                        &mut self.editor_state,
                                        block_shortcuts,
                                    );
                                    if let EditorAction::OpenLink(url) = ed_action {
                                        self.open_link_target(ctx, &url);
                                    }
                                    self.sync_ast_if_needed();
                                    let top_row = self.editor_state.last_visible_rows.start;
                                    if self.settings.sync_scroll
                                        && !(self.autoscroll.active
                                            && self.autoscroll.target == AutoScrollTarget::Reader)
                                        && top_row != self.last_split_top_row
                                    {
                                        self.last_split_top_row = top_row;
                                        if let Some(b_idx) =
                                            self.block_index.block_for_source_line(top_row)
                                        {
                                            if b_idx < self.block_index.blocks.len() {
                                                self.reader_state.scroll_to_y = Some(
                                                    (self.block_index.block_y_start(b_idx) - 16.0)
                                                        .max(0.0),
                                                );
                                            }
                                        }
                                    }
                                },
                            );

                            // Draggable splitter handle
                            let (split_rect, split_resp) = ui.allocate_exact_size(
                                Vec2::new(splitter_w, avail.height()),
                                Sense::drag(),
                            );
                            if split_resp.hovered() || split_resp.dragged() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                            }
                            if split_resp.dragged() {
                                let delta_x = split_resp.drag_delta().x;
                                self.split_ratio = (self.split_ratio + delta_x / total_w)
                                    .clamp(0.25, 0.75);
                            }
                            if split_resp.drag_stopped() {
                                self.settings.split_ratio = self.split_ratio;
                                let _ = self.settings.save_to(&self.settings_path);
                            }
                            let divider_color = if split_resp.hovered() || split_resp.dragged() {
                                self.theme.accent
                            } else {
                                self.theme.border
                            };
                            ui.painter().rect_filled(split_rect, 0.0, self.theme.desk_bg);
                            ui.painter().line_segment(
                                [
                                    egui::pos2(split_rect.center().x, split_rect.top()),
                                    egui::pos2(split_rect.center().x, split_rect.bottom()),
                                ],
                                Stroke::new(
                                    if split_resp.hovered() || split_resp.dragged() {
                                        2.0_f32
                                    } else {
                                        1.0_f32
                                    },
                                    divider_color,
                                ),
                            );

                            ui.allocate_ui_with_layout(
                                egui::vec2(right_w, avail.height()),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                                    let action = render_virtual_reader(
                                        ui,
                                        &mut self.buffer,
                                        &mut self.block_index,
                                        &self.theme,
                                        false,
                                        false,
                                        false,
                                        &mut self.reader_state,
                                    );
                                    self.handle_reader_action(ctx, action);
                                },
                            );
                        });
                    }
                }
            });

        if self.ast_needs_sync() {
            self.sync_ast_if_needed();
            ctx.request_repaint();
        }
        if self.block_index.version != self.buffer.version {
            // AST worker pushes immediate repaints via Context; keep a gentle fallback timer.
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        if self.view_mode != ViewMode::Reader {
            self.table_modal.show(ctx, &mut self.buffer);
            if let Some((text, url)) = self.insert_link_modal.show(ctx, &self.theme) {
                insert_link(&mut self.buffer, &text, &url);
                self.sync_ast_if_needed();
                if self.view_mode == ViewMode::Wysiwyg
                    && self.reader_state.active_block_lines.is_some()
                {
                    let line = self.buffer.char_to_line(self.buffer.cursor_char);
                    let _ = activate_wysiwyg_preserving_selection(
                        &self.buffer,
                        &self.block_index,
                        line,
                        self.show_markup,
                        &mut self.reader_state,
                    );
                }
                self.refocus_active_editor();
            }
        }

        let doc_name = self.doc_display_name();
        if let Some((pending, choice)) = self.unsaved_modal.show(ctx, &doc_name, &self.theme) {
            match choice {
                UnsavedPromptChoice::Save => {
                    if self.save_current_document() {
                        if matches!(pending, PendingDiscardAction::CloseWindow) {
                            if let Some(dirty_idx) = self.first_dirty_tab_index() {
                                self.switch_to_tab(dirty_idx);
                                self.unsaved_modal.open(PendingDiscardAction::CloseWindow);
                            } else {
                                self.apply_pending_discard_action(ctx, pending);
                            }
                        } else {
                            self.apply_pending_discard_action(ctx, pending);
                        }
                    }
                }
                UnsavedPromptChoice::DontSave => {
                    if matches!(pending, PendingDiscardAction::CloseWindow) && self.tabs.len() > 1 {
                        let cur = self.active_tab;
                        self.close_tab_discard(ctx, cur);
                        if let Some(dirty_idx) = self.first_dirty_tab_index() {
                            self.switch_to_tab(dirty_idx);
                            self.unsaved_modal.open(PendingDiscardAction::CloseWindow);
                        } else {
                            self.apply_pending_discard_action(ctx, pending);
                        }
                    } else {
                        self.apply_pending_discard_action(ctx, pending);
                    }
                }
                UnsavedPromptChoice::Cancel => {}
            }
        }

        if let Some(print_opts) = self.print_modal.show(ctx, &self.theme) {
            self.exporter.start_job_with_base_dir(
                self.buffer.rope().clone(),
                self.doc_title(),
                ExportTarget::PrintWithOptions(print_opts),
                self.current_base_dir(),
            );
        }

        if let Some(msg) =
            self.os_modal
                .show(ctx, &self.theme, &mut self.settings, &self.settings_path)
        {
            self.exporter.status_text = msg;
        }

        if let Some(diff_choice) = self.external_diff_modal.show(ctx, &self.theme) {
            match diff_choice {
                ExternalDiffChoice::ReloadFromDisk => {
                    if self.reload_active_file_from_disk() {
                        self.exporter.status_text = "Reloaded file from disk".to_string();
                    }
                }
                ExternalDiffChoice::KeepLocal => {
                    self.refresh_disk_stamp();
                    self.exporter.status_text = "Kept local unsaved edits".to_string();
                }
            }
        }
    }
}

fn block_has_range(b: &DocBlock, r: &std::ops::Range<usize>) -> bool {
    match b {
        DocBlock::Heading { source_lines, .. }
        | DocBlock::Paragraph { source_lines, .. }
        | DocBlock::CodeBlock { source_lines, .. } => source_lines == r,
        DocBlock::BlockQuote { blocks, .. } | DocBlock::FootnoteDefinition { blocks, .. } => {
            blocks.iter().any(|sub| block_has_range(sub, r))
        }
        DocBlock::List { items, .. } => items
            .iter()
            .flat_map(|it| it.blocks.iter())
            .any(|sub| block_has_range(sub, r)),
        _ => false,
    }
}

fn block_index_contains_range(index: &ViewportBlockIndex, range: &std::ops::Range<usize>) -> bool {
    let Some(idx) = index.block_for_source_line(range.start) else {
        return false;
    };
    let start_check = idx.saturating_sub(1);
    let end_check = (idx + 2).min(index.blocks.len());
    index.blocks[start_check..end_check]
        .iter()
        .any(|entry| block_has_range(&entry.block, range))
}

impl MarkdownApp {
    /// Opens or navigates to a validated link target (`#anchor`, `https://`, `www.`, `mailto:`, or local file).
    pub fn open_link_target(&mut self, ctx: &egui::Context, raw_url: &str) {
        let Some(target) = classify_link_url(raw_url) else {
            self.exporter.status_text = format!("Blocked unsafe or invalid link: {raw_url}");
            return;
        };

        match target {
            LinkTarget::Anchor(slug) => {
                self.jump_to_anchor(Some(ctx), &slug);
            }
            LinkTarget::External(url) => {
                self.last_opened_link = Some(url.clone());
                if self.open_external_links_with_os {
                    match open_with_os(&url) {
                        Ok(()) => {
                            self.exporter.status_text = format!("Opened {url}");
                        }
                        Err(err) => {
                            ctx.open_url(egui::OpenUrl::new_tab(&url));
                            self.exporter.status_text =
                                format!("Opened {url} via fallback ({err})");
                        }
                    }
                } else {
                    ctx.open_url(egui::OpenUrl::new_tab(&url));
                    self.exporter.status_text = format!("Opened {url}");
                }
            }
            LinkTarget::LocalFile { path, fragment } => {
                let raw_path = PathBuf::from(&path);
                let is_unc_raw = path.starts_with(r"\\") || path.starts_with("//");
                let resolved = if raw_path.is_absolute() || is_unc_raw {
                    raw_path
                } else if let Some(base) = self.current_base_dir() {
                    base.join(&raw_path)
                } else {
                    raw_path
                };
                if is_unc_raw || is_dangerous_local_link_path(&resolved) {
                    self.exporter.status_text = format!(
                        "Blocked unsafe executable or network link: {}",
                        resolved.display()
                    );
                    return;
                }
                if !resolved.exists() {
                    self.exporter.status_text =
                        format!("Linked file not found: {}", resolved.display());
                    return;
                }
                let is_markdown_or_text = resolved
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| {
                        matches!(
                            ext.to_ascii_lowercase().as_str(),
                            "md" | "markdown" | "mdown" | "mkd" | "txt"
                        )
                    });
                self.last_opened_link = Some(raw_url.to_string());
                if is_markdown_or_text {
                    self.pending_anchor_after_open = fragment;
                    self.open_specific_file(&resolved);
                } else if self.open_external_links_with_os {
                    let target_str = resolved.to_string_lossy().to_string();
                    match open_with_os(&target_str) {
                        Ok(()) => {
                            self.exporter.status_text =
                                format!("Opened {}", resolved.display());
                        }
                        Err(err) => {
                            self.exporter.status_text =
                                format!("Failed to open {}: {err}", resolved.display());
                        }
                    }
                } else {
                    self.exporter.status_text = format!("Opened {}", resolved.display());
                }
            }
        }
    }

    fn jump_to_anchor(&mut self, ctx: Option<&egui::Context>, slug: &str) {
        let decoded = slug.replace("%20", " ");
        let norm_target = heading_slug(&decoded);
        let mut slug_counts: FxHashMap<String, usize> = FxHashMap::default();
        let mut match_entry: Option<(f32, std::ops::Range<usize>)> = None;

        for (idx, entry) in self.block_index.blocks.iter().enumerate() {
            match &entry.block {
                DocBlock::Heading {
                    runs,
                    source_lines,
                    ..
                } => {
                    let base = heading_slug(&runs_to_plain_text(runs));
                    let count = slug_counts.entry(base.clone()).or_insert(0);
                    let candidate = if *count == 0 {
                        base
                    } else {
                        format!("{base}-{}", *count)
                    };
                    *count += 1;
                    if candidate == norm_target {
                        match_entry =
                            Some((self.block_index.block_y_start(idx), source_lines.clone()));
                        break;
                    }
                }
                DocBlock::FootnoteDefinition {
                    name,
                    source_lines,
                    ..
                } if heading_slug(name) == norm_target
                    || format!("fn-{name}").eq_ignore_ascii_case(slug) =>
                {
                    match_entry = Some((self.block_index.block_y_start(idx), source_lines.clone()));
                    break;
                }
                _ => {}
            }
        }

        if let Some((y_start, src_lines)) = match_entry {
            self.reader_state.active_block_lines = None;
            let target_y = (y_start - 20.0).max(0.0);
            if let Some(c) = ctx {
                self.reader_state.smooth_scroll_to(target_y);
                let now = c.input(|i| i.time);
                self.reader_state.highlight_block_lines = Some((src_lines, now + 1.4));
                c.request_repaint();
            } else {
                self.reader_state.scroll_to_y = Some(target_y);
            }
        } else {
            self.exporter.status_text = format!("Section not found: #{slug}");
        }
    }

    fn handle_reader_action(&mut self, ctx: &egui::Context, action: ReaderAction) {
        match action {
            ReaderAction::None => {}
            ReaderAction::EditTable(lines) => {
                if self.view_mode != ViewMode::Reader {
                    if let Some(table) = table_at_line(&self.buffer, lines.start) {
                        self.table_modal.open_with(table);
                    }
                }
            }
            ReaderAction::RecalculateTable(lines) => {
                if self.view_mode != ViewMode::Reader {
                    if let Some(mut table) = table_at_line(&self.buffer, lines.start) {
                        let count = calc::recalculate_table(&mut table);
                        apply_table_edit(&mut self.buffer, &table);
                        self.exporter.status_text = format!("Recalculated {count} table formula(s)");
                    }
                }
            }
            ReaderAction::JumpToSourceLine(line) => {
                if self.view_mode != ViewMode::Reader {
                    let c = self.buffer.line_to_char(line);
                    self.buffer.set_cursor(c, false);
                    self.editor_state.scroll_to_cursor = true;
                }
            }
            ReaderAction::JumpToAnchor(slug) => {
                self.jump_to_anchor(Some(ctx), &slug);
            }
            ReaderAction::JumpToFootnote(name) => {
                let mut found = false;
                for (idx, entry) in self.block_index.blocks.iter().enumerate() {
                    if let DocBlock::FootnoteDefinition {
                        name: def_name,
                        source_lines,
                        ..
                    } = &entry.block
                    {
                        if def_name.eq_ignore_ascii_case(&name) {
                            self.reader_state.active_block_lines = None;
                            self.reader_state.smooth_scroll_to(
                                (self.block_index.block_y_start(idx) - 20.0).max(0.0),
                            );
                            let now = ctx.input(|i| i.time);
                            self.reader_state.highlight_block_lines =
                                Some((source_lines.clone(), now + 1.4));
                            ctx.request_repaint();
                            found = true;
                            break;
                        }
                    }
                }
                if !found {
                    self.exporter.status_text = format!("Footnote not found: [^{name}]");
                }
            }
            ReaderAction::OpenLink(url) => {
                self.open_link_target(ctx, &url);
            }
        }
    }

    fn find_fenced_math_block_at_cursor(&self) -> Option<(usize, usize)> {
        let total = self.buffer.len_lines();
        if total == 0 {
            return None;
        }
        let cursor_line = self.buffer.char_to_line(self.buffer.cursor_char).min(total - 1);
        let search_lo = cursor_line.saturating_sub(256);
        let mut open_info: Option<(usize, char, usize)> = None;
        for l in (search_lo..=cursor_line).rev() {
            let line = self.buffer.line_text(l);
            if let Some((_, fc, fl, info)) = parse_code_fence_line(&line) {
                if fc == '$' || is_math_code_info(&info) {
                    open_info = Some((l, fc, fl));
                    break;
                }
                return None;
            }
        }
        let (open_line, fc, fl) = open_info?;
        let search_hi = (cursor_line + 256).min(total);
        for l in (open_line + 1)..search_hi {
            let line = self.buffer.line_text(l);
            let trimmed = line.trim();
            let is_close = if fc == '$' {
                trimmed == "$$"
            } else {
                let cnt = trimmed.bytes().take_while(|&b| b == fc as u8).count();
                cnt >= fl && trimmed[cnt..].trim().is_empty()
            };
            if is_close {
                if cursor_line >= open_line && cursor_line <= l {
                    return Some((open_line, l));
                }
                return None;
            }
        }
        None
    }

    fn inline_math_span_at_cursor(&self) -> Option<(usize, usize)> {
        let (line_idx, col) = self.buffer.char_to_line_col(self.buffer.cursor_char);
        let line = self.buffer.line_text(line_idx);
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            if chars[i] == '\\' {
                i += 2;
                continue;
            }
            if chars[i] == '$' && chars.get(i + 1) != Some(&'$') {
                let start_inner = i + 1;
                let mut j = start_inner;
                while j < chars.len() {
                    if chars[j] == '\\' {
                        j += 2;
                        continue;
                    }
                    if chars[j] == '$' && chars.get(j + 1) != Some(&'$') {
                        if col >= i && col <= j + 1 {
                            let line_start = self.buffer.line_to_char(line_idx);
                            return Some((line_start + start_inner, line_start + j));
                        }
                        i = j + 1;
                        break;
                    }
                    j += 1;
                }
                if j >= chars.len() {
                    break;
                }
            } else {
                i += 1;
            }
        }
        None
    }

    fn insert_math_snippet_at_cursor(&mut self, snippet: &str) {
        self.buffer.restore_cached_selection();
        let inside_math = self.find_fenced_math_block_at_cursor().is_some()
            || self.inline_math_span_at_cursor().is_some();
        let to_insert = if inside_math {
            snippet.to_string()
        } else {
            format!("${snippet}$")
        };
        let len_chars = to_insert.chars().count();
        if let Some(sel) = self.buffer.selection_range() {
            let start = sel.start;
            self.buffer.replace_range(sel, &to_insert);
            self.buffer.set_cursor(start + len_chars, false);
        } else {
            let pos = self.buffer.cursor_char;
            self.buffer.insert(pos, &to_insert);
            self.buffer.set_cursor(pos + len_chars, false);
        }
        self.editor_state.scroll_to_cursor = true;
        self.refocus_active_editor();
    }

    fn calculate_math_at_cursor(&mut self) {
        self.buffer.restore_cached_selection();
        if let Some((open_line, close_line)) = self.find_fenced_math_block_at_cursor() {
            if open_line + 1 < close_line {
                let range = self.buffer.line_range_chars(open_line + 1, close_line - 1);
                let raw_body = self.buffer.rope().slice(range.clone()).to_string();
                let norm_body = raw_body.replace("\r\n", "\n");
                let (updated, summary) = calc::evaluate_math_block_text(&norm_body);
                if let Some(res) = summary {
                    if updated != norm_body {
                        let le = self.buffer.line_ending();
                        let final_body = if le == "\n" {
                            updated
                        } else {
                            updated.replace('\n', le)
                        };
                        let start = range.start;
                        let end_pos = start + final_body.chars().count();
                        self.buffer.replace_range(range, &final_body);
                        self.buffer.set_cursor(end_pos.min(self.buffer.len_chars()), false);
                    }
                    self.exporter.status_text = format!("Calculated: {res}");
                    self.refocus_active_editor();
                    return;
                }
            }
        }

        if let Some((inner_start, inner_end)) = self.inline_math_span_at_cursor() {
            if inner_start < inner_end {
                let expr = self.buffer.rope().slice(inner_start..inner_end).to_string();
                let (updated, summary) = calc::evaluate_math_block_text(&expr);
                if let Some(res) = summary {
                    if updated != expr {
                        let end_pos = inner_start + updated.chars().count();
                        self.buffer.replace_range(inner_start..inner_end, &updated);
                        self.buffer.set_cursor(end_pos.min(self.buffer.len_chars()), false);
                    }
                    self.exporter.status_text = format!("Calculated: {res}");
                    self.refocus_active_editor();
                    return;
                }
            }
        }

        if let Some(sel) = self.buffer.selection_range() {
            let selected = self.buffer.rope().slice(sel.clone()).to_string();
            let stripped = selected.trim().trim_matches('$').trim();
            let (updated, summary) = calc::evaluate_math_block_text(stripped);
            if let Some(res) = summary {
                let start = sel.start;
                let final_txt = if selected.trim().starts_with('$') && selected.trim().ends_with('$') {
                    format!("${updated}$")
                } else {
                    updated
                };
                let end_pos = start + final_txt.chars().count();
                self.buffer.replace_range(sel, &final_txt);
                self.buffer.set_cursor(end_pos.min(self.buffer.len_chars()), false);
                self.exporter.status_text = format!("Calculated: {res}");
                self.refocus_active_editor();
                return;
            }
        }

        let cur_line = self.buffer.char_to_line(self.buffer.cursor_char);
        let line_txt = self.buffer.line_text(cur_line);
        let trimmed = line_txt.trim();
        let (updated, summary) = calc::evaluate_math_block_text(trimmed);
        if let Some(res) = summary {
            if updated != trimmed {
                let range = self.buffer.line_range_chars(cur_line, cur_line);
                let start = range.start;
                let end_pos = start + updated.chars().count();
                self.buffer.replace_range(range, &updated);
                self.buffer.set_cursor(end_pos.min(self.buffer.len_chars()), false);
            }
            self.exporter.status_text = format!("Calculated: {res}");
            self.refocus_active_editor();
            return;
        }

        self.exporter.status_text =
            "No evaluable math expression at cursor".to_string();
    }

    fn recalculate_tables_at_cursor_or_all(&mut self) {
        let line = self.buffer.char_to_line(self.buffer.cursor_char);
        if let Some(mut table) = table_at_line(&self.buffer, line) {
            let count = calc::recalculate_table(&mut table);
            if count > 0 {
                apply_table_edit(&mut self.buffer, &table);
                self.exporter.status_text = format!("Recalculated {count} table formula(s)");
            } else {
                self.exporter.status_text = "No Excel formulas (=...) in current table".to_string();
            }
            return;
        }

        let mut tables = if self.block_index.doc_id == self.doc_id
            && self.block_index.version == self.buffer.version
            && !self.ast_needs_sync()
        {
            let mut tbls = Vec::new();
            for indexed in &self.block_index.blocks {
                if let crate::export::ir::DocBlock::Table { source_lines, .. } = &indexed.block {
                    if let Some(t) = table_at_line(&self.buffer, source_lines.start) {
                        tbls.push(t);
                    }
                }
            }
            tbls
        } else if self.buffer.len_bytes() >= 512_000 {
            let mut tbls = Vec::new();
            let total_lines = self.buffer.len_lines();
            let mut in_fence: Option<(char, usize)> = None;
            let mut line_idx = 0usize;
            while line_idx < total_lines {
                let line_slice = self.buffer.rope().line(line_idx);
                let first_non_space = line_slice.bytes().find(|b| *b != b' ');
                if matches!(first_non_space, Some(b'`' | b'~')) {
                    let line_str = self.buffer.line_text(line_idx);
                    if let Some((_, f_ch, f_len, _)) = parse_code_fence_line(&line_str) {
                        match in_fence {
                            None => in_fence = Some((f_ch, f_len)),
                            Some((open_ch, open_len)) if open_ch == f_ch && f_len >= open_len => {
                                in_fence = None;
                            }
                            _ => {}
                        }
                        line_idx += 1;
                        continue;
                    }
                }
                if in_fence.is_none() && line_idx + 1 < total_lines {
                    let next_slice = self.buffer.rope().line(line_idx + 1);
                    let cur_has_pipe = line_slice.bytes().any(|b| b == b'|');
                    let next_has_pipe = next_slice.bytes().any(|b| b == b'|');
                    let next_has_dash = next_slice.bytes().any(|b| b == b'-');
                    if (cur_has_pipe || next_has_pipe) && next_has_dash {
                        if let Some(t) = table_at_line(&self.buffer, line_idx) {
                            let next_line = t.source_lines.end.max(line_idx + 1);
                            tbls.push(t);
                            line_idx = next_line;
                            continue;
                        }
                    }
                }
                line_idx += 1;
            }
            tbls
        } else {
            extract_tables(&self.buffer.as_string())
        };
        let mut total_cells = 0usize;
        let mut updated_tables = 0usize;
        let mut replacements: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        for tbl in tables.iter_mut() {
            if tbl.source_lines.is_empty() {
                continue;
            }
            let cnt = calc::recalculate_table(tbl);
            if cnt > 0 {
                let last_line = tbl.source_lines.end.saturating_sub(1);
                let range = self.buffer.line_range_chars(tbl.source_lines.start, last_line);
                let serialized = build_table_markdown(&tbl.alignments, &tbl.header, &tbl.rows);
                replacements.push((range, serialized));
                total_cells += cnt;
                updated_tables += 1;
            }
        }
        if !replacements.is_empty() {
            self.buffer.replace_ranges_atomic(replacements);
        }
        if total_cells > 0 {
            self.exporter.status_text =
                format!("Recalculated {total_cells} formula(s) across {updated_tables} table(s)");
        } else {
            self.exporter.status_text = "No Excel table formulas (=...) found".to_string();
        }
    }
}

fn build_outline_items(index: &ViewportBlockIndex) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    for (idx, b) in index.blocks.iter().enumerate() {
        if let DocBlock::Heading {
            level,
            runs,
            source_lines,
            ..
        } = &b.block
        {
            if *level <= 6 {
                let raw_title = runs_to_plain_text(runs);
                if !raw_title.is_empty() {
                    let dir = detect_paragraph_direction(&raw_title);
                    let title = if dir == TextDirection::Rtl {
                        shape_bidi_for_display(&raw_title, dir)
                    } else {
                        raw_title
                    };
                    items.push(OutlineItem {
                        level: *level,
                        title,
                        source_line: source_lines.start,
                        y_start: index.block_y_start(idx),
                    });
                }
            }
        }
        if items.len() >= 2500 {
            break;
        }
    }
    items
}

fn render_outline_row(
    ui: &mut egui::Ui,
    item: &OutlineItem,
    is_active: bool,
    has_children: bool,
    is_collapsed: bool,
    show_chevron: bool,
    theme: &WordTheme,
) -> (bool, bool) {
    let mut toggled = false;
    let mut clicked = false;
    let indent = (item.level.saturating_sub(1) as f32) * 12.0;
    ui.horizontal(|ui| {
        ui.set_min_height(22.0);
        ui.add_space(indent);
        if show_chevron {
            if has_children {
                let glyph = if is_collapsed { "▸" } else { "▾" };
                let chev = ui.add(
                    egui::Button::new(RichText::new(glyph).size(11.0).color(theme.text_muted))
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(Stroke::NONE)
                        .min_size(Vec2::new(16.0, 18.0))
                        .corner_radius(CornerRadius::same(4)),
                );
                if chev.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if chev.clicked() {
                    toggled = true;
                }
            } else {
                ui.add_space(16.0);
            }
        }
        let resp = vivid_button(
            ui,
            RichText::new(&item.title)
                .size(if item.level == 1 { 13.0 } else { 12.0 })
                .strong()
                .color(if is_active {
                    theme.accent
                } else {
                    theme.text_primary
                }),
            Vec2::ZERO,
            5,
            is_active,
            true,
            ToolbarButtonKind::Subtle,
            "",
            theme,
        );
        if is_active {
            let bar = egui::Rect::from_min_max(
                egui::pos2(resp.rect.left() - 5.0, resp.rect.top() + 3.0),
                egui::pos2(resp.rect.left() - 2.0, resp.rect.bottom() - 3.0),
            );
            ui.painter().rect_filled(bar, 1.5, theme.accent);
        }
        if resp.clicked() {
            clicked = true;
        }
    });
    (toggled, clicked)
}

pub(crate) fn visible_outline_indices(
    items: &[OutlineItem],
    filter: &str,
    collapsed: &FxHashSet<usize>,
) -> Vec<(usize, bool, bool)> {
    let q = filter.trim();
    if !q.is_empty() {
        let q_lower = q.to_lowercase();
        return items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                if item.title.to_lowercase().contains(&q_lower) {
                    let has_children = i + 1 < items.len() && items[i + 1].level > item.level;
                    Some((i, has_children, false))
                } else {
                    None
                }
            })
            .collect();
    }

    let mut out = Vec::with_capacity(items.len());
    let mut skip_above_level: Option<u8> = None;
    for (i, item) in items.iter().enumerate() {
        if let Some(lvl) = skip_above_level {
            if item.level > lvl {
                continue;
            }
            skip_above_level = None;
        }
        let has_children = i + 1 < items.len() && items[i + 1].level > item.level;
        let is_collapsed = has_children && collapsed.contains(&item.source_line);
        out.push((i, has_children, is_collapsed));
        if is_collapsed {
            skip_above_level = Some(item.level);
        }
    }
    out
}

pub(crate) fn find_active_outline_index(
    items: &[OutlineItem],
    use_scroll_spy: bool,
    probe_y: f32,
    cur_line: usize,
    block_index: &ViewportBlockIndex,
) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    if use_scroll_spy {
        let pos = items.partition_point(|item| {
            let live_y = block_index
                .block_for_source_line(item.source_line)
                .map(|idx| block_index.block_y_start(idx))
                .unwrap_or(item.y_start);
            live_y <= probe_y
        });
        Some(pos.saturating_sub(1))
    } else {
        let pos = items.partition_point(|item| item.source_line <= cur_line);
        if pos > 0 {
            Some(pos - 1)
        } else {
            None
        }
    }
}

const FONT_GROUP_EMOJI: u8 = 1 << 0;
const FONT_GROUP_EXTRA_SCRIPTS: u8 = 1 << 1;
const FONT_GROUP_CJK: u8 = 1 << 2;
const ALL_LAZY_FONT_GROUPS: u8 =
    FONT_GROUP_EMOJI | FONT_GROUP_EXTRA_SCRIPTS | FONT_GROUP_CJK;

fn lazy_group_font_specs(group: u8) -> &'static [(&'static str, &'static [&'static str])] {
    match group {
        FONT_GROUP_EMOJI => &[(
            "system_emoji",
            &[
                r"C:\Windows\Fonts\seguiemj.ttf",
                "/System/Library/Fonts/Apple Color Emoji.ttc",
                "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
            ],
        )],
        FONT_GROUP_EXTRA_SCRIPTS => &[
            ("system_se_asia", &[r"C:\Windows\Fonts\LeelawUI.ttf"]),
            ("system_syllabics", &[r"C:\Windows\Fonts\gadugi.ttf"]),
            ("system_armenian_ethiop", &[r"C:\Windows\Fonts\sylfaen.ttf"]),
            ("system_african", &[r"C:\Windows\Fonts\ebrima.ttf"]),
        ],
        FONT_GROUP_CJK => &[(
            "system_cjk",
            &[
                r"C:\Windows\Fonts\malgun.ttf",
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.otf",
                "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttf",
                "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
            ],
        )],
        _ => &[],
    }
}

fn core_fonts() -> &'static FontDefinitions {
    static CORE_FONTS: OnceLock<FontDefinitions> = OnceLock::new();
    CORE_FONTS.get_or_init(|| {
        let mut fonts = FontDefinitions::default();

        // 1. Primary native OS proportional UI & Latin font.
        let mut reg_inserted = false;
        for path in [
            r"C:\Windows\Fonts\segoeui.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("system_regular".to_owned(), FontData::from_owned(bytes).into());
                if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
                    list.insert(0, "system_regular".to_owned());
                }
                reg_inserted = true;
                break;
            }
        }

        // 2. Multilingual / Arabic script + UI symbol fonts (✓, ▾, ▴, ▶, ◀).
        let mut extra_fallbacks: Vec<String> = Vec::new();
        for path in [
            r"C:\Windows\Fonts\tahoma.ttf",
            r"C:\Windows\Fonts\arial.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("system_arabic".to_owned(), FontData::from_owned(bytes).into());
                extra_fallbacks.push("system_arabic".to_owned());
                break;
            }
        }
        for path in [
            r"C:\Windows\Fonts\seguisym.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("system_symbols".to_owned(), FontData::from_owned(bytes).into());
                extra_fallbacks.push("system_symbols".to_owned());
                break;
            }
        }

        if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
            let insert_at = usize::from(reg_inserted).min(list.len());
            for (offset, name) in extra_fallbacks.iter().enumerate() {
                list.insert(insert_at + offset, name.clone());
            }
        }

        // 3. Monospace font + multilingual fallbacks for Source Editor & Code Blocks.
        for path in [
            r"C:\Windows\Fonts\CascadiaMono.ttf",
            r"C:\Windows\Fonts\consola.ttf",
            "/System/Library/Fonts/Menlo.ttc",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("system_mono".to_owned(), FontData::from_owned(bytes).into());
                if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
                    list.insert(0, "system_mono".to_owned());
                }
                break;
            }
        }
        if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
            for name in &extra_fallbacks {
                if !list.contains(name) {
                    list.push(name.clone());
                }
            }
        }

        let proportional_chain = fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();

        // 4. Bold proportional & Bold Arabic font family preserving full fallback chain.
        let mut bold_chain = Vec::with_capacity(2 + proportional_chain.len());
        for path in [
            r"C:\Windows\Fonts\segoeuib.ttf",
            r"C:\Windows\Fonts\arialbd.ttf",
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("system_bold".to_owned(), FontData::from_owned(bytes).into());
                bold_chain.push("system_bold".to_owned());
                break;
            }
        }
        for path in [
            r"C:\Windows\Fonts\tahomabd.ttf",
            r"C:\Windows\Fonts\arialbd.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts.font_data.insert(
                    "system_arabic_bold".to_owned(),
                    FontData::from_owned(bytes).into(),
                );
                bold_chain.push("system_arabic_bold".to_owned());
                break;
            }
        }
        bold_chain.extend(proportional_chain);
        fonts
            .families
            .insert(FontFamily::Name("bold".into()), bold_chain);

        fonts
    })
}

type LazyFontCacheMap = HashMap<u8, Vec<(String, Arc<FontData>)>>;

fn lazy_font_cache() -> &'static Mutex<LazyFontCacheMap> {
    static LAZY_FONT_CACHE: OnceLock<Mutex<LazyFontCacheMap>> = OnceLock::new();
    LAZY_FONT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_font_groups_mask(mask: u8) -> u8 {
    let guard = lazy_font_cache().lock().unwrap_or_else(|e| e.into_inner());
    let mut covered = 0u8;
    for bit in [
        FONT_GROUP_EMOJI,
        FONT_GROUP_EXTRA_SCRIPTS,
        FONT_GROUP_CJK,
    ] {
        if (mask & bit) != 0 && guard.contains_key(&bit) {
            covered |= bit;
        }
    }
    covered
}

fn build_fonts_with_loaded_groups(loaded_mask: u8) -> FontDefinitions {
    let mut fonts = core_fonts().clone();
    if loaded_mask == 0 {
        return fonts;
    }

    let mut lazy_names: Vec<String> = Vec::new();
    {
        let guard = lazy_font_cache().lock().unwrap_or_else(|e| e.into_inner());
        for bit in [
            FONT_GROUP_EMOJI,
            FONT_GROUP_EXTRA_SCRIPTS,
            FONT_GROUP_CJK,
        ] {
            if (loaded_mask & bit) != 0 {
                if let Some(entries) = guard.get(&bit) {
                    for (name, data) in entries {
                        fonts.font_data.insert(name.clone(), Arc::clone(data));
                        lazy_names.push(name.clone());
                    }
                }
            }
        }
    }

    if lazy_names.is_empty() {
        return fonts;
    }

    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
        let insert_at = list
            .iter()
            .take_while(|n| {
                matches!(
                    n.as_str(),
                    "system_regular" | "system_arabic" | "system_symbols"
                )
            })
            .count();
        for (offset, name) in lazy_names.iter().enumerate() {
            list.insert(insert_at + offset, name.clone());
        }
    }

    if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
        for name in &lazy_names {
            if !list.contains(name) {
                list.push(name.clone());
            }
        }
    }

    if let Some(list) = fonts.families.get_mut(&FontFamily::Name("bold".into())) {
        let insert_at = list
            .iter()
            .take_while(|n| {
                matches!(
                    n.as_str(),
                    "system_bold"
                        | "system_arabic_bold"
                        | "system_regular"
                        | "system_arabic"
                        | "system_symbols"
                )
            })
            .count();
        for (offset, name) in lazy_names.iter().enumerate() {
            list.insert(insert_at + offset, name.clone());
        }
    }

    fonts
}

fn detect_needed_font_groups_in_str(s: &str, already_covered: u8) -> u8 {
    if s.is_ascii() || (already_covered & ALL_LAZY_FONT_GROUPS) == ALL_LAZY_FONT_GROUPS {
        return 0;
    }
    let mut needed = 0u8;
    for ch in s.chars() {
        if ('\u{1F000}'..='\u{1FAFF}').contains(&ch)
            || (('\u{2600}'..='\u{27BF}').contains(&ch)
                && !('\u{2713}'..='\u{2718}').contains(&ch))
            || ('\u{2300}'..='\u{23FF}').contains(&ch)
            || ch == '\u{FE0F}'
            || ch == '\u{200D}'
        {
            needed |= FONT_GROUP_EMOJI;
        } else if ('\u{2E80}'..='\u{9FFF}').contains(&ch)
            || ('\u{AC00}'..='\u{D7AF}').contains(&ch)
            || ('\u{F900}'..='\u{FAFF}').contains(&ch)
            || ('\u{FF00}'..='\u{FFEF}').contains(&ch)
            || ('\u{20000}'..='\u{2FA1F}').contains(&ch)
        {
            needed |= FONT_GROUP_CJK;
        } else if ('\u{0530}'..='\u{058F}').contains(&ch)
            || ('\u{0E00}'..='\u{17FF}').contains(&ch)
            || ('\u{2C00}'..='\u{2DDF}').contains(&ch)
            || ('\u{A500}'..='\u{ABBF}').contains(&ch)
        {
            needed |= FONT_GROUP_EXTRA_SCRIPTS;
        }

        if ((needed | already_covered) & ALL_LAZY_FONT_GROUPS) == ALL_LAZY_FONT_GROUPS {
            return needed & !already_covered;
        }
    }
    needed & !already_covered
}

fn detect_needed_font_groups(rope: &ropey::Rope, already_covered: u8) -> u8 {
    if (already_covered & ALL_LAZY_FONT_GROUPS) == ALL_LAZY_FONT_GROUPS {
        return 0;
    }
    let mut needed = 0u8;
    for chunk in rope.chunks() {
        needed |= detect_needed_font_groups_in_str(chunk, already_covered | needed);
        if ((needed | already_covered) & ALL_LAZY_FONT_GROUPS) == ALL_LAZY_FONT_GROUPS {
            break;
        }
    }
    needed & !already_covered
}

fn detect_needed_font_groups_around_cursor(buf: &DocumentBuffer, already_covered: u8) -> u8 {
    if (already_covered & ALL_LAZY_FONT_GROUPS) == ALL_LAZY_FONT_GROUPS || buf.len_lines() == 0 {
        return 0;
    }
    let cur_line = buf.char_to_line(buf.cursor_char);
    let start_line = cur_line.saturating_sub(2);
    let end_line = (cur_line + 2).min(buf.len_lines().saturating_sub(1));
    let mut needed = 0u8;
    for line_idx in start_line..=end_line {
        for chunk in buf.rope().line(line_idx).chunks() {
            needed |= detect_needed_font_groups_in_str(chunk, already_covered | needed);
            if ((needed | already_covered) & ALL_LAZY_FONT_GROUPS) == ALL_LAZY_FONT_GROUPS {
                return needed & !already_covered;
            }
        }
    }
    needed & !already_covered
}

fn spawn_lazy_font_loader(missing_mask: u8, ctx: egui::Context) -> mpsc::Receiver<u8> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("md-word-font-loader".to_string())
        .spawn(move || {
            for bit in [
                FONT_GROUP_EMOJI,
                FONT_GROUP_EXTRA_SCRIPTS,
                FONT_GROUP_CJK,
            ] {
                if (missing_mask & bit) == 0 {
                    continue;
                }
                {
                    let guard = lazy_font_cache().lock().unwrap_or_else(|e| e.into_inner());
                    if guard.contains_key(&bit) {
                        continue;
                    }
                }
                let mut entries: Vec<(String, Arc<FontData>)> = Vec::new();
                for (name, paths) in lazy_group_font_specs(bit) {
                    for path in *paths {
                        if let Ok(bytes) = std::fs::read(path) {
                            entries.push((
                                (*name).to_owned(),
                                Arc::new(FontData::from_owned(bytes)),
                            ));
                            break;
                        }
                    }
                }
                let mut guard = lazy_font_cache().lock().unwrap_or_else(|e| e.into_inner());
                guard.entry(bit).or_insert(entries);
            }
            let _ = tx.send(missing_mask);
            ctx.request_repaint();
        });
    rx
}

/// Returns `true` for UNC/network paths or executable/script/shortcut extensions
/// that must never be launched automatically via OS shell link clicks.
pub fn is_dangerous_local_link_path(path: &Path) -> bool {
    let raw_str = path.to_string_lossy();
    if raw_str.starts_with(r"\\") || raw_str.starts_with("//") {
        return true;
    }
    if matches!(
        path.components().next(),
        Some(std::path::Component::Prefix(p))
            if matches!(
                p.kind(),
                std::path::Prefix::UNC(_, _) | std::path::Prefix::VerbatimUNC(_, _)
            )
    ) {
        return true;
    }
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "exe"
                    | "bat"
                    | "cmd"
                    | "com"
                    | "msi"
                    | "msp"
                    | "ps1"
                    | "psm1"
                    | "psd1"
                    | "vbs"
                    | "vbe"
                    | "js"
                    | "jse"
                    | "wsf"
                    | "wsh"
                    | "scr"
                    | "pif"
                    | "cpl"
                    | "hta"
                    | "jar"
                    | "lnk"
                    | "reg"
                    | "dll"
                    | "sys"
                    | "drv"
                    | "sh"
                    | "command"
                    | "app"
            )
        })
}

pub fn configure_fonts(ctx: &egui::Context) {
    ctx.set_fonts(core_fonts().clone());
}

fn paths_match(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    if a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
    {
        return true;
    }
    #[cfg(not(windows))]
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

#[cfg(test)]
mod outline_tests {
    use super::*;

    fn mk(level: u8, title: &str, line: usize, y: f32) -> OutlineItem {
        OutlineItem {
            level,
            title: title.to_string(),
            source_line: line,
            y_start: y,
        }
    }

    #[test]
    fn outline_collapse_and_search_filter_work() {
        let items = vec![
            mk(1, "Intro", 0, 10.0),
            mk(2, "Install Guide", 4, 80.0),
            mk(3, "Windows Setup", 8, 150.0),
            mk(1, "Usage", 15, 240.0),
        ];
        let mut collapsed = FxHashSet::default();
        let all = visible_outline_indices(&items, "", &collapsed);
        assert_eq!(all.len(), 4);
        assert_eq!(all[0], (0, true, false));
        assert_eq!(all[1], (1, true, false));
        assert_eq!(all[2], (2, false, false));
        assert_eq!(all[3], (3, false, false));

        // Collapsing "Intro" (line 0) hides its H2 & H3 descendants, keeps "Usage"
        collapsed.insert(0);
        let vis_collapsed = visible_outline_indices(&items, "", &collapsed);
        assert_eq!(vis_collapsed, vec![(0, true, true), (3, false, false)]);

        // Filtering bypasses collapse so nested matching headings still appear
        let filtered = visible_outline_indices(&items, "windows", &collapsed);
        assert_eq!(filtered, vec![(2, false, false)]);

        let idx = ViewportBlockIndex::default();
        assert_eq!(find_active_outline_index(&items, false, 0.0, 9, &idx), Some(2));
        assert_eq!(find_active_outline_index(&items, true, 90.0, 0, &idx), Some(1));
        assert_eq!(find_active_outline_index(&items, true, 0.0, 0, &idx), Some(0));
    }

    #[test]
    fn ast_sync_skips_repeat_parses_for_comment_docs_and_catches_direct_swaps() {
        let ctx = egui::Context::default();
        let dir = std::env::temp_dir().join(format!("ms_ast_sync_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut app =
            MarkdownApp::new_with_settings_path(&ctx, None, dir.join("settings.json"));
        app.buffer = DocumentBuffer::from_str("   \n\n   \n");
        assert!(app.ast_needs_sync());
        app.sync_ast_if_needed();
        assert!(app.block_index.blocks.is_empty());
        assert!(!app.ast_needs_sync(), "whitespace-only doc must not re-parse every frame");

        // Swapping buffer directly with identical byte length must still trigger AST sync
        app.buffer = DocumentBuffer::from_str("# Title\nA");
        assert_eq!(app.buffer.len_bytes(), "   \n\n   \n".len());
        assert!(app.ast_needs_sync());
        app.sync_ast_if_needed();
        assert!(!app.block_index.blocks.is_empty());
        assert!(!app.ast_needs_sync());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn incremental_font_detection_scans_around_cursor_line() {
        let mut buf = DocumentBuffer::from_str("Line 0\nLine 1\nLine 2\nLine 3\nLine 4\nLine 5\n");
        let target = buf.line_to_char(3);
        buf.insert(target, "🚀 日本語 ");
        assert_eq!(
            detect_needed_font_groups_around_cursor(&buf, 0),
            FONT_GROUP_EMOJI | FONT_GROUP_CJK
        );
        assert_eq!(
            detect_needed_font_groups_around_cursor(&buf, FONT_GROUP_EMOJI),
            FONT_GROUP_CJK
        );
    }

    #[test]
    fn dangerous_local_link_detection_blocks_executables_and_unc() {
        assert!(is_dangerous_local_link_path(Path::new("setup.exe")));
        assert!(is_dangerous_local_link_path(Path::new("scripts/run.BAT")));
        assert!(is_dangerous_local_link_path(Path::new("evil.ps1")));
        assert!(is_dangerous_local_link_path(Path::new("shortcut.lnk")));
        assert!(is_dangerous_local_link_path(Path::new("payload.cmd")));
        assert!(is_dangerous_local_link_path(Path::new("install.msi")));
        assert!(is_dangerous_local_link_path(Path::new(
            r"\\server\share\doc.md"
        )));
        assert!(is_dangerous_local_link_path(Path::new(
            "//server/share/file.exe"
        )));

        assert!(!is_dangerous_local_link_path(Path::new("notes/guide.md")));
        assert!(!is_dangerous_local_link_path(Path::new("docs/readme.txt")));
        assert!(!is_dangerous_local_link_path(Path::new("report.pdf")));
        assert!(!is_dangerous_local_link_path(Path::new("diagram.png")));
    }
}
