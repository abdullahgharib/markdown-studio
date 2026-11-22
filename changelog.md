# Changelog — Markdown Studio

All notable changes to **Markdown Studio** (`markdown_studio`) are documented in this file.
For architecture, features, shortcuts, and build instructions, see [README.md](README.md).

---

### `v0.7.12` (2026-11-22) — Reader Mode Image Lightbox
- **Reader Mode Click-to-Enlarge Image Lightbox (`src/ui/reader/mod.rs`, `src/ui/mod.rs`)**:
  - Added `ImageLightboxState` and `render_image_lightbox` so clicking any image in **Reader Mode**—whether inline, inside a GFM table cell, list, or blockquote—opens a dimmed centered inspection modal with native resolution badge, zoom controls (`−`, `%`, `+`, `Fit`, `50%–400%`, keyboard `+`/`-`/`0`), live animated GIF & crisp 2× SVG rendering, optional `"Open Link ↗"` button for linked images (`Ctrl+Click` still opens links directly), and `Esc` / backdrop click dismissal without exiting Focus Mode.
- **Verification**: Full test suite passes (**177 tests**: `86` unit, `14` export, `11` roundtrip, `66` UI integration) with **0 Clippy warnings** (`cargo clippy --all-targets -- -D warnings`).

### `v0.7.11` (2026-11-22) — Local & Online Images, Animated GIFs, SVG Rendering & WYSIWYG Image Label Editing
- **Remote Images, Animated GIFs & SVG/SVGZ Support (`Cargo.toml`, `src/ui/reader/images.rs`)**:
  - Added `resvg` (`0.50`) for 2× HiDPI SVG/SVGZ rasterization (with `OnceLock` system `fontdb`), `ureq` (`3.5`) for non-blocking off-thread `http://`/`https://` image fetching (10s timeout, 5 redirects, 16 MB body cap, 60s retry TTL, `MD_STUDIO_DISABLE_REMOTE_IMAGES=1` offline toggle), and `webp` decoding via `image`.
  - Upgraded `ImageDecodePool` to 4 workers supporting multi-frame animated GIF playback (`MAX_GIF_FRAMES = 120`, `MAX_GIF_TOTAL_BYTES = 48 MB`, `MAX_GIF_TEXTURE_DIM = 1280`, frame-delay scheduling via `ctx.request_repaint_after` outside `ctx.data_mut`).
- **In-Place WYSIWYG Image Label Editing & Remote Badge Rendering (`src/document/editing.rs`, `src/ui/reader/mod.rs`)**:
  - Added surgical `update_image_alt_text` supporting standalone, linked (`[![alt](url)](href)`), `<...>` spaced-path, and table-cell images in one undoable `Ctrl+Z` step.
  - Added `ImageLabelEditorState` in `VirtualReaderState` so clicking an image's `"<label>  ✎"` / `"✎ Add image label..."` button in WYSIWYG mode opens an inline editor (`Enter`/`✓` commits, `Esc`/`✕` cancels) while keeping Reader mode strictly read-only.
  - Consecutive remote shields.io/workflow badge groups now display live fetched SVG/PNG textures (falling back to compact clickable pills while loading or offline), while standalone remote images render full-size with editable WYSIWYG captions.
- **Verification**: Full test suite passes (**176 tests**: `86` unit, `14` export, `11` roundtrip, `65` UI integration) with **0 Clippy warnings** (`cargo clippy --all-targets -- -D warnings`).

### `v0.7.10` (2026-11-22) — Fix Async Image Deadlock, Table Cell Images & Remote Badge Pills
- **Async Image Loading Deadlock Fix (`src/ui/reader/images.rs`)**:
  - Fixed a UI freeze/deadlock when opening documents containing images `> 256 KB` (such as animated GIFs or high-res PNGs): moved `ctx.load_texture(...)` outside `ctx.data_mut(|d| ...)` using `ImageLoadState::{Loaded, Loading, Unavailable}` (`load_cached_image_state`) so worker-decoded textures upload without re-entering `egui::Context`'s `RwLock`.
- **GFM Table Cell Images & Remote Badge UX (`src/ui/reader/mod.rs`)**:
  - Added native scaled image rendering inside `render_table_cell` (e.g., side-by-side image galleries in Markdown tables) using a shared `render_image_run` helper.
  - Grouped consecutive remote `http(s)://` badge images (`shields.io`, GitHub CI badges) into wrapping horizontal rows of clickable badge pills (`render_remote_badge_pill`) and added a subtle `"Loading ..."` placeholder while large local images decode asynchronously.
- **Verification**: Full test suite passes (**173 tests**: `84` unit, `14` export, `11` roundtrip, `64` UI integration) with **0 Clippy warnings** (`cargo clippy --all-targets -- -D warnings`).

### `v0.7.9` (2026-11-22) — Performance Pack, Reader Modularization, Bug Fixes, PDF Headers/Footers, External Diff Modal & Clipboard Image Paste
- **Performance & Memory Efficiency Pack (`src/document/search.rs`, `src/document/viewport.rs`, `src/document/ast.rs`, `src/document/mod.rs`, `benches/editor_bench.rs`)**:
  - Added zero-full-copy streaming ASCII case-insensitive literal search (`find_all_literal_ci_ascii_streaming` with SIMD `memchr::memchr2`) and line-by-line single-line regex streaming (`find_all_regex_streaming_lines` for scopes `>= 256 KB`), eliminating full-buffer heap string copies during Find/Replace.
  - Replaced $O(B)$ height-shift loops in `ViewportBlockIndex` with an $O(\log B)$ `FenwickTree` (`block_y_start`, `visible_range`, `set_measured_height`, `set_measured_heights_batch`).
  - Added streaming `parse_rope_to_ir` / `preprocess_rope_for_ir` (iterating `rope.lines()` without `req.rope.to_string()` + fast-path byte skipping for plain lines) and wired `BackgroundAstParser` to use it directly.
  - Added single-pass `StreamingWordCountReader` during `DocumentBuffer::open_file` and `open_recovery_file`, removing the extra full-document word-count scan after loading.
  - Expanded Criterion benchmarks in `benches/editor_bench.rs` for 50k-block Fenwick batch updates, `parse_rope_to_ir` (1 MB), and `math::layout_math` / `mermaid::compile_mermaid_scene`.
- **Reader Modularization & Bug Fixes (`src/ui/reader/`, `src/ui/instance.rs`, `src/ui/mod.rs`, `src/ui/editor.rs`, `src/ui/dialogs.rs`)**:
  - Split `src/ui/reader.rs` into `src/ui/reader/{mod.rs, caches.rs, images.rs}`, upgraded `TableEvalCache` to LRU half-eviction at 256 entries, and replaced unbounded per-image thread spawning with a persistent 2-thread bounded `ImageDecodePool` (64-job cap).
  - Added `write_handoff_file_from_rope` (64 KB `BufWriter` streaming) for detached dirty/untitled tab handoffs without full-buffer `.as_string()` clones, added `… [line truncated for display]` badges and safe caret clamping past `MAX_EDITOR_LAYOUT_CHARS` in `src/ui/editor.rs`, and cached `VisualBidiLineMap` + `Galley` in `BidiInputState`.
- **New Features (`src/export/pdf.rs`, `src/export/print.rs`, `src/export/mod.rs`, `src/ui/dialogs.rs`, `src/document/clipboard.rs`, `src/ui/mod.rs`)**:
  - **PDF & Print Headers & Footers**: Added `PdfHeaderFooterOptions` (`{title}`, `{page}`, `{pages}`, `{date}` tokens across every page via `PdfCursor::page_layers`) and Header/Footer controls in `PrintDialogModal` (shared with PDF export).
  - **External Change Diff Modal**: Added a `"Compare Diff"` button on the external-change banner opening `ExternalDiffModal` with bounded line diffing, `+`/`-` color coding across Light/Sepia/Dark themes, and **Reload from Disk** / **Keep Local Changes** actions.
  - **Paste Image from Clipboard (`Ctrl+V`)**: Added Windows `"PNG"` & 24/32-bit `CF_DIB` clipboard image reading (`read_os_clipboard_image_png`, `dib_to_png_bytes`) and `save_pasted_image_png`, saving screenshots into `assets/` and inserting `![pasted-image](...)` automatically.
- **Verification**: Full test suite passes (**171 tests**: `83` unit, `14` export, `11` roundtrip, `63` UI integration) with **0 Clippy warnings** (`cargo clippy --all-targets -- -D warnings`).

### `v0.7.8` (2026-11-21) — Security Hardening, Performance Bottleneck Fixes & Smart Rich-Text Paste
- **Security & Stability Hardening (`src/ui/mod.rs`, `src/ui/reader.rs`)**:
  - Added `is_dangerous_local_link_path` blocking executable/script/shortcut extensions (`.exe`, `.bat`, `.cmd`, `.ps1`, `.vbs`, `.msi`, `.lnk`, etc.) and UNC/network paths (`\\server\share\...`, `//server/share/...`, `file:////...`) in `open_link_target` before filesystem probing or `ShellExecuteW`.
  - Bounded local image decoding in `decode_color_image` (`MAX_IMAGE_PIXELS = 64 MP` header check + `MAX_IMAGE_TEXTURE_DIM = 4096 px` downscale) to protect against decompression bombs and GPU texture OOM.
- **Performance & Parser Optimizations (`src/ui/explorer.rs`, `src/document/ast.rs`, `src/ui/editor.rs`, `src/document/mod.rs`, `src/ui/mod.rs`)**:
  - Precomputed lowercased File Explorer entries, added zero-allocation `filtered_cache` index reuse across frames, and added non-blocking background tree scanning (`scan_tree_with_budget` + `md-explorer-scan` worker thread).
  - Eliminated `O(L²)` worst-case backtracking in `preprocess_math_in_line` and `normalize_inline_marker_spaces` (`BacktickFailCache` + `no_more_*` flags) and fixed `$5 ($USD)` / `$5/$10` / `$5,$EUR` currency punctuation false-positive inline math.
  - Added `MAX_EDITOR_LAYOUT_CHARS` (`8,192` chars) fast-path line capping in `src/ui/editor.rs`, `EditOp::Batch` / `replace_ranges_atomic` in `src/document/mod.rs`, and streaming table scans (`>= 512 KB`) + reverse-order surgical batch updates in `recalculate_tables_at_cursor_or_all`.
- **Smart Rich-Text Paste (`src/document/clipboard.rs`, `src/ui/editor.rs`, `src/ui/reader.rs`)**:
  - Added zero-dependency Win32 `CF_HTML` / `CF_UNICODETEXT` clipboard readers (with `html_matches_plain_text` stale-clipboard guard), pure-Rust `html_to_markdown`, and `tsv_to_markdown_table` converters supporting copied formatted content from websites, Microsoft Word, Google Docs, and Excel/Sheets (`Ctrl+V` converts rich HTML/TSV into GFM Markdown; `Ctrl+Shift+V` pastes verbatim plain text; plain IDE code copies pass through automatically).
- **Verification**: Full test suite passes (**158 tests**: `72` unit, `13` export, `11` roundtrip, `62` UI integration) with **0 Clippy warnings** (`cargo clippy --all-targets -- -D warnings`).

### `v0.7.7` (2026-11-21) — Smart Undo Coalescing, Performance Optimizations & AST Sync Fixes
- **Smart Undo Coalescing & Byte-Budgeted History (`src/document/mod.rs`)**:
  - Added typing-burst coalescing for contiguous single-character inserts, backspaces, and forward deletes within a 1.5 s window (capped at 128 chars, automatically splitting on whitespace-to-word boundaries, newlines, save points, cursor jumps, and undo/redo).
  - Enforced a 32 MiB byte budget on `undo_stack` (always keeping at least 16 recent operations) so repeated multi-megabyte edits cannot grow memory unboundedly.
- **High-Impact UI & Background Pipeline Optimizations (`src/ui/reader.rs`, `src/ui/mod.rs`, `src/ui/session.rs`, `src/ui/dialogs.rs`, `src/document/search.rs`)**:
  - **Content-Hashed `ReaderGalleyCache`**: Replaced pointer-based keys and `index_version` cache wipes with `hash_inline_runs` + search needle hashing so unchanged blocks keep cached `Galley`s across keystrokes and AST re-parses.
  - **Incremental Lazy Font Detection**: Scanned full `Rope` once per buffer (`font_scanned_buf_key`) and scanned only `±2` lines around the cursor on subsequent edits instead of re-scanning multi-megabyte documents on every keystroke.
  - **Off-Thread Crash Recovery & Streaming Restore**: Added coalescing `BackgroundSessionSaver` (`md-session-saver`) for the periodic 20 s recovery tick and streaming `DocumentBuffer::open_recovery_file` on startup restore; upgraded save `BufWriter`s to 64 KB.
  - **Zero-Clone `FindReplaceBar` & Background Search (`>= 256 KB`)**: Stored search matches in a single `Arc<Vec<Range<usize>>>`, added `find_all_matches_in_rope`, and offloaded live searches on buffers `>= 256 KB` to a coalescing `BackgroundSearcher` (`md-bg-search`).
  - **Faster Dirty Checks & Table Recalc**: Switched `compute_rope_hash` to `FxHasher` (skipping files `> 2 MB` where hash comparison isn't used), and looked up tables via `block_index` + `table_at_line` in `recalculate_tables_at_cursor_or_all` instead of calling `as_string()`.
- **AST Sync & Startup Threshold Fixes (`src/ui/mod.rs`)**:
  - Replaced `blocks.is_empty() && !buffer.is_empty()` with exact `(doc_id, buf_id, version)` tracking (`ast_needs_sync()`) so whitespace-only documents never re-parse every frame while direct test buffer swaps still trigger sync.
  - Aligned startup AST background threshold to `32 KB` (matching `sync_ast_if_needed`) so opening 32–255 KB documents renders immediately without blocking the first frame.

### `v0.7.6` (2026-11-21) — Performance Caching Pass, Bug Fixes, External Auto-Reload, Session/Crash Recovery, Outline Tree Filter & Writing Goals
- **High-Impact Performance & Caching Pass (`src/document/mermaid.rs`, `src/document/math.rs`, `src/ui/reader.rs`, `src/ui/mod.rs`)**:
  - Added bounded atlas-synced LRU half-eviction caches for Mermaid scene compilation (`MermaidSceneCache`, cap 64), 2D LaTeX math layouts (`MathLayoutCache`, cap 256), live math calc previews (`math_preview_cache`, cap 128), and live Excel table formula evaluation (`table_eval_cache`), eliminating redundant per-frame parsing and layout.
  - Upgraded `ReaderGalleyCache` from total-clear thrashing at 512 entries to 1024-entry LRU half-eviction (`last_tick`).
  - Offloaded startup Windows Registry path sync (`os_integration::sync_exe_path_if_registered`) to a background thread (`os_sync_rx`) and eliminated per-frame `window_title` `String` heap allocations (`last_window_title_key`).
- **UI & Undo Bug Fixes (`src/ui/mod.rs`, `src/ui/toolbar.rs`)**:
  - Updated Focus Mode floating pill buttons (`Light`, `Sepia`, `Dark`, `Outline`, `Exit Focus`) to reuse `vivid_button` with vivid hover feedback and `PointingHand` cursor.
  - Combined multi-table `fx Recalculate` (`recalculate_tables_at_cursor_or_all`) into a single atomic `replace_range` edit so `Ctrl+Z` reverts all tables in one undo step, and fixed `sync_ast_if_needed()` outline cache invalidation when replacing buffers at version 1.
- **External File Change Detection & Auto-Reload Banner (`src/document/mod.rs`, `src/ui/tabs.rs`, `src/ui/mod.rs`)**:
  - Added `DiskFileStamp` `(mtime, len)` tracking checked every 2 s, on window focus gain, and on tab switch: clean buffers auto-reload seamlessly preserving cursor/scroll, while dirty buffers show a non-blocking amber banner (`Reload from Disk` / `Keep Local Changes`).
- **Session Restore & Crash-Safe Auto-Recovery (`src/ui/session.rs`, `src/ui/mod.rs`)**:
  - Added zero-serde `session.json` persistence and atomic streaming `Rope` crash-recovery snapshots (`recovery/tab_*.md`) written periodically for unsaved/untitled drafts, cleaned up on clean exit, and restored automatically on startup.
- **Outline Search Filter, Collapsible Heading Tree & Fast Scroll-Spy (`src/ui/mod.rs`, `src/ui/tabs.rs`)**:
  - Added live heading search filtering (`outline_filter`), collapsible nested sections (`collapsed_outline_lines` with `▸`/`▾` & `+`/`−` expand/collapse all), $O(\log H \cdot \log B)$ `partition_point` scroll-spy, and `ScrollArea::show_rows` virtualization for large outlines (raised cap to 2,500 headings).
- **Word / Character Writing Goals & Enhanced Reading Time (`src/ui/settings.rs`, `src/ui/dialogs.rs`, `src/ui/mod.rs`)**:
  - Added persisted `word_goal` and `char_goal` settings, character count and `< 1 min read` / `~N min read` status bar stats, and an interactive `🎯 Goal` pill with mini progress bar and floating popover presets.
- **Verification**: Full test suite passes (**136 tests**: `52` unit, `13` export, `11` roundtrip, `60` UI integration) with **0 Clippy warnings** (`-D warnings -W clippy::perf -W clippy::correctness -W clippy::suspicious`).

### `v0.7.5` (2026-11-21) — Undo/Redo File Renaming & Cross-Window Tab Drag-and-Drop
- **Undo & Redo for File Renaming (`src/document/mod.rs`, `src/ui/mod.rs`)**:
  - Upgraded `EditOp` into `EditOp::Text` and `EditOp::Rename { old_path, new_path }` with `UndoRedoOutcome`, `record_file_rename`, and `undo_with_outcome` / `redo_with_outcome` so `Ctrl+Z` / `Ctrl+Y` (and the Undo/Redo history dropdowns) seamlessly reverse and reapply on-disk renames while preserving the buffer's clean/dirty state, collision safety, `recent_files`, `base_dir`, and Explorer refresh.
- **Chrome-Style Cross-Window Tab Drag-and-Drop (`src/ui/instance.rs`, `src/ui/tabs.rs`, `src/ui/mod.rs`)**:
  - Added a lightweight peer window bounds registry (`windows/win_<pid>_<port>.info`, `PeerWindowInfo`, `update_window_bounds`, `list_peer_windows`) and `InstanceMessage::AdoptHandoff` / `HANDOFF` IPC transfer (`try_send_open_to_peer`, `try_send_handoff_to_peer`).
  - Dragging any tab onto another open Markdown Studio window displays `"↘ Release to move into other window"` and transfers clean, dirty, or untitled tabs directly into the target window's tab strip (including dragging the sole remaining tab from a window, closing the emptied source window cleanly).
- **Verification**: Full test suite passes (**125 tests**: `46` unit, `13` export, `11` roundtrip, `55` UI integration) with **0 Clippy warnings** (`-D warnings`).

### `v0.7.4` (2026-11-21) — Inline Click-to-Rename File Name
- **Click-to-Rename Filename in Tier 1 Toolbar (`src/ui/toolbar.rs`, `src/ui/mod.rs`)**:
  - Clicking the active filename next to `M↓` switches it into a focused inline `TextEdit` with only the stem selected (`rename_stem_char_len`), `Enter`/blur commit, and `Escape` cancel (plus global shortcut suppression while editing and a read-only guard in Reader mode).
  - `MarkdownApp::rename_active_file` validates filenames, preserves the existing extension when omitted, prevents accidental collisions/overwrites, renames saved files cleanly on disk, saves untitled files into the Explorer root (or opens a native Save dialog if no folder is open), updates `recent_files`, `settings.json`, `base_dir`, and refreshes the File Explorer tree.
- **Verification**: Full test suite passes (**121 tests**: `44` unit, `13` export, `11` roundtrip, `53` UI integration) with **0 Clippy warnings** (`-D warnings`).

### `v0.7.3` (2026-11-21) — Drag-to-Reorder Tabs
- **Drag-to-Reorder Tabs (`src/ui/tabs.rs`, `src/ui/mod.rs`)**:
  - Added horizontal drag-to-reorder (`TabBarState::reorder_target_idx`, `TabBarAction::Reorder { from, to }`) with a crisp foreground `theme.accent` drop-indicator line between tabs and foreground drag ghost pill (`Esc` cancels without reordering; dragging vertically out still detaches to a new window).
  - Implemented `MarkdownApp::reorder_tab(from, to)` preserving active tab index and unsaved (`dirty`) tab buffers across active and inactive moves.
- **Verification**: Full test suite passes (**120 tests**: `44` unit, `13` export, `11` roundtrip, `52` UI integration) with **0 Clippy warnings** (`-D warnings`).

### `v0.7.2` (2026-11-21) — Single-Instance Tab Forwarding & Chrome-Style Tab Drag-Out to New Window
- **Single-Instance OS/CLI File Forwarding (`src/ui/instance.rs`, `src/main.rs`, `src/ui/mod.rs`)**:
  - Opening additional `.md`/text files from Windows Explorer or the CLI now forwards them via a nonce-authenticated localhost TCP channel + atomic `instance.lock` (`SingleInstanceServer`, `try_forward_files_to_running_instance`) directly into the running instance as new tabs (or switches to the existing tab without duplicating), restoring & focusing the window.
  - Focusing any open Markdown Studio window claims `claim_active_target()` so new files land in the most recently active window; stale lockfiles recover cleanly, and `--new-window` / `-n` explicitly launches a separate instance.
- **Chrome-Style Tab Drag-Out (`src/ui/tabs.rs`, `src/ui/mod.rs`, `src/document/mod.rs`)**:
  - Dragging a tab vertically past the tab strip or outside the window bounds (when 2+ tabs are open) displays a crisp foreground ghost pill (`layer_painter`, `"↗ Release to open in new window"`) and detaches that tab on release into a new standalone `--new-window` positioned near the drop coordinate.
  - Clean saved files pass their path directly; unsaved or untitled tabs transfer cleanly with full `dirty` state via a self-deleting `.handoff` file (`DetachedTabHandoff`, `DocumentBuffer::from_handoff`).
- **Verification**: Full test suite passes (**119 tests**: `44` unit, `13` export, `11` roundtrip, `51` UI integration) with **0 Clippy warnings** (`-D warnings`).

### `v0.7.1` (2026-11-21) — Tab Bar Hover Fix, 3-Way Theme Cycle & Clickable Clean DOCX Export
- **Tab Bar `×` & `+` Hover/Click Fix**: Reserved `Shape::Noop` background slots before `ui.add(close_btn)` and `ui.add(new_btn)` in `src/ui/tabs.rs` so hover/press highlight pills paint strictly behind the `×` and `+` symbols rather than covering them. Verified with headless hover paint-order test `test_tab_bar_close_and_new_hover_keeps_symbols_visible` across Dark, Sepia, and Light themes.
- **3-Way Theme Toggle (`Light → Sepia → Dark`)**: Updated `ToolbarAction::ToggleTheme` (`src/ui/mod.rs`) and `render_workspace_toggles` (`src/ui/toolbar.rs`) so clicking the theme button cycles through `Light → Sepia → Dark → Light` (showing the next mode: `"Sepia Mode"`, `"Dark Mode"`, or `"Light Mode"`).
- **Clickable & Clean `.docx` Exports (`src/export/docx.rs`)**:
  - **Clickable Links & Bookmarks**: Added unique Word bookmarks (`w:bookmarkStart`/`w:bookmarkEnd`, `md_*` ASCII IDs <= 40 chars) on every heading and footnote definition; `#section` anchors and `[^ref]` footnotes emit internal `HyperlinkType::Anchor` jumps, `www.` links normalize to `https://` external hyperlinks, safe relative paths remain external hyperlinks, and unsafe schemes (`javascript:`, `vbscript:`, `data:text/html`) render as plain text.
  - **Word Navigation Outline & Clean Formatting**: Added `outlineLvl` on `Heading1..Heading6` styles and heading paragraphs, `keep_next` + spacing and subtle `H1`/`H2` bottom rules, clean `Segoe UI`/`Tahoma` (`22` half-pt, `line(276).after(120)`) defaults with 1-inch (`1440` twip) margins and `"1B2230"` body text, `F8FAFC` blockquote shading with `"1D5FC4"` accent border, and cleanly bordered/padded code blocks and tables (`TableCellMargins`, `cantSplit`). Verified via `quick-xml` inspection in `tests/export_tests.rs`.
- **Verification**: Full test suite passes (**114 tests**: `41` unit, `13` export, `11` roundtrip, `49` UI integration) with **0 Clippy warnings** (`-D warnings`).

### `v0.7.0` (2026-11-21) — Multi-Tab Workspace, File Explorer with Search & Filter, HTML/JSON Export & Clickable PDF/HTML Links
- **Multi-Tab Workspace (`src/ui/tabs.rs`, `src/ui/mod.rs`, `src/ui/dialogs.rs`)**:
  - Added a VS Code / Chrome-style scrollable horizontal tab bar (`render_tab_bar`) supporting independent per-tab buffers, undo/redo history, cursors, scroll offsets, dirty `●` indicators, `×` / middle-click close, `+` new tab button, and keyboard shortcuts (`Ctrl+T`, `Ctrl+W`, `Ctrl+Tab` / `Ctrl+PageDown`, `Ctrl+Shift+Tab` / `Ctrl+PageUp`).
  - Opening files via `Open`, `Recent ▾`, File Explorer, Drag-and-Drop, or relative `.md` links opens them in a new tab (or switches to an already-open tab without duplication, reusing the initial clean untitled buffer), while closing dirty tabs or closing the window with unsaved tabs prompts per-tab via `UnsavedChangesModal` (`PendingDiscardAction::CloseTab`).
- **File Explorer Sidebar with Search & Filter (`src/ui/explorer.rs`, `src/ui/settings.rs`, `src/ui/toolbar.rs`)**:
  - Added a cached VS Code-style File Explorer inside a tabbed **`Explorer | Outline`** left sidebar (`Explorer` and `Outline` toolbar pills), complete with directory tree expand/collapse, `Folder` picker, parent (`↑`), refresh (`↻`), live substring search, file type filters (`All`, `MD`, `Text`, `Code`), hidden/build-folder toggle (`.*`), bounded 2,500-entry scanning, auto-root on file open, and persisted `explorer_root` in `settings.json`.
- **Standalone HTML5 & Structured JSON Export (`src/export/html.rs`, `src/export/json.rs`, `src/export/mod.rs`, `src/ui/toolbar.rs`)**:
  - Added `Export ▾` popup menu on the toolbar (`Export HTML (.html)`, `Export JSON (.json)`, `Export PDF (.pdf)`, `Export Word (.docx)`), wired through `BackgroundExporter`.
  - HTML exports generate self-contained responsive HTML5 documents with light/dark CSS, smooth scrolling, a sticky clickable `<nav class="outline">` sidebar linked to deduplicated heading `id`s, clickable section `#anchor` & `[^ref]` footnote links, safe external `href`s, base64-embedded local images, and rasterized PNG Mermaid/Math diagrams.
  - JSON exports produce structured, zero-serde pretty-printed JSON containing document metadata, `stats`, `outline` slugs, categorized `links`, and full AST `blocks`.
- **Clickable Links & Bookmarks in PDF Export (`src/export/ir.rs`, `src/export/pdf.rs`)**:
  - Shared `heading_slug` / `SlugTracker` across Reader, PDF, HTML, and JSON exports so duplicate headings deduplicate consistently (`slug`, `slug-1`, ...).
  - Embedded clickable `/Subtype /Link` PDF annotations via `lopdf` for external URLs (`/URI` actions), internal `#section` links (`/Dest [page_id /XYZ null y_pt null]`), and `[^ref]` footnote references alongside the hierarchical PDF Outline tree.
- **Verification**: Expanded test suite to **112 passing tests** (`41` unit, `12` export, `11` roundtrip, `48` UI integration) with **0 Clippy warnings** (`-D warnings`).

### `v0.6.1` (2026-11-21) — Windows Default Apps Friendly Name & `MuiCache` Fix
- **Windows Shell Friendly Name & `MuiCache` Refresh (`src/ui/os_integration.rs`)**:
  - Updated `register_integration_under`, `unregister_integration_under`, and `sync_exe_path_if_registered_under` to write `MarkdownStudio.md\Application` (`ApplicationName`, `ApplicationDescription`, `ApplicationIcon`), `Applications\<exe>` (`FriendlyAppName`, `DefaultIcon`, `shell\open\command`), and `MuiCache` (`<exe>.FriendlyAppName = "Markdown Studio"`), while deleting legacy `Applications\Markdown Word Studio.exe` and `Applications\md_word.exe` keys.
  - Added automatic startup detection & refresh (`needs_friendly_name_refresh_under`) so Windows Default Apps / Open With menus never keep stale `"Markdown Word Studio"` labels across upgrades.
- **Verification**: Added `refreshes_stale_muicache_and_application_friendly_name` unit test (**105 passing tests**: `39` unit, `9` export, `11` roundtrip, `46` UI integration) with **0 Clippy warnings** (`-D warnings`).

### `v0.6.0` (2026-11-21) — Rebrand to Markdown Studio & New Supersampled `M↓` App Icon
- **Full Project, Crate & Binary Rebrand (`Cargo.toml`, `src/main.rs`, `build.rs`, `src/ui/*`, `src/document/mod.rs`, `src/export/print.rs`)**:
  - Renamed the Rust crate and binary from `md_word` (`Markdown Word Studio`) to `markdown_studio` (`Markdown Studio` / `Markdown Studio.exe`), updated window titles, Windows `VERSIONINFO` metadata, Unsaved Changes exit prompt, atomic save temp suffixes (`.mdstudio_tmp`/`.mdstudio_bak`), and print spool prefixes (`markdown_studio_print_*`, while continuing to clean stale legacy `md_word_print_*` files).
  - Updated toolbar brand badge from `W` to `M↓` (`src/ui/toolbar.rs`).
- **New Supersampled Universal Markdown `M↓` Icon (`src/icon.rs`, `build.rs`, `src/main.rs`)**:
  - Created a shared pure-`std` 4×4 supersampled RGBA icon rasterizer (`render_app_icon_rgba`) rendering a rounded indigo-to-royal-blue gradient badge with translucent top rim highlight, subtle drop shadow, and crisp white `M↓` glyphs.
  - Wired `src/icon.rs` into both `build.rs` (`16, 32, 48, 64, 128, 256` multi-res `.ico` embedded into `Markdown Studio.exe`) and `src/main.rs` (64×64 runtime window icon).
- **Automatic Settings & Windows Registry Migration (`src/ui/settings.rs`, `src/ui/os_integration.rs`, `src/ui/mod.rs`)**:
  - Switched preferences to `%APPDATA%\MarkdownStudio\settings.json` (`~/.markdown_studio/settings.json` on Unix) and `MD_STUDIO_*` env vars (with legacy `MD_WORD_*` fallback), plus automatic one-time migration from `MarkdownWordStudio` settings (clearing stale non-existent `Markdown Word Studio.exe` custom paths).
  - Rebranded Windows Explorer integration to `MarkdownStudio.md` / `MarkdownStudio` (`"Open with Markdown Studio"`) while detecting and migrating/cleaning up legacy `MarkdownWordStudio` keys on sync or registration so Explorer never shows duplicate entries.
- **Verification**: Expanded test suite to **104 passing tests** (`38` unit, `9` export, `11` roundtrip, `46` UI integration) with **0 Clippy warnings** (`-D warnings`).

### `v0.5.0` (2026-11-21) — Word-Style Tabbed Ribbon, Mermaid Fixes, Block Delete, Math Engine & Excel Table Formulas
- **View Mode Switcher Visual Order (`src/ui/toolbar.rs`)**:
  - Updated `render_view_mode_switcher` to pass `rtl: bool` and lay pills out in strict `WYSIWYG | Split | Reader | Source` order across both single-row and wrapped layouts without inflating Tier 1 widths.
- **Mermaid Diagram Layout & Typography Fixes (`src/document/mermaid.rs`)**:
  - Fixed horizontal text centering/right-alignment double-shift in both `render_mermaid_in_ui` and `render_mermaid_to_png_bytes` using `galley.rect` offsets (`min.x`, `center().x`, `max.x`, `center().y`).
  - Added skip-layer edge routing clearance behind intermediate nodes, unified `ClassBox`/`EntityBox` compartment measurement & rendering geometry, widened ER/Class labeled horizontal spacing, and moved Sequence activation bars behind messages.
- **In-Place Block `Delete` Buttons (`src/document/editing.rs`, `src/ui/reader.rs`)**:
  - Added undoable `delete_block_lines` and wired `"Delete"` buttons on active/inactive Code, Math, Mermaid, and GFM Table cards (`Ctrl+Z` undoable, hidden in read-only Reader mode).
- **Word-Style Tabbed Ribbon (`src/ui/toolbar.rs`, `src/ui/settings.rs`, `src/ui/mod.rs`)**:
  - Reorganized Tier 2 formatting controls into three compact tabs (`✎ Text`, `∫ Math`, `⊞ Tables`) while keeping compact responsive toolbar wrapping across all widths and zoom factors.
  - Added categorized Math symbol palettes (**Greek**, **Operators**, **Relations**, **Accents**, **`Textbook ▾`** multi-line templates) plus textbook `\boxed{...}`, `\tag{...}`, and multi-line `\\` aligned math rendering (`src/document/math.rs`).
- **Native LaTeX Calculation Engine & Excel-Like Tables (`src/document/calc.rs`, `src/document/ast.rs`, `src/ui/dialogs.rs`, `src/ui/reader.rs`)**:
  - Built native scientific/LaTeX equation evaluation (`= Calculate`, `= Calc`, and toggleable live `⚡ Auto-Calc` pills) supporting fractions, roots, trigonometry, logs, factorials, summations, and multi-line variable assignments.
  - Built an Excel-like table formula engine (`SUM`, `AVERAGE`, `MIN`, `MAX`, `COUNT`, `IF`, `A1:B3` ranges, `ABOVE`/`LEFT`/`BELOW`/`RIGHT`, CommonMark backslash formula unescaping, and lossless `val <!-- =FORMULA -->` persistence hidden from rendered display IR), paired with upgraded Excel headers, Formula Bar (`[A1] fx`), live evaluation previews, and `"fx Recalculate"` in both `TableEditorModal` and rendered tables.
- **Verification**: Expanded test suite to **99 passing tests** (`33` unit, `9` export, `11` roundtrip, `46` UI integration) with **0 Clippy warnings** (`-D warnings`).

### `v0.4.0` (2026-11-21) — Native LaTeX Math 2D Typesetting, Mermaid Diagrams, Horizontal Rule Tool & PDF/DOCX Export
- **Native 2D LaTeX Math Engine (`src/document/math.rs`, `src/document/ast.rs`, `src/export/ir.rs`)**:
  - Added full inline (`$...$`, `\(...\)`) and display (`$$...$$`, `\[...\]`, ```` ```math ```` / ```` ```latex ```` / ```` ```tex ````) LaTeX parsing and 2D visual typesetting supporting fractions, square/nth roots, superscripts/subscripts, limits (`\sum`, `\int`, `\prod`, `\lim`), matrices (`pmatrix`, `bmatrix`, `vmatrix`, `cases`, `aligned`), extensible delimiters, accents, Greek letters, and mathematical symbols — while protecting currency values (`$10`) and code spans and keeping 1-to-1 AST source-line mapping intact.
- **Native Mermaid Diagram Engine (`src/document/mermaid.rs`)**:
  - Added zero-external-runtime Mermaid parsing, automatic graph layout, and crisp vector/canvas rendering for all 6 diagram types (`flowchart`/`graph`, `sequenceDiagram`, `stateDiagram`/`stateDiagram-v2`, `classDiagram`, `erDiagram`, and `pie showData`) across `Light`, `Sepia`, and `Dark` themes, plus starter templates (`MERMAID_TEMPLATES`).
- **Toolbar Insert Tools & WYSIWYG Live Preview (`src/document/editing.rs`, `src/ui/toolbar.rs`, `src/ui/reader.rs`, `src/ui/mod.rs`)**:
  - Added **`― HR`** (`InsertHorizontalRule` with blank-line separation so CommonMark never creates Setext H2 headings), **`∑ Math`** (`InsertMath` contextual inline `$...$` toggle or `$$...$$` block insertion), and **`◇ Mermaid`** (`InsertMermaid`) buttons to the Formatting Ribbon Insert group.
  - Enhanced WYSIWYG mode (`ViewMode::Wysiwyg`) with live rendered previews inside active Math & Mermaid editors, a `"Template ▾"` diagram switcher dropdown, `"mermaid"` / `"math"` syntax language entries, and click-to-edit selection for `---` Horizontal Rules.
- **PDF, DOCX & Print Export Parity (`src/export/pdf.rs`, `src/export/docx.rs`)**:
  - Exported PDFs/Print spools embed 2x HiDPI rasterized Mermaid diagrams and centered 2D display math equations as crisp `ImageXObject`s alongside normalized Unicode inline math.
  - Exported `.docx` Word files embed 2x PNG drawings for Mermaid diagrams, display math equations, and 2D inline equations (`\frac`, `\sqrt`, matrices, etc.) and format simple inline equations with `Cambria Math`.
- **Verification**: Expanded the test suite from 63 to **84 passing tests** (`22` unit, `9` export, `11` roundtrip, `42` UI integration) with **0 Clippy warnings**.

### `v0.3.1` (2026-11-21) — Window Resize & Narrow-Viewport Toolbar Layout Fix
- **Eliminated Vertical Character-by-Character Button Wrapping on Resize**: Removed nested `ui.scope` and `add_enabled_ui` wrappers inside `horizontal_wrapped` (`src/ui/toolbar.rs`) and enforced `TextWrapMode::Extend` across all toolbar buttons (`vivid_button`) and inline labels (`inline_label`). Toolbar buttons and labels (`Find`, `1. List`, `[x] Task`, `> Quote`, `{ } Code`, `Image`, reading progress `%`, etc.) now stay strictly single-line and wrap cleanly onto whole new rows when resizing narrow or zooming (`480px–1360px`).
- **Tuned Tier 2 Responsive Breakpoints & Find Bar Wrapping**: Adjusted Tier 2 side-by-side breakpoints (`900px` / `780px`) so narrow/zoomed windows reclaim full width instead of spilling extra rows, and removed `right_to_left` from `FindReplaceBar`'s `horizontal_wrapped` row (`src/ui/dialogs.rs`).
- **Regression Coverage**: Added `test_toolbar_resize_sweep_no_vertical_char_wrap_or_blowup` sweeping widths `480..=1360` at `100%` and `110%` zoom across workspace modes.

### `v0.3.0` (2026-11-21) — Responsive Ribbon, Vivid Hover, Undo/Redo History, Universal Text Drag-Drop & Windows Context Menu
- **Responsive Multi-Row Ribbon & Status Bar**: Fixed Tier 1 overlap on narrow/zoomed viewports (`src/ui/toolbar.rs`, `src/ui/dialogs.rs`), lowered minimum window size to `480×360` (`src/main.rs`), and ensured zero-overlap wrapping across all window widths and zoom levels.
- **Vivid Hover & Active Feedback**: Upgraded all toolbar buttons and pills with accent fills, borders, outer glow halos, bottom accent indicators, and `PointingHand` cursors (`vivid_button`).
- **Undo / Redo History Lists**: Added `[Undo][▾]` and `[Redo][▾]` split buttons with numbered history popups and multi-step jump (`DocumentBuffer::undo_steps` / `redo_steps`, `EditOp::describe`).
- **Universal Text File Drag-and-Drop & Open Dialog**: Expanded `DocumentBuffer::is_likely_text_file` and `open_file` to support UTF-8, UTF-16 LE/BE, code, config, logs, and extensionless text files while rejecting binary blobs.
- **Windows Explorer Integration (`src/ui/os_integration.rs`)**: Added per-user (`HKCU\Software\Classes`) registration for **Right-Click -> New -> Markdown Document (.md)** and **Right-Click -> Open with Markdown Word Studio**, plus `OsIntegrationModal` (`OS Menu` button) with configurable `.exe` path (`custom_exe_path`) and startup auto-sync (`auto_sync_exe_path`).
- **Ground-of-Truth Docs**: Added `context.md`, `agents.md`, and `plan.md`.

### `v0.2.0` — Link Navigation, PDF Outline Bookmarks, WYSIWYG Code Blocks & Codebase Hardening

#### Fixed & Added
- **Unified Link & Footnote Navigation Across All Views & Tables**:
  - Fixed non-working link clicks across **Reader** (`Click`), **WYSIWYG** (inactive & active inline editing, `Ctrl+Click`), **Split** (preview & source, `Ctrl+Click`), **Source** (`Ctrl+Click`), and **GFM table cells** (`render_table_cell`).
  - Added `classify_link_url`, `extract_markdown_link_at_char`, and `hit_test_galley_link` supporting `http://`, `https://`, `www.`, `mailto:`, internal `#anchor` fragments, relative `.md`/local files (with optional `#anchor` and pending-jump support in `open_specific_file`), and explicit scheme blocking (`javascript:`, `data:`, `vbscript:`).
  - Replaced `egui::Area` tooltips with direct `Order::Tooltip` `layer_painter` rendering (`show_link_hover_tooltip`) so hover URL previews never steal or block link clicks beneath the pointer, and wired native OS launching (`ShellExecuteW` on Windows / `open` or `xdg-open` on Unix) via `open_with_os`.
- **Hierarchical PDF Document Outline Bookmarks**:
  - Exported PDFs now embed a nested `H1`–`H6` bookmark tree (`PdfOutlineEntry`, `inject_pdf_outlines`) via `lopdf` with `/PageMode /UseOutlines`, `/XYZ` page + vertical offset destinations, and UTF-16BE BOM title encoding (`encode_pdf_outline_title`) for Unicode and Arabic headings.
- **WYSIWYG Code Block Editing & Language Selector Overhaul**:
  - Clicking `{ } Code` (`ToolbarAction::InsertCodeBlock`) inserts the fenced block via `editing::insert_code_block` (CRLF & selection aware) and immediately activates inline editing focused inside the code body.
  - Added an active inline code block header bar (`Language:` preset `ComboBox` via `COMMON_CODE_LANGUAGES`, custom language `TextEdit`, `Copy`, and `Done (Esc)`), inactive WYSIWYG language selector dropdown, `"Click to type code..."` empty placeholder, `Tab`/`Shift+Tab` indentation (with `.lock_focus(true)`), `Ctrl+Enter` exit, live syntax highlighting while active, clickable trailing empty-workspace focus below code blocks, and fixes for 2-line empty fences (`\n``` `) and CRLF cursor/writeback offsets (`doc_to_local_chars` / `local_to_doc_chars`).
- **Additional Codebase Fixes & Optimizations**:
  - Added header-aware code block pagination and zero-allocation `approx_text_width_mm` in [src/export/pdf.rs](src/export/pdf.rs), plus `<...>` and `%20` local image path resolution in both PDF and DOCX exports ([src/export/docx.rs](src/export/docx.rs)).
  - Trimmed `comrak` to `default-features = false` in [Cargo.toml](Cargo.toml), added zero-allocation `find_ascii_case_insensitive` in [src/ui/reader.rs](src/ui/reader.rs), added `VirtualEditorState::reset_for_new_document` on buffer resets ([src/ui/editor.rs](src/ui/editor.rs), [src/ui/mod.rs](src/ui/mod.rs)), cleared active WYSIWYG inline editing when jumping from Outline headings, and brought the test suite to **63 passing tests** (`10` unit, `8` export, `11` roundtrip, `34` UI integration) with **0 Clippy warnings**.

---

### `v0.2.0` (2026-11-20) — Reading Experience, Performance Engine & Rendering Fixes

#### Reading & Viewing Experience
- **Reader Comfort Bar & Editorial Presets**: Dedicated single-row controls in `Reader` mode for `Page Width` (`Narrow` 680 px / `Standard` 820 px / `Wide` 1040 px), `Line Spacing` (`Compact` 1.28× / `Comfortable` 1.52× / `Relaxed` 1.74×), **Light / Sepia (*Warm Paper*) / Dark** themes ([src/ui/theme.rs](src/ui/theme.rs), [src/ui/settings.rs](src/ui/settings.rs)), and persisted JSON settings.
- **Distraction-Free Focus Mode (`F11`)**: Hides chrome into a compact floating top-right control pill (`Light`/`Sepia`/`Dark`, `Outline`, zoom %, `Exit Focus (F11)`) while preserving slim Find bar (`Ctrl+F`) and `Esc` exit.
- **Fluid Smooth Scrolling, Keyboard Nav & Navigation Aids**: Frame-rate-independent eased scrolling (`Space`/`PageDown`, `PageUp`, `J`/`K`/`ArrowUp`/`ArrowDown`, `Home`/`End`), top `2.5 px` reading progress bar, remaining reading time in status bar, floating `↑ Top` pill, live **Outline scroll-spy** with 3 px active bar, internal `#anchor` TOC (`heading_slug`) & clickable `[^ref]` footnote jumps with 1.4 s arrival halo, and horizontal-scrolling fenced code blocks with cached multi-language syntax highlighting.

#### Performance & Architecture
- **O(1) Document Metrics & Streaming I/O**: Incremental `word_count()` & `line_ending()` updates via `apply_rope_edit` (`0.52 ns` on 10 MB); 64 KB `BufReader` streaming `open_file`.
- **Instant Large Selections & Word Jumps**: O(1) `query_active_format` guard for multi-line/`Ctrl+A` selections (`327.84 ns` on 10 MB); O(log L) grapheme/word navigation; cached `ActiveFormatState` in `MarkdownApp`.
- **SIMD & Linear-Time Search**: Switched literal search to chunked `memchr::memmem::Finder` (`3.36 ms` / 10 MB) and case-insensitive/regex search to `regex::RegexBuilder` (`12.31–15.54 ms` / 10 MB) with monotonic `LinearByteToChar` mapping, zero-`Vec<char>` `replace_all_matches`, and `FindReplaceBar` search memoization + `O(log M)` jump.
- **Galley Caching & Aggressive Virtualization**: Added 512-entry `LineLayoutCache` (Source) and 512-entry `ReaderGalleyCache` (Reader/WYSIWYG), zero-clone Reader block borrowing, 48-block viewport virtualization, `O(log B)` block lookup, `>80`-row table virtualization, 2 s TTL image stat caching, and async `>256 KB` image decoding.
- **Startup Font Footprint & Event-Driven Wakeups**: Split fonts into startup core (`6,561,392` bytes on Windows, including `seguisym.ttf` UI icons) plus background lazy groups for Emoji, CJK, and regional scripts (`28,309,628` bytes deferred on demand); wired `egui::Context` wakeups to background AST/export/printer threads; tuned `[profile.release]` (`opt-level = 3`, `lto = "thin"`, `codegen-units = 1`, `strip = "symbols"`).

#### Features & Bug Fixes
- **Added `Ctrl + Scroll Wheel` & Trackpad Pinch Zoom**: Smooth `60%–200%` zooming in [src/ui/mod.rs](src/ui/mod.rs) with locked viewport scroll and debounced `settings.json` saving.
- **Added Middle-Click Two-Arrow Auto-Scroll**: Implemented [src/ui/autoscroll.rs](src/ui/autoscroll.rs) supporting both tap-to-toggle and hold-drag-release smooth scrolling across Editor, Reader, WYSIWYG, and Split panes.
- **Fixed Garbled Text on File Open / Font Atlas Rebuild**: Tracked live `Weak<Mutex<TextureAtlas>>` handles in both `ReaderGalleyCache` and `LineLayoutCache` and hardened `FxHasher` keys so every cached `Galley` invalidates automatically when `egui` replaces its `TextureAtlas`.
- **Fixed Missing Startup Chrome Icons (`✓ Saved`, `Recent ▾`, `Table ▾`)**: Restored `seguisym.ttf` (`system_symbols`) to the core startup font stack and excluded monochrome checkmarks (`U+2713..=U+2718`) from triggering the Emoji font loader.
- **Expanded Verification Coverage**: Expanded suite to 57 passing tests (`10` unit, `6` export, `9` roundtrip, `32` UI integration) and 11 Criterion benchmarks with zero Clippy warnings (`-D warnings`).

---

### `v0.2.0` (2026-11-20) — Version Update, Export Parity & UX Upgrades

#### Added
- **Version Tracking & Binary Metadata**: Bumped crate version to `0.2.0`, added `md_word::APP_VERSION`, displayed the version in the startup/dynamic window title bar, toolbar badge tooltip, and status bar, and embedded a Windows `VERSIONINFO` resource block in `build.rs`.
- **Footnotes & HTML `<br>` / `<ins>` Support**: Parsed GFM footnotes (`DocBlock::FootnoteDefinition`, `InlineRun::is_footnote_ref`) and `<br>` / `<ins>` inline HTML across the AST IR, WYSIWYG/Reader views, vector PDF export, and OpenXML `.docx` export.
- **Insert Hyperlink Modal (`Ctrl+K`) & Clickable Links**: Added `InsertLinkModal` + `editing::insert_link` (with selection prefill and bracket/URL escaping) plus clickable external links (`Click` in Reader, `Ctrl+Click` in Edit/Split) with hover URL tooltips.
- **Recent Files Menu (`Recent ▾`) & Drag-and-Drop**: Added persisted JSON `recent_files` history (top-10, case-insensitive deduplication on Windows) in `settings.json`, toolbar `Recent ▾` popup, and window drag-and-drop for opening `.md` files or inserting dropped images.
- **UI Zoom Controls**: Added persistent `zoom_factor` (`0.6..=2.0`) with toolbar `-` / `%` / `+` buttons and `Ctrl+=` / `Ctrl+-` / `Ctrl+0` shortcuts.
- **Enhanced Status Bar Metrics**: Added chunk-streamed `DocumentBuffer::word_count()`, estimated reading time (~200 wpm), detected EOL indicator (`CRLF`/`LF`), current zoom percentage, and app version.
- **Code Block Copy & Wide Table Scrolling**: Added non-interactive header with language pill and one-click `Copy` button (with `"✓ Copied"` feedback) on code blocks, plus horizontal scrolling on wide tables (`min_col_width >= 90`).
- **Source Editor Word Operations & Inline Syntax Highlighting**: Added `Ctrl+ArrowLeft`/`Right` word movement, `Ctrl+Backspace`/`Ctrl+Delete` word deletion, and inline syntax highlighting (`code`, `**bold**`, `*italic*`, `~~strike~~`, `[link](url)`, `+ ` lists).
- **Regex Capture Group Replacements**: Added capture groups `( ... )` and `$0..$9` / `$$` expansion across both single-match replacement (`replace_single_match_with_opts`) and atomic `replace_all_matches`.
- **Persian/Urdu & UAX #9 Bracket Mirroring**: Extended `arabic_forms` with Persian/Urdu Presentation Forms-A letters (`U+0679..=U+06D2`) and added `mirror_rtl_bracket` to RTL visual shaping.

#### Improved & Fixed
- **PDF & DOCX Export Parity**:
  - **PDF**: Embedded system TrueType fonts (`load_font_set`) so Arabic/multilingual Unicode renders cleanly (with WinAnsi fallback), honored block alignment/indent and RTL run ordering, embedded local images (`ImageXObject`), painted multi-page blockquote accent bars cleanly across page breaks, precomputed table row heights before repeating headers, and rendered underlines and footnotes.
  - **DOCX**: Refactored into `DocxCtx` eliminating redundant `Docx::default()` allocations, added ordered list start overrides (`LevelOverride`), alignment/indent/BiDi RTL + `Tahoma` CS font, clickable `Hyperlink`s, PNG `Pic` embedding, `BreakType::TextWrapping` for `\n`, single-edge blockquote/HR `ParagraphBorders::with_empty()` borders, and footnotes.
  - **Print Spooler**: Added hourly stale temp spool cleanup, fixed PowerShell reversed/spaced page-range parsing, prevented silent Print-All fallbacks on invalid ranges, deleted temp spool PDFs immediately after successful WinRT print, and wired full CUPS `-o` flags on Unix/macOS.
- **Document Buffer & Parser Safety**: Replaced `Vec::remove(0)` undo capping with `VecDeque` (`O(1)` pop_front), implemented atomic `save_to_file` (`.mdword_tmp` + `sync_data` + `.mdword_bak` swap), tracked `doc_id` in `ViewportBlockIndex`/`BackgroundAstParser` to reject stale background AST results after file switches, batched viewport height updates (`set_measured_heights_batch`), scoped regex/literal searches to active selection slices, enforced regex VM step budgets returning explicit errors on complexity overflow, prevented regex `.` from matching `\r`, and preserved `CRLF` line endings across editing transforms.
- **WYSIWYG & UI Fixes**: Fixed stale document state on file switch (`reset_state_for_new_buffer`), guarded inline shortcuts when modals are open, added cross-block Backspace merging and BiDi-accurate `ArrowUp`/`ArrowDown` navigation between adjacent blocks, replaced unbounded image texture map with a bounded 64-entry mtime-checked cache, expanded GFM table detection scan window to `±4096` lines, fixed `build.rs` so source tree `assets/app.ico` is never overwritten at build time, and added tests across `export_tests.rs`, `roundtrip_tests.rs`, and `ui_integration_tests.rs` (48 passing tests, zero Clippy warnings).

---

### `v0.1.0` — Update `ce72aed` (2026-11-17)
*Commit `ce72aed`: Fix Find & Replace highlighting, shortcuts, selection scope, and BiDi CRLF bugs*
- Fixed Find & Replace inline match highlighting across Reader/WYSIWYG and Source modes, focus-safe keyboard shortcuts while Find inputs hold focus, dynamic selection-scope boundary adjustments across single/batch replacements, and BiDi/CRLF cursor mapping edge cases.
- Expanded `tests/ui_integration_tests.rs` with end-to-end headless frame coverage for Arabic typing, Lam-Alef ligatures, Find & Replace workflows, and Reader match highlighting.

---

### `v0.1.0` (2026-11-17)
*Commits `e4ba37b`, `f3ca82b`: Initial Release of Markdown Word Studio*
- Launched initial desktop Markdown reader & Word-style editor in Rust (`eframe`/`egui`, `ropey`, `comrak`, `printpdf`, `docx-rs`).
- Delivered 4 workspace modes (`WYSIWYG`, `Split`, `Reader`, `Source`), virtualized rope editor & viewport block renderer, Arabic contextual shaping + BiDi cursor/selection mapping, CommonMark + GFM parsing/round-trip editing, two themes, Document Outline sidebar, GFM Table Editor modal, Print dialog, PDF & DOCX background export, `.gitignore`/`.gitattributes` setup, 10 MB Criterion benchmark suite, and initial unit/integration tests.
