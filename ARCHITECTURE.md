# Architecture & Internals

**Markdown Studio** (`markdown_studio` v`0.7.11`) is a native desktop Markdown reader and studio editor written in Rust with `eframe`/`egui`, `ropey`, `comrak`, `memchr`, and `regex`.

---

## Architecture & Data Flow

`Markdown Studio` separates storage, parsing/layout, UI presentation, and heavy exports into non-blocking layers so UI frames stay responsive on multi-megabyte Markdown files:

```text
                        +----------------------------------+
                        |     DocumentBuffer (Rope)        |
                        |  - O(log n) edits & line lookup  |
                        |  - O(1) incremental word count & |
                        |    line-ending detection         |
                        |  - VecDeque<EditOp> (coalesced, 32MB)|
                        +--------+----------------+--------+
                                 |                |
          (O(1) Arc Rope clone)  |                | (O(1) Arc Rope clone)
                                 v                v
                 +--------------------+      +-------------------------+
                 | BackgroundAstParser|      |   BackgroundExporter    |
                 |  - >=32KB off-UI   |      |  - parse_to_ir ->       |
                 |  - comrak + IR     |      |    PDF / DOCX / Print   |
                 |  - eguiCtx wakeup  |      |  - eguiCtx wakeup       |
                 +---------+----------+      +------------+------------+
                           |                              |
                           v                              |
                 +--------------------+                   |
                 | ViewportBlockIndex |                   |
                 |  - O(log B) range  |                   |
                 |  - batch heights   |                   |
                 +---------+----------+                   |
                           |                              |
                           v                              v
         +----------------------------------------------------+
         |                MarkdownApp (egui)                  |
         | Toolbar (cached ActiveFormatState) · AutoScroll    |
         | LineLayoutCache & ReaderGalleyCache (atlas-tracked)|
         | VirtualEditor · VirtualReader · Lazy Font Loader   |
         +----------------------------------------------------+
```

### Key Architectural Principles
1. **Immutable Structural Sharing via `ropey::Rope`**: Document opens stream through a 64 KB `BufReader`, `word_count()` / `line_ending()` update incrementally in $O(\text{edit})$, and cloning the document snapshot to background threads (`BackgroundAstParser` using streaming `parse_rope_to_ir` for $\ge 32\text{ KB}$, `BackgroundExporter`, `BackgroundSessionSaver`, and `BackgroundSearcher` for $\ge 256\text{ KB}$) is $O(1)$.
2. **Double Virtualization & Atlas-Tracked Caching**: Source mode virtualizes rows via `ScrollArea::vertical().show_rows(...)` backed by a 512-entry `LineLayoutCache`; Reader/WYSIWYG/Split virtualizes above **48 blocks** (`FenwickTree`-backed `ViewportBlockIndex::visible_range` in $O(\log B)$) and tables above **80 rows**, backed by a 1024-entry LRU half-evicted `ReaderGalleyCache`, bounded `MermaidSceneCache` (64), `MathLayoutCache` (256), `TableEvalCache` (LRU half-eviction at 256), `math_preview_cache` (128), and a persistent 2-thread bounded `ImageDecodePool` — all automatically invalidated via `Weak<Mutex<TextureAtlas>>` whenever egui rebuilds the font atlas.
3. **Document Session Isolation (`doc_id`) & Event-Driven Wakeups**: Monotonically incremented `doc_id` prevents in-flight background AST parses from stale documents from overwriting new buffers, and worker threads notify `egui::Context` directly instead of spinning the UI loop.
4. **AST Round-Trip Verification**: Markdown edits preserve canonical CommonMark + GFM structure (`parse(serialize(ast)) == ast`), leaving unrelated paragraphs, alignment/indent metadata comments, escaped markers, and multi-backtick code spans intact.

---

## Codebase Module Map

| Path | Role & Key Symbols |
| :--- | :--- |
| [Cargo.toml](Cargo.toml) | Crate manifest (`markdown_studio` v`0.7.11`), runtime (`eframe`, `egui`, `rfd`, `ropey`, trimmed `comrak` with `default-features = false`, `memchr`, `regex`, `rustc-hash`, `printpdf`, `lopdf`, `docx-rs`, `image` with `webp`, `resvg`, `ureq`), dev (`zip`, `quick-xml`, `criterion`, `tempfile`), release profile tuning (`lto = "thin"`, `codegen-units = 1`, `strip = "symbols"`), and benchmark config. |
| [build.rs](build.rs) | Windows build script generating multi-resolution RGBA `.ico` (`16,32,48,64,128,256`) via `src/icon.rs` in `OUT_DIR` and compiling `ICON` + `VERSIONINFO` (`FileVersion`/`ProductVersion`) via `rc.exe`. |
| [src/icon.rs](src/icon.rs) | Pure-`std` 4×4 supersampled universal Markdown `M↓` badge RGBA icon rasterizer (`render_app_icon_rgba`) shared by `build.rs` and `src/main.rs`. |
| [src/lib.rs](src/lib.rs) | Library crate root exporting `APP_VERSION` (`env!("CARGO_PKG_VERSION")`), `document`, `export`, `icon`, and `ui`. |
| [src/main.rs](src/main.rs) | Native GUI entry point (`MarkdownApp`), CLI `LaunchArgs` parsing, single-instance OS/CLI file forwarding (`try_forward_files_to_running_instance` unless `--new-window`), `--window-pos` positioning, runtime window title with version, and embedded 64×64 `M↓` window icon. |
| [src/document/mod.rs](src/document/mod.rs) | `DocumentBuffer` (64 KB `BufReader` UTF-8 BOM stripping, `DiskFileStamp`, single-pass `StreamingWordCountReader`, `open_recovery_file` streaming restore, `VecDeque` undo/redo stacks with smart typing/backspace/delete burst coalescing, atomic multi-range `EditOp::Batch` via `replace_ranges_atomic`, 32 MB byte-budgeted eviction, reversible text edits & on-disk file renames via `UndoRedoOutcome` / `record_file_rename`, `FxHasher` dirty hash comparison, `from_handoff`, 64 KB `BufWriter` atomic `.mdstudio_tmp`/`.mdstudio_bak` writes, O(1) incrementally maintained `line_ending()` & `word_count()`). |
| [src/document/clipboard.rs](src/document/clipboard.rs) | Zero-dependency Win32 `CF_HTML`, `CF_UNICODETEXT`, & bitmap image clipboard reader (`read_os_clipboard_html`, `read_os_clipboard_html_matching`, `read_os_clipboard_plain_text`, `read_os_clipboard_image_png`, `dib_to_png_bytes`, `save_pasted_image_png`, `extract_cf_html_fragment`) and Smart Rich-Text Paste engine (`html_to_markdown`, `tsv_to_markdown_table`, `smart_paste_markdown`) converting Web, Word, Google Docs, and Excel HTML/TSV to clean GFM Markdown while skipping `data:` image blobs, stripping unsafe link schemes, and passing plain IDE code through verbatim. |
| [src/document/ast.rs](src/document/ast.rs) | `comrak` CommonMark+GFM parser/serializer, `is_math_code_info`, `is_mermaid_code_info`, inline/display math sentinel preprocessing preserving exact 1-to-1 `source_lines`, skipping cached formula HTML comments (`<!-- =... -->`) in display IR, `parse_to_ir` & streaming `parse_rope_to_ir` (`Cow`-normalized headings/markers, payload move), `TableData` extraction/building, footnotes, `<br>` splitting, `verify_roundtrip_invariant`. |
| [src/document/editing.rs](src/document/editing.rs) | Surgical inline toggles (`toggle_bold`, `toggle_italic`, `toggle_underline`, `toggle_strikethrough`, `toggle_inline_code`, `toggle_inline_math`), headings, lists/tasks (`-`/`*`/`+`), `set_block_alignment`, `indent_lines`/`outdent_lines`, `parse_code_fence_line` (` ``` `/`~~~`/`$$`), `set_code_block_language`, `insert_code_block`, `insert_horizontal_rule`, `insert_math_block`, `insert_math_template_block`, `insert_mermaid_block`, `insert_image`, `update_image_alt_text`, `insert_link`, `table_at_line`, `apply_table_edit`, `delete_block_lines`, `O(1)` large-selection `query_active_format`. |
| [src/document/math.rs](src/document/math.rs) | Native LaTeX parser (`parse_latex_math`), `math_to_plain_text`, multi-line `\\` aligned equations, `\boxed`/`\tag`, categorized symbol & textbook template constants, 2D box layout engine (`layout_math`), `egui` renderer (`render_math_in_ui`), and anti-aliased 2x RGBA/PNG software rasterizer (`render_math_to_rgba`, `render_math_to_png_bytes`). |
| [src/document/calc.rs](src/document/calc.rs) | Native LaTeX/scientific calculator (`eval_math_expression*`, `evaluate_math_block_text`, `preview_math_result`, `format_calc_number`) + Excel-like table formula engine (`evaluate_table_grid`, `recalculate_table`, `extract_table_cell_formula`, `col_index_to_letters`, `parse_cell_ref`, `ABOVE`/`LEFT`/`BELOW`/`RIGHT` & `SUM`/`AVERAGE`/`MIN`/`MAX`/`COUNT`/`IF`, CommonMark backslash unescaping on leading `\=`). |
| [src/document/mermaid.rs](src/document/mermaid.rs) | Native Mermaid diagram parser, layout engine & vector/canvas renderer (`compile_mermaid_scene`, `render_mermaid_in_ui`, `render_mermaid_to_png_bytes`, `MERMAID_TEMPLATES`) for Flowchart/Graph, Sequence, State, Class, ER, and Pie diagrams with `galley.rect`-accurate alignment, skip-edge clearance, and unified UML/ER boxes. |
| [src/document/search.rs](src/document/search.rs) | Scoped SIMD `memchr::memmem::Finder` literal search (`find_all_matches`, `find_all_matches_in_rope`), `regex::RegexBuilder` linear-time regex search, `LinearByteToChar` monotonic mapper, capture group `$0..$9`/`$$` expansion (`expand_regex_replacement`), atomic `replace_all_matches`. |
| [src/document/unicode.rs](src/document/unicode.rs) | BiDi direction detection (`detect_paragraph_direction`), ASCII fast paths, Arabic + Persian/Urdu shaping (`arabic_forms`), RTL bracket mirroring, `VisualBidiLineMap`, `MultilineBidiMap`, extended grapheme cluster boundaries, metadata comment parsing/formatting. |
| [src/document/viewport.rs](src/document/viewport.rs) | `ViewportBlockIndex` (`visible_range`, `block_for_source_line`, `set_measured_heights_batch`), zero-alloc `estimate_block_height`, coalescing `BackgroundAstParser::with_repaint_ctx`. |
| [src/export/ir.rs](src/export/ir.rs) | Shared export intermediate representation: `DocBlock`, `InlineRun`, `ListItem`, `TableAlign`, `runs_to_plain_text`, `heading_slug`, and `SlugTracker`. |
| [src/export/mod.rs](src/export/mod.rs) | `BackgroundExporter::with_repaint_ctx` worker thread (`ExportTarget`, `ExportEvent`, `start_job_with_base_dir`) pushing immediate `egui::Context` wakeups for PDF, DOCX, HTML, JSON, and Print jobs. |
| [src/export/pdf.rs](src/export/pdf.rs) | Vector PDF compiler (`PdfHeaderFooterOptions`, `PdfLayoutOptions`, `render_pdf_bytes_with_options_and_base_dir`, `export_pdf_to_file_with_options_and_base_dir`) with configurable `{title}`/`{page}`/`{pages}`/`{date}` headers & footers, `lopdf` outline bookmarks and clickable `/Subtype /Link` URI, `#anchor`, & footnote annotations (`PdfOutlineEntry`, `inject_pdf_outlines_and_links`), `OnceLock` cached TTF bytes, BiDi shaping, alignment, image embedding (`<...>`/`%20`), rasterized Mermaid & 2D math PNGs, header-aware code block pagination, zero-alloc `approx_text_width_mm`, table pagination, blockquote bars, and footnotes. |
| [src/export/docx.rs](src/export/docx.rs) | OpenXML `.docx` generator (`render_docx_bytes_with_base_dir`, `export_docx_to_file_with_base_dir`) with `Heading1..Heading6` styles & `outlineLvl`, heading/footnote bookmarks, clickable internal `#section`/`[^ref]` anchor & normalized external hyperlinks, `Segoe UI`/1-inch margin defaults, numbering overrides, BiDi/RTL flags, `<...>`/`%20` PNG media, bordered/padded tables & code blocks, and bookmarked footnotes. |
| [src/export/html.rs](src/export/html.rs) | Standalone HTML5 exporter (`render_html_string_with_base_dir`, `export_html_to_file_with_base_dir`) with clickable sticky `<nav class="outline">`, deduplicated heading `id`s, smooth section `#anchor` & footnote links, safe external links, base64 local images, and embedded Mermaid/Math PNG figures. |
| [src/export/json.rs](src/export/json.rs) | Zero-serde structured JSON exporter (`render_json_string`, `export_json_to_file`) outputting metadata, `stats`, `outline`, `links`, and full AST `blocks`. |
| [src/export/print.rs](src/export/print.rs) | OS printer enumeration (`list_os_printers`), page range parser (`parse_page_range`), PDF spool file manager, Windows WinRT/PowerShell & Unix CUPS `lp` dispatchers. |
| [src/ui/mod.rs](src/ui/mod.rs) | `MarkdownApp` multi-tab state machine (`tabs`, `active_tab`, `SidebarTab::{Explorer, Outline}`, `new_with_launch`, `detach_tab_to_new_window`, `reorder_tab`, `rename_active_file`, `perform_undo_steps`/`perform_redo_steps`, peer-window bounds & IPC `OpenFile`/`AdoptHandoff` receiver, focus claim), tiered `configure_fonts` + background lazy font loader, `Ctrl+Scroll`/Pinch zoom with debounced persistence, cached `ActiveFormatState`, `AutoScrollState` coordination, `Focus Mode (F11)`, Outline scroll-spy + smooth jump, native OS link launcher (`open_with_os` / `open_link_target`), `#anchor`/relative-file/footnote resolver, immediate WYSIWYG code block activation on insert, AST/outline sync, Find/modal orchestration, and 4-mode layout. |
| [src/ui/tabs.rs](src/ui/tabs.rs) | Multi-tab strip UI and parked tab state (`TabSlot`, `InactiveTabState`, `TabVisualInfo`, `TabBarState`, `TabBarAction`, `render_tab_bar`) supporting per-tab dirty indicators, tooltips, middle-click/button close, pre-allocated hover/press pill backgrounds behind `×`/`+`, horizontal drag-to-reorder with insertion line, Chrome-style tab drag-out & cross-window drop with live foreground ghost pill, and `+` new tab creation. |
| [src/ui/explorer.rs](src/ui/explorer.rs) | Cached VS Code-style File Explorer sidebar (`FileExplorerState`, `ExplorerFileFilter::{All, Markdown, Text, CodeAndData}`, `ExplorerAction`, `render_explorer_panel`) with directory tree, search box, file-type filter pills, and hidden/build-folder toggle. |
| [src/ui/instance.rs](src/ui/instance.rs) | Single-instance localhost IPC (`SingleInstanceServer`, `try_forward_files_to_running_instance`, focus-following `instance.lock`), sibling `PeerWindowInfo` bounds registry (`update_window_bounds`, `list_peer_windows`, `try_send_open_to_peer`, `try_send_handoff_to_peer`), CLI `LaunchArgs` (`--new-window`, `--handoff`, `--window-pos`), and self-deleting `DetachedTabHandoff` persistence for clean/dirty transferred tabs. |
| [src/ui/autoscroll.rs](src/ui/autoscroll.rs) | `AutoScrollState`, `AutoScrollTarget`: middle-click tap-toggle & hold-drag-release state machine, foreground circular two-arrow overlay, cursor icons, and quadratic scroll curve. |
| [src/ui/theme.rs](src/ui/theme.rs) | `ThemeMode` (`Light`, `Sepia`, `Dark`), `WordTheme::light()` (*Classic Paper*), `WordTheme::sepia()` (*Warm Paper*), `WordTheme::dark()` (*Midnight Studio*), syntax colors (`syn_*`), and `egui::Visuals` integration. |
| [src/ui/toolbar.rs](src/ui/toolbar.rs) | Tabbed Word Ribbon & header bar (`render_toolbar`, `RibbonTab`, `CalcToolbarInfo`, `ToolbarAction`, `ReadingToolbarInfo`), clickable inline file rename (`RenameFile`, stem selection, `Enter`/blur commit, `Esc` cancel), left-to-right mode switcher (`WYSIWYG \| Split \| Reader \| Source`), 3-way Theme cycle (`Light → Sepia → Dark`), `Explorer` & `Outline` sidebar toggles, `Export ▾` format popup (`HTML`, `JSON`, `PDF`, `DOCX`), dedicated Reader Comfort Bar, Equations palettes & calculator controls, Tables Excel formula bar & grid picker, recent files & zoom controls. |
| [src/ui/editor.rs](src/ui/editor.rs) | Virtualized source code editor (`render_virtual_editor`, `VirtualEditorState::reset_for_new_document`) with 512-entry `LineLayoutCache` (`ThemeMode`- & `Weak<Mutex<TextureAtlas>>`-tracked invalidation), `MAX_EDITOR_LAYOUT_CHARS` truncated-line badge & safe caret clamping, BiDi caret mapping, IME composition overlay, word jump/delete, horizontal scrollbar, syntax highlighting, `Ctrl+Click` link dispatch, and O(log M) search highlights. |
| [src/ui/reader/mod.rs](src/ui/reader/mod.rs), [caches.rs](src/ui/reader/caches.rs), [images.rs](src/ui/reader/images.rs) | Rich `VirtualReader` + seamless inline WYSIWYG block editor (`render_inline_wysiwyg_block`, code/math/mermaid header controls + in-place `Delete` buttons, `Template ▾` / `Formula ▾` pickers, live `= Calculate` / `⚡ Auto-Calc` pills, live formula-evaluated GFM tables & `fx Recalculate`), eased smooth-scroll & Reader keyboard nav, reading progress bar, floating `↑ Top` pill, 48-block & 80-row table virtualization, 1024-entry LRU content-hashed `ReaderGalleyCache` (persisting across AST re-parses) + LRU `TableEvalCache` (256) & `math_preview_cache` (128), 2-thread -> 4-thread bounded `ImageDecodePool` (local & `http(s)://` remote PNG/JPEG/WebP/TIFF, animated GIFs, and 2× `resvg` SVGs/SVGZs), in-place WYSIWYG image label editor (`ImageLabelEditorState`), `#anchor` & footnote jumps, unified link hit-testing & non-blocking hover tooltips (`Order::Tooltip`), and inline search highlights. |
| [src/ui/dialogs.rs](src/ui/dialogs.rs) | Excel-like `TableEditorModal` (column letters, row numbers, Formula Bar, quick formulas, live `→ <val>` evaluation, `fx Recalculate`), `InsertLinkModal`, `UnsavedChangesModal` (including per-tab `CloseTab` prompts), `PrintDialogModal` (with Headers & Footers configuration), `ExternalDiffModal` (bounded unified diff viewer), BiDi singleline inputs with per-widget layout caching (`BidiInputState`), `FindReplaceBar` (zero-clone `Arc` matches, search memoization, `O(log M)` jump, & `>=256 KB` coalescing `BackgroundSearcher`), `StatusBarGoalState`, `format_reading_time_label`, and `render_status_bar` (char count, reading `%`/time remaining, and interactive Word/Char Goal pill). |
| [src/ui/session.rs](src/ui/session.rs) | Zero-serde deterministic session manifest (`SessionState`, `SessionTabEntry`, `session.json`), `BackgroundSessionSaver` off-thread coalescing persistence, and 64 KB `BufWriter` streaming `Rope` crash-safe auto-recovery snapshots (`recovery/tab_*.md`, `write_recovery_snapshot`, `cleanup_unused_recovery_files`). |
| [src/ui/settings.rs](src/ui/settings.rs) | `AppSettings`, `PageWidth`, `LineSpacing` JSON serialization/deserialization (`load_from`, `save_to`, `record_recent_file`), zero-dependency JSON parser for persisted workspace, reading comfort, `explorer_root`, `auto_calc_math`, `auto_calc_tables`, `word_goal`, `char_goal`, `custom_exe_path`, and `auto_sync_exe_path` state. |
| [src/ui/os_integration.rs](src/ui/os_integration.rs) | Per-user Windows Explorer integration under `HKCU\Software\Classes` (`OsIntegrationModal`, `query_integration_status`, `register_integration`, `unregister_integration`, `sync_exe_path_if_registered`) for `"Right-Click -> New -> Markdown Document (.md)"`, `"Open with Markdown Studio"`, `MuiCache` friendly name refresh, startup auto-sync, and legacy `MarkdownWordStudio` migration/cleanup. |
| [tests/export_tests.rs](tests/export_tests.rs) | Integration tests validating multi-page PDF structure, configurable PDF headers/footers, hierarchical UTF-16BE Document Outline bookmarks & clickable `/Subtype /Link` URI/`#section`/footnote annotations (`lopdf`), standalone HTML5 export with clickable `<nav class="outline">` & anchors, structured JSON export, `.docx` OpenXML/ZIP integrity (`zip` + `quick-xml`) including clickable internal/external/footnote links, `outlineLvl`, and clean formatting, Math & Mermaid PNG/Cambria Math/HR export, `<...>`/`%20` image path resolution, Unicode/Arabic/footnotes parity, print ranges, and async exporter. |
| [tests/roundtrip_tests.rs](tests/roundtrip_tests.rs) | Integration tests validating GFM round-trip invariants, table editing, footnotes/`<br>`/marker escaping, code block fence language changes + CRLF cursor/undo preservation, link classification & extraction, Persian/Urdu shaping, atomic saves/`VecDeque` undo/EOL/word count, regex capture replacement, and editing guards. |
| [tests/ui_integration_tests.rs](tests/ui_integration_tests.rs) | Headless `egui` frame integration tests covering multi-tab opening/switching/closing/shortcuts, drag-to-reorder tabs, inline click-to-rename filename + undo/redo & guards, single-instance IPC forwarding, Chrome-style tab drag-out & cross-window drag-and-drop, tab `×`/`+` hover glyph visibility, File Explorer search/filtering/tab opening & `Export ▾` menu, view mode order, 3-way Theme cycle, tabbed Ribbon (`✎ Text`, `∫ Math`, `⊞ Tables`), block `Delete` buttons & Reader hiding, live Math & Excel table formula calculation/recalculation, Light/Sepia/Dark themes, Reader Comfort Bar, Focus Mode (`F11`), smooth keyboard scroll & `↑ Top` pill, `#anchor`/footnote jumps & Outline scroll-spy + search/collapse tree, external file change auto-reload, dirty banner & `ExternalDiffModal`, clipboard image paste, session restore & crash-safe recovery, status bar reading time & writing goals, links across all views/tables, all 6 Mermaid diagram renders, BiDi/RTL, Find & Replace, zoom shortcuts & `Ctrl+Scroll`/Pinch, middle-click auto-scroll, `>100 KB` perf, startup UI glyphs, `Galley` cache invalidation, Print dialog, and unsaved prompts. |
| [benches/editor_bench.rs](benches/editor_bench.rs) | Criterion benchmarks across 10 MB rope edits & line slicing, 10 MB viewport block layout & 45-row source slicing, 10 MB cached `word_count` & `Ctrl+A` `query_active_format`, 10 MB literal/case-insensitive/regex search, 1 MB `parse_to_ir`, and Arabic `shape_bidi_for_display`. |

---

## Block Metadata, Settings & Environment Variables

### Persisted Block Alignment & Indentation Metadata
Block alignment and logical indentation are persisted directly inside Markdown lines using compact HTML comments stripped transparently from the AST during parsing/serialization:

```markdown
<!-- align:center --># Centered Heading
<!-- align:right,indent:2 -->Indented right-aligned paragraph
```

Also accepts `<!-- md:align=center,indent=2 -->` syntax on import.

### User Settings (`settings.json`)
Automatically loaded at startup and saved on workspace changes (`dark_mode`, `theme_mode`, `page_width`, `line_spacing`, `view_mode`, `show_markup`, `outline_open`, `split_ratio`, `sync_scroll`, `zoom_factor`, `auto_calc_math`, `auto_calc_tables`, `word_goal`, `char_goal`, `explorer_root`, `custom_exe_path`, `auto_sync_exe_path`, and up to 10 `recent_files`), with automatic one-time migration from legacy `MarkdownWordStudio`:
- **Windows**: `%APPDATA%\MarkdownStudio\settings.json`
- **Unix / macOS**: `~/.markdown_studio/settings.json`

### Environment Variables
| Variable | Purpose |
| :--- | :--- |
| `MD_STUDIO_SETTINGS_PATH` | Override path to `settings.json` (useful for portable setups or isolated tests; falls back to `MD_WORD_SETTINGS_PATH`). |
| `MD_STUDIO_VIEW` | Initial view mode override: `wysiwyg`, `split`, `reader`, or `source` (falls back to `MD_WORD_VIEW`). |
| `MD_STUDIO_SHOW_MARKUP` | Enable WYSIWYG inline markup (`1`, `true`, `yes`; falls back to `MD_WORD_SHOW_MARKUP`). |
| `MD_STUDIO_EDIT_LINE` | Automatically activate inline WYSIWYG editing at a 0-based line index when starting in WYSIWYG mode (falls back to `MD_WORD_EDIT_LINE`). |

---

## Measured Performance Benchmarks (`cargo bench`)

| Benchmark | Workload | Mean Time |
| :--- | :--- | ---: |
| `doc_ops_10mb/word_count_10mb_cached` | Cached word count on 10 MB Markdown | **520.88 ps** |
| `rope_10mb/line_jump_and_slice_10mb` | Random line jump & slice on 10 MB Rope | **331.37 ns** |
| `doc_ops_10mb/query_active_format_10mb_ctrl_a` | Toolbar format query with full 10 MB `Ctrl+A` selection | **327.84 ns** |
| `viewport_10mb/visible_blocks_lookup_and_layout` | O(log B) viewport query + layout on 10 MB doc | **2.3682 µs** |
| `rope_10mb/insert_and_delete_middle_char` | Mid-document insert + delete + incremental word count (10 MB) | **2.9549 µs** |
| `bidi_shape_arabic_line` | Mixed Arabic/English/number BiDi contextual shaping | **5.8962 µs** |
| `viewport_10mb/visible_source_rows_45_lines` | 45 visible Source rows slice on 10 MB Rope | **14.113 µs** |
| `search_10mb/literal_case_sensitive` | SIMD `memchr::memmem` search across 10 MB | **3.3637 ms** |
| `search_10mb/regex` | Linear-time regex (`ID-\d+`) across 10 MB | **12.312 ms** |
| `search_10mb/case_insensitive` | Unicode case-insensitive search across 10 MB | **15.539 ms** |
| `ast_parse_1mb/parse_to_ir_1mb` | Full `comrak` AST -> IR parse of 1 MB GFM (runs off-thread >= 32 KB) | **136.74 ms** |
