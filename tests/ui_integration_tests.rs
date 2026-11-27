use egui::{Event, Key, Modifiers, Pos2, RawInput, Rect};

use markdown_studio::document::ast::{parse_to_ir, verify_roundtrip_invariant};
use markdown_studio::document::editing::{
    query_active_format, toggle_bold, toggle_underline, LineBlockKind,
};
use markdown_studio::document::unicode::{
    build_visual_bidi_line_map, detect_paragraph_direction, shape_bidi_for_display,
    BlockAlignment, TextDirection,
};
use markdown_studio::document::DocumentBuffer;
use markdown_studio::export::ir::DocBlock;
use markdown_studio::export::print::{DuplexMode, PageOrientation, PaperSize};
use markdown_studio::ui::dialogs::PendingDiscardAction;
use markdown_studio::ui::settings::AppSettings;
use markdown_studio::ui::toolbar::{ToolbarAction, ViewMode};
use markdown_studio::ui::MarkdownApp;

fn new_isolated_app(ctx: &egui::Context) -> (tempfile::TempDir, MarkdownApp) {
    let dir = tempfile::tempdir().expect("create temp settings dir");
    let settings_path = dir.path().join("settings.json");
    let app = MarkdownApp::new_with_settings_path(ctx, None, settings_path);
    (dir, app)
}

fn run_frames(ctx: &egui::Context, app: &mut MarkdownApp, count: usize) {
    for _ in 0..count {
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            ..Default::default()
        };
        let _ = ctx.run(raw_input, |ctx| {
            app.update_ui(ctx);
        });
    }
}

#[test]
fn test_all_view_modes_and_theme_toggle_render_cleanly() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    assert!(app.buffer.is_empty());
    app.buffer = DocumentBuffer::from_str("# Title\n\nParagraph text.\n\n## Subtitle\n");

    // Run through all 4 view modes across Light -> Sepia -> Dark theme cycle, with Outline & Find Bar
    for expected_mode in [
        markdown_studio::ui::theme::ThemeMode::Light,
        markdown_studio::ui::theme::ThemeMode::Sepia,
        markdown_studio::ui::theme::ThemeMode::Dark,
    ] {
        while app.theme.mode != expected_mode {
            app.dispatch_action(&ctx, ToolbarAction::ToggleTheme);
        }
        assert_eq!(app.theme.mode, expected_mode);
        assert_eq!(
            app.theme.dark,
            expected_mode == markdown_studio::ui::theme::ThemeMode::Dark
        );

        app.dispatch_action(&ctx, ToolbarAction::ToggleOutline);
        app.dispatch_action(&ctx, ToolbarAction::ToggleFindBar);

        for mode in [
            ViewMode::Split,
            ViewMode::Wysiwyg,
            ViewMode::Reader,
            ViewMode::Source,
        ] {
            app.dispatch_action(&ctx, ToolbarAction::SetViewMode(mode));
            run_frames(&ctx, &mut app, 3);
            assert_eq!(app.view_mode, mode);
            assert!(!app.block_index.blocks.is_empty());
            if matches!(mode, ViewMode::Split | ViewMode::Wysiwyg | ViewMode::Reader)
                && app.block_index.blocks.len() >= 3
            {
                assert!(
                    app.block_index.blocks[1].y_start > app.block_index.blocks[0].y_start,
                    "blocks must stack vertically, not horizontally"
                );
                assert!(
                    app.block_index.blocks[2].y_start > app.block_index.blocks[1].y_start,
                    "subsequent blocks must advance vertically down the page"
                );
            }
        }
        if !app.theme.dark {
            assert_ne!(
                ctx.style().visuals.strong_text_color(),
                egui::Color32::WHITE,
                "light/sepia theme strong text must not be white-on-white"
            );
        }

        // Toggle back off cleanly
        app.dispatch_action(&ctx, ToolbarAction::ToggleOutline);
        app.dispatch_action(&ctx, ToolbarAction::ToggleFindBar);
    }
    // 3rd toggle from Dark returns to Light
    app.dispatch_action(&ctx, ToolbarAction::ToggleTheme);
    assert_eq!(
        app.theme.mode,
        markdown_studio::ui::theme::ThemeMode::Light
    );
}

#[test]
fn test_toolbar_formatting_and_table_insertion_workflow() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("Hello world\n\nSecond paragraph.\n");
    app.buffer.selection_anchor = Some(0);
    app.buffer.cursor_char = 5; // Select "Hello"

    // Apply formatting via UI dispatch actions and render frames
    let actions = [
        ToolbarAction::ToggleBold,
        ToolbarAction::ToggleItalic,
        ToolbarAction::ToggleStrike,
        ToolbarAction::ToggleInlineCode,
        ToolbarAction::SetHeading(Some(2)),
        ToolbarAction::ToggleBlock(LineBlockKind::BulletList),
        ToolbarAction::InsertCodeBlock,
    ];

    for action in actions {
        app.dispatch_action(&ctx, action);
        run_frames(&ctx, &mut app, 2);
        assert!(
            app.editor_state.request_focus
                || ctx.memory(|m| m.has_focus(egui::Id::new("virtual_rope_editor")))
                || !app.buffer.as_string().is_empty()
        );
    }

    // Insert a 3x3 table at the end
    let end = app.buffer.len_chars();
    app.buffer.set_cursor(end, false);
    app.dispatch_action(&ctx, ToolbarAction::InsertTable { rows: 3, cols: 3 });
    run_frames(&ctx, &mut app, 2);

    let md = app.buffer.as_string();
    assert!(md.contains("| Column 1 | Column 2 | Column 3 |"));
    assert!(verify_roundtrip_invariant(&md));

    // Open table editor modal at the inserted table
    app.dispatch_action(&ctx, ToolbarAction::OpenTableEditor);
    run_frames(&ctx, &mut app, 2);
    assert!(app.table_modal.open);

    // Undo and redo cleanly
    app.table_modal.open = false;
    let before_undo = app.buffer.as_string();
    app.dispatch_action(&ctx, ToolbarAction::Undo);
    run_frames(&ctx, &mut app, 2);
    assert_ne!(app.buffer.as_string(), before_undo);

    app.dispatch_action(&ctx, ToolbarAction::Redo);
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.buffer.as_string(), before_undo);
}

#[test]
fn test_editor_typing_and_navigation_no_scroll_jump() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let mut initial = String::new();
    for i in 0..80 {
        initial.push_str(&format!("Line {i}: sample markdown line for testing.\n"));
    }
    app.buffer = DocumentBuffer::from_str(&initial);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    app.buffer.set_cursor(0, false);

    // Initial render focuses the source editor
    run_frames(&ctx, &mut app, 3);
    let initial_top_row = app.editor_state.last_visible_rows.start;
    assert_eq!(initial_top_row, 0);

    // Type text on line 0 and move down 3 lines inside the visible window
    let type_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![
            Event::Text("# ".to_string()),
            Event::Key {
                key: Key::ArrowDown,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Key {
                key: Key::ArrowDown,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
        ..Default::default()
    };
    let _ = ctx.run(type_input, |ctx| {
        app.update_ui(ctx);
    });
    run_frames(&ctx, &mut app, 2);

    assert!(app.buffer.line_text(0).starts_with("# Line 0:"));
    assert_eq!(app.buffer.char_to_line(app.buffer.cursor_char), 2);
    // Viewport must NOT jump vertically while moving within already-visible lines!
    assert_eq!(app.editor_state.last_visible_rows.start, 0);
}

#[test]
fn test_empty_and_short_documents_and_table_modal_update() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);

    // Empty / new document across all view modes
    app.dispatch_action(&ctx, ToolbarAction::NewFile);
    for mode in [
        ViewMode::Split,
        ViewMode::Wysiwyg,
        ViewMode::Reader,
        ViewMode::Source,
    ] {
        app.dispatch_action(&ctx, ToolbarAction::SetViewMode(mode));
        run_frames(&ctx, &mut app, 2);
    }

    // Load document with a GFM table & task list, open table modal and mutate it
    app.buffer = DocumentBuffer::from_str(
        "# Title\n\n- [ ] Task one\n\n| A | B |\n| :--- | ---: |\n| 1 | 2 |\n",
    );
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    let table_char = app.buffer.line_to_char(4);
    app.buffer.set_cursor(table_char, false);
    app.dispatch_action(&ctx, ToolbarAction::OpenTableEditor);
    run_frames(&ctx, &mut app, 2);
    assert!(app.table_modal.open);

    if let Some(ref mut table) = app.table_modal.table {
        table.set_cell(None, 0, "Metric".to_string());
        table.add_row(None);
        table.set_cell(Some(1), 0, "Throughput".to_string());
    }
    markdown_studio::document::editing::apply_table_edit(
        &mut app.buffer,
        app.table_modal.table.as_ref().unwrap(),
    );
    app.table_modal.open = false;
    run_frames(&ctx, &mut app, 2);

    let result_md = app.buffer.as_string();
    assert!(result_md.contains("| Metric | B |"));
    assert!(result_md.contains("| Throughput |"));
    assert!(verify_roundtrip_invariant(&result_md));
}

#[test]
fn test_reader_mode_is_strictly_read_only() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let sample = "# Protected Doc\n\nSome **bold** text.\n\n- [ ] Check me\n\n| A | B |\n| :--- | :--- |\n| 1 | 2 |\n";
    app.buffer = DocumentBuffer::from_str(sample);
    app.buffer.set_cursor(19, false);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    run_frames(&ctx, &mut app, 2);

    let orig_version = app.buffer.version;
    let orig_text = app.buffer.as_string();

    for action in [
        ToolbarAction::NewFile,
        ToolbarAction::Undo,
        ToolbarAction::Redo,
        ToolbarAction::ToggleBold,
        ToolbarAction::ToggleItalic,
        ToolbarAction::ToggleUnderline,
        ToolbarAction::ToggleStrike,
        ToolbarAction::ToggleInlineCode,
        ToolbarAction::SetHeading(Some(2)),
        ToolbarAction::ToggleBlock(LineBlockKind::BulletList),
        ToolbarAction::SetAlignment(BlockAlignment::Center),
        ToolbarAction::Indent,
        ToolbarAction::Outdent,
        ToolbarAction::InsertCodeBlock,
        ToolbarAction::InsertTable { rows: 2, cols: 2 },
        ToolbarAction::OpenTableEditor,
    ] {
        app.dispatch_action(&ctx, action);
        run_frames(&ctx, &mut app, 1);
    }

    assert_eq!(app.buffer.version, orig_version);
    assert_eq!(app.buffer.as_string(), orig_text);
    assert!(!app.table_modal.open);

    // Keyboard shortcuts (Ctrl+B, Ctrl+I, Ctrl+U, Ctrl+Z, Ctrl+N) must also be blocked in Reader mode.
    for key in [Key::B, Key::I, Key::U, Key::Z, Key::N] {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            modifiers: Modifiers {
                command: true,
                ctrl: true,
                ..Default::default()
            },
            events: vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers {
                    command: true,
                    ctrl: true,
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            app.update_ui(ctx);
        });
    }

    assert_eq!(app.buffer.version, orig_version);
    assert_eq!(app.buffer.as_string(), orig_text);

    // Non-mutating Find bar toggle should still render cleanly in Reader mode.
    app.dispatch_action(&ctx, ToolbarAction::ToggleFindBar);
    assert!(app.find_bar.open);
    run_frames(&ctx, &mut app, 2);
}

#[test]
fn test_insert_image_and_wysiwyg_markup_toggle_with_real_png() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("# Greeting\n\nParagraph text\n");

    let img_path = dir.path().join("markdown_studio_test_badge.png");
    let img_buf = image::RgbaImage::from_pixel(8, 8, image::Rgba([37, 99, 235, 255]));
    img_buf.save(&img_path).expect("failed to write test PNG");
    let path_str = img_path.to_string_lossy().to_string();

    app.buffer.set_cursor(app.buffer.len_chars(), false);
    markdown_studio::document::editing::insert_image(&mut app.buffer, "Test Badge", &path_str);
    let md = app.buffer.as_string();
    assert!(md.contains("![Test Badge]("));
    assert!(verify_roundtrip_invariant(&md));

    let blocks = parse_to_ir(&md);
    let has_img_run = blocks.iter().any(|b| match b {
        DocBlock::Paragraph { runs, .. } => runs
            .iter()
            .any(|r| r.image_url.is_some() && r.text == "Test Badge"),
        _ => false,
    });
    assert!(has_img_run);

    // WYSIWYG defaults to Word experience (markup hidden).
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    assert!(!app.show_markup);
    run_frames(&ctx, &mut app, 3);

    // Toggle Show Markup on and back off cleanly.
    app.dispatch_action(&ctx, ToolbarAction::ToggleShowMarkup);
    assert!(app.show_markup);
    run_frames(&ctx, &mut app, 2);

    app.dispatch_action(&ctx, ToolbarAction::ToggleShowMarkup);
    assert!(!app.show_markup);
    run_frames(&ctx, &mut app, 2);

    // Reader mode also renders the inserted image without panics.
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    run_frames(&ctx, &mut app, 2);
}

#[test]
fn test_wysiwyg_inline_editing_and_formatting_preserves_word_experience() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("# Markdown Studio\n\nWelcome!\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    assert!(!app.show_markup);
    run_frames(&ctx, &mut app, 3);

    // Click heading line 0 directly in rendered WYSIWYG paper or activate line 0 cleanly
    let click_pos = Pos2::new(420.0, 155.0);
    for pressed in [true, false] {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            events: vec![Event::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            app.update_ui(ctx);
        });
    }
    if app.reader_state.active_block_lines.is_none() {
        assert!(markdown_studio::ui::reader::activate_wysiwyg_at_line(
            &mut app.buffer,
            &app.block_index,
            0,
            app.show_markup,
            &mut app.reader_state,
        ));
    }
    run_frames(&ctx, &mut app, 2);

    assert_eq!(app.reader_state.active_block_lines, Some(0..1));
    assert_eq!(app.reader_state.active_block_prefix, "# ");
    assert_eq!(app.reader_state.active_block_text, "Markdown Studio");

    // Move cursor to end of edited heading text and type " Pro" in WYSIWYG mode
    let type_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![
            Event::Key {
                key: Key::End,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Key {
                key: Key::End,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Text(" Pro".to_string()),
        ],
        ..Default::default()
    };
    let _ = ctx.run(type_input, |ctx| {
        app.update_ui(ctx);
    });
    run_frames(&ctx, &mut app, 2);

    assert_eq!(app.buffer.line_text(0), "# Markdown Studio Pro");
    assert_eq!(app.reader_state.active_block_prefix, "# ");
    assert_eq!(
        app.reader_state.active_block_text,
        "Markdown Studio Pro"
    );

    // Toggling Show Markup ON immediately reveals raw Markdown syntax inside active block
    app.dispatch_action(&ctx, ToolbarAction::ToggleShowMarkup);
    assert!(app.show_markup);
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.reader_state.active_block_prefix, "");
    assert_eq!(
        app.reader_state.active_block_text,
        "# Markdown Studio Pro"
    );

    // Toggling Show Markup back OFF hides "# " again cleanly
    app.dispatch_action(&ctx, ToolbarAction::ToggleShowMarkup);
    assert!(!app.show_markup);
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.reader_state.active_block_prefix, "# ");
    assert_eq!(
        app.reader_state.active_block_text,
        "Markdown Studio Pro"
    );
}

#[test]
fn test_selection_and_toolbar_formatting_bold_underline_alignment_indent() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("Hello world\nSecond line\n");
    app.buffer.selection_anchor = Some(0);
    app.buffer.cursor_char = 5; // Select "Hello"
    app.buffer.cache_current_selection();
    run_frames(&ctx, &mut app, 2);

    // Simulate focus blur stealing active selection_anchor before toolbar action runs
    app.buffer.selection_anchor = None;
    app.dispatch_action(&ctx, ToolbarAction::ToggleBold);
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.buffer.selected_text(), "Hello");
    assert_eq!(app.buffer.line_text(0), "**Hello** world");
    assert!(query_active_format(&app.buffer).bold);
    assert!(
        app.editor_state.request_focus
            || ctx.memory(|m| m.has_focus(egui::Id::new("virtual_rope_editor")))
    );

    // Toggle Underline on the preserved selection
    app.buffer.selection_anchor = None; // simulate blur again
    app.dispatch_action(&ctx, ToolbarAction::ToggleUnderline);
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.buffer.selected_text(), "Hello");
    assert_eq!(app.buffer.line_text(0), "**<u>Hello</u>** world");
    let fmt = query_active_format(&app.buffer);
    assert!(fmt.bold);
    assert!(fmt.underline);

    // Apply Center alignment and Indentation while preserving selection on "Hello"
    app.dispatch_action(&ctx, ToolbarAction::SetAlignment(BlockAlignment::Center));
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.buffer.selected_text(), "Hello");
    assert!(app.buffer.line_text(0).starts_with("<!-- align:center -->"));
    assert_eq!(
        query_active_format(&app.buffer).alignment,
        BlockAlignment::Center
    );

    app.dispatch_action(&ctx, ToolbarAction::Indent);
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.buffer.selected_text(), "Hello");
    assert_eq!(
        app.buffer.line_text(0),
        "<!-- align:center,indent:1 -->**<u>Hello</u>** world"
    );
    let fmt2 = query_active_format(&app.buffer);
    assert_eq!(fmt2.alignment, BlockAlignment::Center);
    assert_eq!(fmt2.indent_level, 1);
    assert!(fmt2.bold);
    assert!(fmt2.underline);

    // Verify AST + file save/reload persistence
    let file_path = dir.path().join("formatted.md");
    app.buffer.save_to_file(&file_path).expect("save md");
    let reloaded = DocumentBuffer::open_file(&file_path).expect("reload md");
    assert_eq!(reloaded.as_string(), app.buffer.as_string());

    let blocks = parse_to_ir(&reloaded.as_string());
    match &blocks[0] {
        DocBlock::Paragraph {
            runs,
            alignment,
            indent_level,
            direction,
            ..
        } => {
            assert_eq!(*alignment, BlockAlignment::Center);
            assert_eq!(*indent_level, 1);
            assert_eq!(*direction, TextDirection::Ltr);
            assert!(runs
                .iter()
                .any(|r| r.text == "Hello" && r.bold && r.underline));
        }
        other => panic!("expected Paragraph, got {other:?}"),
    }
}

#[test]
fn test_multilingual_arabic_rtl_and_emoji_utf8_roundtrip_and_graphemes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("multilingual_bidi.md");

    let raw_doc = "مرحبا بالعالم 👨‍👩‍👧‍👦\n\nHello World 👨👩👧👦!\n\n<!-- align:right,indent:1 -->مرحبا مع محاذاة\n";
    let mut buf = DocumentBuffer::from_str(raw_doc);

    // Verify BiDi auto-detection per paragraph
    assert_eq!(
        detect_paragraph_direction(&buf.line_text(0)),
        TextDirection::Rtl
    );
    assert_eq!(
        detect_paragraph_direction(&buf.line_text(2)),
        TextDirection::Ltr
    );

    // Grapheme-cluster-aware navigation across multi-codepoint emoji "👨👩👧👦" on line 2
    let line2_emoji_start = buf.line_col_to_char(2, 12);
    buf.set_cursor(line2_emoji_start, false);
    buf.move_right_grapheme(false);
    assert_eq!(buf.char_to_line_col(buf.cursor_char), (2, 16)); // stepped over full 4-codepoint family cluster to '!'
    buf.move_left_grapheme(false);
    assert_eq!(buf.char_to_line_col(buf.cursor_char), (2, 12));

    // Selection midpoint inside "👨👩👧👦" snaps outward so formatting never splits the emoji cluster
    buf.selection_anchor = Some(buf.line_col_to_char(2, 13));
    buf.cursor_char = buf.line_col_to_char(2, 15);
    toggle_bold(&mut buf);
    assert!(buf.line_text(2).contains("**👨👩👧👦**"));
    assert_eq!(buf.selected_text(), "👨👩👧👦");

    // Toggle Underline on Arabic greeting "مرحبا"
    buf.selection_anchor = Some(0);
    buf.cursor_char = 5;
    toggle_underline(&mut buf);
    assert!(buf.line_text(0).starts_with("<u>مرحبا</u> بالعالم 👨‍👩‍👧‍👦"));
    assert_eq!(
        detect_paragraph_direction(&buf.line_text(0)),
        TextDirection::Rtl
    );

    // Save UTF-8 to disk and reload byte-for-byte
    buf.save_to_file(&file_path).expect("save utf8 file");
    let disk_bytes = std::fs::read(&file_path).expect("read raw bytes");
    assert_eq!(
        std::str::from_utf8(&disk_bytes).expect("valid utf8 on disk"),
        buf.as_string()
    );

    let reloaded = DocumentBuffer::open_file(&file_path).expect("reload utf8 file");
    assert_eq!(reloaded.as_string(), buf.as_string());

    let blocks = parse_to_ir(&reloaded.as_string());
    assert_eq!(blocks.len(), 3);
    match &blocks[0] {
        DocBlock::Paragraph {
            runs, direction, ..
        } => {
            assert_eq!(*direction, TextDirection::Rtl);
            assert!(runs.iter().any(|r| r.underline && r.text == "مرحبا"));
        }
        other => panic!("expected Rtl paragraph, got {other:?}"),
    }
    match &blocks[1] {
        DocBlock::Paragraph { direction, .. } => assert_eq!(*direction, TextDirection::Ltr),
        other => panic!("expected Ltr paragraph, got {other:?}"),
    }
    match &blocks[2] {
        DocBlock::Paragraph {
            alignment,
            indent_level,
            direction,
            ..
        } => {
            assert_eq!(*alignment, BlockAlignment::Right);
            assert_eq!(*indent_level, 1);
            assert_eq!(*direction, TextDirection::Rtl);
        }
        other => panic!("expected Rtl aligned paragraph, got {other:?}"),
    }

    // Verify VisualBidiLineMap preserves 1-to-1 char lengths, keeps markup/tags/numbers LTR, and never splits ZWJ emojis
    let rtl_line = "مرحبا بالعالم 👨‍👩‍👧‍👦";
    let bidi_map = build_visual_bidi_line_map(rtl_line);
    assert_eq!(bidi_map.base_dir, TextDirection::Rtl);
    assert_eq!(bidi_map.logical_char_len, rtl_line.chars().count());
    assert_eq!(bidi_map.visual_text.chars().count(), rtl_line.chars().count());
    // Family emoji 👨‍👩‍👧‍👦 spans chars 14..21; logical_col_for_visual must never land strictly inside 15..20
    for vis_col in 0..=bidi_map.logical_char_len {
        let mapped = bidi_map.logical_col_for_visual(rtl_line, vis_col);
        assert!(
            mapped <= 14 || mapped >= 21,
            "visual column {vis_col} split ZWJ emoji cluster at {mapped}"
        );
    }

    let meta_rtl_line = "<!-- align:right,indent:1 -->مرحبا 2026-11-17";
    let meta_map = build_visual_bidi_line_map(meta_rtl_line);
    assert_eq!(meta_map.base_dir, TextDirection::Rtl);
    assert!(
        meta_map.visual_text.contains("<!-- align:right,indent:1 -->"),
        "block metadata tag scrambled in visual text: {:?}",
        meta_map.visual_text
    );
    assert!(
        meta_map.visual_text.contains("2026-11-17"),
        "date number punctuation scrambled in visual text: {:?}",
        meta_map.visual_text
    );

    let tag_rtl_line = "<u>مرحبا</u> بالعالم";
    let tag_map = build_visual_bidi_line_map(tag_rtl_line);
    assert_eq!(tag_map.base_dir, TextDirection::Rtl);
    assert!(
        tag_map.visual_text.contains("<u>") && tag_map.visual_text.contains("</u>"),
        "HTML underline tags scrambled in visual text: {:?}",
        tag_map.visual_text
    );

    // Ensure full UI renders mixed Arabic + ZWJ/family emojis smoothly across ALL modes including Source & Split,
    // and supports typing, grapheme navigation/deletion, double-click, and formatting in Source & Split!
    let ctx = egui::Context::default();
    let (_d2, mut app) = new_isolated_app(&ctx);
    app.buffer = reloaded;
    for mode in [
        ViewMode::Split,
        ViewMode::Wysiwyg,
        ViewMode::Reader,
        ViewMode::Source,
    ] {
        app.dispatch_action(&ctx, ToolbarAction::SetViewMode(mode));
        run_frames(&ctx, &mut app, 2);
    }

    // Verify Source mode renders RTL line right-aligned while LTR line remains left-aligned (dir="auto")
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    let out_src = run_frame_at(&ctx, &mut app, 1.0);
    let rtl_visual_prefix = shape_bidi_for_display("بالعالم", TextDirection::Rtl);
    let (_, rtl_ts) = find_rendered_text_shapes(&out_src, &rtl_visual_prefix)
        .into_iter()
        .find(|(_, ts)| ts.pos.y > 90.0)
        .expect("Source RTL line TextShape");
    let (_, ltr_ts) = find_rendered_text_shapes(&out_src, "Hello World")
        .into_iter()
        .find(|(_, ts)| ts.pos.y > 90.0)
        .expect("Source LTR line TextShape");
    assert!(
        rtl_ts.pos.x > ltr_ts.pos.x + 100.0,
        "Source RTL line ({}) must align right compared to LTR line ({})",
        rtl_ts.pos.x,
        ltr_ts.pos.x
    );

    // Type Arabic & ZWJ emoji directly in Source mode and verify 1-press grapheme Backspace deletes whole ZWJ cluster
    let end_char = app.buffer.len_chars();
    app.buffer.set_cursor(end_char, false);
    run_frames(&ctx, &mut app, 2);
    let source_type = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![Event::Text("سلام 👨‍👩‍👧‍👦".to_string())],
        ..Default::default()
    };
    let _ = ctx.run(source_type, |ctx| {
        app.update_ui(ctx);
    });
    run_frames(&ctx, &mut app, 2);
    assert!(app.buffer.as_string().ends_with("سلام 👨‍👩‍👧‍👦"));

    let bs_zwj = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![Event::Key {
            key: Key::Backspace,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(bs_zwj, |ctx| {
        app.update_ui(ctx);
    });
    assert!(
        app.buffer.as_string().ends_with("سلام "),
        "single Backspace in Source must remove whole ZWJ emoji cluster, got {:?}",
        app.buffer.as_string()
    );

    // Vertical ArrowUp onto line containing **👨👩👧👦** must snap to a valid grapheme boundary
    let line2_len = app.buffer.line_len_without_newline(2);
    let target_below = app.buffer.line_col_to_char(3, 14.min(app.buffer.line_len_without_newline(3)));
    app.buffer.set_cursor(target_below, false);
    let up_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![Event::Key {
            key: Key::ArrowUp,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(up_input, |ctx| app.update_ui(ctx));
    let (up_l, up_c) = app.buffer.char_to_line_col(app.buffer.cursor_char);
    assert_eq!(up_l, 2);
    let line2_str = app.buffer.line_text(2);
    let bounds2 = markdown_studio::document::unicode::grapheme_char_boundaries(&line2_str);
    assert!(
        bounds2.contains(&up_c.min(line2_len)),
        "ArrowUp landed inside multi-codepoint cluster at col {up_c} in {line2_str:?}"
    );

    // Split View: edit Arabic in the Source pane, format with toolbar, and verify lossless round-trip across view switches
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Split));
    app.editor_state.request_focus = true;
    run_frames(&ctx, &mut app, 2);
    let split_end = app.buffer.len_chars();
    app.buffer.set_cursor(split_end, false);
    let split_ime = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![Event::Ime(egui::ImeEvent::Commit("أهلا".to_string()))],
        ..Default::default()
    };
    let _ = ctx.run(split_ime, |ctx| app.update_ui(ctx));
    run_frames(&ctx, &mut app, 2);
    assert!(app.buffer.as_string().ends_with("أهلا"));

    // Format "أهلا" Bold via toolbar in Split View and switch Split -> Wysiwyg -> Reader -> Source
    let len_after_split = app.buffer.len_chars();
    app.buffer.selection_anchor = Some(len_after_split - 4);
    app.buffer.cursor_char = len_after_split;
    app.buffer.cache_current_selection();
    app.dispatch_action(&ctx, ToolbarAction::ToggleBold);
    assert!(app.buffer.as_string().ends_with("**أهلا**"));

    let expected_utf8 = app.buffer.as_string();
    for mode in [
        ViewMode::Wysiwyg,
        ViewMode::Reader,
        ViewMode::Source,
        ViewMode::Split,
    ] {
        app.dispatch_action(&ctx, ToolbarAction::SetViewMode(mode));
        run_frames(&ctx, &mut app, 2);
        assert_eq!(
            app.buffer.as_string(),
            expected_utf8,
            "switching to {mode:?} altered UTF-8 content"
        );
    }

    // Final lossless UTF-8 save & reload roundtrip check
    app.buffer.save_to_file(&file_path).expect("save split utf8");
    let reloaded_final = DocumentBuffer::open_file(&file_path).expect("reload split utf8");
    assert_eq!(reloaded_final.as_string(), expected_utf8);
}

#[test]
fn test_rapid_typing_formatting_and_undo_redo_across_wysiwyg_and_split() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("# Title\n\nParagraph text\n\n- First bullet\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    run_frames(&ctx, &mut app, 2);

    // Activate heading line 0 in WYSIWYG mode
    assert!(markdown_studio::ui::reader::activate_wysiwyg_at_line(
        &mut app.buffer,
        &app.block_index,
        0,
        false,
        &mut app.reader_state,
    ));
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.reader_state.active_block_text, "Title");

    // Rapid typing at end of line without dropped chars or caret jumps
    let type_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![
            Event::Key {
                key: Key::End,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Text(" Studio".to_string()),
        ],
        ..Default::default()
    };
    let _ = ctx.run(type_input, |ctx| {
        app.update_ui(ctx);
    });
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.buffer.line_text(0), "# Title Studio");
    assert_eq!(app.reader_state.active_block_text, "Title Studio");

    // Ctrl+Z Undo and Ctrl+Y Redo while WYSIWYG inline TextEdit is active must use DocumentBuffer cleanly
    let send_cmd = |ctx: &egui::Context, app: &mut MarkdownApp, key: Key| {
        let mods = Modifiers {
            command: true,
            ctrl: true,
            ..Default::default()
        };
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            modifiers: mods,
            events: vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: mods,
            }],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            app.update_ui(ctx);
        });
        run_frames(ctx, app, 1);
    };

    send_cmd(&ctx, &mut app, Key::Z);
    assert_ne!(app.buffer.line_text(0), "# Title Studio");
    send_cmd(&ctx, &mut app, Key::Y);
    assert_eq!(app.buffer.line_text(0), "# Title Studio");

    // Activate bullet item line 4, move to end, press Enter => automatic "- " continuation!
    assert!(markdown_studio::ui::reader::activate_wysiwyg_at_line(
        &mut app.buffer,
        &app.block_index,
        4,
        false,
        &mut app.reader_state,
    ));
    run_frames(&ctx, &mut app, 2);
    let enter_bullet = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![
            Event::Key {
                key: Key::End,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
        ..Default::default()
    };
    let _ = ctx.run(enter_bullet, |ctx| {
        app.update_ui(ctx);
    });
    run_frames(&ctx, &mut app, 2);
    assert!(app.buffer.as_string().contains("- First bullet\n- "));

    // Switch to Split View (preserving cursor context!) and type rapidly in Source pane while preview stays in sync
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Split));
    run_frames(&ctx, &mut app, 2);
    let split_type = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![Event::Text("Second item".to_string())],
        ..Default::default()
    };
    let _ = ctx.run(split_type, |ctx| {
        app.update_ui(ctx);
    });
    run_frames(&ctx, &mut app, 2);
    assert!(
        app.buffer.as_string().contains("- Second item"),
        "expected '- Second item' in:\n{}",
        app.buffer.as_string()
    );

    // Verify WYSIWYG Backspace at col 0 of heading cleanly un-prefixes back to paragraph
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    run_frames(&ctx, &mut app, 1);
    assert!(markdown_studio::ui::reader::activate_wysiwyg_at_line(
        &mut app.buffer,
        &app.block_index,
        0,
        false,
        &mut app.reader_state,
    ));
    run_frames(&ctx, &mut app, 2);
    for key in [Key::Home, Key::Backspace] {
        let step_input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            events: vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run(step_input, |ctx| {
            app.update_ui(ctx);
        });
        run_frames(&ctx, &mut app, 1);
    }
    assert_eq!(app.buffer.line_text(0), "Title Studio");
}

#[test]
fn test_find_and_replace_whole_word_case_regex_arabic_and_atomic_undo() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let doc = "Cat category cat.\nمرحبا ومرحبا بالعالم مرحبا!\nOrder 12 and Order 345.\n";
    app.buffer = DocumentBuffer::from_str(doc);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Split));
    run_frames(&ctx, &mut app, 2);

    // Ctrl+F opens Find bar
    let mods = Modifiers {
        command: true,
        ctrl: true,
        ..Default::default()
    };
    let open_find = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        modifiers: mods,
        events: vec![Event::Key {
            key: Key::F,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: mods,
        }],
        ..Default::default()
    };
    let _ = ctx.run(open_find, |ctx| {
        app.update_ui(ctx);
    });
    assert!(app.find_bar.open);

    // 1. Substring vs Whole Word vs Case Sensitive
    app.find_bar.query = "cat".to_string();
    app.find_bar.case_sensitive = false;
    app.find_bar.whole_word = false;
    app.find_bar.refresh_matches(&app.buffer);
    assert_eq!(app.find_bar.matches.len(), 3);

    app.find_bar.whole_word = true;
    app.find_bar.refresh_matches(&app.buffer);
    assert_eq!(app.find_bar.matches.len(), 2); // "Cat" and "cat", not "category"

    app.find_bar.case_sensitive = true;
    app.find_bar.refresh_matches(&app.buffer);
    assert_eq!(app.find_bar.matches.len(), 1); // only lowercase "cat"

    // 2. Unicode Whole Word for Arabic ("مرحبا" must not match "ومرحبا")
    app.find_bar.query = "مرحبا".to_string();
    app.find_bar.case_sensitive = true;
    app.find_bar.whole_word = true;
    app.find_bar.refresh_matches(&app.buffer);
    assert_eq!(app.find_bar.matches.len(), 2);

    // Navigation wrap-around (Next -> Next -> wraps back to first)
    app.buffer.set_cursor(0, false);
    assert!(app.find_bar.find_next(&mut app.buffer));
    let first_sel = app.buffer.selection_range().unwrap();
    assert_eq!(app.find_bar.active_match_idx, Some(0));
    assert!(app.find_bar.find_next(&mut app.buffer));
    assert_eq!(app.find_bar.active_match_idx, Some(1));
    assert!(app.find_bar.find_next(&mut app.buffer));
    assert_eq!(app.buffer.selection_range().unwrap(), first_sel);
    assert!(app.find_bar.find_prev(&mut app.buffer));
    assert_eq!(app.find_bar.active_match_idx, Some(1));

    // Render live highlights in Source, Split, and WYSIWYG without errors
    for mode in [ViewMode::Split, ViewMode::Source, ViewMode::Wysiwyg] {
        app.dispatch_action(&ctx, ToolbarAction::SetViewMode(mode));
        run_frames(&ctx, &mut app, 2);
    }

    // 3. Regex search + inline error feedback on invalid pattern
    app.find_bar.whole_word = false;
    app.find_bar.use_regex = true;
    app.find_bar.query = "[unclosed".to_string();
    app.find_bar.refresh_matches(&app.buffer);
    assert!(app.find_bar.regex_error.is_some());
    assert!(app.find_bar.matches.is_empty());

    app.find_bar.query = r"Order \d+".to_string();
    app.find_bar.refresh_matches(&app.buffer);
    assert!(app.find_bar.regex_error.is_none());
    assert_eq!(app.find_bar.matches.len(), 2);

    // 4. In Selection scope toggle (only line 0)
    app.find_bar.use_regex = false;
    app.find_bar.whole_word = false;
    app.find_bar.case_sensitive = false;
    app.find_bar.in_selection = true;
    app.find_bar.selection_scope = Some(0..17);
    app.find_bar.query = "Order".to_string();
    app.find_bar.refresh_matches(&app.buffer);
    assert!(app.find_bar.matches.is_empty());
    app.find_bar.query = "cat".to_string();
    app.find_bar.refresh_matches(&app.buffer);
    assert_eq!(app.find_bar.matches.len(), 3);

    // 5. Replace All wrapped as a SINGLE atomic Undo step (Ctrl+Z reverts all occurrences at once!)
    app.find_bar.in_selection = false;
    app.find_bar.selection_scope = None;
    app.find_bar.whole_word = true;
    app.find_bar.case_sensitive = true;
    app.find_bar.query = "مرحبا".to_string();
    app.find_bar.replacement = "أهلا".to_string();
    let count = app.find_bar.replace_all(&mut app.buffer);
    assert_eq!(count, 2);
    assert_eq!(
        app.buffer.as_string(),
        "Cat category cat.\nأهلا ومرحبا بالعالم أهلا!\nOrder 12 and Order 345.\n"
    );

    // ONE Undo restores exact original doc
    app.dispatch_action(&ctx, ToolbarAction::Undo);
    assert_eq!(app.buffer.as_string(), doc);

    // Ctrl+H opens Replace mode
    app.find_bar.open = false;
    let open_replace = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        modifiers: mods,
        events: vec![Event::Key {
            key: Key::H,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: mods,
        }],
        ..Default::default()
    };
    let _ = ctx.run(open_replace, |ctx| {
        app.update_ui(ctx);
    });
    assert!(app.find_bar.open && app.find_bar.show_replace);
}

#[test]
fn test_blank_document_on_startup_and_persistent_dark_mode_settings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings_path = dir.path().join("nested").join("settings.json");

    // First launch: clean empty document + default light mode
    let ctx1 = egui::Context::default();
    let mut app1 = MarkdownApp::new_with_settings_path(&ctx1, None, settings_path.clone());
    assert!(
        app1.buffer.is_empty(),
        "application must start with a blank document buffer"
    );
    assert!(!app1.theme.dark);
    assert!(!ctx1.style().visuals.dark_mode);

    // User cycles Light -> Sepia -> Dark Mode, switches to WYSIWYG mode, and updates split ratio & sync_scroll
    app1.dispatch_action(&ctx1, ToolbarAction::ToggleTheme);
    assert_eq!(
        app1.theme.mode,
        markdown_studio::ui::theme::ThemeMode::Sepia
    );
    assert!(!app1.theme.dark);
    app1.dispatch_action(&ctx1, ToolbarAction::ToggleTheme);
    assert_eq!(
        app1.theme.mode,
        markdown_studio::ui::theme::ThemeMode::Dark
    );
    app1.dispatch_action(&ctx1, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    app1.settings.split_ratio = 0.62;
    app1.settings.sync_scroll = false;
    app1.settings
        .save_to(&app1.settings_path)
        .expect("save settings");
    assert!(app1.theme.dark);
    assert!(settings_path.exists());

    // Simulate app restart with brand-new Context and same settings file
    let ctx2 = egui::Context::default();
    let mut app2 = MarkdownApp::new_with_settings_path(&ctx2, None, settings_path);

    // Must hydrate synchronously before initial UI render AND open blank document
    assert!(app2.buffer.is_empty());
    assert!(app2.theme.dark);
    assert!(
        ctx2.style().visuals.dark_mode,
        "dark mode must apply synchronously before initial frame"
    );
    assert_eq!(app2.view_mode, ViewMode::Wysiwyg);
    assert!((app2.split_ratio - 0.62).abs() < 0.01);
    assert!(!app2.settings.sync_scroll);

    // Toggling from Dark returns cleanly to Light
    app2.dispatch_action(&ctx2, ToolbarAction::ToggleTheme);
    assert_eq!(
        app2.theme.mode,
        markdown_studio::ui::theme::ThemeMode::Light
    );
    assert!(!app2.theme.dark);
}

fn collect_matching_text_shapes<'a>(
    shape: &'a egui::Shape,
    clip_rect: Rect,
    needle: &str,
    out: &mut Vec<(Rect, &'a egui::epaint::TextShape)>,
) {
    match shape {
        egui::Shape::Text(ts) => {
            if ts.galley.text().contains(needle) {
                out.push((clip_rect, ts));
            }
        }
        egui::Shape::Vec(shapes) => {
            for inner in shapes {
                collect_matching_text_shapes(inner, clip_rect, needle, out);
            }
        }
        _ => {}
    }
}

fn find_rendered_text_shapes<'a>(
    full_output: &'a egui::FullOutput,
    needle: &str,
) -> Vec<(Rect, &'a egui::epaint::TextShape)> {
    let mut found = Vec::new();
    for clipped in &full_output.shapes {
        collect_matching_text_shapes(&clipped.shape, clipped.clip_rect, needle, &mut found);
    }
    found
}

fn run_frame_at(ctx: &egui::Context, app: &mut MarkdownApp, time_sec: f64) -> egui::FullOutput {
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        time: Some(time_sec),
        ..Default::default()
    };
    ctx.run(input, |ctx| {
        app.update_ui(ctx);
    })
}

fn send_clicks(
    ctx: &egui::Context,
    app: &mut MarkdownApp,
    pos: Pos2,
    count: usize,
    time_sec: &mut f64,
) -> egui::FullOutput {
    let mut last_out = None;
    for _ in 0..count {
        for pressed in [true, false] {
            *time_sec += 0.04;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
                time: Some(*time_sec),
                events: vec![
                    Event::PointerMoved(pos),
                    Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            last_out = Some(ctx.run(input, |ctx| {
                app.update_ui(ctx);
            }));
        }
    }
    last_out.expect("at least one click")
}

#[test]
fn test_wysiwyg_word_alignment_indentation_and_direction_stay_visible_immediately() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("Hello\n\nSecond line\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    run_frames(&ctx, &mut app, 3);

    assert!(markdown_studio::ui::reader::activate_wysiwyg_at_line(
        &mut app.buffer,
        &app.block_index,
        0,
        app.show_markup,
        &mut app.reader_state,
    ));
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.reader_state.active_block_lines, Some(0..1));
    assert_eq!(app.reader_state.active_block_text, "Hello");

    // Apply every alignment, start/end, and indent/outdent action WITHOUT clicking another line.
    // On the very next frame after each action, "Hello" must remain active and inside its clip rect.
    let actions = [
        ToolbarAction::SetAlignment(BlockAlignment::Center),
        ToolbarAction::SetAlignment(BlockAlignment::Right),
        ToolbarAction::SetAlignment(BlockAlignment::End),
        ToolbarAction::SetAlignment(BlockAlignment::Start),
        ToolbarAction::SetAlignment(BlockAlignment::Justify),
        ToolbarAction::SetAlignment(BlockAlignment::Left),
        ToolbarAction::Indent,
        ToolbarAction::Indent,
        ToolbarAction::Outdent,
    ];

    for (step, action) in actions.into_iter().enumerate() {
        app.dispatch_action(&ctx, action.clone());
        let out = run_frame_at(&ctx, &mut app, (step as f64 + 1.0) * 0.1);
        assert_eq!(
            app.reader_state.active_block_lines,
            Some(0..1),
            "step {step} ({action:?}) dropped active WYSIWYG block"
        );
        assert_eq!(
            app.reader_state.active_block_text, "Hello",
            "step {step} ({action:?}) mutated active block text"
        );
        let shapes = find_rendered_text_shapes(&out, "Hello");
        assert!(
            !shapes.is_empty(),
            "step {step} ({action:?}) rendered no TextShape for Hello"
        );
        for (clip_rect, ts) in shapes {
            let bounds = ts.visual_bounding_rect();
            assert!(
                bounds.width() > 0.0 && bounds.height() > 0.0,
                "step {step} ({action:?}) produced degenerate bounds {bounds:?}"
            );
            assert!(
                bounds.left() >= clip_rect.left() - 1.0
                    && bounds.right() <= clip_rect.right() + 1.0
                    && bounds.top() >= clip_rect.top() - 1.0
                    && bounds.bottom() <= clip_rect.bottom() + 1.0,
                "step {step} ({action:?}) rendered Hello outside clip_rect: bounds={bounds:?}, clip={clip_rect:?}"
            );
        }
    }
}

#[test]
fn test_double_click_and_triple_click_selection_in_source_and_wysiwyg() {
    // 1. Source mode double-click word, double-click blank space/gutter, and triple-click line
    {
        let ctx = egui::Context::default();
        let (_dir, mut app) = new_isolated_app(&ctx);
        app.buffer = DocumentBuffer::from_str("alpha beta gamma\nsecond line here\n");
        app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
        let mut t = 1.0;
        let out = run_frame_at(&ctx, &mut app, t);
        let shapes = find_rendered_text_shapes(&out, "alpha beta gamma");
        let (clip_rect, ts) = shapes
            .into_iter()
            .find(|(_, ts)| ts.pos.y > 90.0)
            .expect("source line 0 shape");

        // Double-click inside the word "beta" (chars 6..10 -> probe char index 8)
        let beta_local = ts
            .galley
            .pos_from_ccursor(egui::text::CCursor::new(8))
            .center();
        let beta_pos = ts.pos + beta_local.to_vec2();
        send_clicks(&ctx, &mut app, beta_pos, 2, &mut t);
        assert_eq!(app.buffer.selected_text(), "beta");

        // Advance time beyond double-click delay, then double-click blank space past end of line 0
        t += 1.0;
        let _ = run_frame_at(&ctx, &mut app, t);
        let line0_y = ts.visual_bounding_rect().center().y;
        let blank_after_line0_x = (ts.pos.x + ts.galley.size().x + 80.0).min(clip_rect.right() - 20.0);
        send_clicks(
            &ctx,
            &mut app,
            Pos2::new(blank_after_line0_x, line0_y),
            2,
            &mut t,
        );
        assert_eq!(app.buffer.selected_text(), "alpha beta gamma");

        // Advance time and double-click in gutter to the left of line 1
        t += 1.0;
        let out2 = run_frame_at(&ctx, &mut app, t);
        let shapes1 = find_rendered_text_shapes(&out2, "second line here");
        let (_, ts1) = shapes1
            .into_iter()
            .find(|(_, ts)| ts.pos.y > 90.0)
            .expect("source line 1 shape");
        let gutter_x = (ts1.pos.x - 24.0).max(8.0);
        let line1_y = ts1.visual_bounding_rect().center().y;
        send_clicks(&ctx, &mut app, Pos2::new(gutter_x, line1_y), 2, &mut t);
        assert_eq!(app.buffer.selected_text(), "second line here");

        // Advance time and triple-click on "alpha" on line 0
        t += 1.0;
        let _ = run_frame_at(&ctx, &mut app, t);
        let alpha_local = ts
            .galley
            .pos_from_ccursor(egui::text::CCursor::new(2))
            .center();
        send_clicks(&ctx, &mut app, ts.pos + alpha_local.to_vec2(), 3, &mut t);
        assert_eq!(app.buffer.selected_text(), "alpha beta gamma");
    }

    // 2. WYSIWYG mode double-click word, double-click blank space after line, and triple-click line
    {
        let ctx = egui::Context::default();
        let (_dir, mut app) = new_isolated_app(&ctx);
        app.buffer = DocumentBuffer::from_str("alpha beta gamma\nsecond line here\n");
        app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
        let mut t = 1.0;
        let _ = run_frame_at(&ctx, &mut app, t);
        t += 0.1;
        let out = run_frame_at(&ctx, &mut app, t);

        let shapes = find_rendered_text_shapes(&out, "alpha beta gamma");
        let (clip_rect, ts) = shapes
            .into_iter()
            .find(|(_, ts)| ts.pos.y > 90.0)
            .expect("wysiwyg line 0 shape");

        // Double-click inside "beta" -> selects "beta"
        let beta_local = ts
            .galley
            .pos_from_ccursor(egui::text::CCursor::new(8))
            .center();
        let beta_pos = ts.pos + beta_local.to_vec2();
        let out_after_dbl = send_clicks(&ctx, &mut app, beta_pos, 2, &mut t);
        assert_eq!(
            app.buffer.selected_text(),
            "beta",
            "active={:?} cursor={} anchor={:?} beta_pos={:?} ts_pos={:?}",
            app.reader_state.active_block_lines,
            app.buffer.cursor_char,
            app.buffer.selection_anchor,
            beta_pos,
            ts.pos,
        );

        // Advance time past double-click delay, then double-click blank space past end of line 0
        t += 1.0;
        let out_idle = run_frame_at(&ctx, &mut app, t);
        let active_shapes = find_rendered_text_shapes(&out_idle, "alpha beta gamma");
        let (active_clip, active_ts) = active_shapes
            .into_iter()
            .find(|(_, s)| s.pos.y > 90.0)
            .unwrap_or((clip_rect, ts));
        let line0_y = active_ts.visual_bounding_rect().center().y;
        let blank_after_x =
            (active_ts.pos.x + active_ts.galley.size().x + 80.0).min(active_clip.right() - 20.0);
        let _ = out_after_dbl;
        send_clicks(
            &ctx,
            &mut app,
            Pos2::new(blank_after_x, line0_y),
            2,
            &mut t,
        );
        assert_eq!(app.buffer.selected_text(), "alpha beta gamma");

        // Advance time and triple-click on "alpha" -> selects whole line
        t += 1.0;
        let _ = run_frame_at(&ctx, &mut app, t);
        let alpha_local = active_ts
            .galley
            .pos_from_ccursor(egui::text::CCursor::new(2))
            .center();
        send_clicks(
            &ctx,
            &mut app,
            active_ts.pos + alpha_local.to_vec2(),
            3,
            &mut t,
        );
        assert_eq!(app.buffer.selected_text(), "alpha beta gamma");
    }
}

#[test]
fn test_multiline_rendering_tab_indent_and_wysiwyg_structural_transitions() {
    // 1. Multi-line paragraphs keep line breaks and per-line alignment/indent metadata,
    // `#Heading` without space normalizes into Heading, and `** bold **` normalizes into Bold.
    let md = "<!-- align:center -->Line one\n<!-- align:right,indent:2 -->Line two\n\n#Heading\n\n** bold ** text\n";
    let blocks = parse_to_ir(md);
    assert!(
        blocks.iter().any(|b| matches!(
            b,
            DocBlock::Paragraph { alignment: BlockAlignment::Center, indent_level: 0, runs, .. }
            if runs.iter().any(|r| r.text == "Line one")
        )),
        "expected Line one centered paragraph in {blocks:?}"
    );
    assert!(
        blocks.iter().any(|b| matches!(
            b,
            DocBlock::Paragraph { alignment: BlockAlignment::Right, indent_level: 2, runs, .. }
            if runs.iter().any(|r| r.text == "Line two")
        )),
        "expected Line two right-aligned indented paragraph in {blocks:?}"
    );
    assert!(
        blocks.iter().any(|b| matches!(
            b,
            DocBlock::Heading { level: 1, runs, .. }
            if runs.iter().any(|r| r.text == "Heading")
        )),
        "expected normalized #Heading in {blocks:?}"
    );
    assert!(
        blocks.iter().any(|b| matches!(
            b,
            DocBlock::Paragraph { runs, .. }
            if runs.iter().any(|r| r.bold && r.text == "bold")
        )),
        "expected normalized bold run in {blocks:?}"
    );

    // Render cleanly across Reader, Split, and WYSIWYG with all lines visible
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str(md);
    for mode in [ViewMode::Reader, ViewMode::Split, ViewMode::Wysiwyg] {
        app.dispatch_action(&ctx, ToolbarAction::SetViewMode(mode));
        run_frames(&ctx, &mut app, 2);
        let out = run_frame_at(&ctx, &mut app, 0.5);
        assert!(!find_rendered_text_shapes(&out, "Line one").is_empty());
        assert!(!find_rendered_text_shapes(&out, "Line two").is_empty());
        assert!(!find_rendered_text_shapes(&out, "Heading").is_empty());
        assert!(!find_rendered_text_shapes(&out, "bold").is_empty());
    }

    // 2. Pressing Tab in Source mode indents via metadata instead of turning line into raw CodeBlock
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    app.buffer = DocumentBuffer::from_str("Hello\n");
    app.editor_state.request_focus = true;
    run_frames(&ctx, &mut app, 2);
    let tab_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![
            Event::Key {
                key: Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Key {
                key: Key::Tab,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
        ..Default::default()
    };
    let _ = ctx.run(tab_input, |ctx| {
        app.update_ui(ctx);
    });
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.buffer.line_text(0), "<!-- indent:1 -->Hello");
    let after_tab_blocks = parse_to_ir(&app.buffer.as_string());
    assert!(matches!(
        after_tab_blocks.first(),
        Some(DocBlock::Paragraph { indent_level: 1, .. })
    ));

    // 3. Typing "# " at the start of a paragraph in WYSIWYG live-transitions to a Heading block
    app.buffer = DocumentBuffer::from_str("Hello\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    assert!(!app.show_markup);
    run_frames(&ctx, &mut app, 2);
    assert!(markdown_studio::ui::reader::activate_wysiwyg_at_line(
        &mut app.buffer,
        &app.block_index,
        0,
        app.show_markup,
        &mut app.reader_state,
    ));
    run_frames(&ctx, &mut app, 2);
    let type_heading_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![
            Event::Key {
                key: Key::Home,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Key {
                key: Key::Home,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Text("# ".to_string()),
        ],
        ..Default::default()
    };
    let _ = ctx.run(type_heading_input, |ctx| {
        app.update_ui(ctx);
    });
    run_frames(&ctx, &mut app, 2);
    assert_eq!(app.buffer.line_text(0), "# Hello");
    assert_eq!(app.reader_state.active_block_lines, Some(0..1));
    assert_eq!(app.reader_state.active_block_prefix, "# ");
    assert_eq!(app.reader_state.active_block_text, "Hello");
    assert!(matches!(
        app.block_index.blocks.first().map(|b| &b.block),
        Some(DocBlock::Heading { level: 1, .. })
    ));
}

#[test]
fn test_saved_status_badge_and_unsaved_prompts_on_new_open_close() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    // 1. Clean blank document starts in Saved state with green "✓ Saved" badge rendered.
    assert!(!app.buffer.dirty);
    let out_clean = run_frame_at(&ctx, &mut app, 0.5);
    assert!(
        !find_rendered_text_shapes(&out_clean, "Saved").is_empty(),
        "expected green Saved badge on untouched document"
    );

    // 2. Editing flips to Unsaved ("● Unsaved"), while undoing or deleting back to original restores Saved!
    app.buffer.insert(0, "Draft edit");
    assert!(app.buffer.dirty);
    let out_dirty = run_frame_at(&ctx, &mut app, 0.7);
    assert!(
        !find_rendered_text_shapes(&out_dirty, "Unsaved").is_empty(),
        "expected Unsaved badge after edit"
    );

    assert!(app.buffer.undo());
    assert!(
        !app.buffer.dirty,
        "undoing back to clean state must return status to Saved"
    );
    let out_undone = run_frame_at(&ctx, &mut app, 0.9);
    assert!(!find_rendered_text_shapes(&out_undone, "Saved").is_empty());

    // Manual edit + backspace back to identical content also restores Saved.
    app.buffer.insert(0, "X");
    assert!(app.buffer.dirty);
    app.buffer.backspace_grapheme();
    assert!(!app.buffer.dirty);

    // 3. Save to file -> clean; modify -> dirty; then New, Open, and Close all trigger Unsaved Changes modal.
    let file_path = dir.path().join("note.md");
    app.buffer.insert(0, "Original saved text\n");
    app.buffer.save_to_file(&file_path).expect("save note.md");
    assert!(!app.buffer.dirty);

    app.buffer.insert(app.buffer.len_chars(), "Unsaved trailing edit\n");
    assert!(app.buffer.dirty);

    // New creates a new tab without discarding edits in tab 0.
    app.dispatch_action(&ctx, ToolbarAction::NewFile);
    assert_eq!(app.tab_count(), 2);
    assert_eq!(app.active_tab_index(), 1);
    assert!(!app.unsaved_modal.is_open());

    // Switch back to dirty tab 0 -> edits are preserved, and closing it triggers Unsaved Changes modal.
    app.switch_to_tab(0);
    assert!(app.buffer.as_string().contains("Unsaved trailing edit"));
    app.dispatch_action(&ctx, ToolbarAction::CloseTab);
    assert_eq!(
        app.unsaved_modal.pending,
        Some(PendingDiscardAction::CloseTab(0))
    );
    assert!(app.buffer.as_string().contains("Unsaved trailing edit"));
    run_frames(&ctx, &mut app, 1);
    let out_modal = run_frame_at(&ctx, &mut app, 1.1);
    assert!(!find_rendered_text_shapes(&out_modal, "Unsaved Changes").is_empty());

    // Pressing Escape cancels modal and keeps edits intact.
    let esc_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(esc_input, |ctx| app.update_ui(ctx));
    assert!(!app.unsaved_modal.is_open());
    assert!(app.buffer.as_string().contains("Unsaved trailing edit"));

    // Close clean tab 1 without prompt.
    app.request_close_tab(&ctx, 1);
    assert_eq!(app.tab_count(), 1);
    assert!(!app.unsaved_modal.is_open());

    // Close request emits ViewportCommand::CancelClose and opens CloseWindow modal.
    let mut close_raw = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        ..Default::default()
    };
    close_raw.viewports.insert(
        egui::ViewportId::ROOT,
        egui::ViewportInfo {
            events: vec![egui::ViewportEvent::Close],
            ..Default::default()
        },
    );
    let close_out = ctx.run(close_raw, |ctx| app.update_ui(ctx));
    assert_eq!(
        app.unsaved_modal.pending,
        Some(PendingDiscardAction::CloseWindow)
    );
    let root_cmds = &close_out.viewport_output[&egui::ViewportId::ROOT].commands;
    assert!(
        root_cmds.contains(&egui::ViewportCommand::CancelClose),
        "expected CancelClose when closing with unsaved changes: {root_cmds:?}"
    );
}

#[test]
fn test_find_and_replace_arabic_multilang_and_emoji_rendering_and_search() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let doc = "مرحبا بالعالم! Café ☕ Привет 👋 👨‍👩‍👧‍👦\n";
    app.buffer = DocumentBuffer::from_str(doc);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    app.dispatch_action(&ctx, ToolbarAction::OpenReplaceBar);
    run_frames(&ctx, &mut app, 2);

    // Type Arabic + ZWJ family emoji directly into the focused Find input.
    let type_ar = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![
            Event::Ime(egui::ImeEvent::Commit("مرحبا ".to_string())),
            Event::Text("👨‍👩‍👧‍👦".to_string()),
        ],
        ..Default::default()
    };
    let _ = ctx.run(type_ar, |ctx| app.update_ui(ctx));
    assert_eq!(app.find_bar.query, "مرحبا 👨‍👩‍👧‍👦");

    // Backspace deletes the entire multi-codepoint emoji cluster cleanly in one press.
    let bs_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![Event::Key {
            key: Key::Backspace,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(bs_input, |ctx| app.update_ui(ctx));
    assert_eq!(app.find_bar.query, "مرحبا ");

    // Set query to "مرحبا" and check that the Find bar paints connected visual BiDi Arabic!
    app.find_bar.query = "مرحبا".to_string();
    app.find_bar.refresh_matches(&app.buffer);
    let out = run_frame_at(&ctx, &mut app, 1.0);
    let shaped_expected = shape_bidi_for_display("مرحبا", TextDirection::Rtl);
    assert!(
        !find_rendered_text_shapes(&out, &shaped_expected).is_empty(),
        "Find bar must render connected RTL-shaped Arabic '{shaped_expected}'"
    );

    // Searching multilingual tokens (Arabic, Cyrillic, accented Latin, and ZWJ emoji) + WYSIWYG selection preservation
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    run_frames(&ctx, &mut app, 2);

    for token in ["مرحبا", "Café", "Привет", "👨‍👩‍👧‍👦"] {
        app.find_bar.query = token.to_string();
        app.find_bar.whole_word = true;
        assert!(
            app.find_bar.find_next(&mut app.buffer),
            "expected whole-word match for '{token}'"
        );
        assert_eq!(app.buffer.selected_text(), token);
    }

    // Whole-word emoji search must NOT split an internal person out of a ZWJ family sequence.
    app.find_bar.query = "👨".to_string();
    app.find_bar.whole_word = true;
    app.find_bar.refresh_matches(&app.buffer);
    assert!(app.find_bar.matches.is_empty());

    // Replace ZWJ emoji with another emoji and verify exact UTF-8 result.
    app.find_bar.query = "👨‍👩‍👧‍👦".to_string();
    app.find_bar.replacement = "🎉".to_string();
    assert_eq!(app.find_bar.replace_all(&mut app.buffer), 1);
    assert_eq!(app.buffer.as_string(), "مرحبا بالعالم! Café ☕ Привет 👋 🎉\n");
}

#[test]
fn test_print_button_opens_print_dialog_instead_of_auto_printing() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("# Printable Report\n\nBody paragraph.\n");
    run_frames(&ctx, &mut app, 2);

    // Clicking Print or pressing Ctrl+P opens the interactive Print dialog without starting a job automatically.
    app.dispatch_action(&ctx, ToolbarAction::PrintDoc);
    assert!(app.print_modal.open);
    assert!(
        !app.exporter.busy,
        "Print button must open dialog first instead of printing automatically"
    );

    // Configure print options cleanly and render the dialog UI without errors.
    app.print_modal.options.copies = 3;
    app.print_modal.options.paper_size = PaperSize::Letter;
    app.print_modal.options.orientation = PageOrientation::Landscape;
    app.print_modal.options.duplex = DuplexMode::LongEdge;
    app.print_modal.options.all_pages = false;
    app.print_modal.options.page_range = "1-2".to_string();

    run_frames(&ctx, &mut app, 1);
    let out = run_frame_at(&ctx, &mut app, 0.5);
    assert!(!find_rendered_text_shapes(&out, "Print Document").is_empty());
    assert!(!find_rendered_text_shapes(&out, "Copies:").is_empty());
    assert!(!find_rendered_text_shapes(&out, "Paper Size:").is_empty());
    assert!(!app.exporter.busy);

    // Pressing Escape cancels the Print dialog without dispatching any print job.
    let esc_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(esc_input, |ctx| app.update_ui(ctx));
    assert!(!app.print_modal.open);
    assert!(!app.exporter.busy);
}

#[test]
fn test_arabic_typing_in_wysiwyg_source_and_split_renders_rtl_and_connects_lam_alef() {
    // 1. Lam-Alef must NOT be locked into a forward 2-char RTL cluster in VisualBidiLineMap:
    // "لا" visually reads left-to-right as Alef-final ('\u{FE8E}') then Lam-initial ('\u{FEDF}').
    let lam_alef_map = build_visual_bidi_line_map("لا");
    let vis_la: Vec<char> = lam_alef_map.visual_text.chars().collect();
    assert_eq!(vis_la, vec!['\u{FE8E}', '\u{FEDF}']);

    let salam_map = build_visual_bidi_line_map("سلام أهلا");
    assert_eq!(salam_map.base_dir, TextDirection::Rtl);
    assert_eq!(salam_map.visual_text.chars().count(), "سلام أهلا".chars().count());

    // 2. Typing Arabic from a blank document in WYSIWYG mode renders visually connected RTL immediately
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    run_frames(&ctx, &mut app, 2);

    let typed = "مرحبا لا";
    let mut t = 0.2;
    for ch in typed.chars() {
        t += 0.05;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(t),
            events: vec![Event::Text(ch.to_string())],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.update_ui(ctx));
    }
    t += 0.05;
    let out = run_frame_at(&ctx, &mut app, t);

    assert_eq!(app.buffer.as_string(), typed);
    assert_eq!(app.reader_state.active_block_lines, Some(0..1));
    assert_eq!(app.reader_state.active_block_text, typed);

    let expected_visual = build_visual_bidi_line_map(typed).visual_text;
    assert!(
        !find_rendered_text_shapes(&out, &expected_visual).is_empty(),
        "active WYSIWYG block must render connected RTL text {expected_visual:?}"
    );

    // Backspace after "لا" removes only 'ا', leaving "مرحبا ل".
    t += 0.05;
    let bs = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        time: Some(t),
        events: vec![Event::Key {
            key: Key::Backspace,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(bs, |ctx| app.update_ui(ctx));
    t += 0.05;
    let _ = run_frame_at(&ctx, &mut app, t);
    assert_eq!(app.buffer.as_string(), "مرحبا ل");

    // 3. Visual Left/Right arrow keys on Arabic RTL lines in WYSIWYG, Source, and Find bar:
    // From end of "مرحبا ل" (col 7), ArrowRight moves visually right -> towards logical start (col 6),
    // and ArrowLeft moves visually left -> back to logical end (col 7).
    let end_c = app.buffer.cursor_char;
    assert_eq!(end_c, "مرحبا ل".chars().count());
    let arrow_right = |ctx: &egui::Context, app: &mut MarkdownApp, time_val: &mut f64| {
        *time_val += 0.05;
        let inp = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(*time_val),
            events: vec![Event::Key {
                key: Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run(inp, |c| app.update_ui(c));
    };
    let arrow_left = |ctx: &egui::Context, app: &mut MarkdownApp, time_val: &mut f64| {
        *time_val += 0.05;
        let inp = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(*time_val),
            events: vec![Event::Key {
                key: Key::ArrowLeft,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run(inp, |c| app.update_ui(c));
    };

    arrow_right(&ctx, &mut app, &mut t);
    assert_eq!(app.buffer.cursor_char, end_c - 1);
    arrow_left(&ctx, &mut app, &mut t);
    assert_eq!(app.buffer.cursor_char, end_c);

    // Source mode visual Left/Right arrows on RTL line
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    app.buffer.set_cursor(end_c, false);
    run_frames(&ctx, &mut app, 2);
    arrow_right(&ctx, &mut app, &mut t);
    assert_eq!(app.buffer.cursor_char, end_c - 1);
    arrow_left(&ctx, &mut app, &mut t);
    assert_eq!(app.buffer.cursor_char, end_c);

    // Find bar visual Left/Right arrows on RTL query
    app.dispatch_action(&ctx, ToolbarAction::ToggleFindBar);
    run_frames(&ctx, &mut app, 2);
    let q_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        time: Some(t + 0.05),
        events: vec![Event::Text("مرحبا".to_string())],
        ..Default::default()
    };
    t += 0.05;
    let _ = ctx.run(q_input, |c| app.update_ui(c));
    assert_eq!(app.find_bar.query_input_state.cursor_char, 5);
    arrow_right(&ctx, &mut app, &mut t);
    assert_eq!(app.find_bar.query_input_state.cursor_char, 4);
    arrow_left(&ctx, &mut app, &mut t);
    assert_eq!(app.find_bar.query_input_state.cursor_char, 5);
}

#[test]
fn test_find_and_replace_full_ui_workflow_highlighting_focus_and_shortcuts() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let doc = "alpha beta alpha\ngamma alpha delta\n\nAlpha end\n";
    app.buffer = DocumentBuffer::from_str(doc);
    // Double-click / select "alpha" at 0..5 before pressing Ctrl+F
    app.buffer.selection_anchor = Some(0);
    app.buffer.cursor_char = 5;
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    run_frames(&ctx, &mut app, 2);

    let mut t = 0.2;
    let send_key = |app: &mut MarkdownApp, time: &mut f64, key: Key, mods: Modifiers| -> egui::FullOutput {
        *time += 0.05;
        let inp = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(*time),
            modifiers: mods,
            events: vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: mods,
            }],
            ..Default::default()
        };
        ctx.run(inp, |c| app.update_ui(c))
    };
    let send_text = |app: &mut MarkdownApp, time: &mut f64, txt: &str| -> egui::FullOutput {
        *time += 0.05;
        let inp = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(*time),
            events: vec![Event::Text(txt.to_string())],
            ..Default::default()
        };
        ctx.run(inp, |c| app.update_ui(c))
    };
    let find_button_rect = |out: &egui::FullOutput, label: &str| -> Rect {
        let mut rect = None;
        fn walk(s: &egui::Shape, needle: &str, out: &mut Option<Rect>) {
            match s {
                egui::Shape::Text(ts) if ts.galley.text() == needle => {
                    *out = Some(ts.visual_bounding_rect());
                }
                egui::Shape::Vec(v) => v.iter().for_each(|x| walk(x, needle, out)),
                _ => {}
            }
        }
        for c in &out.shapes {
            walk(&c.shape, label, &mut rect);
        }
        rect.unwrap_or_else(|| panic!("expected button/label {label:?} in output"))
    };

    let cmd = Modifiers {
        command: true,
        ctrl: true,
        ..Default::default()
    };

    // 1. Ctrl+F prefills selected word "alpha", keeps show_replace=false initially, and focuses Find.
    let _ = send_key(&mut app, &mut t, Key::F, cmd);
    assert!(app.find_bar.open);
    assert!(!app.find_bar.show_replace);
    assert_eq!(app.find_bar.query, "alpha");
    assert_eq!(app.find_bar.matches.len(), 4);
    assert_eq!(app.find_bar.active_match_idx, Some(0));
    let out = run_frame_at(&ctx, &mut app, t + 0.05);
    t += 0.05;
    assert!(!find_rendered_text_shapes(&out, "1 of 4").is_empty());

    // 2. Typing while Find is focused updates matches & counter immediately on the SAME frame.
    let out = send_text(&mut app, &mut t, "beta");
    assert_eq!(app.find_bar.query, "beta");
    assert!(!find_rendered_text_shapes(&out, "1 of 1").is_empty());
    let out = send_text(&mut app, &mut t, " ");
    let _ = send_key(&mut app, &mut t, Key::Backspace, Modifiers::NONE);
    let _ = out;
    let _ = send_key(&mut app, &mut t, Key::A, cmd);
    let _ = send_text(&mut app, &mut t, "alpha");
    assert_eq!(app.find_bar.query, "alpha");

    // 3. Ctrl+B / Ctrl+Z while Find input is focused must NOT mutate the document!
    let ver_before = app.buffer.version;
    let _ = send_key(&mut app, &mut t, Key::B, cmd);
    let _ = send_key(&mut app, &mut t, Key::Z, cmd);
    assert_eq!(app.buffer.version, ver_before);
    assert_eq!(app.buffer.as_string(), doc);

    // 4. Enter / Shift+Enter navigation + WYSIWYG selection/match highlighting while unfocused
    let _ = send_key(&mut app, &mut t, Key::Enter, Modifiers::NONE); // idx 1 ("11..16")
    assert_eq!(app.buffer.selection_range(), Some(11..16));
    assert_eq!(app.reader_state.active_block_lines, Some(0..1));
    let out = run_frame_at(&ctx, &mut app, t + 0.05);
    t += 0.05;
    // Active block in WYSIWYG must paint highlight rects even though Find input has focus!
    let mut painted_rects = 0usize;
    for c in &out.shapes {
        fn count_hi(s: &egui::Shape, n: &mut usize) {
            match s {
                egui::Shape::Rect(r)
                    if r.fill == egui::Color32::from_rgba_unmultiplied(255, 150, 20, 128) =>
                {
                    *n += 1;
                }
                egui::Shape::Vec(v) => v.iter().for_each(|x| count_hi(x, n)),
                _ => {}
            }
        }
        count_hi(&c.shape, &mut painted_rects);
    }
    assert!(
        painted_rects >= 1,
        "WYSIWYG must paint active Find highlight rect while Find input holds focus"
    );

    let shift_mods = Modifiers {
        shift: true,
        ..Default::default()
    };
    let _ = send_key(&mut app, &mut t, Key::Enter, shift_mods); // back to 0..5
    assert_eq!(app.buffer.selection_range(), Some(0..5));

    // 5. Clicking "Next ▶" and "Aa" updates matches on the same frame and keeps Find input focus!
    let out = run_frame_at(&ctx, &mut app, t + 0.05);
    t += 0.05;
    let next_rect = find_button_rect(&out, "Next ▶");
    let out = send_clicks(&ctx, &mut app, next_rect.center(), 1, &mut t);
    assert_eq!(app.buffer.selection_range(), Some(11..16));

    let aa_rect = find_button_rect(&out, "Aa");
    let out = send_clicks(&ctx, &mut app, aa_rect.center(), 1, &mut t);
    assert!(app.find_bar.case_sensitive);
    assert_eq!(app.find_bar.matches.len(), 3);
    assert!(!find_rendered_text_shapes(&out, "2 of 3").is_empty());
    // Still focused on Find input after clicking toolbar pills/buttons!
    assert!(app.find_bar.has_input_focus(&ctx));

    // 6. Ctrl+F when already open keeps Find open; F3 jumps next; Escape closes Find without closing WYSIWYG block
    let _ = send_key(&mut app, &mut t, Key::F, cmd);
    assert!(app.find_bar.open);
    let _ = send_key(&mut app, &mut t, Key::F3, Modifiers::NONE);
    assert_eq!(app.buffer.selection_range(), Some(23..28));
    let _ = send_key(&mut app, &mut t, Key::Escape, Modifiers::NONE);
    assert!(!app.find_bar.open);
    assert!(app.reader_state.active_block_lines.is_some());

    // 7. Replace within Selection scope adjusts scope bounds dynamically
    app.find_bar.case_sensitive = false;
    app.find_bar.in_selection = true;
    app.find_bar.selection_scope = Some(0..16); // covers both "alpha"s on line 0
    app.find_bar.query = "alpha".to_string();
    app.find_bar.replacement = "longword".to_string();
    app.find_bar.open_replace(&app.buffer);
    assert!(app.find_bar.show_replace);
    assert_eq!(app.find_bar.matches.len(), 2);
    app.buffer.set_cursor(0, false);
    assert!(app.find_bar.replace_and_next(&mut app.buffer));
    // Scope grew by +3 ("longword" vs "alpha") so second "alpha" inside line 0 is still found!
    assert_eq!(app.find_bar.selection_scope, Some(0..19));
    assert!(app.find_bar.replace_current(&mut app.buffer));
    assert_eq!(
        app.buffer.as_string(),
        "longword beta longword\ngamma alpha delta\n\nAlpha end\n"
    );

    // 8. Reader mode also renders highlighted inline TextFormat background on matching runs
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    app.find_bar.in_selection = false;
    app.find_bar.selection_scope = None;
    app.find_bar.query = "gamma".to_string();
    app.find_bar.refresh_matches(&app.buffer);
    let out = run_frame_at(&ctx, &mut app, t + 0.05);
    let mut reader_highlighted = false;
    for c in &out.shapes {
        fn check_bg(s: &egui::Shape, hit: &mut bool) {
            match s {
                egui::Shape::Text(ts) => {
                    if ts.galley.job.sections.iter().any(|sec| {
                        sec.format.background != egui::Color32::TRANSPARENT
                            && ts.galley.text()[sec.byte_range.clone()].contains("gamma")
                    }) {
                        *hit = true;
                    }
                }
                egui::Shape::Vec(v) => v.iter().for_each(|x| check_bg(x, hit)),
                _ => {}
            }
        }
        check_bg(&c.shape, &mut reader_highlighted);
    }
    assert!(
        reader_highlighted,
        "Reader mode must highlight matching runs inline"
    );
}

#[test]
fn test_insert_link_modal_ctrl_k_and_link_insertion() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("Hello Rust world\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    run_frames(&ctx, &mut app, 2);

    // Select "Rust" (chars 6..10) and trigger Ctrl+K
    app.buffer.selection_anchor = Some(6);
    app.buffer.cursor_char = 10;
    app.buffer.cache_current_selection();

    let cmd = Modifiers {
        command: true,
        ctrl: true,
        ..Default::default()
    };
    let k_ev = RawInput {
        time: Some(0.5),
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        modifiers: cmd,
        events: vec![Event::Key {
            key: Key::K,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: cmd,
        }],
        ..Default::default()
    };
    let _ = ctx.run(k_ev, |ctx| app.update_ui(ctx));
    assert!(app.insert_link_modal.open);
    assert_eq!(app.insert_link_modal.text, "Rust");

    app.insert_link_modal.url = "https://rust-lang.org".to_string();
    let enter_ev = RawInput {
        time: Some(0.7),
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events: vec![Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(enter_ev, |ctx| app.update_ui(ctx));
    assert!(!app.insert_link_modal.open);
    assert_eq!(
        app.buffer.as_string(),
        "Hello [Rust](https://rust-lang.org) world\n"
    );
}

#[test]
fn test_zoom_shortcuts_and_recent_files_json_roundtrip() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("# Zoom Test\n");
    run_frames(&ctx, &mut app, 2);

    app.dispatch_action(&ctx, ToolbarAction::ZoomIn);
    assert!((app.settings.zoom_factor - 1.1).abs() < 0.01);
    app.dispatch_action(&ctx, ToolbarAction::ZoomOut);
    assert!((app.settings.zoom_factor - 1.0).abs() < 0.01);
    app.dispatch_action(&ctx, ToolbarAction::ZoomIn);
    app.dispatch_action(&ctx, ToolbarAction::ZoomReset);
    assert!((app.settings.zoom_factor - 1.0).abs() < 0.01);

    let mut s = AppSettings {
        zoom_factor: 1.25,
        ..Default::default()
    };
    let win_path = std::path::PathBuf::from(r"C:\Users\Demo\docs\notes.md");
    s.record_recent_file(&win_path);
    let json = s.to_json_string();
    let parsed = AppSettings::from_json_str(&json);
    assert!((parsed.zoom_factor - 1.25).abs() < 0.01);
    assert_eq!(parsed.recent_files, vec![win_path]);
}

#[test]
fn test_reader_whole_word_highlighting_skips_substrings() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("cat category\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    app.find_bar.open = true;
    app.find_bar.query = "cat".to_string();
    app.find_bar.whole_word = true;
    app.find_bar.refresh_matches(&app.buffer);

    let out = run_frame_at(&ctx, &mut app, 0.5);
    let mut highlighted_slices: Vec<String> = Vec::new();
    for c in &out.shapes {
        fn collect(s: &egui::Shape, acc: &mut Vec<String>) {
            match s {
                egui::Shape::Text(ts) if ts.galley.text().contains("category") => {
                    for sec in &ts.galley.job.sections {
                        if sec.format.background != egui::Color32::TRANSPARENT {
                            acc.push(ts.galley.text()[sec.byte_range.clone()].to_string());
                        }
                    }
                }
                egui::Shape::Vec(v) => v.iter().for_each(|x| collect(x, acc)),
                _ => {}
            }
        }
        collect(&c.shape, &mut highlighted_slices);
    }
    assert_eq!(
        highlighted_slices,
        vec!["cat".to_string()],
        "whole-word Reader search must highlight only 'cat', not 'cat' inside 'category'"
    );
}

#[test]
fn test_pending_discard_open_specific_file_and_doc_id_increment() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);
    let file_b = dir.path().join("second.md");
    std::fs::write(&file_b, "# Second File\n").unwrap();

    app.buffer = DocumentBuffer::from_str("Unsaved draft\n");
    app.buffer.insert_at_cursor("!");
    assert!(app.buffer.dirty);
    let initial_doc_id = app.doc_id;

    app.dispatch_action(&ctx, ToolbarAction::OpenRecentFile(file_b.clone()));
    assert!(!app.unsaved_modal.is_open());
    assert_eq!(app.tab_count(), 2);
    assert_eq!(app.active_tab_index(), 1);
    assert_ne!(app.doc_id, initial_doc_id);
    assert_eq!(app.buffer.as_string(), "# Second File\n");
    assert!(!app.buffer.dirty);

    // Dirty draft in tab 0 remains preserved
    app.switch_to_tab(0);
    assert_eq!(app.buffer.as_string(), "!Unsaved draft\n");
    assert!(app.buffer.dirty);
}

#[test]
fn test_ctrl_scroll_wheel_zoom_in_out_and_clamp() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);
    let settings_path = dir.path().join("settings.json");

    let mut tall_doc = String::from("# Tall Document\n\n");
    for i in 0..200 {
        tall_doc.push_str(&format!("Line {i}: Sample paragraph content for zoom test.\n"));
    }
    app.buffer = DocumentBuffer::from_str(&tall_doc);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    run_frames(&ctx, &mut app, 2);

    let step_frame = |ctx: &egui::Context,
                      app: &mut MarkdownApp,
                      t: f64,
                      modifiers: Modifiers,
                      events: Vec<Event>| {
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(t),
            modifiers,
            events,
            ..Default::default()
        };
        ctx.run(raw_input, |ctx| {
            app.update_ui(ctx);
        })
    };

    assert!((app.settings.zoom_factor - 1.0).abs() < 0.001);

    // Ctrl + Scroll Up increments by +10% without scrolling the editor viewport
    step_frame(
        &ctx,
        &mut app,
        1.0,
        Modifiers::CTRL,
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            modifiers: Modifiers::CTRL,
        }],
    );
    assert!((app.settings.zoom_factor - 1.1).abs() < 0.001);
    assert!(
        app.editor_state.last_scroll_offset_y.abs() < 0.1,
        "Ctrl+Scroll must zoom instead of scrolling the document"
    );

    // Two more Ctrl + Scroll Up ticks -> 1.3
    for i in 0..2 {
        step_frame(
            &ctx,
            &mut app,
            1.05 + i as f64 * 0.02,
            Modifiers::CTRL,
            vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, 1.0),
                modifiers: Modifiers::CTRL,
            }],
        );
    }
    assert!((app.settings.zoom_factor - 1.3).abs() < 0.001);

    // Two Ctrl + Scroll Down ticks -> 1.1
    for i in 0..2 {
        step_frame(
            &ctx,
            &mut app,
            1.10 + i as f64 * 0.02,
            Modifiers::CTRL,
            vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -1.0),
                modifiers: Modifiers::CTRL,
            }],
        );
    }
    assert!((app.settings.zoom_factor - 1.1).abs() < 0.001);

    // Clamp at upper bound 2.0
    for i in 0..15 {
        step_frame(
            &ctx,
            &mut app,
            1.15 + i as f64 * 0.02,
            Modifiers::CTRL,
            vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, 1.0),
                modifiers: Modifiers::CTRL,
            }],
        );
    }
    assert!((app.settings.zoom_factor - 2.0).abs() < 0.001);

    // Clamp at lower bound 0.6
    for i in 0..20 {
        step_frame(
            &ctx,
            &mut app,
            1.50 + i as f64 * 0.02,
            Modifiers::CTRL,
            vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -1.0),
                modifiers: Modifiers::CTRL,
            }],
        );
    }
    assert!((app.settings.zoom_factor - 0.6).abs() < 0.001);

    // Trackpad pinch Event::Zoom multiplies factor: 0.6 * 1.5 = 0.9
    step_frame(
        &ctx,
        &mut app,
        2.0,
        Modifiers::NONE,
        vec![Event::Zoom(1.5)],
    );
    assert!((app.settings.zoom_factor - 0.9).abs() < 0.01);

    // Advance past 350ms debounce window and verify persisted to settings.json
    step_frame(&ctx, &mut app, 2.5, Modifiers::NONE, vec![]);
    let loaded = AppSettings::load_from(&settings_path);
    assert!((loaded.zoom_factor - 0.9).abs() < 0.01);
}

#[test]
fn test_middle_click_autoscroll_two_arrows_scrolls_up_down_and_cancels() {
    use egui::{PointerButton, Shape};
    use markdown_studio::ui::autoscroll::AutoScrollTarget;

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);

    let mut tall_doc = String::from("# Autoscroll Document\n\n");
    for i in 0..220 {
        tall_doc.push_str(&format!(
            "Paragraph {i}: Smooth middle-button auto-scrolling verification line.\n\n"
        ));
    }
    app.buffer = DocumentBuffer::from_str(&tall_doc);

    let run_step = |ctx: &egui::Context,
                    app: &mut MarkdownApp,
                    t: f64,
                    events: Vec<Event>| {
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(t),
            ..Default::default()
        };
        let mut raw_input = raw_input;
        raw_input.events = events;
        ctx.run(raw_input, |ctx| {
            app.update_ui(ctx);
        })
    };

    // 1) Test Source editor pane (tap-toggle + Escape cancel + hold-drag-release)
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    run_step(&ctx, &mut app, 0.10, vec![]);
    run_step(&ctx, &mut app, 0.12, vec![]);

    let editor_anchor = app.editor_state.last_pane_rect.center();
    // Quick middle click tap
    let out_start = run_step(
        &ctx,
        &mut app,
        1.00,
        vec![
            Event::PointerMoved(editor_anchor),
            Event::PointerButton {
                pos: editor_anchor,
                button: PointerButton::Middle,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    assert!(app.autoscroll.active);
    assert_eq!(app.autoscroll.target, AutoScrollTarget::Editor);
    fn count_autoscroll_shapes(shape: &Shape, anchor: Pos2, circles: &mut usize, tris: &mut usize) {
        match shape {
            Shape::Vec(v) => {
                for item in v {
                    count_autoscroll_shapes(item, anchor, circles, tris);
                }
            }
            Shape::Circle(c)
                if c.center.distance(anchor) < 1.0 && (c.radius - 15.0).abs() < 0.5 =>
            {
                *circles += 1;
            }
            Shape::Path(p)
                if p.points.len() == 3
                    && p.points.iter().all(|pt| pt.distance(anchor) < 18.0) =>
            {
                *tris += 1;
            }
            _ => {}
        }
    }
    let mut circles = 0usize;
    let mut tris = 0usize;
    for s in &out_start.shapes {
        count_autoscroll_shapes(&s.shape, editor_anchor, &mut circles, &mut tris);
    }
    assert!(
        circles >= 1 && tris >= 2,
        "expected circular indicator and 2 arrow triangles near anchor, found circles={circles}, tris={tris}"
    );

    run_step(
        &ctx,
        &mut app,
        1.05,
        vec![
            Event::PointerMoved(editor_anchor),
            Event::PointerButton {
                pos: editor_anchor,
                button: PointerButton::Middle,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    assert!(
        app.autoscroll.active,
        "quick middle-click tap must toggle autoscroll on"
    );

    // Move pointer 100px below anchor -> scroll downward
    let down_pos = editor_anchor + egui::vec2(0.0, 100.0);
    for i in 0..8 {
        run_step(
            &ctx,
            &mut app,
            1.10 + i as f64 * 0.03,
            vec![Event::PointerMoved(down_pos)],
        );
    }
    let peak_editor_scroll = app.editor_state.last_scroll_offset_y;
    assert!(
        peak_editor_scroll > 10.0,
        "moving below anchor should scroll editor downward, got {peak_editor_scroll}"
    );

    // Move pointer 100px above anchor -> scroll upward
    let up_pos = editor_anchor - egui::vec2(0.0, 100.0);
    for i in 0..5 {
        run_step(
            &ctx,
            &mut app,
            1.40 + i as f64 * 0.03,
            vec![Event::PointerMoved(up_pos)],
        );
    }
    assert!(
        app.editor_state.last_scroll_offset_y < peak_editor_scroll,
        "moving above anchor should scroll editor upward (was {peak_editor_scroll}, now {})",
        app.editor_state.last_scroll_offset_y
    );

    // Escape exits tap-toggle autoscroll
    run_step(
        &ctx,
        &mut app,
        1.60,
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(!app.autoscroll.active);

    // Hold-drag-release middle button: scrolls while held, exits on release
    run_step(
        &ctx,
        &mut app,
        2.00,
        vec![
            Event::PointerMoved(editor_anchor),
            Event::PointerButton {
                pos: editor_anchor,
                button: PointerButton::Middle,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    assert!(app.autoscroll.active);
    let before_drag_scroll = app.editor_state.last_scroll_offset_y;
    for i in 0..5 {
        run_step(
            &ctx,
            &mut app,
            2.05 + i as f64 * 0.03,
            vec![Event::PointerMoved(editor_anchor + egui::vec2(0.0, 90.0))],
        );
    }
    assert!(app.editor_state.last_scroll_offset_y > before_drag_scroll);
    run_step(
        &ctx,
        &mut app,
        2.25,
        vec![Event::PointerButton {
            pos: editor_anchor + egui::vec2(0.0, 90.0),
            button: PointerButton::Middle,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(
        !app.autoscroll.active,
        "releasing middle button after hold-drag must exit autoscroll"
    );

    // 2) Test Reader pane autoscroll as well
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    run_step(&ctx, &mut app, 3.00, vec![]);
    run_step(&ctx, &mut app, 3.02, vec![]);

    let reader_anchor = app.reader_state.last_pane_rect.center();
    run_step(
        &ctx,
        &mut app,
        3.10,
        vec![
            Event::PointerMoved(reader_anchor),
            Event::PointerButton {
                pos: reader_anchor,
                button: PointerButton::Middle,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    assert!(app.autoscroll.active);
    assert_eq!(app.autoscroll.target, AutoScrollTarget::Reader);
    run_step(
        &ctx,
        &mut app,
        3.15,
        vec![
            Event::PointerMoved(reader_anchor),
            Event::PointerButton {
                pos: reader_anchor,
                button: PointerButton::Middle,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    assert!(app.autoscroll.active);

    for i in 0..8 {
        run_step(
            &ctx,
            &mut app,
            3.20 + i as f64 * 0.03,
            vec![Event::PointerMoved(reader_anchor + egui::vec2(0.0, 100.0))],
        );
    }
    assert!(
        app.reader_state.last_scroll_offset_y > 10.0,
        "reader autoscroll downward expected > 10px, got {}",
        app.reader_state.last_scroll_offset_y
    );

    // Exit with Escape
    run_step(
        &ctx,
        &mut app,
        3.50,
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(!app.autoscroll.active);
}

#[test]
fn test_large_selection_ctrl_a_and_incremental_word_count_fast() {
    let chunk = "# Large Section Header\n\nThis paragraph contains twelve distinct words for benchmarking incremental rope word tracking.\n\n";
    let mut big_text = String::with_capacity(chunk.len() * 1400);
    for _ in 0..1400 {
        big_text.push_str(chunk);
    }
    assert!(big_text.len() > 100_000);

    let mut buf = DocumentBuffer::from_str(&big_text);
    assert_eq!(buf.word_count(), buf.as_string().split_whitespace().count());

    // Incremental edits: insert, remove, replace, undo, redo
    let mid = buf.len_chars() / 2;
    buf.set_cursor(mid, false);
    buf.insert_at_cursor(" alpha beta ");
    assert_eq!(buf.word_count(), buf.as_string().split_whitespace().count());

    buf.remove(mid..mid + 6);
    assert_eq!(buf.word_count(), buf.as_string().split_whitespace().count());

    buf.replace_range(10..30, "one two three");
    assert_eq!(buf.word_count(), buf.as_string().split_whitespace().count());

    assert!(buf.undo());
    assert_eq!(buf.word_count(), buf.as_string().split_whitespace().count());

    assert!(buf.redo());
    assert_eq!(buf.word_count(), buf.as_string().split_whitespace().count());

    // Select entire >100 KB document (Ctrl+A simulation)
    buf.selection_anchor = Some(0);
    buf.cursor_char = buf.len_chars();
    let expected_wc = buf.word_count();

    let fmt = query_active_format(&buf);
    assert!(!fmt.bold);

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = buf;
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Split));
    run_frames(&ctx, &mut app, 2);
    assert_eq!(expected_wc, app.buffer.word_count());
}

#[test]
fn test_core_ui_chrome_glyphs_present_at_startup() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    run_frames(&ctx, &mut app, 2);

    let prop = egui::FontId::proportional(14.0);
    for ch in [
        '✓', '▾', '▴', '▶', '◀', '↑', '●', '•', '—', '–', '−', '×', '·', '―', '∑', '◇',
    ] {
        assert!(
            ctx.fonts(|f| f.has_glyph(&prop, ch)),
            "expected startup core font stack to contain UI chrome glyph '{ch}' (U+{:04X})",
            ch as u32
        );
    }
}

#[test]
fn test_galley_caches_invalidate_across_font_atlas_rebuild() {
    use egui::{FontData, FontDefinitions, FontFamily, Shape};
    use std::sync::Arc;

    fn collect_doc_galleys(shape: &Shape, out: &mut Vec<Arc<egui::Galley>>) {
        match shape {
            Shape::Vec(v) => {
                for item in v {
                    collect_doc_galleys(item, out);
                }
            }
            Shape::Text(t) => {
                let txt = t.galley.text();
                if txt.contains("Atlas Rebuild Heading")
                    || txt.contains("cached Galley invalidation")
                    || txt.contains("fn demo()")
                    || txt.contains("CellVal")
                {
                    out.push(Arc::clone(&t.galley));
                }
            }
            _ => {}
        }
    }

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str(
        "# Atlas Rebuild Heading\n\n\
         This paragraph tests cached Galley invalidation on file open with [link](https://example.com).\n\n\
         ```rust\n\
         fn demo() {}\n\
         ```\n\n\
         - Item alpha\n\n\
         | ColA | ColB |\n\
         |---|---|\n\
         | CellVal | 42 |\n",
    );

    let step_collect = |ctx: &egui::Context, app: &mut MarkdownApp| -> Vec<Arc<egui::Galley>> {
        let mut collected = Vec::new();
        for i in 0..2 {
            let raw_input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
                time: Some(i as f64 * 0.02 + 0.1),
                ..Default::default()
            };
            let output = ctx.run(raw_input, |ctx| {
                app.update_ui(ctx);
            });
            if i == 1 {
                for s in &output.shapes {
                    collect_doc_galleys(&s.shape, &mut collected);
                }
            }
        }
        collected
    };

    // 1) Verify ReaderGalleyCache rebuilds every document Galley after TextureAtlas replacement
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    let reader_before = step_collect(&ctx, &mut app);
    assert!(
        reader_before.len() >= 3,
        "expected cached heading, paragraph, code, and cell galleys in Reader"
    );

    let mut modified_fonts = FontDefinitions::default();
    if let Some((_first_name, first_data)) = modified_fonts.font_data.iter().next() {
        let copy_data: Arc<FontData> = Arc::clone(first_data);
        modified_fonts
            .font_data
            .insert("custom_atlas_rebuild_test".to_owned(), copy_data);
        if let Some(list) = modified_fonts.families.get_mut(&FontFamily::Proportional) {
            list.push("custom_atlas_rebuild_test".to_owned());
        }
    }
    let bold_fallback = modified_fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    modified_fonts
        .families
        .insert(FontFamily::Name("bold".into()), bold_fallback);
    ctx.set_fonts(modified_fonts.clone());

    let reader_after = step_collect(&ctx, &mut app);
    assert!(reader_after.len() >= 3);
    for after_g in &reader_after {
        let after_ptr = Arc::as_ptr(after_g) as usize;
        assert!(
            reader_before
                .iter()
                .all(|b| Arc::as_ptr(b) as usize != after_ptr),
            "ReaderGalleyCache returned a stale pre-atlas-rebuild Galley pointer"
        );
    }

    // 2) Verify Source Editor LineLayoutCache also rebuilds every line Galley after TextureAtlas replacement
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    let source_before = step_collect(&ctx, &mut app);
    assert!(source_before.len() >= 3);

    modified_fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("custom_atlas_rebuild_test".to_owned());
    ctx.set_fonts(modified_fonts);

    let source_after = step_collect(&ctx, &mut app);
    assert!(source_after.len() >= 3);
    for after_g in &source_after {
        let after_ptr = Arc::as_ptr(after_g) as usize;
        assert!(
            source_before
                .iter()
                .all(|b| Arc::as_ptr(b) as usize != after_ptr),
            "LineLayoutCache returned a stale pre-atlas-rebuild Galley pointer"
        );
    }
}

#[test]
fn test_reader_smooth_scroll_keyboard_nav_and_back_to_top() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);

    let mut md = String::from("# Smooth Reader Start\n\n");
    md.push_str("```rust\nfn greet(name: &str) -> usize {\n    let count = 42; // comment\n    println!(\"hello {name}\");\n    count\n}\n```\n\n");
    for i in 0..150 {
        md.push_str(&format!(
            "Paragraph {i}: Smooth scrolling reading comfort verification block with rich typography.\n\n"
        ));
    }
    app.buffer = DocumentBuffer::from_str(&md);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));

    let step = |ctx: &egui::Context,
                app: &mut MarkdownApp,
                t: f64,
                modifiers: Modifiers,
                events: Vec<Event>| {
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(t),
            modifiers,
            events,
            ..Default::default()
        };
        ctx.run(raw_input, |ctx| {
            app.update_ui(ctx);
        })
    };

    for i in 0..3 {
        step(&ctx, &mut app, i as f64 * 0.02 + 0.05, Modifiers::NONE, vec![]);
    }
    assert!(app.reader_state.last_max_scroll_y > 500.0);

    // Space / PageDown schedules smooth scroll target
    step(
        &ctx,
        &mut app,
        1.0,
        Modifiers::NONE,
        vec![Event::Key {
            key: Key::PageDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(app.reader_state.smooth_scroll_target_y.is_some());

    // Advance frames so exponential smoothing glides past 360 px and reveals the floating Top pill
    let mut last_out = None;
    for i in 0..14 {
        last_out = Some(step(
            &ctx,
            &mut app,
            1.02 + i as f64 * 0.025,
            Modifiers::NONE,
            vec![],
        ));
    }
    assert!(
        app.reader_state.last_scroll_offset_y > 360.0,
        "expected PageDown smooth scroll past 360px, got {}",
        app.reader_state.last_scroll_offset_y
    );

    fn has_text_label(shape: &egui::Shape, needle: &str) -> bool {
        match shape {
            egui::Shape::Vec(v) => v.iter().any(|s| has_text_label(s, needle)),
            egui::Shape::Text(t) => t.galley.text().contains(needle),
            _ => false,
        }
    }
    let out_ref = last_out.expect("output present");
    assert!(
        out_ref.shapes.iter().any(|s| has_text_label(&s.shape, "↑ Top")),
        "expected floating '↑ Top' pill button when scrolled past 360 px"
    );

    let after_pgdn = app.reader_state.last_scroll_offset_y;

    // J scrolls down smoothly; wheel cancels smooth animation cleanly
    step(
        &ctx,
        &mut app,
        1.5,
        Modifiers::NONE,
        vec![Event::Key {
            key: Key::J,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(app.reader_state.smooth_scroll_target_y.unwrap_or(0.0) > after_pgdn);
    step(
        &ctx,
        &mut app,
        1.52,
        Modifiers::NONE,
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -1.0),
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(app.reader_state.smooth_scroll_target_y.is_none());

    // Home returns cleanly to top (0.0)
    step(
        &ctx,
        &mut app,
        1.6,
        Modifiers::NONE,
        vec![Event::Key {
            key: Key::Home,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    for i in 0..20 {
        step(
            &ctx,
            &mut app,
            1.62 + i as f64 * 0.025,
            Modifiers::NONE,
            vec![],
        );
    }
    assert!(
        app.reader_state.last_scroll_offset_y < 5.0,
        "Home smooth scroll expected to return near 0, got {}",
        app.reader_state.last_scroll_offset_y
    );
}

#[test]
fn test_reader_internal_anchor_and_footnote_navigation_plus_scroll_spy() {
    use egui::{PointerButton, Shape};

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);

    let mut md = String::from(
        "# Intro Heading\n\nGo to [Jump to Details](#details-section) now.\n\nReference sentence[^fn1].\n\n",
    );
    for i in 0..35 {
        md.push_str(&format!("Filler top paragraph {i} for layout spacing.\n\n"));
    }
    md.push_str("## Details Section\n\nDetailed section body content.\n\n");
    for i in 0..35 {
        md.push_str(&format!("Filler lower paragraph {i} for scroll spy.\n\n"));
    }
    md.push_str("## Final Section\n\nEnd body.\n\n[^fn1]: Resolved footnote destination.\n");

    app.buffer = DocumentBuffer::from_str(&md);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    app.dispatch_action(&ctx, ToolbarAction::ToggleOutline);

    let step = |ctx: &egui::Context, app: &mut MarkdownApp, t: f64, events: Vec<Event>| {
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(t),
            events,
            ..Default::default()
        };
        ctx.run(raw_input, |ctx| {
            app.update_ui(ctx);
        })
    };

    let mut init_out = None;
    for i in 0..3 {
        init_out = Some(step(&ctx, &mut app, i as f64 * 0.02 + 0.05, vec![]));
    }

    fn find_text_pos(shape: &Shape, needle: &str) -> Option<Pos2> {
        match shape {
            Shape::Vec(v) => v.iter().find_map(|s| find_text_pos(s, needle)),
            Shape::Text(t) => {
                let full = t.galley.text();
                let byte_idx = full.find(needle)?;
                let start_c = full[..byte_idx].chars().count();
                let mid_c = start_c + (needle.chars().count() / 2);
                let caret = t.galley.pos_from_ccursor(egui::text::CCursor::new(mid_c));
                Some(t.pos + caret.center().to_vec2())
            }
            _ => None,
        }
    }

    let plain_pos = init_out
        .as_ref()
        .and_then(|o| o.shapes.iter().find_map(|s| find_text_pos(&s.shape, "Go to")))
        .expect("expected plain prose before anchor link");
    step(
        &ctx,
        &mut app,
        0.5,
        vec![
            Event::PointerMoved(plain_pos),
            Event::PointerButton {
                pos: plain_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    step(
        &ctx,
        &mut app,
        0.52,
        vec![Event::PointerButton {
            pos: plain_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(
        app.reader_state.smooth_scroll_target_y.is_none(),
        "clicking plain text in a paragraph with a link must NOT activate the link"
    );

    let link_pos = init_out
        .as_ref()
        .and_then(|o| o.shapes.iter().find_map(|s| find_text_pos(&s.shape, "Jump to Details")))
        .expect("expected rendered anchor link in Reader");

    step(
        &ctx,
        &mut app,
        1.0,
        vec![
            Event::PointerMoved(link_pos),
            Event::PointerButton {
                pos: link_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    step(
        &ctx,
        &mut app,
        1.02,
        vec![Event::PointerButton {
            pos: link_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
    );

    assert!(
        app.reader_state.smooth_scroll_target_y.unwrap_or(0.0) > 200.0,
        "clicking #details-section must schedule smooth scroll to Details Section"
    );
    assert!(app.reader_state.highlight_block_lines.is_some());

    for i in 0..16 {
        step(&ctx, &mut app, 1.05 + i as f64 * 0.025, vec![]);
    }
    assert!(app.reader_state.last_scroll_offset_y > 200.0);

    // Clicking back to top then footnote jump also resolves FootnoteDefinition cleanly
    app.reader_state.scroll_to_y = Some(0.0);
    let mut top_out = None;
    for i in 0..2 {
        top_out = Some(step(&ctx, &mut app, 2.0 + i as f64 * 0.02, vec![]));
    }
    let fn_pos = top_out
        .as_ref()
        .and_then(|o| o.shapes.iter().find_map(|s| find_text_pos(&s.shape, "[^fn1]")))
        .expect("expected rendered footnote ref in Reader");

    step(
        &ctx,
        &mut app,
        2.1,
        vec![
            Event::PointerMoved(fn_pos),
            Event::PointerButton {
                pos: fn_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    step(
        &ctx,
        &mut app,
        2.12,
        vec![Event::PointerButton {
            pos: fn_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(
        app.reader_state.smooth_scroll_target_y.unwrap_or(0.0) > 400.0,
        "clicking footnote reference must smooth-scroll to footnote definition"
    );
}

#[test]
fn test_reading_comfort_settings_sepia_theme_and_focus_mode() {
    use markdown_studio::ui::reader::heading_slug;
    use markdown_studio::ui::settings::{LineSpacing, PageWidth};
    use markdown_studio::ui::theme::ThemeMode;

    assert_eq!(heading_slug("Architecture & Data Flow"), "architecture--data-flow");
    assert_eq!(heading_slug("Keyboard Shortcuts"), "keyboard-shortcuts");

    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);
    let settings_path = dir.path().join("settings.json");
    app.buffer = DocumentBuffer::from_str("# Reading Studio\n\nSepia and focus mode check.\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));

    app.dispatch_action(&ctx, ToolbarAction::SetThemeMode(ThemeMode::Sepia));
    app.dispatch_action(&ctx, ToolbarAction::SetPageWidth(PageWidth::Wide));
    app.dispatch_action(&ctx, ToolbarAction::SetLineSpacing(LineSpacing::Relaxed));
    app.dispatch_action(&ctx, ToolbarAction::ToggleFocusMode);
    run_frames(&ctx, &mut app, 3);

    assert_eq!(app.theme.mode, ThemeMode::Sepia);
    assert!(!app.theme.dark);
    assert_eq!(app.reader_state.page_width, PageWidth::Wide);
    assert_eq!(app.reader_state.line_spacing, LineSpacing::Relaxed);
    assert!(app.focus_mode);

    let reloaded = AppSettings::load_from(&settings_path);
    assert_eq!(reloaded.theme_mode, ThemeMode::Sepia);
    assert!(!reloaded.dark_mode);
    assert_eq!(reloaded.page_width, PageWidth::Wide);
    assert_eq!(reloaded.line_spacing, LineSpacing::Relaxed);

    // Pressing Escape exits Focus Mode cleanly
    let esc_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        time: Some(2.0),
        events: vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(esc_input, |ctx| {
        app.update_ui(ctx);
    });
    assert!(!app.focus_mode);
}

fn ui_step_with_mods(
    ctx: &egui::Context,
    app: &mut MarkdownApp,
    t: f64,
    events: Vec<Event>,
    modifiers: Modifiers,
) -> egui::FullOutput {
    let raw_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        time: Some(t),
        modifiers,
        events,
        ..Default::default()
    };
    ctx.run(raw_input, |ctx| {
        app.update_ui(ctx);
    })
}

fn ui_step(
    ctx: &egui::Context,
    app: &mut MarkdownApp,
    t: f64,
    events: Vec<Event>,
) -> egui::FullOutput {
    ui_step_with_mods(ctx, app, t, events, Modifiers::NONE)
}

fn find_shape_text_pos_in_band(
    shape: &egui::Shape,
    needle: &str,
    x_min: f32,
    x_max: f32,
    y_min: f32,
) -> Option<Pos2> {
    use egui::Shape;
    match shape {
        Shape::Vec(v) => v
            .iter()
            .find_map(|s| find_shape_text_pos_in_band(s, needle, x_min, x_max, y_min)),
        Shape::Text(t) if t.pos.y >= y_min => {
            let full = t.galley.text();
            let byte_idx = full.find(needle)?;
            let start_c = full[..byte_idx].chars().count();
            let mid_c = start_c + (needle.chars().count() / 2);
            let caret = t.galley.pos_from_ccursor(egui::text::CCursor::new(mid_c));
            let pos = t.pos + caret.center().to_vec2();
            if pos.x >= x_min && pos.x <= x_max {
                Some(pos)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn find_rendered_text(
    out: &egui::FullOutput,
    needle: &str,
    x_min: f32,
    x_max: f32,
    y_min: f32,
) -> Pos2 {
    if let Some(p) = out
        .shapes
        .iter()
        .find_map(|s| find_shape_text_pos_in_band(&s.shape, needle, x_min, x_max, y_min))
    {
        return p;
    }
    fn collect_hits(shape: &egui::Shape, needle: &str, hits: &mut Vec<(Pos2, String)>) {
        use egui::Shape;
        match shape {
            Shape::Vec(v) => {
                for s in v {
                    collect_hits(s, needle, hits);
                }
            }
            Shape::Text(t) => {
                let txt = t.galley.text();
                if txt.contains(needle) || hits.len() < 12 {
                    hits.push((t.pos, txt.chars().take(60).collect()));
                }
            }
            _ => {}
        }
    }
    let mut hits = Vec::new();
    for s in &out.shapes {
        collect_hits(&s.shape, needle, &mut hits);
    }
    panic!("expected rendered text {needle:?} in band [{x_min}..{x_max}, y>={y_min}]; sample text shapes: {hits:?}")
}

fn click_at(
    ctx: &egui::Context,
    app: &mut MarkdownApp,
    t: &mut f64,
    pos: Pos2,
    modifiers: Modifiers,
) -> egui::FullOutput {
    *t += 0.02;
    let _ = ui_step_with_mods(
        ctx,
        app,
        *t,
        vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            },
        ],
        modifiers,
    );
    *t += 0.02;
    ui_step_with_mods(
        ctx,
        app,
        *t,
        vec![Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers,
        }],
        modifiers,
    )
}

#[test]
fn test_links_work_across_reader_wysiwyg_split_source_and_tables() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let mut guide_md = String::from("# Linked Guide\n\n## Sec One\n\nIntro paragraph.\n\n");
    for i in 0..30 {
        guide_md.push_str(&format!("Guide filler paragraph {i}.\n\n"));
    }
    guide_md.push_str("## Sec Two\n\nArrived at target section.\n");
    let guide_path = dir.path().join("guide.md");
    std::fs::write(&guide_path, &guide_md).unwrap();

    let main_md = "\
# Link Test\n\n\
First [AlphaLink](https://alpha.example.com) middle prose [BetaWww](www.beta.example.org) and [UnsafeJs](javascript:alert(1)).\n\n\
See [DocsSource](https://rust-lang.org) and [GuideRef](guide.md#sec-two).\n\n\
| Column | Site |\n\
| --- | --- |\n\
| Item | [TableLink](https://table.example.org) |\n";
    let main_path = dir.path().join("main.md");
    std::fs::write(&main_path, main_md).unwrap();

    app.open_specific_file(&main_path);
    let mut t = 0.1_f64;

    // Phase A: Reader mode (single click opens each specific link, skips plain prose, blocks unsafe URI)
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    let mut out = ui_step(&ctx, &mut app, t, vec![]);
    for _ in 0..2 {
        t += 0.02;
        out = ui_step(&ctx, &mut app, t, vec![]);
    }

    let plain_pos = find_rendered_text(&out, "middle prose", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, plain_pos, Modifiers::NONE);
    assert_eq!(app.last_opened_link, None, "plain prose click must not open a link");

    let alpha_pos = find_rendered_text(&out, "AlphaLink", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, alpha_pos, Modifiers::NONE);
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://alpha.example.com")
    );

    let beta_pos = find_rendered_text(&out, "BetaWww", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, beta_pos, Modifiers::NONE);
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://www.beta.example.org")
    );

    let table_pos = find_rendered_text(&out, "TableLink", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, table_pos, Modifiers::NONE);
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://table.example.org")
    );

    let unsafe_pos = find_rendered_text(&out, "UnsafeJs", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, unsafe_pos, Modifiers::NONE);
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://table.example.org"),
        "unsafe javascript: link must not open"
    );

    // Phase B: WYSIWYG mode (plain click edits block; Ctrl+Click opens active and inactive links)
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    for _ in 0..3 {
        t += 0.02;
        out = ui_step(&ctx, &mut app, t, vec![]);
    }

    let wys_beta = find_rendered_text(&out, "BetaWww", 0.0, 1280.0, 110.0);
    out = click_at(&ctx, &mut app, &mut t, wys_beta, Modifiers::NONE);
    assert!(
        app.reader_state.active_block_lines.is_some(),
        "plain click on WYSIWYG link should enter inline editing"
    );
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://table.example.org"),
        "plain click in WYSIWYG must not open link"
    );

    let active_alpha = find_rendered_text(&out, "AlphaLink", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, active_alpha, Modifiers::CTRL);
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://alpha.example.com"),
        "Ctrl+Click on link inside active inline WYSIWYG block must open link"
    );

    t += 0.02;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    t += 0.02;
    out = ui_step(&ctx, &mut app, t, vec![]);
    assert!(app.reader_state.active_block_lines.is_none());

    let inactive_beta = find_rendered_text(&out, "BetaWww", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, inactive_beta, Modifiers::CTRL);
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://www.beta.example.org"),
        "Ctrl+Click on inactive WYSIWYG link must open link"
    );
    assert!(
        app.reader_state.active_block_lines.is_none(),
        "Ctrl+Click on inactive WYSIWYG link must not activate editor"
    );

    // Phase C: Split mode (preview Ctrl+Click opens link, plain click jumps source line, Source pane Ctrl+Click)
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Split));
    for _ in 0..3 {
        t += 0.02;
        out = ui_step(&ctx, &mut app, t, vec![]);
    }

    let split_preview_alpha = find_rendered_text(&out, "AlphaLink", 640.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, split_preview_alpha, Modifiers::CTRL);
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://alpha.example.com")
    );

    let split_preview_plain = find_rendered_text(&out, "middle prose", 640.0, 1280.0, 110.0);
    let _ = click_at(&ctx, &mut app, &mut t, split_preview_plain, Modifiers::NONE);
    assert_eq!(app.buffer.char_to_line(app.buffer.cursor_char), 2);
    t += 0.02;
    out = ui_step(&ctx, &mut app, t, vec![]);

    let split_src_docs = find_rendered_text(&out, "DocsSource", 0.0, 620.0, 110.0);
    click_at(&ctx, &mut app, &mut t, split_src_docs, Modifiers::CTRL);
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://rust-lang.org")
    );

    // Phase D: Source mode Ctrl+Click
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    for _ in 0..3 {
        t += 0.02;
        out = ui_step(&ctx, &mut app, t, vec![]);
    }
    app.last_opened_link = None;
    let src_alpha = find_rendered_text(&out, "AlphaLink", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, src_alpha, Modifiers::CTRL);
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://alpha.example.com")
    );

    // Phase E: Relative local .md + #fragment navigation in Reader
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    for _ in 0..3 {
        t += 0.02;
        out = ui_step(&ctx, &mut app, t, vec![]);
    }
    app.last_opened_link = None;
    let guide_pos = find_rendered_text(&out, "GuideRef", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, guide_pos, Modifiers::NONE);
    for _ in 0..3 {
        t += 0.02;
        let _ = ui_step(&ctx, &mut app, t, vec![]);
    }
    assert_eq!(
        app.buffer.file_path.as_deref(),
        Some(guide_path.as_path()),
        "relative markdown link must open target document in app"
    );
    assert!(
        app.reader_state.last_scroll_offset_y > 100.0,
        "linked #sec-two fragment must scroll to Sec Two after opening guide.md"
    );
}

#[test]
fn test_local_executable_and_unc_links_are_blocked() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let main_path = dir.path().join("main.md");
    let bat_path = dir.path().join("setup.bat");
    let exe_path = dir.path().join("tool.exe");
    let ps1_path = dir.path().join("script.ps1");
    let lnk_path = dir.path().join("shortcut.lnk");
    let safe_txt = dir.path().join("notes.txt");
    let safe_pdf = dir.path().join("manual.pdf");

    std::fs::write(&bat_path, "@echo off\n").unwrap();
    std::fs::write(&exe_path, b"MZ").unwrap();
    std::fs::write(&ps1_path, "Write-Host hi\n").unwrap();
    std::fs::write(&lnk_path, b"LNK").unwrap();
    std::fs::write(&safe_txt, "Safe plain notes\n").unwrap();
    std::fs::write(&safe_pdf, b"%PDF-1.4\n").unwrap();
    std::fs::write(&main_path, "# Host Doc\n").unwrap();

    app.open_specific_file(&main_path);
    app.open_external_links_with_os = false;

    for dangerous in [
        "./setup.bat",
        "./tool.exe",
        "./script.ps1",
        "./shortcut.lnk",
        r"\\server\share\file.exe",
        "//server/share/doc.md",
        "file:////server/share/payload.bat",
    ] {
        app.last_opened_link = None;
        app.exporter.status_text.clear();
        app.open_link_target(&ctx, dangerous);
        assert!(
            app.last_opened_link.is_none(),
            "dangerous link {dangerous} must be blocked before opening"
        );
        assert!(
            app.exporter.status_text.contains("Blocked unsafe"),
            "expected blocked status for {dangerous}, got: {}",
            app.exporter.status_text
        );
    }

    // Safe non-text document (.pdf) passes the dangerous-link filter
    app.last_opened_link = None;
    app.open_link_target(&ctx, "./manual.pdf");
    assert_eq!(app.last_opened_link.as_deref(), Some("./manual.pdf"));

    // Safe local .txt opens internally in editor
    app.last_opened_link = None;
    app.open_link_target(&ctx, "./notes.txt");
    assert_eq!(app.buffer.file_path.as_deref(), Some(safe_txt.as_path()));
}

#[test]
fn test_wysiwyg_code_block_insert_typing_tab_and_language_change() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("Hello world\n\n");
    app.buffer.set_cursor(app.buffer.len_chars(), false);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));

    let mut t = 0.1_f64;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    app.dispatch_action(&ctx, ToolbarAction::InsertCodeBlock);
    t += 0.02;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.02;
    let _ = ui_step(&ctx, &mut app, t, vec![]);

    assert!(
        app.reader_state.active_block_lines.is_some(),
        "InsertCodeBlock in WYSIWYG must immediately activate code editor"
    );

    // Type line 1 + Enter, then Tab, then line 2
    t += 0.02;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::Text("fn demo() {".to_string()),
            Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.02;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Key {
            key: Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    t += 0.02;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Text("println!(\"ok\");".to_string())],
    );
    t += 0.02;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    let content = app.buffer.as_string();
    assert!(
        content.contains("```rust\nfn demo() {\n    println!(\"ok\");\n```"),
        "expected fenced code block with indented body, got:\n{content}"
    );

    // Shift+Tab outdents the current line cleanly, then Home + Tab indents it back at line start
    t += 0.02;
    let _ = ui_step_with_mods(
        &ctx,
        &mut app,
        t,
        vec![Event::Key {
            key: Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::SHIFT,
        }],
        Modifiers::SHIFT,
    );
    assert!(
        app.buffer.as_string().contains("\nprintln!(\"ok\");\n```"),
        "Shift+Tab must outdent 4 leading spaces"
    );

    t += 0.02;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Key {
            key: Key::Home,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    t += 0.02;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Key {
            key: Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    t += 0.02;
    let mut out = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        app.buffer.as_string().contains("\n    println!(\"ok\");\n```"),
        "Home + Tab must re-indent line back to 4 leading spaces, got:\n{}",
        app.buffer.as_string()
    );

    // Custom language input box: click `"rust"` draft input, select all, type `"zig"`, press Enter
    let lang_input_pos = find_rendered_text(&out, "rust", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, lang_input_pos, Modifiers::NONE);
    t += 0.02;
    let _ = ui_step_with_mods(
        &ctx,
        &mut app,
        t,
        vec![Event::Key {
            key: Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND | Modifiers::CTRL,
        }],
        Modifiers::COMMAND | Modifiers::CTRL,
    );
    t += 0.02;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::Text("zig".to_string()),
            Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.02;
    out = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        app.buffer
            .as_string()
            .contains("```zig\nfn demo() {\n    println!(\"ok\");\n```"),
        "custom language box must update opening fence while preserving code body: {}",
        app.buffer.as_string()
    );
    assert!(
        app.reader_state.active_block_lines.is_some(),
        "changing language must keep code block active"
    );

    // Preset language ComboBox: open dropdown ("zig" label on ComboBox left of input) and choose "Python"
    let combo_pos = find_rendered_text(&out, "zig", 0.0, 475.0, 110.0);
    let _ = click_at(&ctx, &mut app, &mut t, combo_pos, Modifiers::NONE);
    t += 0.02;
    out = ui_step(&ctx, &mut app, t, vec![]);
    let py_pos = find_rendered_text(&out, "Python", 0.0, 1280.0, 110.0);
    let _ = click_at(&ctx, &mut app, &mut t, py_pos, Modifiers::NONE);
    t += 0.02;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        app.buffer
            .as_string()
            .contains("```python\nfn demo() {\n    println!(\"ok\");\n```"),
        "preset dropdown must update fence to python without losing code body: {}",
        app.buffer.as_string()
    );

    // Existing 2-line empty fence ```js\n``` edits without corrupting closing fence
    app.buffer = DocumentBuffer::from_str("```js\n```\n");
    app.reader_state.active_block_lines = None;
    for _ in 0..3 {
        t += 0.02;
        out = ui_step(&ctx, &mut app, t, vec![]);
    }
    let placeholder_pos = find_rendered_text(&out, "Click to type code...", 0.0, 1280.0, 110.0);
    click_at(&ctx, &mut app, &mut t, placeholder_pos, Modifiers::NONE);
    assert!(
        app.reader_state.active_block_lines.is_some(),
        "clicking empty code block placeholder must activate it"
    );
    t += 0.02;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Text("const x = 1;".to_string())],
    );
    assert_eq!(app.buffer.as_string(), "```js\nconst x = 1;\n```\n");
}

#[test]
fn test_toolbar_vivid_hover_feedback() {
    fn find_exact_label_pos(out: &egui::FullOutput, label: &str) -> Pos2 {
        fn walk(shape: &egui::Shape, label: &str) -> Option<Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, label)),
                egui::Shape::Text(t) if t.galley.text() == label => {
                    Some(t.pos + t.galley.rect.center().to_vec2())
                }
                _ => None,
            }
        }
        out.shapes
            .iter()
            .find_map(|s| walk(&s.shape, label))
            .unwrap_or_else(|| panic!("expected exact toolbar label {label:?}"))
    }

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let mut t = 1.0_f64;
    let out_idle = ui_step(&ctx, &mut app, t, vec![]);

    for label in ["New", "Open", "Save", "Find", "PDF", "Word (.docx)"] {
        let pos = find_exact_label_pos(&out_idle, label);
        t += 0.02;
        let out_hover = ui_step(&ctx, &mut app, t, vec![Event::PointerMoved(pos)]);
        assert_eq!(
            out_hover.platform_output.cursor_icon,
            egui::CursorIcon::PointingHand,
            "hovering toolbar button {label:?} must set PointingHand cursor"
        );
        // Verify a vivid hover stroke or accent_hover fill is painted around the hovered button.
        fn has_accent_near(
            shape: &egui::Shape,
            pos: Pos2,
            accent: egui::Color32,
            accent_hover: egui::Color32,
        ) -> bool {
            match shape {
                egui::Shape::Vec(v) => v
                    .iter()
                    .any(|s| has_accent_near(s, pos, accent, accent_hover)),
                egui::Shape::Rect(r) => {
                    r.rect.expand(4.0).contains(pos)
                        && (r.stroke.color == accent || r.fill == accent_hover)
                }
                egui::Shape::Path(p) => {
                    p.stroke.color == egui::epaint::ColorMode::Solid(accent)
                }
                egui::Shape::LineSegment { points, stroke } => {
                    stroke.color == accent
                        && (points[0].distance(pos) < 80.0 || points[1].distance(pos) < 80.0)
                }
                _ => false,
            }
        }
        let accent = app.theme.accent;
        let accent_hover = app.theme.accent_hover;
        let painted_accent = out_hover
            .shapes
            .iter()
            .any(|s| has_accent_near(&s.shape, pos, accent, accent_hover));
        assert!(
            painted_accent,
            "hovering toolbar button {label:?} must paint vivid accent highlight"
        );
    }
}

#[test]
fn test_responsive_toolbar_never_overlaps_across_widths_and_zoom() {
    fn collect_exact_label_rects(
        shape: &egui::Shape,
        labels: &[&str],
        y_max: f32,
        out: &mut Vec<(String, Rect)>,
    ) {
        match shape {
            egui::Shape::Vec(v) => {
                for s in v {
                    collect_exact_label_rects(s, labels, y_max, out);
                }
            }
            egui::Shape::Text(t) if t.pos.y <= y_max => {
                let txt = t.galley.text();
                if labels.contains(&txt) {
                    let rect = Rect::from_min_size(t.pos, t.galley.rect.size());
                    out.push((txt.to_string(), rect));
                }
            }
            _ => {}
        }
    }

    let tier1_labels = [
        "New",
        "Open",
        "Recent ▾",
        "Save",
        "Save As",
        "Undo",
        "Redo",
        "WYSIWYG",
        "Split",
        "Reader",
        "Source",
        "Find",
        "Explorer",
        "Outline",
        "Light Mode",
        "Sepia Mode",
        "Dark Mode",
        "OS Menu",
        "PDF",
        "Word (.docx)",
        "Export ▾",
        "Print",
    ];

    for (width, zoom, mode) in [
        (1280.0_f32, 1.0_f32, ViewMode::Split),
        (1100.0_f32, 1.10_f32, ViewMode::Wysiwyg),
        (980.0_f32, 1.0_f32, ViewMode::Split),
        (780.0_f32, 1.0_f32, ViewMode::Wysiwyg),
        (520.0_f32, 1.0_f32, ViewMode::Split),
        (520.0_f32, 1.0_f32, ViewMode::Reader),
        (1280.0_f32, 1.40_f32, ViewMode::Wysiwyg),
    ] {
        let ctx = egui::Context::default();
        let (_dir, mut app) = new_isolated_app(&ctx);
        app.dispatch_action(&ctx, ToolbarAction::SetViewMode(mode));
        app.settings.zoom_factor = zoom;

        let mut out = egui::FullOutput::default();
        for frame_idx in 0..4 {
            let raw_input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 800.0))),
                time: Some(1.0 + frame_idx as f64 * 0.02),
                ..Default::default()
            };
            out = ctx.run(raw_input, |ctx| {
                app.update_ui(ctx);
            });
        }

        let mut found = Vec::new();
        for s in &out.shapes {
            collect_exact_label_rects(&s.shape, &tier1_labels, 120.0, &mut found);
        }
        assert!(
            found.len() >= 14,
            "all Tier 1 buttons must be visible at width={width}, zoom={zoom}: found {found:?}"
        );
        for i in 0..found.len() {
            for j in (i + 1)..found.len() {
                let (ref name_a, rect_a) = found[i];
                let (ref name_b, rect_b) = found[j];
                let overlap = rect_a.intersect(rect_b);
                assert!(
                    !overlap.is_positive(),
                    "Tier 1 controls {name_a:?} ({rect_a:?}) and {name_b:?} ({rect_b:?}) overlap at width={width}, zoom={zoom}, mode={mode:?}"
                );
            }
        }
        if (width - 1280.0).abs() < 1.0 && (zoom - 1.0).abs() < 0.01 {
            for (name, rect) in &found {
                assert!(
                    rect.max.y <= 42.0,
                    "at 1280px 100% zoom, Tier 1 label {name:?} should stay on single row (rect={rect:?})"
                );
            }
        }
        let find_x = |lbl: &str| {
            found
                .iter()
                .find(|(n, _)| n == lbl)
                .map(|(_, r)| r.min.x)
                .unwrap_or_else(|| panic!("missing mode button {lbl:?} at width={width}, zoom={zoom}"))
        };
        let x_wys = find_x("WYSIWYG");
        let x_split = find_x("Split");
        let x_reader = find_x("Reader");
        let x_source = find_x("Source");
        assert!(
            x_wys < x_split && x_split < x_reader && x_reader < x_source,
            "View mode order must be WYSIWYG < Split < Reader < Source at width={width}, zoom={zoom}, got ({x_wys}, {x_split}, {x_reader}, {x_source})"
        );
    }
}

#[test]
fn test_undo_redo_history_popup_and_multi_step_jump() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("One");
    app.buffer.insert(3, " Two");
    app.buffer.insert(7, " Three");
    app.buffer.insert(13, " Four");
    assert_eq!(app.buffer.as_string(), "One Two Three Four");

    let mut t = 1.0_f64;
    let out = ui_step(&ctx, &mut app, t, vec![]);

    // Find the Undo label and click the adjacent "▾" dropdown arrow on its right
    let undo_pos = find_rendered_text(&out, "Undo", 0.0, 900.0, 0.0);
    let undo_chevron_pos = find_rendered_text(&out, "▾", undo_pos.x, undo_pos.x + 60.0, 0.0);
    let _ = click_at(&ctx, &mut app, &mut t, undo_chevron_pos, Modifiers::NONE);
    t += 0.02;
    let out_popup = ui_step(&ctx, &mut app, t, vec![]);

    // History popup must display numbered recent steps
    let step2_pos = find_rendered_text(&out_popup, "2. Typed", 0.0, 900.0, 25.0);
    let _ = click_at(&ctx, &mut app, &mut t, step2_pos, Modifiers::NONE);
    assert_eq!(app.buffer.as_string(), "One Two");
    assert_eq!(app.buffer.redo_depth(), 2);

    // Now click the Redo history dropdown and jump forward 2 steps back to "One Two Three Four"
    let redo_pos = find_rendered_text(&out, "Redo", 0.0, 900.0, 0.0);
    let redo_chevron_pos = find_rendered_text(&out, "▾", redo_pos.x, redo_pos.x + 60.0, 0.0);
    let _ = click_at(&ctx, &mut app, &mut t, redo_chevron_pos, Modifiers::NONE);
    t += 0.02;
    let out_redo_popup = ui_step(&ctx, &mut app, t, vec![]);
    let redo2_pos = find_rendered_text(&out_redo_popup, "2. Typed", 0.0, 900.0, 25.0);
    let _ = click_at(&ctx, &mut app, &mut t, redo2_pos, Modifiers::NONE);
    assert_eq!(app.buffer.as_string(), "One Two Three Four");
    assert_eq!(app.buffer.redo_depth(), 0);
}

#[test]
fn test_drag_and_drop_opens_any_text_file_and_rejects_binary() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let json_path = dir.path().join("config.json");
    std::fs::write(&json_path, "{\n  \"name\": \"markdown_studio\",\n  \"ok\": true\n}\n").unwrap();

    let rs_path = dir.path().join("main.rs");
    std::fs::write(&rs_path, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();

    let extless_path = dir.path().join("NOTES");
    std::fs::write(&extless_path, "Plain extensionless note file\n").unwrap();

    let bin_path = dir.path().join("blob.dat");
    std::fs::write(&bin_path, b"PK\x03\x04\x00\x00\x00\x00\xFF\xFE").unwrap();

    let drop_file = |ctx: &egui::Context, app: &mut MarkdownApp, path: std::path::PathBuf| {
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            dropped_files: vec![egui::DroppedFile {
                path: Some(path),
                ..Default::default()
            }],
            ..Default::default()
        };
        let _ = ctx.run(raw_input, |ctx| {
            app.update_ui(ctx);
        });
    };

    // Drop .json file
    drop_file(&ctx, &mut app, json_path.clone());
    assert_eq!(app.buffer.file_path.as_deref(), Some(json_path.as_path()));
    assert!(app.buffer.as_string().contains("\"markdown_studio\""));

    // Drop .rs file
    drop_file(&ctx, &mut app, rs_path.clone());
    assert_eq!(app.buffer.file_path.as_deref(), Some(rs_path.as_path()));
    assert!(app.buffer.as_string().contains("println!"));

    // Drop extensionless text file while dirty -> opens cleanly in a new tab preserving dirty .rs tab
    app.buffer.insert(0, "// dirty\n");
    let tabs_before_extless = app.tab_count();
    drop_file(&ctx, &mut app, extless_path.clone());
    assert!(!app.unsaved_modal.is_open());
    assert_eq!(app.tab_count(), tabs_before_extless + 1);
    assert_eq!(
        app.buffer.file_path.as_deref(),
        Some(extless_path.as_path())
    );
    assert!(app.buffer.as_string().contains("Plain extensionless note file"));

    // Drop binary file -> does not replace buffer and shows Unsupported binary status
    drop_file(&ctx, &mut app, bin_path);
    assert_eq!(
        app.buffer.file_path.as_deref(),
        Some(extless_path.as_path())
    );
    assert!(app.exporter.status_text.contains("Unsupported binary file"));
}

#[test]
fn test_os_integration_modal_and_settings_exe_path_roundtrip() {
    // 1. Settings JSON round-trip preserves custom_exe_path and auto_sync_exe_path
    let mut s = AppSettings::default();
    assert!(s.auto_sync_exe_path);
    assert_eq!(s.custom_exe_path, None);

    let custom = std::path::PathBuf::from(r"E:\MyApps\Markdown Studio.exe");
    s.custom_exe_path = Some(custom.clone());
    s.auto_sync_exe_path = false;
    let json = s.to_json_string();
    let parsed = AppSettings::from_json_str(&json);
    assert_eq!(parsed.custom_exe_path.as_ref(), Some(&custom));
    assert!(!parsed.auto_sync_exe_path);
    assert_eq!(parsed.effective_exe_path(), custom);

    // 2. Clicking "OS Menu" opens OsIntegrationModal, allows editing path & toggling checkboxes, and Esc closes it
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let mut t = 1.0_f64;
    let out = ui_step(&ctx, &mut app, t, vec![]);
    let os_btn_pos = find_rendered_text(&out, "OS Menu", 0.0, 1280.0, 0.0);
    let _ = click_at(&ctx, &mut app, &mut t, os_btn_pos, Modifiers::NONE);
    assert!(app.os_modal.open);

    t += 0.02;
    let out_modal = ui_step(&ctx, &mut app, t, vec![]);
    let _ = find_rendered_text(
        &out_modal,
        "Windows Explorer & Context Menu Setup",
        0.0,
        1280.0,
        0.0,
    );
    let _ = find_rendered_text(
        &out_modal,
        "Target Executable (.exe) Path:",
        0.0,
        1280.0,
        0.0,
    );

    // Close modal with Escape
    t += 0.02;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(!app.os_modal.open);
}

#[test]
fn test_toolbar_resize_sweep_no_vertical_char_wrap_or_blowup() {
    fn collect_toolbar_shapes(
        shape: &egui::Shape,
        pane_top_y: f32,
        out: &mut Vec<(String, Rect, usize)>,
    ) {
        match shape {
            egui::Shape::Vec(v) => {
                for s in v {
                    collect_toolbar_shapes(s, pane_top_y, out);
                }
            }
            egui::Shape::Text(t) if t.pos.y < pane_top_y => {
                let txt = t.galley.text().to_string();
                let rect = Rect::from_min_size(t.pos, t.galley.rect.size());
                let rows = t.galley.rows.len();
                out.push((txt, rect, rows));
            }
            _ => {}
        }
    }

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("# Heading\n\nBody text.\n");

    let mut t = 1.0_f64;
    // Sweep continuously from 1360 down to 480 in 80px steps (and 880/760/640) at 110% zoom & 100% zoom across modes
    for (zoom, mode) in [
        (1.10_f32, ViewMode::Wysiwyg),
        (1.0_f32, ViewMode::Split),
        (1.0_f32, ViewMode::Reader),
    ] {
        app.dispatch_action(&ctx, ToolbarAction::SetViewMode(mode));
        app.settings.zoom_factor = zoom;

        for width in (480..=1360).step_by(80) {
            let mut out = egui::FullOutput::default();
            for _ in 0..3 {
                t += 0.02;
                let raw_input = RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        Pos2::ZERO,
                        egui::vec2(width as f32, 800.0),
                    )),
                    time: Some(t),
                    ..Default::default()
                };
                out = ctx.run(raw_input, |ctx| {
                    app.update_ui(ctx);
                });
            }

            let pane_top_y = match mode {
                ViewMode::Split => app.editor_state.last_pane_rect.top(),
                ViewMode::Wysiwyg | ViewMode::Reader => app.reader_state.last_pane_rect.top(),
                ViewMode::Source => app.editor_state.last_pane_rect.top(),
            };
            assert!(
                pane_top_y > 40.0,
                "pane_top_y={pane_top_y} invalid at width={width}, mode={mode:?}"
            );
            let logical_w = width as f32 / zoom;
            let max_expected_toolbar_h = if logical_w >= 800.0 {
                217.0
            } else if logical_w >= 580.0 {
                257.0
            } else {
                310.0
            };
            assert!(
                pane_top_y <= max_expected_toolbar_h,
                "toolbar blew up vertically to {pane_top_y}px at width={width} (logical={logical_w:.1}), zoom={zoom}, mode={mode:?}"
            );

            let mut items = Vec::new();
            for s in &out.shapes {
                collect_toolbar_shapes(&s.shape, pane_top_y - 2.0, &mut items);
            }
            assert!(items.len() >= 15);
            for (txt, rect, rows) in &items {
                assert_eq!(
                    *rows, 1,
                    "toolbar item {txt:?} wrapped vertically into {rows} rows (rect={rect:?}) at width={width}, zoom={zoom}, mode={mode:?}"
                );
                assert!(
                    rect.height() <= 24.0,
                    "toolbar item {txt:?} height={} too tall at width={width}, zoom={zoom}, mode={mode:?}",
                    rect.height()
                );
            }
        }
    }
}

#[test]
fn test_insert_hr_math_mermaid_toolbar_and_wysiwyg_live_preview() {
    use markdown_studio::ui::theme::ThemeMode;

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("First paragraph with alpha\n");
    let mut t = 1.0_f64;

    // 1. Reader mode remains strictly read-only for InsertHorizontalRule, InsertMath, InsertMermaid
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    let snapshot = app.buffer.as_string();
    for action in [
        ToolbarAction::InsertHorizontalRule,
        ToolbarAction::InsertMath,
        ToolbarAction::InsertMermaid,
    ] {
        app.dispatch_action(&ctx, action);
        assert_eq!(app.buffer.as_string(), snapshot, "Reader mode must block mutations");
    }

    // 2. Toolbar renders "― HR", "∑ Math", and "◇ Mermaid" cleanly across all 3 themes
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    t += 0.05;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    for mode in [ThemeMode::Light, ThemeMode::Sepia, ThemeMode::Dark] {
        app.dispatch_action(&ctx, ToolbarAction::SetThemeMode(mode));
        t += 0.05;
        let _ = ui_step(&ctx, &mut app, t, vec![]);
        t += 0.05;
        let out = ui_step(&ctx, &mut app, t, vec![]);
        let _ = find_rendered_text(&out, "HR", 0.0, 1280.0, 0.0);
        let _ = find_rendered_text(&out, "Math", 0.0, 1280.0, 0.0);
        let _ = find_rendered_text(&out, "Mermaid", 0.0, 1280.0, 0.0);
    }
    app.dispatch_action(&ctx, ToolbarAction::SetThemeMode(ThemeMode::Light));

    // 3. InsertHorizontalRule directly under paragraph creates Paragraph + ThematicBreak (not Setext H2)
    let para_len = "First paragraph with alpha".chars().count();
    app.buffer.set_cursor(para_len, false);
    app.dispatch_action(&ctx, ToolbarAction::InsertHorizontalRule);
    let after_hr = app.buffer.as_string();
    assert!(after_hr.contains("\n\n---\n"));
    let blocks = parse_to_ir(&after_hr);
    assert!(matches!(blocks[0], DocBlock::Paragraph { .. }));
    assert!(blocks.iter().any(|b| matches!(b, DocBlock::ThematicBreak { .. })));

    // 4. Select "alpha" and toggle inline math via ToolbarAction::InsertMath twice
    let alpha_start = "First paragraph with ".chars().count();
    app.buffer.selection_anchor = Some(alpha_start);
    app.buffer.cursor_char = alpha_start + 5;
    app.dispatch_action(&ctx, ToolbarAction::InsertMath);
    assert!(app.buffer.as_string().contains("$alpha$"));
    let alpha_inner_start = app.buffer.as_string().find("alpha").unwrap();
    app.buffer.selection_anchor = Some(alpha_inner_start);
    app.buffer.cursor_char = alpha_inner_start + 5;
    app.dispatch_action(&ctx, ToolbarAction::InsertMath);
    assert!(!app.buffer.as_string().contains("$alpha$"));

    // 5. InsertMath on empty line inserts $$...$$ block and activates live WYSIWYG inline editor
    app.buffer.set_cursor(app.buffer.len_chars(), false);
    app.dispatch_action(&ctx, ToolbarAction::InsertMath);
    t += 0.04;
    let out_math = ui_step(&ctx, &mut app, t, vec![]);
    assert!(app.buffer.as_string().contains("$$"));
    assert!(app.reader_state.active_block_lines.is_some());
    let _ = find_rendered_text(&out_math, "Done (Esc)", 0.0, 1280.0, 40.0);

    // Commit with Escape, then InsertMermaid at end and switch template via "Template ▾"
    t += 0.04;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    app.buffer.set_cursor(app.buffer.len_chars(), false);
    app.dispatch_action(&ctx, ToolbarAction::InsertMermaid);
    t += 0.04;
    let out_mermaid = ui_step(&ctx, &mut app, t, vec![]);
    assert!(app.buffer.as_string().contains("```mermaid"));
    assert!(app.reader_state.active_block_lines.is_some());
    let tpl_btn = find_rendered_text(&out_mermaid, "Template", 0.0, 1280.0, 40.0);
    let _ = click_at(&ctx, &mut app, &mut t, tpl_btn, Modifiers::NONE);
    t += 0.02;
    let out_menu = ui_step(&ctx, &mut app, t, vec![]);
    let seq_item = find_rendered_text(&out_menu, "Sequence", 0.0, 1280.0, 40.0);
    let _ = click_at(&ctx, &mut app, &mut t, seq_item, Modifiers::NONE);
    t += 0.04;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        app.buffer.as_string().contains("sequenceDiagram"),
        "expected Template picker to switch Mermaid diagram to sequenceDiagram, got:\n{}",
        app.buffer.as_string()
    );
}

#[test]
fn test_math_and_all_six_mermaid_diagrams_render_across_modes_and_themes() {
    use markdown_studio::ui::theme::ThemeMode;

    fn count_vector_and_text_shapes(shape: &egui::Shape) -> (usize, usize) {
        match shape {
            egui::Shape::Vec(v) => v.iter().fold((0, 0), |(va, ta), s| {
                let (vc, tc) = count_vector_and_text_shapes(s);
                (va + vc, ta + tc)
            }),
            egui::Shape::Text(_) => (0, 1),
            egui::Shape::Rect(_)
            | egui::Shape::Circle(_)
            | egui::Shape::LineSegment { .. }
            | egui::Shape::Path(_)
            | egui::Shape::Mesh(_)
            | egui::Shape::QuadraticBezier(_)
            | egui::Shape::CubicBezier(_) => (1, 0),
            _ => (0, 0),
        }
    }

    let md = r#"# Physics $F = ma$

Inline math $E = mc^2$ and \(\alpha + \beta = \gamma\).

$$
\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}
$$

\[
A = \begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}
\]

```math
f(x) = \begin{cases} x^2 & x \ge 0 \\ -x & x < 0 \end{cases}
```

| Expression | Value |
| :--- | :---: |
| $\frac{a}{b}$ | $\sqrt{2}$ |

---

```mermaid
flowchart LR
    Start([Input]) --> Check{Valid?}
    Check -->|Yes| Save[(Store)]
    Check -->|No| Retry[Retry]
```

```mermaid
sequenceDiagram
    Alice->>Bob: Ping
    Bob-->>Alice: Pong
```

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> Running : start
```

```mermaid
classDiagram
    Animal <|-- Duck
    class Animal {
        +int age
        +eat()
    }
```

```mermaid
erDiagram
    USER ||--o{ ORDER : places
```

```mermaid
pie showData
    title Distribution
    "Alpha" : 60
    "Beta" : 40
```
"#;

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str(md);

    let mut t = 1.0_f64;
    for theme_mode in [ThemeMode::Light, ThemeMode::Sepia, ThemeMode::Dark] {
        app.dispatch_action(&ctx, ToolbarAction::SetThemeMode(theme_mode));
        for view_mode in [ViewMode::Reader, ViewMode::Wysiwyg, ViewMode::Split] {
            app.dispatch_action(&ctx, ToolbarAction::SetViewMode(view_mode));
            let mut out = egui::FullOutput::default();
            for _ in 0..3 {
                t += 0.03;
                out = ui_step(&ctx, &mut app, t, vec![]);
            }
            assert!(
                app.block_index.blocks.len() >= 11,
                "expected all blocks indexed in {view_mode:?}/{theme_mode:?}, got {}",
                app.block_index.blocks.len()
            );
            for w in app.block_index.blocks.windows(2) {
                assert!(
                    w[1].y_start > w[0].y_start,
                    "blocks must strictly advance vertically in {view_mode:?}/{theme_mode:?}"
                );
            }
            let (vec_count, txt_count) = out.shapes.iter().fold((0, 0), |(va, ta), s| {
                let (vc, tc) = count_vector_and_text_shapes(&s.shape);
                (va + vc, ta + tc)
            });
            assert!(vec_count >= 15, "expected rich vector/math/diagram primitives, got {vec_count}");
            assert!(txt_count >= 10, "expected rendered text primitives, got {txt_count}");
        }
    }
}

#[test]
fn test_delete_buttons_remove_code_math_mermaid_blocks_and_hide_in_reader() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str(
        "Intro\n\n```rust\nfn main() {}\n```\n\n$$\n1 + 2\n$$\n\n```mermaid\nflowchart LR\n    A --> B\n```\n",
    );

    // 1. Reader mode must NOT show Delete buttons on Code, Math, or Mermaid blocks
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    let mut t = 1.0_f64;
    let out_reader = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        find_rendered_text_shapes(&out_reader, "Delete").is_empty(),
        "Reader mode must never render block Delete buttons"
    );

    // 2. WYSIWYG mode: click Delete on inactive Rust code block, verify removal + Ctrl+Z undo
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    t += 0.03;
    let out_wys = ui_step(&ctx, &mut app, t, vec![]);
    let del_pos = find_rendered_text(&out_wys, "Delete", 0.0, 1280.0, 90.0);
    let _ = click_at(&ctx, &mut app, &mut t, del_pos, Modifiers::NONE);
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!app.buffer.as_string().contains("fn main()"));
    assert!(app.buffer.as_string().contains("1 + 2"));

    // Undo restores Rust block
    app.dispatch_action(&ctx, ToolbarAction::Undo);
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(app.buffer.as_string().contains("fn main()"));

    // 3. Insert active Math block and click Delete directly in active header
    app.buffer.set_cursor(app.buffer.len_chars(), false);
    app.dispatch_action(&ctx, ToolbarAction::InsertMath);
    t += 0.03;
    let out_active_math = ui_step(&ctx, &mut app, t, vec![]);
    assert!(app.reader_state.active_block_lines.is_some());
    // Find the Delete button inside the active block (below Done (Esc) y-band)
    let done_pos = find_rendered_text(&out_active_math, "Done (Esc)", 0.0, 1280.0, 90.0);
    let active_del = find_rendered_text(
        &out_active_math,
        "Delete",
        0.0,
        done_pos.x,
        done_pos.y - 12.0,
    );
    let _ = click_at(&ctx, &mut app, &mut t, active_del, Modifiers::NONE);
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(app.reader_state.active_block_lines.is_none());

    // 4. Delete Mermaid block in WYSIWYG mode
    let out_before_mmd_del = ui_step(&ctx, &mut app, t + 0.03, vec![]);
    let dels = find_rendered_text_shapes(&out_before_mmd_del, "Delete");
    let last_del = dels.last().expect("mermaid Delete button").1.pos;
    let _ = click_at(
        &ctx,
        &mut app,
        &mut t,
        Pos2::new(last_del.x + 12.0, last_del.y + 6.0),
        Modifiers::NONE,
    );
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!app.buffer.as_string().contains("```mermaid"));
}

#[test]
fn test_ribbon_tabs_equations_math_calc_and_auto_calc() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("$$\n\\frac{3}{4} + 2.25\n$$\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    let mut t = 1.0_f64;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let out_home = ui_step(&ctx, &mut app, t, vec![]);

    // 1. Switch to Equations ("∫ Math") ribbon tab by clicking it
    let math_tab_pos = find_rendered_text(&out_home, "∫ Math", 0.0, 1280.0, 30.0);
    let _ = click_at(&ctx, &mut app, &mut t, math_tab_pos, Modifiers::NONE);
    t += 0.03;
    let out_math_tab = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        !find_rendered_text_shapes(&out_math_tab, "Textbook ▾").is_empty(),
        "Equations tab must expose Textbook ▾ template palette"
    );
    assert!(
        !find_rendered_text_shapes(&out_math_tab, "= Calculate").is_empty(),
        "Equations tab must expose = Calculate button"
    );

    // 2. Enable Auto-Calc Math: live "= 3" pill renders without mutating buffer yet
    assert!(!app.settings.auto_calc_math);
    app.dispatch_action(&ctx, ToolbarAction::ToggleAutoCalcMath);
    assert!(app.settings.auto_calc_math);
    t += 0.03;
    let out_auto = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        !find_rendered_text_shapes(&out_auto, "= 3").is_empty(),
        "Auto-Calc Math must render live = 3 preview pill"
    );
    assert!(
        !app.buffer.as_string().contains("= 3"),
        "Auto-Calc preview alone must not mutate document buffer"
    );

    // 3. Explicit Calculate appends = 3 to fenced math block
    app.buffer.set_cursor(app.buffer.line_to_char(1), false);
    app.dispatch_action(&ctx, ToolbarAction::CalculateMath);
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        app.buffer.as_string().contains("= 3"),
        "CalculateMath must write calculated = 3 into equation: {}",
        app.buffer.as_string()
    );

    // 4. Switch to Tables ("⊞ Tables") ribbon tab and insert textbook math snippet
    let tables_tab_pos = find_rendered_text(&out_auto, "⊞ Tables", 0.0, 1280.0, 30.0);
    let _ = click_at(&ctx, &mut app, &mut t, tables_tab_pos, Modifiers::NONE);
    t += 0.03;
    let out_tables_tab = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        !find_rendered_text_shapes(&out_tables_tab, "fx Recalculate").is_empty(),
        "Tables tab must expose fx Recalculate"
    );

    app.buffer = DocumentBuffer::new();
    app.dispatch_action(&ctx, ToolbarAction::InsertMathSnippet("\\alpha + \\beta"));
    assert!(app.buffer.as_string().contains("\\alpha + \\beta"));
}

#[test]
fn test_excel_table_formulas_modal_live_preview_and_recalculate() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str(
        "| Item | Price | Qty | Total |\n| :--- | :---: | :---: | :---: |\n| Pen | 10 | 2 | =B1*C1 |\n| Book | 20 | 3 | =B2*C2 |\n| Sum | | | =SUM(D1:D2) |\n",
    );
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    let mut t = 1.0_f64;

    // 1. Auto-Calc Tables (default ON) displays live evaluated results cleanly
    assert!(app.settings.auto_calc_tables);
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let out_live = ui_step(&ctx, &mut app, t, vec![]);
    for expected in ["20", "60", "80"] {
        assert!(
            !find_rendered_text_shapes(&out_live, expected).is_empty(),
            "rendered formula table must display live evaluated value {expected:?}"
        );
    }

    // 2. Open TableEditorModal and verify Excel headers, formula bar, and Apply recalculation
    app.buffer.set_cursor(app.buffer.line_to_char(2), false);
    app.dispatch_action(&ctx, ToolbarAction::OpenTableEditor);
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let out_modal = ui_step(&ctx, &mut app, t, vec![]);
    assert!(app.table_modal.open);
    assert!(
        !find_rendered_text_shapes(&out_modal, "[A1]").is_empty(),
        "TableEditorModal must show active Excel cell ref [A1]"
    );
    assert!(
        !find_rendered_text_shapes(&out_modal, "fx Recalculate").is_empty(),
        "TableEditorModal must show fx Recalculate"
    );
    let apply_pos = find_rendered_text(&out_modal, "Apply Changes", 0.0, 1280.0, 80.0);
    let _ = click_at(&ctx, &mut app, &mut t, apply_pos, Modifiers::NONE);
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!app.table_modal.open);

    let saved_md = app.buffer.as_string();
    assert!(saved_md.contains("20 <!-- =B1*C1 -->"), "got:\n{saved_md}");
    assert!(saved_md.contains("60 <!-- =B2*C2 -->"), "got:\n{saved_md}");
    assert!(saved_md.contains("80 <!-- =SUM(D1:D2) -->"), "got:\n{saved_md}");
    assert!(verify_roundtrip_invariant(&saved_md));

    // 3. Modify source cell from 10 -> 15 and click CalculateTables to update cached values
    let updated_src = saved_md.replacen("| Pen | 10 |", "| Pen | 15 |", 1);
    app.buffer = DocumentBuffer::from_str(&updated_src);
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let out_recalc_live = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        !find_rendered_text_shapes(&out_recalc_live, "90").is_empty(),
        "auto_calc_tables must reflect updated sum 90 live before explicit recalculation"
    );
    app.dispatch_action(&ctx, ToolbarAction::CalculateTables);
    let final_md = app.buffer.as_string();
    assert!(final_md.contains("30 <!-- =B1*C1 -->"), "got:\n{final_md}");
    assert!(final_md.contains("90 <!-- =SUM(D1:D2) -->"), "got:\n{final_md}");
}

#[test]
fn test_reader_mode_blocks_math_calc_and_table_formula_mutations() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let initial = "$$\n2 + 3\n$$\n\n| A | B |\n| - | - |\n| 5 | =A1*2 |\n";
    app.buffer = DocumentBuffer::from_str(initial);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));

    let mut t = 1.0_f64;
    let out_reader = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        find_rendered_text_shapes(&out_reader, "= Calc").is_empty(),
        "Reader mode must not show mutating = Calc button"
    );
    assert!(
        find_rendered_text_shapes(&out_reader, "fx Recalculate").is_empty(),
        "Reader mode must not show mutating fx Recalculate button"
    );

    for action in [
        ToolbarAction::InsertInlineMath,
        ToolbarAction::InsertMathSnippet("\\pi"),
        ToolbarAction::InsertMathTemplate("x^2"),
        ToolbarAction::CalculateMath,
        ToolbarAction::InsertTableFormula("=SUM(ABOVE)"),
        ToolbarAction::CalculateTables,
    ] {
        app.dispatch_action(&ctx, action);
        t += 0.02;
        let _ = ui_step(&ctx, &mut app, t, vec![]);
    }
    assert_eq!(app.buffer.as_string(), initial);
}

#[test]
fn test_multi_tab_opening_switching_closing_dedup_and_shortcuts() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let file_a = dir.path().join("alpha.md");
    let file_b = dir.path().join("beta.md");
    std::fs::write(&file_a, "# Alpha Doc\n\nFirst body.\n").unwrap();
    std::fs::write(&file_b, "# Beta Doc\n\nSecond body.\n").unwrap();

    // Initial blank clean tab is reused by the first opened file
    assert_eq!(app.tab_count(), 1);
    app.open_specific_file(&file_a);
    assert_eq!(app.tab_count(), 1);
    assert_eq!(app.active_tab_index(), 0);
    assert_eq!(app.buffer.file_path.as_deref(), Some(file_a.as_path()));

    // Opening a second file creates tab 2 and switches to it
    app.open_specific_file(&file_b);
    assert_eq!(app.tab_count(), 2);
    assert_eq!(app.active_tab_index(), 1);
    assert_eq!(app.buffer.file_path.as_deref(), Some(file_b.as_path()));

    // Edit beta.md + check cursor/dirty isolation when switching back to alpha.md
    app.buffer.insert(app.buffer.len_chars(), "Edited beta.\n");
    app.buffer.set_cursor(5, false);
    assert!(app.buffer.dirty);

    // Opening an already-open file switches to existing tab without duplicating
    app.open_specific_file(&file_a);
    assert_eq!(app.tab_count(), 2);
    assert_eq!(app.active_tab_index(), 0);
    assert_eq!(app.buffer.file_path.as_deref(), Some(file_a.as_path()));
    assert!(!app.buffer.dirty);

    // Ctrl+T creates a 3rd untitled tab; Ctrl+Tab cycles tabs
    let mut t = 1.0_f64;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    let send_cmd_key = |ctx: &egui::Context, app: &mut MarkdownApp, time: &mut f64, key: Key, shift: bool| {
        *time += 0.03;
        let mods = Modifiers {
            command: true,
            ctrl: true,
            shift,
            ..Default::default()
        };
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(*time),
            modifiers: mods,
            events: vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: mods,
            }],
            ..Default::default()
        };
        ctx.run(raw, |ctx| app.update_ui(ctx))
    };

    let out_tabs = send_cmd_key(&ctx, &mut app, &mut t, Key::T, false);
    assert_eq!(app.tab_count(), 3);
    assert_eq!(app.active_tab_index(), 2);
    assert!(
        !find_rendered_text_shapes(&out_tabs, "alpha.md").is_empty()
            && !find_rendered_text_shapes(&out_tabs, "beta.md").is_empty(),
        "tab bar must display open filenames"
    );

    // Ctrl+Tab from tab 2 wraps to tab 0; Ctrl+Shift+Tab goes back to tab 2
    let _ = send_cmd_key(&ctx, &mut app, &mut t, Key::Tab, false);
    assert_eq!(app.active_tab_index(), 0);
    let _ = send_cmd_key(&ctx, &mut app, &mut t, Key::Tab, true);
    assert_eq!(app.active_tab_index(), 2);

    // Ctrl+W closes clean untitled tab 2 -> back to 2 tabs
    let _ = send_cmd_key(&ctx, &mut app, &mut t, Key::W, false);
    assert_eq!(app.tab_count(), 2);
    assert_eq!(app.active_tab_index(), 1);
    assert!(app.buffer.dirty);
    assert_eq!(app.buffer.cursor_char, 5);
    assert!(app.buffer.as_string().contains("Edited beta."));

    // Ctrl+W on dirty tab 1 triggers UnsavedChangesModal -> Don't Save discards & leaves tab 0
    let _ = send_cmd_key(&ctx, &mut app, &mut t, Key::W, false);
    assert_eq!(
        app.unsaved_modal.pending,
        Some(PendingDiscardAction::CloseTab(1))
    );
    t += 0.03;
    let out_modal = ui_step(&ctx, &mut app, t, vec![]);
    let dont_save = find_rendered_text(&out_modal, "Don't Save", 0.0, 1280.0, 100.0);
    let _ = click_at(&ctx, &mut app, &mut t, dont_save, Modifiers::NONE);
    assert_eq!(app.tab_count(), 1);
    assert_eq!(app.active_tab_index(), 0);
    assert_eq!(app.buffer.file_path.as_deref(), Some(file_a.as_path()));

    // Closing the last clean tab resets to a single blank Untitled.md tab
    let _ = send_cmd_key(&ctx, &mut app, &mut t, Key::W, false);
    assert_eq!(app.tab_count(), 1);
    assert!(app.buffer.file_path.is_none());
    assert!(app.buffer.is_empty());
}

#[test]
fn test_file_explorer_sidebar_search_filter_open_tab_and_export_menu() {
    use markdown_studio::ui::explorer::ExplorerFileFilter;
    use markdown_studio::ui::SidebarTab;

    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let sub = dir.path().join("notes");
    std::fs::create_dir_all(&sub).unwrap();
    let md_alpha = dir.path().join("alpha.md");
    let md_beta = sub.join("beta_plan.md");
    let json_cfg = dir.path().join("data.json");
    std::fs::write(&md_alpha, "# Alpha Heading\n\nAlpha text.\n").unwrap();
    std::fs::write(&md_beta, "# Beta Plan\n\nBeta body.\n").unwrap();
    std::fs::write(&json_cfg, "{\"ok\": true}\n").unwrap();

    // Open alpha.md first -> explorer root auto-initializes to parent dir
    app.open_specific_file(&md_alpha);
    assert_eq!(app.explorer_state.root_dir.as_deref(), Some(dir.path()));

    // Toggle Explorer sidebar on
    app.dispatch_action(&ctx, ToolbarAction::ToggleExplorer);
    assert!(app.outline_open);
    assert_eq!(app.sidebar_tab, SidebarTab::Explorer);

    let mut t = 1.0_f64;
    let out_exp = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!find_rendered_text_shapes(&out_exp, "alpha.md").is_empty());
    assert!(!find_rendered_text_shapes(&out_exp, "data.json").is_empty());

    // Filter by Markdown + search "beta" -> finds nested notes/beta_plan.md, hides data.json
    app.explorer_state.file_filter = ExplorerFileFilter::Markdown;
    app.explorer_state.search_query = "beta".to_string();
    t += 0.03;
    let out_filt = ui_step(&ctx, &mut app, t, vec![]);
    assert!(find_rendered_text_shapes(&out_filt, "data.json").is_empty());
    let beta_pos = find_rendered_text(&out_filt, "beta_plan.md", 0.0, 260.0, 100.0);

    // Clicking beta_plan.md in Explorer opens it in a 2nd tab
    let _ = click_at(&ctx, &mut app, &mut t, beta_pos, Modifiers::NONE);
    assert_eq!(app.tab_count(), 2);
    assert_eq!(app.buffer.file_path.as_deref(), Some(md_beta.as_path()));

    // Switch sidebar to Outline tab via ToggleOutline, verify heading appears
    app.dispatch_action(&ctx, ToolbarAction::ToggleOutline);
    assert!(app.outline_open);
    assert_eq!(app.sidebar_tab, markdown_studio::ui::SidebarTab::Outline);
    t += 0.03;
    let out_outline = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!find_rendered_text_shapes(&out_outline, "Beta Plan").is_empty());

    // Clicking Outline again closes sidebar
    app.dispatch_action(&ctx, ToolbarAction::ToggleOutline);
    assert!(!app.outline_open);

    // Open Export ▾ popup and verify HTML & JSON export options render
    let out_closed = ui_step(&ctx, &mut app, t + 0.03, vec![]);
    let export_pos = find_rendered_text(&out_closed, "Export ▾", 600.0, 1280.0, 0.0);
    let _ = click_at(&ctx, &mut app, &mut t, export_pos, Modifiers::NONE);
    t += 0.03;
    let out_popup = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!find_rendered_text_shapes(&out_popup, "Export HTML (.html)").is_empty());
    assert!(!find_rendered_text_shapes(&out_popup, "Export JSON (.json)").is_empty());
}

#[test]
fn test_tab_bar_close_and_new_hover_keeps_symbols_visible() {
    use markdown_studio::ui::theme::ThemeMode;

    fn flatten_shapes_in_order(shape: &egui::Shape, out: &mut Vec<(bool, Rect, String)>) {
        match shape {
            egui::Shape::Vec(v) => {
                for s in v {
                    flatten_shapes_in_order(s, out);
                }
            }
            egui::Shape::Rect(r) if r.fill != egui::Color32::TRANSPARENT => {
                out.push((false, r.rect, String::new()));
            }
            egui::Shape::Text(t) => {
                let rect = Rect::from_min_size(t.pos, t.galley.rect.size());
                out.push((true, rect, t.galley.text().to_string()));
            }
            _ => {}
        }
    }

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let mut t = 1.0_f64;

    for mode in [ThemeMode::Dark, ThemeMode::Sepia, ThemeMode::Light] {
        app.dispatch_action(&ctx, ToolbarAction::SetThemeMode(mode));
        t += 0.03;
        let out_idle = ui_step(&ctx, &mut app, t, vec![]);

        let plus_pos = find_rendered_text(&out_idle, "+", 0.0, 1280.0, 50.0);
        let close_pos = find_rendered_text(&out_idle, "×", 0.0, 1280.0, 50.0);

        for (symbol, pos) in [("+", plus_pos), ("×", close_pos)] {
            t += 0.03;
            let out_hov = ui_step(
                &ctx,
                &mut app,
                t,
                vec![Event::PointerMoved(pos)],
            );
            let mut flat = Vec::new();
            for s in &out_hov.shapes {
                flatten_shapes_in_order(&s.shape, &mut flat);
            }
            let sym_idx = flat
                .iter()
                .position(|(is_text, rect, txt)| *is_text && txt == symbol && rect.expand(6.0).contains(pos))
                .unwrap_or_else(|| panic!("expected hovered symbol {symbol:?} in mode {mode:?}"));
            let sym_rect = flat[sym_idx].1;
            let occluding_rect_after = flat[sym_idx + 1..].iter().any(|(is_text, rect, _)| {
                !*is_text && rect.intersects(sym_rect) && rect.width() >= 10.0 && rect.height() >= 10.0
            });
            assert!(
                !occluding_rect_after,
                "filled rect painted after {symbol:?} in mode {mode:?} occludes the symbol"
            );
        }
    }
}

#[test]
fn test_single_instance_ipc_opens_incoming_file_as_new_tab_and_dedups() {
    use markdown_studio::ui::instance::{
        try_forward_files_to_running_instance, SingleInstanceServer,
    };

    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);
    let lock_path = dir.path().join("instance.lock");

    let (server, rx) = SingleInstanceServer::start(lock_path.clone(), Some(ctx.clone()))
        .expect("start IPC server");
    app.attach_instance_receiver(rx);

    let file1 = dir.path().join("first_doc.md");
    let file2 = dir.path().join("second_doc.md");
    std::fs::write(&file1, "# First\n\nBody 1.\n").unwrap();
    std::fs::write(&file2, "# Second\n\nBody 2.\n").unwrap();

    // Opening external .md files via secondary launch forwards them as tabs into the active window
    assert!(
        try_forward_files_to_running_instance(&lock_path, &[file1.clone(), file2.clone()])
            .expect("forward two files")
    );
    let mut t = 1.0_f64;
    let _ = ui_step(&ctx, &mut app, t, vec![]);

    assert_eq!(app.tab_count(), 2);
    assert_eq!(app.active_tab_index(), 1);
    assert_eq!(app.buffer.file_path.as_deref(), Some(file2.as_path()));
    assert!(app.buffer.as_string().contains("# Second"));

    // Forwarding an already-open file activates its existing tab instead of duplicating
    assert!(
        try_forward_files_to_running_instance(&lock_path, std::slice::from_ref(&file1))
            .expect("forward existing file")
    );
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);

    assert_eq!(app.tab_count(), 2);
    assert_eq!(app.active_tab_index(), 0);
    assert_eq!(app.buffer.file_path.as_deref(), Some(file1.as_path()));

    drop(server);
}

#[test]
fn test_drag_tab_out_of_window_detaches_clean_and_dirty_tabs_to_new_instance() {
    use markdown_studio::ui::instance::read_and_remove_handoff_file;

    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let clean1 = dir.path().join("clean_one.md");
    let clean2 = dir.path().join("clean_two.md");
    std::fs::write(&clean1, "# Clean One\n").unwrap();
    std::fs::write(&clean2, "# Clean Two\n").unwrap();

    app.open_specific_file(&clean1);
    app.open_specific_file(&clean2);
    assert_eq!(app.tab_count(), 2);

    let mut t = 1.0_f64;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let out = ui_step(&ctx, &mut app, t, vec![]);

    let tab2_pos = find_rendered_text_shapes(&out, "clean_two.md")
        .into_iter()
        .find_map(|(_, shape)| {
            let text_rect = Rect::from_min_size(shape.pos, shape.galley.rect.size());
            (shape.galley.text() == "clean_two.md"
                && text_rect.center().y > 50.0
                && text_rect.center().y < 140.0)
                .then_some(text_rect.center())
        })
        .expect("expected clean_two.md tab in tab bar");

    // Drag clean_two.md down into tear-off zone and release outside the window
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(tab2_pos),
            Event::PointerButton {
                pos: tab2_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::PointerMoved(Pos2::new(
            tab2_pos.x + 6.0,
            tab2_pos.y + 30.0,
        ))],
    );
    t += 0.03;
    let tear_pos = Pos2::new(tab2_pos.x + 18.0, tab2_pos.y + 180.0);
    let out_ghost = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::PointerMoved(tear_pos)],
    );
    assert!(
        !find_rendered_text_shapes(&out_ghost, "Release to open in new window").is_empty(),
        "expected Chrome-style detach ghost pill while dragging tab outside bar"
    );

    t += 0.03;
    let outside_pos = Pos2::new(1340.0, 260.0);
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(outside_pos),
            Event::PointerButton {
                pos: outside_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );

    assert_eq!(app.tab_count(), 1);
    assert_eq!(app.buffer.file_path.as_deref(), Some(clean1.as_path()));
    assert!(app.last_detached_window_args.contains(&"--new-window".to_string()));
    assert!(app
        .last_detached_window_args
        .contains(&clean2.to_string_lossy().to_string()));

    // Now open an untitled dirty tab and drag it out -> must hand off content + dirty state
    app.new_tab(&ctx);
    let dirty_text = "Unsaved detached draft\n\n# Live Note";
    app.buffer.insert(0, dirty_text);
    assert!(app.buffer.dirty);
    assert_eq!(app.tab_count(), 2);

    t += 0.03;
    let out2 = ui_step(&ctx, &mut app, t, vec![]);
    let untitled_pos = find_rendered_text_shapes(&out2, "Untitled-")
        .into_iter()
        .find_map(|(_, shape)| {
            let text_rect = Rect::from_min_size(shape.pos, shape.galley.rect.size());
            (shape.galley.text().starts_with("Untitled-")
                && text_rect.center().y > 50.0
                && text_rect.center().y < 140.0)
                .then_some(text_rect.center())
        })
        .expect("expected untitled tab in tab bar");

    app.last_detached_window_args.clear();
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(untitled_pos),
            Event::PointerButton {
                pos: untitled_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::PointerMoved(Pos2::new(
            untitled_pos.x + 8.0,
            untitled_pos.y + 32.0,
        ))],
    );
    t += 0.03;
    let drop_pos = Pos2::new(untitled_pos.x + 24.0, untitled_pos.y + 190.0);
    let _ = ui_step(&ctx, &mut app, t, vec![Event::PointerMoved(drop_pos)]);
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(drop_pos),
            Event::PointerButton {
                pos: drop_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );

    assert_eq!(app.tab_count(), 1);
    assert!(app.last_detached_window_args.contains(&"--new-window".to_string()));
    let handoff_flag_idx = app
        .last_detached_window_args
        .iter()
        .position(|a| a == "--handoff")
        .expect("dirty untitled tab must use --handoff");
    let handoff_path = std::path::PathBuf::from(&app.last_detached_window_args[handoff_flag_idx + 1]);
    let handoff = read_and_remove_handoff_file(&handoff_path).expect("read detached handoff");
    assert!(handoff.dirty);
    assert_eq!(handoff.file_path, None);
    assert_eq!(handoff.content, dirty_text);
    let restored_buf = DocumentBuffer::from_handoff(&handoff.content, handoff.file_path, handoff.dirty);
    assert!(restored_buf.dirty);
    assert_eq!(restored_buf.as_string(), dirty_text);

    // Dragging the sole remaining tab must NOT detach it
    app.last_detached_window_args.clear();
    t += 0.03;
    let out3 = ui_step(&ctx, &mut app, t, vec![]);
    let sole_pos = find_rendered_text_shapes(&out3, "clean_one.md")
        .into_iter()
        .find_map(|(_, shape)| {
            let text_rect = Rect::from_min_size(shape.pos, shape.galley.rect.size());
            (shape.galley.text() == "clean_one.md"
                && text_rect.center().y > 50.0
                && text_rect.center().y < 140.0)
                .then_some(text_rect.center())
        })
        .expect("expected clean_one.md tab");
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(sole_pos),
            Event::PointerButton {
                pos: sole_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::PointerMoved(Pos2::new(sole_pos.x, sole_pos.y + 180.0))],
    );
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::PointerButton {
            pos: Pos2::new(sole_pos.x, sole_pos.y + 180.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert_eq!(app.tab_count(), 1);
    assert!(app.last_detached_window_args.is_empty());
}

#[test]
fn test_drag_tab_reorders_tabs_preserving_active_and_dirty_state() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let file_a = dir.path().join("tab_a.md");
    let file_b = dir.path().join("tab_b.md");
    let file_c = dir.path().join("tab_c.md");
    std::fs::write(&file_a, "# Tab A\n").unwrap();
    std::fs::write(&file_b, "# Tab B\n").unwrap();
    std::fs::write(&file_c, "# Tab C\n").unwrap();

    app.open_specific_file(&file_a);
    app.open_specific_file(&file_b);
    let dirty_edit = "\nDirty B edit.";
    app.buffer.insert(app.buffer.len_chars(), dirty_edit);
    assert!(app.buffer.dirty);
    app.open_specific_file(&file_c);
    assert_eq!(app.tab_count(), 3);
    assert_eq!(app.active_tab_index(), 2);

    let tab_rect = |out: &egui::FullOutput, name: &str| -> Rect {
        find_rendered_text_shapes(out, name)
            .into_iter()
            .find_map(|(_, shape)| {
                let r = Rect::from_min_size(shape.pos, shape.galley.rect.size());
                (shape.galley.text() == name && r.center().y > 50.0 && r.center().y < 140.0)
                    .then_some(r)
            })
            .unwrap_or_else(|| panic!("expected tab {name:?} in tab bar"))
    };

    let mut t = 1.0_f64;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let out0 = ui_step(&ctx, &mut app, t, vec![]);
    let a_rect0 = tab_rect(&out0, "tab_a.md");
    let b_rect0 = tab_rect(&out0, "tab_b.md");
    let c_rect0 = tab_rect(&out0, "tab_c.md");
    assert!(a_rect0.center().x < b_rect0.center().x && b_rect0.center().x < c_rect0.center().x);

    // 1. Drag active tab_c.md (idx 2) left past tab_a.md center -> order becomes [tab_c, tab_a, tab_b]
    let c_pos = c_rect0.center();
    let before_a = Pos2::new((a_rect0.left() - 2.0).max(4.0), a_rect0.center().y);
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(c_pos),
            Event::PointerButton {
                pos: c_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::PointerMoved(Pos2::new(c_pos.x - 20.0, c_pos.y))],
    );
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![Event::PointerMoved(before_a)]);
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(before_a),
            Event::PointerButton {
                pos: before_a,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let out1 = ui_step(&ctx, &mut app, t, vec![]);

    assert_eq!(app.tab_count(), 3);
    assert_eq!(app.active_tab_index(), 0);
    assert_eq!(app.buffer.file_path.as_deref(), Some(file_c.as_path()));
    let c_rect1 = tab_rect(&out1, "tab_c.md");
    let a_rect1 = tab_rect(&out1, "tab_a.md");
    let b_rect1 = tab_rect(&out1, "tab_b.md");
    assert!(c_rect1.center().x < a_rect1.center().x && a_rect1.center().x < b_rect1.center().x);

    // 2. Drag inactive dirty tab_b.md (idx 2) left between tab_c and tab_a -> order [tab_c, tab_b, tab_a]
    let b_pos = b_rect1.center();
    let between_c_a = Pos2::new(
        (c_rect1.center().x + a_rect1.center().x) * 0.5,
        b_pos.y,
    );
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(b_pos),
            Event::PointerButton {
                pos: b_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::PointerMoved(Pos2::new(b_pos.x - 20.0, b_pos.y))],
    );
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![Event::PointerMoved(between_c_a)]);
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(between_c_a),
            Event::PointerButton {
                pos: between_c_a,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let out2 = ui_step(&ctx, &mut app, t, vec![]);

    assert_eq!(app.active_tab_index(), 0);
    assert_eq!(app.buffer.file_path.as_deref(), Some(file_c.as_path()));
    let c_rect2 = tab_rect(&out2, "tab_c.md");
    let b_rect2 = tab_rect(&out2, "tab_b.md");
    let a_rect2 = tab_rect(&out2, "tab_a.md");
    assert!(c_rect2.center().x < b_rect2.center().x && b_rect2.center().x < a_rect2.center().x);

    // Switching to moved tab 1 preserves dirty buffer & edit
    app.switch_to_tab(1);
    assert_eq!(app.buffer.file_path.as_deref(), Some(file_b.as_path()));
    assert!(app.buffer.dirty);
    assert!(app.buffer.as_string().contains("Dirty B edit."));

    // 3. Pressing Escape during drag cancels reordering
    let a_pos = a_rect2.center();
    let esc_target = Pos2::new((c_rect2.left() - 2.0).max(4.0), a_pos.y);
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(a_pos),
            Event::PointerButton {
                pos: a_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::PointerMoved(Pos2::new(a_pos.x - 20.0, a_pos.y))],
    );
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::PointerMoved(esc_target),
            Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::PointerButton {
            pos: esc_target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
    );
    t += 0.03;
    let out3 = ui_step(&ctx, &mut app, t, vec![]);
    let c_rect3 = tab_rect(&out3, "tab_c.md");
    let b_rect3 = tab_rect(&out3, "tab_b.md");
    let a_rect3 = tab_rect(&out3, "tab_a.md");
    assert!(c_rect3.center().x < b_rect3.center().x && b_rect3.center().x < a_rect3.center().x);
}

#[test]
fn test_click_toolbar_filename_to_rename_file_and_cancel_or_guard() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let orig = dir.path().join("original.md");
    let taken = dir.path().join("existing.md");
    std::fs::write(&orig, "# Hello\n").unwrap();
    std::fs::write(&taken, "# Taken\n").unwrap();

    app.open_specific_file(&orig);
    app.settings.record_recent_file(&orig);
    let mut t = 1.0_f64;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let out0 = ui_step(&ctx, &mut app, t, vec![]);

    let click_toolbar_name = |ctx: &egui::Context,
                              app: &mut MarkdownApp,
                              time: &mut f64,
                              out: &egui::FullOutput,
                              name: &str| {
        let name_pos = find_rendered_text_shapes(out, name)
            .into_iter()
            .find_map(|(_, shape)| {
                let r = Rect::from_min_size(shape.pos, shape.galley.rect.size());
                (shape.galley.text() == name && r.center().y < 50.0).then_some(r.center())
            })
            .unwrap_or_else(|| panic!("expected filename {name:?} in Tier 1 toolbar"));
        *time += 0.03;
        let _ = ui_step(
            ctx,
            app,
            *time,
            vec![
                Event::PointerMoved(name_pos),
                Event::PointerButton {
                    pos: name_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        *time += 0.03;
        let _ = ui_step(
            ctx,
            app,
            *time,
            vec![Event::PointerButton {
                pos: name_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        // Let TextEdit mount & take focus with stem selected
        *time += 0.03;
        let _ = ui_step(ctx, app, *time, vec![]);
    };

    // 1. Click filename in toolbar, type "renamed_doc" (replacing stem selection) + Enter
    click_toolbar_name(&ctx, &mut app, &mut t, &out0, "original.md");
    assert!(app.toolbar_state.renaming_file);

    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::Text("renamed_doc".to_string()),
            Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let out1 = ui_step(&ctx, &mut app, t, vec![]);

    let renamed = dir.path().join("renamed_doc.md");
    assert!(!orig.exists(), "original file should be renamed on disk");
    assert!(renamed.exists(), "renamed_doc.md should exist on disk");
    assert_eq!(std::fs::read_to_string(&renamed).unwrap(), "# Hello\n");
    assert_eq!(app.buffer.file_path.as_deref(), Some(renamed.as_path()));
    assert!(!app.toolbar_state.renaming_file);
    assert!(
        app.settings.recent_files.iter().any(|p| p == &renamed)
            && !app.settings.recent_files.iter().any(|p| p == &orig),
        "recent files should replace old path with renamed path"
    );
    assert!(
        !find_rendered_text_shapes(&out1, "renamed_doc.md").is_empty(),
        "tab bar and toolbar should reflect renamed filename"
    );

    // 2. Collision check: attempting to rename to existing.md must be refused safely
    click_toolbar_name(&ctx, &mut app, &mut t, &out1, "renamed_doc.md");
    assert!(app.toolbar_state.renaming_file);
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::Text("existing".to_string()),
            Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    t += 0.03;
    let out2 = ui_step(&ctx, &mut app, t, vec![]);
    assert!(renamed.exists() && taken.exists());
    assert_eq!(std::fs::read_to_string(&taken).unwrap(), "# Taken\n");
    assert_eq!(app.buffer.file_path.as_deref(), Some(renamed.as_path()));

    // 3. Escape cancels without renaming
    click_toolbar_name(&ctx, &mut app, &mut t, &out2, "renamed_doc.md");
    assert!(app.toolbar_state.renaming_file);
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::Text("canceled_name".to_string()),
            Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    assert!(!app.toolbar_state.renaming_file);
    assert!(renamed.exists());
    assert!(!dir.path().join("canceled_name.md").exists());
    assert_eq!(app.buffer.file_path.as_deref(), Some(renamed.as_path()));

    // 4. Invalid filenames and Reader mode remain strictly guarded
    assert!(!app.rename_active_file("bad/name.md"));
    assert!(!app.rename_active_file("bad:name.md"));
    assert!(!app.rename_active_file("   "));
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    app.dispatch_action(&ctx, ToolbarAction::RenameFile("blocked_in_reader.md".to_string()));
    assert_eq!(app.buffer.file_path.as_deref(), Some(renamed.as_path()));
    assert!(!dir.path().join("blocked_in_reader.md").exists());
}

#[test]
fn test_undo_and_redo_file_renaming_syncs_disk_tab_and_recent_files() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let orig = dir.path().join("draft_one.md");
    let renamed = dir.path().join("draft_two.md");
    std::fs::write(&orig, "# Draft\n\nBody.\n").unwrap();

    app.open_specific_file(&orig);
    app.settings.record_recent_file(&orig);

    assert!(app.rename_active_file("draft_two.md"));
    assert!(!orig.exists() && renamed.exists());
    assert_eq!(app.buffer.file_path.as_deref(), Some(renamed.as_path()));
    assert!(!app.buffer.dirty);

    // Undo via Ctrl+Z (ToolbarAction::Undo) restores draft_one.md on disk & recent files
    app.dispatch_action(&ctx, ToolbarAction::Undo);
    assert!(orig.exists() && !renamed.exists());
    assert_eq!(app.buffer.file_path.as_deref(), Some(orig.as_path()));
    assert!(!app.buffer.dirty);
    assert!(app.settings.recent_files.iter().any(|p| p == &orig));
    assert!(!app.settings.recent_files.iter().any(|p| p == &renamed));

    let mut t = 1.0_f64;
    let out_undo = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!find_rendered_text_shapes(&out_undo, "draft_one.md").is_empty());

    // Redo via ToolbarAction::Redo moves draft_one.md back to draft_two.md on disk & UI
    app.dispatch_action(&ctx, ToolbarAction::Redo);
    assert!(!orig.exists() && renamed.exists());
    assert_eq!(app.buffer.file_path.as_deref(), Some(renamed.as_path()));
    assert!(!app.buffer.dirty);
    assert!(app.settings.recent_files.iter().any(|p| p == &renamed));
    assert!(!app.settings.recent_files.iter().any(|p| p == &orig));

    t += 0.03;
    let out_redo = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!find_rendered_text_shapes(&out_redo, "draft_two.md").is_empty());
}

#[test]
fn test_drag_tab_between_two_open_instances_transfers_dirty_and_last_tab() {
    use markdown_studio::ui::instance::SingleInstanceServer;

    let ctx_a = egui::Context::default();
    let ctx_b = egui::Context::default();
    let dir = tempfile::tempdir().expect("temp dir");
    let lock_path = dir.path().join("instance.lock");

    let file_a = dir.path().join("win_a.md");
    let file_b1 = dir.path().join("win_b1.md");
    let file_b2 = dir.path().join("win_b2.md");
    std::fs::write(&file_a, "# Window A\n").unwrap();
    std::fs::write(&file_b1, "# Window B1\n").unwrap();
    std::fs::write(&file_b2, "# Window B2\n").unwrap();

    let mut app_a =
        MarkdownApp::new_with_settings_path(&ctx_a, None, dir.path().join("settings_a.json"));
    let mut app_b =
        MarkdownApp::new_with_settings_path(&ctx_b, None, dir.path().join("settings_b.json"));
    app_a.test_viewport_origin = Some(Pos2::new(0.0, 0.0));
    app_b.test_viewport_origin = Some(Pos2::new(1350.0, 0.0));

    let (srv_a, rx_a) =
        SingleInstanceServer::start(lock_path.clone(), Some(ctx_a.clone())).expect("srv_a");
    srv_a.update_window_bounds(
        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1280.0, 800.0)),
        false,
    );
    app_a.attach_instance_server(srv_a, rx_a);

    let (srv_b, rx_b) =
        SingleInstanceServer::start(lock_path.clone(), Some(ctx_b.clone())).expect("srv_b");
    srv_b.update_window_bounds(
        Rect::from_min_max(Pos2::new(1350.0, 0.0), Pos2::new(2630.0, 800.0)),
        false,
    );
    app_b.attach_instance_server(srv_b, rx_b);

    app_a.open_specific_file(&file_a);
    app_b.open_specific_file(&file_b1);
    app_b.open_specific_file(&file_b2);
    app_b.buffer.insert(app_b.buffer.len_chars(), "\nDirty peer draft");
    assert!(app_b.buffer.dirty);
    assert_eq!(app_a.tab_count(), 1);
    assert_eq!(app_b.tab_count(), 2);

    let tab_center = |out: &egui::FullOutput, name: &str| -> Pos2 {
        find_rendered_text_shapes(out, name)
            .into_iter()
            .find_map(|(_, shape)| {
                let r = Rect::from_min_size(shape.pos, shape.galley.rect.size());
                (shape.galley.text() == name && r.center().y > 50.0 && r.center().y < 140.0)
                    .then_some(r.center())
            })
            .unwrap_or_else(|| panic!("expected tab {name:?} in tab bar"))
    };

    // 1. Drag dirty win_b2.md from Window B (x=1350) into Window A (0..1280)
    let mut tb = 1.0_f64;
    let _ = ui_step(&ctx_b, &mut app_b, tb, vec![]);
    tb += 0.03;
    let out_b0 = ui_step(&ctx_b, &mut app_b, tb, vec![]);
    let b2_pos = tab_center(&out_b0, "win_b2.md");

    tb += 0.03;
    let _ = ui_step(
        &ctx_b,
        &mut app_b,
        tb,
        vec![
            Event::PointerMoved(b2_pos),
            Event::PointerButton {
                pos: b2_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    tb += 0.03;
    let _ = ui_step(
        &ctx_b,
        &mut app_b,
        tb,
        vec![Event::PointerMoved(Pos2::new(b2_pos.x - 24.0, b2_pos.y + 4.0))],
    );
    // Pointer local x = -400 -> screen x = 1350 - 400 = 950 (inside Window A)
    let over_win_a_local = Pos2::new(-400.0, 220.0);
    tb += 0.03;
    let out_b_hover = ui_step(
        &ctx_b,
        &mut app_b,
        tb,
        vec![Event::PointerMoved(over_win_a_local)],
    );
    assert!(
        !find_rendered_text_shapes(&out_b_hover, "Release to move into other window").is_empty(),
        "expected cross-window drop hint while dragging over peer window"
    );

    tb += 0.03;
    let _ = ui_step(
        &ctx_b,
        &mut app_b,
        tb,
        vec![
            Event::PointerMoved(over_win_a_local),
            Event::PointerButton {
                pos: over_win_a_local,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );

    assert_eq!(app_b.tab_count(), 1);
    assert!(app_b.last_detached_window_args.is_empty());

    let mut ta = 1.0_f64;
    let _ = ui_step(&ctx_a, &mut app_a, ta, vec![]);
    assert_eq!(app_a.tab_count(), 2);
    assert_eq!(app_a.buffer.file_path.as_deref(), Some(file_b2.as_path()));
    assert!(app_a.buffer.dirty);
    assert!(app_a.buffer.as_string().contains("Dirty peer draft"));

    // 2. Drag the sole remaining tab (win_b1.md) from Window B into Window A -> Window B closes itself cleanly
    tb += 0.03;
    let out_b1 = ui_step(&ctx_b, &mut app_b, tb, vec![]);
    let b1_pos = tab_center(&out_b1, "win_b1.md");

    tb += 0.03;
    let _ = ui_step(
        &ctx_b,
        &mut app_b,
        tb,
        vec![
            Event::PointerMoved(b1_pos),
            Event::PointerButton {
                pos: b1_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    tb += 0.03;
    let _ = ui_step(
        &ctx_b,
        &mut app_b,
        tb,
        vec![Event::PointerMoved(Pos2::new(b1_pos.x - 24.0, b1_pos.y + 4.0))],
    );
    tb += 0.03;
    let out_b1_hover = ui_step(
        &ctx_b,
        &mut app_b,
        tb,
        vec![Event::PointerMoved(over_win_a_local)],
    );
    assert!(
        !find_rendered_text_shapes(&out_b1_hover, "Release to move into other window").is_empty(),
        "sole tab must also show move-into-other-window hint when hovering peer"
    );
    tb += 0.03;
    let _ = ui_step(
        &ctx_b,
        &mut app_b,
        tb,
        vec![
            Event::PointerMoved(over_win_a_local),
            Event::PointerButton {
                pos: over_win_a_local,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );

    assert!(app_b.allow_close_now, "empty source window should request close");
    ta += 0.03;
    let _ = ui_step(&ctx_a, &mut app_a, ta, vec![]);
    assert_eq!(app_a.tab_count(), 3);
    assert_eq!(app_a.buffer.file_path.as_deref(), Some(file_b1.as_path()));
    assert!(!app_a.buffer.dirty);
}

#[test]
fn test_recalculate_all_tables_is_single_atomic_undo_step() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let initial = "# Report\n\n| A | B |\n|---|---|\n| 4 | =A1*2 |\n\nMiddle paragraph.\n\n| X | Y |\n|---|---|\n| 5 | =A1+10 |\n";
    app.buffer = DocumentBuffer::from_str(initial);
    // Place cursor on heading outside either table so CalculateTables runs across all tables.
    app.buffer.set_cursor(0, false);

    app.dispatch_action(&ctx, ToolbarAction::CalculateTables);
    let after = app.buffer.as_string();
    assert!(after.contains("8 <!-- =A1*2 -->"), "got:\n{after}");
    assert!(after.contains("15 <!-- =A1+10 -->"), "got:\n{after}");
    assert!(after.contains("Middle paragraph."), "got:\n{after}");
    assert_eq!(app.buffer.undo_depth(), 1, "batch recalculation must create exactly 1 undo step");

    app.dispatch_action(&ctx, ToolbarAction::Undo);
    assert_eq!(app.buffer.as_string(), initial);
    assert!(!app.buffer.dirty);
}

#[test]
fn test_outline_search_filter_and_collapsible_heading_tree() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str(
        "# Chapter One\n\n## Alpha Sub\n\n### Deep Leaf\n\n# Chapter Two\n\nText\n",
    );
    app.dispatch_action(&ctx, ToolbarAction::ToggleOutline);
    assert!(app.outline_open);
    assert_eq!(app.sidebar_tab, markdown_studio::ui::SidebarTab::Outline);

    let mut t = 1.0_f64;
    let out_initial = ui_step(&ctx, &mut app, t, vec![]);
    let _ = find_rendered_text(&out_initial, "Alpha Sub", 0.0, 260.0, 80.0);
    let _ = find_rendered_text(&out_initial, "Deep Leaf", 0.0, 260.0, 80.0);

    // Collapse "Chapter One" (line 0)
    let chevron_pos = find_rendered_text(&out_initial, "▾", 0.0, 260.0, 80.0);
    let _ = click_at(&ctx, &mut app, &mut t, chevron_pos, Modifiers::NONE);
    t += 0.03;
    let out_collapsed = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        !out_collapsed
            .shapes
            .iter()
            .any(|s| find_shape_text_pos_in_band(&s.shape, "Alpha Sub", 0.0, 260.0, 80.0).is_some()),
        "descendant headings in outline must hide when Chapter One is collapsed"
    );
    let _ = find_rendered_text(&out_collapsed, "Chapter Two", 0.0, 260.0, 80.0);

    // Search filter reveals nested headings in outline even when parent is collapsed
    app.outline_filter = "deep".to_string();
    t += 0.03;
    let out_filtered = ui_step(&ctx, &mut app, t, vec![]);
    let _ = find_rendered_text(&out_filtered, "Deep Leaf", 0.0, 260.0, 80.0);
    assert!(
        !out_filtered
            .shapes
            .iter()
            .any(|s| find_shape_text_pos_in_band(&s.shape, "Chapter Two", 0.0, 260.0, 80.0).is_some()),
        "non-matching headings must be filtered out of outline"
    );
}

#[test]
fn test_external_change_auto_reloads_when_clean_and_shows_banner_when_dirty() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);
    let file_path = dir.path().join("watched.md");
    std::fs::write(&file_path, "# Initial Title\n\nOriginal content.\n").unwrap();
    app.open_specific_file(&file_path);
    assert!(!app.buffer.dirty);
    assert!(!app.external_change_detected);

    // 1. External edit on clean buffer -> auto-reloads cleanly while preserving cursor
    app.buffer.set_cursor(5, false);
    std::fs::write(
        &file_path,
        "# Updated External Title\n\nUpdated body from another program with more text.\n",
    )
    .unwrap();
    app.check_external_file_changes();
    assert!(!app.external_change_detected);
    assert!(!app.buffer.dirty);
    assert!(app.buffer.as_string().contains("Updated External Title"));
    assert_eq!(app.buffer.cursor_char, 5);

    // 2. External edit on dirty buffer -> preserves local edits and raises banner
    app.buffer.insert(app.buffer.len_chars(), "\nLocal unsaved note.\n");
    assert!(app.buffer.dirty);
    std::fs::write(
        &file_path,
        "# External Conflict Version\n\nMuch longer third version written on disk outside the editor.\n",
    )
    .unwrap();
    app.check_external_file_changes();
    assert!(app.external_change_detected);
    assert!(app.buffer.as_string().contains("Local unsaved note."));

    let mut t = 1.0_f64;
    let out_banner = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!find_rendered_text_shapes(&out_banner, "modified outside Markdown Studio").is_empty());

    // Clicking "Compare Diff" opens the bounded External Changes Diff modal
    let diff_pos = find_rendered_text(&out_banner, "Compare Diff", 0.0, 1280.0, 40.0);
    let _ = click_at(&ctx, &mut app, &mut t, diff_pos, Modifiers::NONE);
    assert!(app.external_diff_modal.open);
    assert!(app.external_diff_modal.added_count > 0);
    assert!(app.external_diff_modal.removed_count > 0);
    // Two frames for egui::Window auto-sizing before inspecting painted modal contents
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let out_diff = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        !find_rendered_text_shapes(&out_diff, "External Changes Diff").is_empty(),
        "expected diff modal header painted"
    );
    assert!(
        !find_rendered_text_shapes(&out_diff, "External Conflict Version").is_empty(),
        "expected disk line rendered in diff viewer"
    );
    // Close diff modal via Close button
    let close_pos = find_rendered_text(&out_diff, "Close", 0.0, 1280.0, 80.0);
    let _ = click_at(&ctx, &mut app, &mut t, close_pos, Modifiers::NONE);
    assert!(!app.external_diff_modal.open);
    t += 0.03;
    let out_banner_after = ui_step(&ctx, &mut app, t, vec![]);

    // Clicking "Keep Local Changes" dismisses banner without losing unsaved edits
    let keep_pos = find_rendered_text(&out_banner_after, "Keep Local Changes", 0.0, 1280.0, 40.0);
    let _ = click_at(&ctx, &mut app, &mut t, keep_pos, Modifiers::NONE);
    t += 0.03;
    let out_kept = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!app.external_change_detected);
    assert!(app.buffer.as_string().contains("Local unsaved note."));
    assert!(find_rendered_text_shapes(&out_kept, "modified outside Markdown Studio").is_empty());

    // Another external edit raises banner again; clicking "Reload from Disk" loads disk content
    std::fs::write(
        &file_path,
        "# Final Disk Reload\n\nReloaded cleanly from disk.\n",
    )
    .unwrap();
    app.check_external_file_changes();
    assert!(app.external_change_detected);
    t += 0.03;
    let out_banner2 = ui_step(&ctx, &mut app, t, vec![]);
    let reload_pos = find_rendered_text(&out_banner2, "Reload from Disk", 0.0, 1280.0, 40.0);
    let _ = click_at(&ctx, &mut app, &mut t, reload_pos, Modifiers::NONE);
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!app.external_change_detected);
    assert!(!app.buffer.dirty);
    assert!(app.buffer.as_string().contains("Final Disk Reload"));
    assert!(!app.buffer.as_string().contains("Local unsaved note."));
}

#[test]
fn test_session_restore_and_crash_safe_auto_recovery() {
    let ctx = egui::Context::default();
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let saved_file = dir.path().join("saved_doc.md");
    std::fs::write(&saved_file, "# Saved Doc\n\nOriginal file contents.\n").unwrap();

    // Session 1: open a file with unsaved edits, plus an unsaved untitled draft in tab 2
    {
        let mut app1 = MarkdownApp::new_with_settings_path(&ctx, None, settings_path.clone());
        app1.open_specific_file(&saved_file);
        app1.buffer.insert(app1.buffer.len_chars(), "\nUnsaved edit in saved file.\n");
        app1.buffer.set_cursor(12, false);
        assert!(app1.buffer.dirty);

        app1.new_tab(&ctx);
        app1.buffer.insert(0, "# Draft Note\n\nRecovered untitled text.");
        app1.buffer.set_cursor(9, false);
        assert!(app1.buffer.dirty);

        app1.save_session_and_recovery_now();
    }

    // Session 2 (e.g., after restart/crash): restore from disk
    let mut app2 = MarkdownApp::new_with_settings_path(&ctx, None, settings_path);
    assert!(app2.restore_session_from_disk());
    assert_eq!(app2.tab_count(), 2);
    assert_eq!(app2.active_tab_index(), 1);
    assert!(app2.buffer.dirty);
    assert_eq!(app2.buffer.cursor_char, 9);
    assert!(app2.buffer.as_string().contains("Recovered untitled text."));

    app2.switch_to_tab(0);
    assert_eq!(app2.buffer.file_path.as_deref(), Some(saved_file.as_path()));
    assert!(app2.buffer.dirty);
    assert_eq!(app2.buffer.cursor_char, 12);
    assert!(app2.buffer.as_string().contains("Unsaved edit in saved file."));
}

#[test]
fn test_status_bar_reading_time_char_count_and_writing_goals() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("One two three four five six seven eight nine ten.\n");
    app.settings.word_goal = 50;
    app.settings.char_goal = 200;

    let mut t = 1.0_f64;
    let out1 = ui_step(&ctx, &mut app, t, vec![]);
    assert!(!find_rendered_text_shapes(&out1, "10 words · 50 chars (< 1 min read)").is_empty());
    assert!(!find_rendered_text_shapes(&out1, "10/50w (20%) · 50/200c (25%)").is_empty());

    // Open the interactive goal popover and clear goals via Clear button
    let goal_pos = find_rendered_text(&out1, "10/50w (20%)", 0.0, 1280.0, 700.0);
    let _ = click_at(&ctx, &mut app, &mut t, goal_pos, Modifiers::NONE);
    t += 0.03;
    let out_pop = ui_step(&ctx, &mut app, t, vec![]);
    assert!(app.goal_state.open);
    assert!(!find_rendered_text_shapes(&out_pop, "Writing Goals").is_empty());

    let clear_pos = find_rendered_text(&out_pop, "Clear", 0.0, 1280.0, 500.0);
    let _ = click_at(&ctx, &mut app, &mut t, clear_pos, Modifiers::NONE);
    t += 0.03;
    let out_cleared = ui_step(&ctx, &mut app, t, vec![]);
    assert_eq!(app.settings.word_goal, 0);
    assert_eq!(app.settings.char_goal, 0);
    assert!(!find_rendered_text_shapes(&out_cleared, "Goal").is_empty());
}

#[test]
fn test_smart_paste_tsv_and_plain_shift_paste_in_source_and_wysiwyg() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);

    let tsv = "Item\tQty\nApple\t5\n";

    // 1. Source mode: regular Paste converts Excel/Sheets TSV into a Markdown table
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    app.buffer = DocumentBuffer::from_str("");
    app.editor_state.request_focus = true;
    let mut t = 1.0_f64;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Paste(tsv.to_string())],
    );
    let src_after_smart = app.buffer.as_string();
    assert!(
        src_after_smart.contains("| Item | Qty |") && src_after_smart.contains("| Apple | 5 |"),
        "Source smart paste must convert TSV to Markdown table, got:\n{src_after_smart}"
    );

    // 2. Source mode: Shift+Paste (Ctrl+Shift+V plain paste) preserves TSV verbatim
    app.buffer = DocumentBuffer::from_str("");
    app.editor_state.request_focus = true;
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let _ = ui_step_with_mods(
        &ctx,
        &mut app,
        t,
        vec![Event::Paste(tsv.to_string())],
        Modifiers::COMMAND | Modifiers::SHIFT,
    );
    assert_eq!(
        app.buffer.as_string(),
        tsv,
        "Source Shift+Paste must keep plain text verbatim"
    );

    // 3. WYSIWYG mode: regular Paste converts TSV inside active inline paragraph
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    app.buffer = DocumentBuffer::from_str("Hello");
    t += 0.03;
    let out_wys = ui_step(&ctx, &mut app, t, vec![]);
    let hello_pos = find_rendered_text(&out_wys, "Hello", 0.0, 1280.0, 110.0);
    let _ = click_at(&ctx, &mut app, &mut t, hello_pos, Modifiers::NONE);
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(app.reader_state.active_block_lines.is_some());

    app.reader_state.active_block_text.clear();
    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Paste(tsv.to_string())],
    );
    let wys_after = app.buffer.as_string();
    assert!(
        wys_after.contains("| Item | Qty |") && wys_after.contains("| Apple | 5 |"),
        "WYSIWYG smart paste must convert TSV to Markdown table, got:\n{wys_after}"
    );
}

#[test]
fn test_paste_image_png_bytes_inserts_markdown_image_and_respects_reader_guard() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);
    let doc_path = dir.path().join("article.md");
    std::fs::write(&doc_path, "# Article\n\n").unwrap();
    app.open_specific_file(&doc_path);
    app.buffer.set_cursor(app.buffer.len_chars(), false);

    // Build valid 2x2 PNG bytes in memory
    let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([40, 140, 240, 255]));
    let mut png_bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut png_bytes),
        image::ImageFormat::Png,
    )
    .unwrap();

    // Reader mode must reject mutating image paste
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    assert!(!app.paste_image_png_bytes(&png_bytes));
    assert!(!app.buffer.as_string().contains("pasted-image"));

    // Source mode inserts ![pasted-image](assets/pasted-image-*.png) and writes the PNG on disk
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));
    assert!(app.paste_image_png_bytes(&png_bytes));
    let text = app.buffer.as_string();
    assert!(
        text.contains("![pasted-image](assets/pasted-image-"),
        "got:\n{text}"
    );
    let assets_dir = dir.path().join("assets");
    let created_files: Vec<_> = std::fs::read_dir(&assets_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(created_files.len(), 1);
}

#[test]
fn test_open_repo_readme_async_images_and_table_gallery_never_freeze() {
    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    let readme_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    assert!(readme_path.is_file(), "README.md must exist at repo root");

    app.open_specific_file(&readme_path);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));

    let mut t = 1.0_f64;
    let mut last_out = ui_step(&ctx, &mut app, t, vec![]);

    // Step across frames briefly while >256 KB local images & optional remote badges decode off-thread
    for _ in 0..8 {
        t += 0.05;
        last_out = ui_step(&ctx, &mut app, t, vec![]);
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    // Remote shields.io badges render either as live SVG textures (online) or compact clickable pills (offline),
    // never as [Image: ...] unavailable-local-file boxes
    assert!(
        find_rendered_text_shapes(&last_out, "[Image: License: AGPL v3]").is_empty(),
        "remote badges must not render missing-local-image fallback box"
    );

    // Switch to Reader mode and scroll down to the Screenshots section & table gallery
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    for _ in 0..8 {
        t += 0.05;
        let _ = ui_step(
            &ctx,
            &mut app,
            t,
            vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -320.0),
                modifiers: Modifiers::NONE,
            }],
        );
    }

    // Step frames until demo.gif and table PNGs finish off-thread decode (without deadlocking)
    let img_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut reader_out = ui_step(&ctx, &mut app, t, vec![]);
    while std::time::Instant::now() < img_deadline
        && !find_rendered_text_shapes(&reader_out, "Loading ").is_empty()
    {
        std::thread::sleep(std::time::Duration::from_millis(25));
        t += 0.05;
        reader_out = ui_step(&ctx, &mut app, t, vec![]);
    }

    assert!(
        find_rendered_text_shapes(&reader_out, "Loading ").is_empty(),
        "async images in README.md should finish loading cleanly without hanging"
    );
    assert!(
        find_rendered_text_shapes(&reader_out, "[Image: WYSIWYG Light]").is_empty(),
        "table gallery local screenshot should render texture, not unavailable placeholder"
    );
    assert!(
        find_rendered_text_shapes(&reader_out, "[Image: Markdown Studio Demo]").is_empty(),
        "hero demo.gif should render texture, not unavailable placeholder"
    );
}

#[test]
fn test_wysiwyg_inline_image_label_editing_and_reader_guard() {
    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let png_path = dir.path().join("sample.png");
    let img = image::RgbaImage::from_pixel(32, 24, image::Rgba([60, 130, 220, 255]));
    img.save(&png_path).unwrap();

    let doc_path = dir.path().join("gallery.md");
    std::fs::write(&doc_path, "# Photos\n\n![Old Label](sample.png)\n").unwrap();
    app.open_specific_file(&doc_path);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));

    let mut t = 1.0_f64;
    let out = ui_step(&ctx, &mut app, t, vec![]);
    let label_pos = find_rendered_text(&out, "Old Label", 0.0, 1280.0, 110.0);

    // Click the WYSIWYG image label button to open inline caption editor
    let _ = click_at(&ctx, &mut app, &mut t, label_pos, Modifiers::NONE);
    t += 0.04;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        app.reader_state.image_label_editor.is_some(),
        "clicking WYSIWYG image label should open inline label editor"
    );

    // Replace draft via Ctrl+A + Backspace + Text + Enter
    let cmd = Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    t += 0.04;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![
            Event::Key {
                key: Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: cmd,
            },
            Event::Key {
                key: Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Text("New Caption".to_string()),
            Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    assert_eq!(
        app.buffer.as_string(),
        "# Photos\n\n![New Caption](sample.png)\n"
    );
    assert!(app.reader_state.image_label_editor.is_none());

    // Ctrl+Z / Undo reverts cleanly
    app.dispatch_action(&ctx, ToolbarAction::Undo);
    assert_eq!(
        app.buffer.as_string(),
        "# Photos\n\n![Old Label](sample.png)\n"
    );

    // Reader mode must hide WYSIWYG image label edit icon
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));
    t += 0.04;
    let reader_out = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        find_rendered_text_shapes(&reader_out, "✎").is_empty(),
        "Reader mode must not show WYSIWYG image label edit icon"
    );
}

#[test]
fn test_reader_mode_image_lightbox_opens_from_inline_and_table_and_closes() {
    fn collect_textured_image_rects(shape: &egui::Shape, out: &mut Vec<Rect>) {
        match shape {
            egui::Shape::Vec(v) => {
                for s in v {
                    collect_textured_image_rects(s, out);
                }
            }
            egui::Shape::Rect(r)
                if r.fill_texture_id() != egui::TextureId::default()
                    && r.rect.min.y > 110.0
                    && r.rect.width() >= 40.0
                    && r.rect.height() >= 30.0 =>
            {
                out.push(r.rect);
            }
            _ => {}
        }
    }

    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let inline_img = image::RgbaImage::from_pixel(64, 48, image::Rgba([30, 120, 220, 255]));
    inline_img.save(dir.path().join("inline.png")).unwrap();
    let table_img = image::RgbaImage::from_pixel(64, 48, image::Rgba([220, 90, 40, 255]));
    table_img.save(dir.path().join("table.png")).unwrap();

    let doc_path = dir.path().join("lightbox.md");
    std::fs::write(
        &doc_path,
        "# Lightbox\n\n![Inline Photo](inline.png)\n\n| Shot |\n| :--- |\n| ![Table Photo](table.png) |\n",
    )
    .unwrap();
    app.open_specific_file(&doc_path);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));

    let mut t = 1.0_f64;
    let out = ui_step(&ctx, &mut app, t, vec![]);
    let mut img_rects = Vec::new();
    for s in &out.shapes {
        collect_textured_image_rects(&s.shape, &mut img_rects);
    }
    img_rects.sort_by(|a, b| a.min.y.total_cmp(&b.min.y));
    assert_eq!(
        img_rects.len(),
        2,
        "expected inline and table images to render in Reader mode, got {img_rects:?}"
    );

    // 1. Click inline image -> opens lightbox
    let _ = click_at(&ctx, &mut app, &mut t, img_rects[0].center(), Modifiers::NONE);
    let lb_open = app
        .reader_state
        .image_lightbox
        .as_ref()
        .expect("clicking inline image in Reader mode must open lightbox");
    assert_eq!(lb_open.url, "inline.png");

    // Step 2 frames so the centered egui::Area finishes auto-sizing before clicking inside it
    t += 0.03;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.03;
    let lb_out = ui_step(&ctx, &mut app, t, vec![]);
    assert!(
        !find_rendered_text_shapes(&lb_out, "Close").is_empty(),
        "lightbox modal header should render Close button"
    );
    assert!(
        !find_rendered_text_shapes(&lb_out, "100%").is_empty(),
        "lightbox modal header should display zoom percentage"
    );

    // 2. Test zoom + button inside lightbox & Esc close
    let card_plus_pos = find_rendered_text_shapes(&lb_out, "+")
        .into_iter()
        .find_map(|(_, ts)| {
            (ts.pos.y > 110.0 && ts.galley.text().trim() == "+")
                .then_some(ts.pos + egui::vec2(3.0, 6.0))
        })
        .expect("lightbox + button must be rendered below toolbar");
    let _ = click_at(&ctx, &mut app, &mut t, card_plus_pos, Modifiers::NONE);
    assert!(
        app.reader_state
            .image_lightbox
            .as_ref()
            .is_some_and(|lb| lb.zoom > 1.0),
        "clicking + in lightbox should increase zoom"
    );

    t += 0.03;
    let _ = ui_step(
        &ctx,
        &mut app,
        t,
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(
        app.reader_state.image_lightbox.is_none(),
        "pressing Esc must close image lightbox"
    );

    // 3. Click table cell image -> opens lightbox for table.png
    let _ = click_at(&ctx, &mut app, &mut t, img_rects[1].center(), Modifiers::NONE);
    assert_eq!(
        app.reader_state
            .image_lightbox
            .as_ref()
            .map(|lb| lb.url.as_str()),
        Some("table.png"),
        "clicking table cell image in Reader mode must open lightbox"
    );

    // 4. Switching out of Reader mode clears lightbox and WYSIWYG image click does not open Reader lightbox
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    t += 0.03;
    let wys_out = ui_step(&ctx, &mut app, t, vec![]);
    assert!(app.reader_state.image_lightbox.is_none());

    let mut wys_rects = Vec::new();
    for s in &wys_out.shapes {
        collect_textured_image_rects(&s.shape, &mut wys_rects);
    }
    if let Some(first_rect) = wys_rects.first() {
        let _ = click_at(&ctx, &mut app, &mut t, first_rect.center(), Modifiers::NONE);
        assert!(
            app.reader_state.image_lightbox.is_none(),
            "clicking image in WYSIWYG mode must not open Reader lightbox"
        );
    }
}

fn right_click_at(
    ctx: &egui::Context,
    app: &mut MarkdownApp,
    t: &mut f64,
    pos: Pos2,
) -> egui::FullOutput {
    *t += 0.02;
    let _ = ui_step_with_mods(
        ctx,
        app,
        *t,
        vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    *t += 0.02;
    let _ = ui_step_with_mods(
        ctx,
        app,
        *t,
        vec![Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    // Run one settle frame so egui::Area completes its first-frame sizing pass and paints menu text.
    *t += 0.02;
    ui_step(ctx, app, *t, vec![])
}

fn drag_select(
    ctx: &egui::Context,
    app: &mut MarkdownApp,
    t: &mut f64,
    from: Pos2,
    to: Pos2,
) -> egui::FullOutput {
    *t += 0.02;
    let _ = ui_step(
        ctx,
        app,
        *t,
        vec![
            Event::PointerMoved(from),
            Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    *t += 0.02;
    let mid = Pos2::new((from.x + to.x) * 0.5, (from.y + to.y) * 0.5);
    let _ = ui_step(ctx, app, *t, vec![Event::PointerMoved(mid)]);
    *t += 0.02;
    let _ = ui_step(ctx, app, *t, vec![Event::PointerMoved(to)]);
    *t += 0.02;
    ui_step(
        ctx,
        app,
        *t,
        vec![Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
    )
}

fn press_key(
    ctx: &egui::Context,
    app: &mut MarkdownApp,
    t: &mut f64,
    key: Key,
    modifiers: Modifiers,
) -> egui::FullOutput {
    *t += 0.02;
    ui_step_with_mods(
        ctx,
        app,
        *t,
        vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }],
        modifiers,
    )
}

fn menu_contains_label(
    items: &[markdown_studio::ui::context_menu::ContextMenuItem<
        markdown_studio::ui::context_menu::ContextMenuAction,
    >],
    target: &str,
) -> bool {
    use markdown_studio::ui::context_menu::ContextMenuItem;
    items.iter().any(|item| match item {
        ContextMenuItem::Action { label, .. } => label == target,
        ContextMenuItem::Submenu { label, items, .. } => {
            label == target || menu_contains_label(items, target)
        }
        ContextMenuItem::Separator => false,
    })
}

fn menu_item_enabled(
    items: &[markdown_studio::ui::context_menu::ContextMenuItem<
        markdown_studio::ui::context_menu::ContextMenuAction,
    >],
    target: &str,
) -> Option<bool> {
    use markdown_studio::ui::context_menu::ContextMenuItem;
    for item in items {
        match item {
            ContextMenuItem::Action { label, enabled, .. } if label == target => {
                return Some(*enabled);
            }
            ContextMenuItem::Submenu { label, enabled, items, .. } => {
                if label == target {
                    return Some(*enabled);
                }
                if let Some(found) = menu_item_enabled(items, target) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn test_editor_context_menu_selection_clipboard_undo_redo_and_modes() {
    use markdown_studio::ui::context_menu::{ContextMenuAction, ContextMenuTarget};

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("Hello **world** today\nSecond line here\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));

    let mut t = 1.0_f64;
    let _out = ui_step(&ctx, &mut app, t, vec![]);

    // Select "**world**" on line 0 (chars 6..15)
    app.buffer.selection_anchor = Some(6);
    app.buffer.cursor_char = 15;
    app.buffer.cache_current_selection();
    t += 0.02;
    let out = ui_step(&ctx, &mut app, t, vec![]);

    let word_pos = find_rendered_text_shapes(&out, "world")
        .into_iter()
        .find_map(|(_, ts)| (ts.pos.y > 110.0).then_some(ts.pos + egui::vec2(72.0, 8.0)))
        .expect("line 0 text must render in Source editor");

    // 1. Right-click inside existing selection -> preserves selection and opens Editor menu
    let menu_out = right_click_at(&ctx, &mut app, &mut t, word_pos);
    assert!(app.context_menu.is_open(), "right-click in Source mode must open context menu");
    assert!(
        matches!(
            app.context_menu.target,
            Some(ContextMenuTarget::Editor {
                from_wysiwyg: false,
                has_selection: true,
                ..
            })
        ),
        "expected Source editor target with selection"
    );
    assert_eq!(app.buffer.selection_range(), Some(6..15));

    let items = app.build_active_context_menu_items();
    assert_eq!(menu_item_enabled(&items, "Undo"), Some(false));
    assert_eq!(menu_item_enabled(&items, "Redo"), Some(false));
    assert_eq!(menu_item_enabled(&items, "Cut"), Some(true));
    assert_eq!(menu_item_enabled(&items, "Copy"), Some(true));
    assert_eq!(menu_item_enabled(&items, "Copy as Plain Text"), Some(true));
    assert_eq!(menu_item_enabled(&items, "Find Selected Text"), Some(true));
    assert!(!find_rendered_text_shapes(&menu_out, "Copy as Plain Text").is_empty());

    // Click "Copy as Plain Text" from rendered menu
    let copy_plain_pos = find_rendered_text_shapes(&menu_out, "Copy as Plain Text")
        .into_iter()
        .find_map(|(_, ts)| (ts.pos.y > 110.0).then_some(ts.pos + egui::vec2(20.0, 6.0)))
        .expect("Copy as Plain Text row must be painted");
    let _ = click_at(&ctx, &mut app, &mut t, copy_plain_pos, Modifiers::NONE);
    assert!(!app.context_menu.is_open(), "executing action must dismiss menu");
    assert_eq!(app.last_copied_text.as_deref(), Some("world"));

    // 2. Right-click outside existing selection -> moves caret and disables selection actions
    app.buffer.selection_anchor = Some(6);
    app.buffer.cursor_char = 15;
    app.buffer.cache_current_selection();
    let out2 = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;
    let line2_pos = find_rendered_text_shapes(&out2, "Second line")
        .into_iter()
        .find_map(|(_, ts)| (ts.pos.y > 110.0).then_some(ts.pos + egui::vec2(18.0, 8.0)))
        .expect("Second line must render");
    let _ = right_click_at(&ctx, &mut app, &mut t, line2_pos);
    assert!(app.context_menu.is_open());
    assert!(
        app.buffer.selection_range().is_none(),
        "right-clicking outside selection must clear selection and position caret"
    );
    assert_eq!(app.buffer.char_to_line(app.buffer.cursor_char), 1);
    let items2 = app.build_active_context_menu_items();
    assert_eq!(menu_item_enabled(&items2, "Cut"), Some(false));
    assert_eq!(menu_item_enabled(&items2, "Copy"), Some(false));
    assert_eq!(menu_item_enabled(&items2, "Find Selected Text"), Some(false));
    app.context_menu.close();

    // 3. Cut, Paste (smart HTML), Paste as Plain Text, Undo, Redo, Select All, Find Selected Text
    let second_start = app.buffer.line_to_char(1);
    app.buffer.selection_anchor = Some(second_start);
    app.buffer.cursor_char = second_start + 6; // "Second"
    app.buffer.cache_current_selection();
    app.execute_context_menu_action(&ctx, ContextMenuAction::Cut);
    assert_eq!(app.last_copied_text.as_deref(), Some("Second"));
    assert!(app.buffer.as_string().contains("\n line here\n"));
    assert!(app.buffer.dirty);
    assert!(app.buffer.undo_depth() >= 1);

    app.test_clipboard_text = Some("Alpha".to_string());
    app.test_clipboard_html = Some("<b>Alpha</b>".to_string());
    app.execute_context_menu_action(&ctx, ContextMenuAction::Paste);
    assert!(app.buffer.as_string().contains("**Alpha** line here"));

    app.test_clipboard_text = Some("Beta".to_string());
    app.test_clipboard_html = Some("<b>Beta</b>".to_string());
    app.execute_context_menu_action(&ctx, ContextMenuAction::PasteAsPlainText);
    assert!(app.buffer.as_string().contains("**Alpha**Beta line here"));

    app.execute_context_menu_action(&ctx, ContextMenuAction::Undo);
    assert!(!app.buffer.as_string().contains("Beta"));
    app.execute_context_menu_action(&ctx, ContextMenuAction::Redo);
    assert!(app.buffer.as_string().contains("Beta"));

    app.execute_context_menu_action(&ctx, ContextMenuAction::SelectAll);
    assert_eq!(app.buffer.selection_range(), Some(0..app.buffer.len_chars()));

    app.execute_context_menu_action(&ctx, ContextMenuAction::FindSelectedText("Alpha".to_string()));
    assert!(app.find_bar.open);
    assert!(!app.find_bar.show_replace);
    assert_eq!(app.find_bar.query, "Alpha");
    assert!(!app.find_bar.matches.is_empty());

    app.execute_context_menu_action(&ctx, ContextMenuAction::FindAndReplace);
    assert!(app.find_bar.open);
    assert!(app.find_bar.show_replace);
    app.find_bar.open = false;

    // 4. WYSIWYG right-click opens Editor context menu and preserves selection
    app.buffer = DocumentBuffer::from_str("Hello world WYSIWYG\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Wysiwyg));
    let w_out = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;
    let w_pos = find_rendered_text_shapes(&w_out, "Hello world WYSIWYG")
        .into_iter()
        .find_map(|(_, ts)| (ts.pos.y > 110.0).then_some(ts.pos + egui::vec2(24.0, 8.0)))
        .expect("WYSIWYG text must render");
    let _ = right_click_at(&ctx, &mut app, &mut t, w_pos);
    assert!(app.context_menu.is_open());
    assert!(matches!(
        app.context_menu.target,
        Some(ContextMenuTarget::Editor {
            from_wysiwyg: true,
            ..
        })
    ));
    app.context_menu.close();

    // 5. Split mode: right-click right preview pane opens Reader { read_only: false }
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Split));
    let s_out = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;
    let split_preview_pos = find_rendered_text_shapes(&s_out, "Hello world WYSIWYG")
        .into_iter()
        .find_map(|(_, ts)| {
            (ts.pos.x > 640.0 && ts.pos.y > 110.0).then_some(ts.pos + egui::vec2(20.0, 8.0))
        })
        .expect("Split preview text must render on right half");
    let _ = right_click_at(&ctx, &mut app, &mut t, split_preview_pos);
    assert!(app.context_menu.is_open());
    assert!(matches!(
        app.context_menu.target,
        Some(ContextMenuTarget::Reader {
            read_only: false,
            ..
        })
    ));
}

#[test]
fn test_editor_context_menu_formatting_submenu_operations() {
    use markdown_studio::document::editing::LineBlockKind;
    use markdown_studio::ui::context_menu::ContextMenuAction;

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("Sample text\n");
    app.buffer.selection_anchor = Some(0);
    app.buffer.cursor_char = 6; // "Sample"
    app.buffer.cache_current_selection();

    for (tb, needle) in [
        (ToolbarAction::ToggleBold, "**Sample**"),
        (ToolbarAction::ToggleItalic, "***Sample***"),
        (ToolbarAction::ToggleUnderline, "<u>"),
        (ToolbarAction::ToggleStrike, "~~"),
        (ToolbarAction::ToggleInlineCode, "`"),
        (ToolbarAction::InsertInlineMath, "$"),
    ] {
        app.execute_context_menu_action(&ctx, ContextMenuAction::Dispatch(tb));
        assert!(
            app.buffer.as_string().contains(needle),
            "expected '{needle}' in '{}'",
            app.buffer.as_string()
        );
    }

    // Headings H1..H6 & Normal Text
    app.buffer = DocumentBuffer::from_str("Heading test\n");
    app.buffer.set_cursor(0, false);
    for level in 1u8..=6 {
        app.execute_context_menu_action(
            &ctx,
            ContextMenuAction::Dispatch(ToolbarAction::SetHeading(Some(level))),
        );
        let expected_prefix = format!("{} ", "#".repeat(level as usize));
        assert!(app.buffer.as_string().starts_with(&expected_prefix));
    }
    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::Dispatch(ToolbarAction::SetHeading(None)),
    );
    assert_eq!(app.buffer.as_string(), "Heading test\n");

    // Lists, Blockquote, Indent, Outdent
    for (kind, prefix) in [
        (LineBlockKind::BulletList, "- "),
        (LineBlockKind::NumberedList, "1. "),
        (LineBlockKind::TaskList, "- [ ] "),
        (LineBlockKind::BlockQuote, "> "),
    ] {
        app.buffer = DocumentBuffer::from_str("Item\n");
        app.buffer.set_cursor(0, false);
        app.execute_context_menu_action(
            &ctx,
            ContextMenuAction::Dispatch(ToolbarAction::ToggleBlock(kind)),
        );
        assert!(
            app.buffer.as_string().starts_with(prefix),
            "expected '{prefix}' for {kind:?}, got '{}'",
            app.buffer.as_string()
        );
    }
    app.execute_context_menu_action(&ctx, ContextMenuAction::Dispatch(ToolbarAction::Indent));
    assert!(app.buffer.as_string().starts_with("<!-- indent:1 -->"));
    app.execute_context_menu_action(&ctx, ContextMenuAction::Dispatch(ToolbarAction::Outdent));
    assert!(!app.buffer.as_string().contains("indent:1"));

    // Insert Code Block, Math, Mermaid, Table, Horizontal Rule, Link dialog
    app.buffer = DocumentBuffer::from_str("Base\n");
    app.buffer.set_cursor(5, false);
    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::Dispatch(ToolbarAction::InsertCodeBlock),
    );
    assert!(app.buffer.as_string().contains("```rust"));

    app.execute_context_menu_action(&ctx, ContextMenuAction::Dispatch(ToolbarAction::InsertMath));
    assert!(app.buffer.as_string().contains("$$"));

    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::Dispatch(ToolbarAction::InsertMermaid),
    );
    assert!(app.buffer.as_string().contains("```mermaid"));

    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::Dispatch(ToolbarAction::InsertTable { rows: 3, cols: 3 }),
    );
    assert!(app.buffer.as_string().contains("| Column 1 |"));

    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::Dispatch(ToolbarAction::InsertHorizontalRule),
    );
    assert!(app.buffer.as_string().contains("---"));

    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::Dispatch(ToolbarAction::OpenLinkDialog),
    );
    assert!(app.insert_link_modal.open);
}

#[test]
fn test_reader_context_menu_read_only_and_element_actions() {
    use markdown_studio::ui::context_menu::{ContextMenuAction, ContextMenuTarget};

    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let img_path = dir.path().join("diagram.png");
    image::RgbaImage::from_pixel(64, 48, image::Rgba([40, 140, 210, 255]))
        .save(&img_path)
        .unwrap();

    let md = concat!(
        "Paragraph alpha beta gamma with [Docs](https://example.com/docs).\n\n",
        "![Diagram](diagram.png)\n\n",
        "```rust\nfn demo() -> i32 { 42 }\n```\n\n",
        "```math\nx^2 + 1\n```\n\n",
        "```mermaid\ngraph TD;\n  A-->B;\n```\n\n",
        "| Item | Price |\n| --- | --- |\n| Pen | 10 |\n| Total | =B1*2 |\n",
    );
    let doc_path = dir.path().join("reader_ctx.md");
    std::fs::write(&doc_path, md).unwrap();
    app.open_specific_file(&doc_path);
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Reader));

    let mut t = 1.0_f64;
    let out = ui_step(&ctx, &mut app, t, vec![]);

    // 1. Drag-select text in Reader mode and right-click inside selection -> preserves selection
    let para_shape = find_rendered_text_shapes(&out, "alpha beta gamma")
        .into_iter()
        .find(|(_, ts)| ts.pos.y > 110.0)
        .expect("paragraph must render in Reader mode")
        .1;
    let drag_from = para_shape.pos + egui::vec2(4.0, 8.0);
    let drag_to = para_shape.pos + egui::vec2(130.0, 8.0);
    let _ = drag_select(&ctx, &mut app, &mut t, drag_from, drag_to);
    let selected_before = app
        .reader_state
        .reader_selected_text
        .clone()
        .expect("dragging in Reader mode must select text");
    assert!(!selected_before.is_empty());

    let _ = right_click_at(&ctx, &mut app, &mut t, para_shape.pos + egui::vec2(55.0, 8.0));
    assert!(app.context_menu.is_open());
    assert_eq!(
        app.reader_state.reader_selected_text.as_deref(),
        Some(selected_before.as_str()),
        "right-clicking inside Reader selection must preserve selection"
    );

    let reader_items = app.build_active_context_menu_items();
    assert!(menu_contains_label(&reader_items, "Copy"));
    assert!(menu_contains_label(&reader_items, "Copy as Plain Text"));
    assert!(menu_contains_label(&reader_items, "Select All"));
    assert!(menu_contains_label(&reader_items, "Find Selected Text"));
    assert!(!menu_contains_label(&reader_items, "Cut"));
    assert!(!menu_contains_label(&reader_items, "Paste"));
    assert!(!menu_contains_label(&reader_items, "Formatting"));
    assert!(!menu_contains_label(&reader_items, "Edit Table..."));
    assert!(!menu_contains_label(&reader_items, "Recalculate Table"));

    app.execute_context_menu_action(&ctx, ContextMenuAction::Copy);
    assert_eq!(
        app.last_copied_text.as_deref(),
        Some(selected_before.as_str())
    );
    app.context_menu.close();

    // 2. Link right-click -> Open Link & Copy Link Address
    let link_pos = find_rendered_text(&out, "Docs", 0.0, 1280.0, 110.0);
    let _ = right_click_at(&ctx, &mut app, &mut t, link_pos);
    let link_items = app.build_active_context_menu_items();
    assert!(menu_contains_label(&link_items, "Open Link"));
    assert!(menu_contains_label(&link_items, "Copy Link Address"));
    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::OpenLink("https://example.com/docs".to_string()),
    );
    assert_eq!(
        app.last_opened_link.as_deref(),
        Some("https://example.com/docs")
    );
    app.context_menu.close();

    // 3. Image right-click -> View Image opens lightbox, Copy Image Address copies URL
    let mut img_rect_opt = None;
    for s in &out.shapes {
        if let egui::Shape::Rect(r) = &s.shape
            && r.fill_texture_id() != egui::TextureId::default()
            && r.rect.min.y > 110.0
            && r.rect.width() >= 40.0
        {
            img_rect_opt = Some(r.rect);
            break;
        }
        if let egui::Shape::Vec(v) = &s.shape {
            for sub in v {
                if let egui::Shape::Rect(r) = sub
                    && r.fill_texture_id() != egui::TextureId::default()
                    && r.rect.min.y > 110.0
                    && r.rect.width() >= 40.0
                {
                    img_rect_opt = Some(r.rect);
                    break;
                }
            }
        }
    }
    let img_rect = img_rect_opt.expect("Reader image must render");
    let _ = right_click_at(&ctx, &mut app, &mut t, img_rect.center());
    let img_items = app.build_active_context_menu_items();
    assert!(menu_contains_label(&img_items, "View Image"));
    assert!(menu_contains_label(&img_items, "Copy Image Address"));
    if let Some(ContextMenuTarget::Reader {
        image: Some(img_ctx),
        ..
    }) = app.context_menu.target.clone()
    {
        app.execute_context_menu_action(&ctx, ContextMenuAction::ViewImage(*img_ctx));
        assert!(app.reader_state.image_lightbox.is_some());
        app.reader_state.image_lightbox = None;
    } else {
        panic!("expected Reader image context");
    }
    app.context_menu.close();

    // 4. Code block right-click -> Copy Code, Copy Code with Markdown Fences, Select Code Block
    let code_pos = find_rendered_text_shapes(&out, "fn demo")
        .into_iter()
        .find_map(|(_, ts)| (ts.pos.y > 110.0).then_some(ts.pos + egui::vec2(20.0, 8.0)))
        .expect("Rust code block must render");
    let _ = right_click_at(&ctx, &mut app, &mut t, code_pos);
    let code_items = app.build_active_context_menu_items();
    assert!(menu_contains_label(&code_items, "Copy Code"));
    assert!(menu_contains_label(&code_items, "Copy Code with Markdown Fences"));
    assert!(menu_contains_label(&code_items, "Select Code Block"));
    if let Some(ContextMenuTarget::Reader {
        code_block: Some(cb),
        ..
    }) = app.context_menu.target.clone()
    {
        app.execute_context_menu_action(&ctx, ContextMenuAction::SelectCodeBlock(cb.source_lines));
        assert_eq!(
            app.reader_state.reader_selected_text.as_deref(),
            Some("fn demo() -> i32 { 42 }")
        );
    } else {
        panic!("expected Reader code block context");
    }
    app.context_menu.close();

    // 5. Math and Mermaid blocks -> Copy Math Source & Copy Mermaid Source
    let math_pos = find_rendered_text_shapes(&out, "MATH")
        .into_iter()
        .find_map(|(_, ts)| (ts.pos.y > 110.0).then_some(ts.pos + egui::vec2(10.0, 32.0)))
        .expect("Math block must render");
    let _ = right_click_at(&ctx, &mut app, &mut t, math_pos);
    let math_items = app.build_active_context_menu_items();
    assert!(menu_contains_label(&math_items, "Copy Math Source"));
    app.context_menu.close();

    let mmd_pos = find_rendered_text_shapes(&out, "MERMAID")
        .into_iter()
        .find_map(|(_, ts)| (ts.pos.y > 110.0).then_some(ts.pos + egui::vec2(10.0, 32.0)))
        .expect("Mermaid block must render");
    let _ = right_click_at(&ctx, &mut app, &mut t, mmd_pos);
    let mmd_items = app.build_active_context_menu_items();
    assert!(menu_contains_label(&mmd_items, "Copy Mermaid Source"));
    app.context_menu.close();

    // 6. Table right-click in Reader (read-only) vs Split (editable + recalculable)
    app.reader_state.scroll_to_y = Some(420.0);
    t += 0.02;
    let _ = ui_step(&ctx, &mut app, t, vec![]);
    t += 0.02;
    let tbl_out_view = ui_step(&ctx, &mut app, t, vec![]);
    let tbl_pos = find_rendered_text_shapes(&tbl_out_view, "Pen")
        .into_iter()
        .find_map(|(_, ts)| {
            (ts.pos.y > 110.0 && ts.pos.y < 740.0).then_some(ts.pos + egui::vec2(8.0, 6.0))
        })
        .expect("Table must render");
    let _ = right_click_at(&ctx, &mut app, &mut t, tbl_pos);
    let tbl_reader_items = app.build_active_context_menu_items();
    assert!(menu_contains_label(&tbl_reader_items, "Copy Table as Markdown"));
    assert!(menu_contains_label(&tbl_reader_items, "Copy Table as TSV"));
    assert!(!menu_contains_label(&tbl_reader_items, "Edit Table..."));
    assert!(!menu_contains_label(&tbl_reader_items, "Recalculate Table"));
    app.context_menu.close();

    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Split));
    app.reader_state.scroll_to_y = Some(420.0);
    let _ = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;
    let split_out = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;
    let split_tbl_pos = find_rendered_text_shapes(&split_out, "Pen")
        .into_iter()
        .find_map(|(_, ts)| {
            (ts.pos.x > 640.0 && ts.pos.y > 110.0 && ts.pos.y < 740.0)
                .then_some(ts.pos + egui::vec2(8.0, 6.0))
        })
        .expect("Table must render in Split preview");
    let _ = right_click_at(&ctx, &mut app, &mut t, split_tbl_pos);
    let tbl_split_items = app.build_active_context_menu_items();
    assert!(menu_contains_label(&tbl_split_items, "Edit Table..."));
    assert!(menu_contains_label(&tbl_split_items, "Recalculate Table"));
    if let Some(ContextMenuTarget::Reader {
        table: Some(tbl_ctx),
        ..
    }) = app.context_menu.target.clone()
    {
        app.execute_context_menu_action(
            &ctx,
            ContextMenuAction::RecalculateTable(tbl_ctx.source_lines.clone()),
        );
        assert!(app.buffer.as_string().contains("20 <!-- =B1*2 -->"));
        app.execute_context_menu_action(&ctx, ContextMenuAction::EditTable(tbl_ctx.source_lines));
        assert!(app.table_modal.open);
    } else {
        panic!("expected Split preview table context");
    }
}

#[test]
fn test_tab_and_file_explorer_context_menus_and_workspace_security() {
    use markdown_studio::ui::context_menu::{ContextMenuAction, ContextMenuTarget};

    let ctx = egui::Context::default();
    let (dir, mut app) = new_isolated_app(&ctx);

    let file_a = dir.path().join("alpha.md");
    let sub_dir = dir.path().join("sub");
    std::fs::create_dir_all(&sub_dir).unwrap();
    let file_b = sub_dir.join("beta.md");
    let file_c = dir.path().join("gamma.md");
    std::fs::write(&file_a, "# Alpha\n").unwrap();
    std::fs::write(&file_b, "# Beta\n").unwrap();
    std::fs::write(&file_c, "# Gamma\n").unwrap();

    // Outside workspace file for boundary security test
    let outside_dir = tempfile::tempdir().unwrap();
    let outside_file = outside_dir.path().join("outside.md");
    std::fs::write(&outside_file, "# Outside\n").unwrap();

    app.open_specific_file(&file_a);
    app.open_specific_file(&file_b);
    app.open_specific_file(&file_c);
    assert_eq!(app.tab_count(), 3);
    app.explorer_state.set_root(dir.path().to_path_buf());
    app.dispatch_action(&ctx, ToolbarAction::ToggleExplorer);

    let mut t = 1.0_f64;
    let out = ui_step(&ctx, &mut app, t, vec![]);

    // 1. Right-click Tab 0 ("alpha.md") in tab strip (50 < y < 140)
    let tab0_pos = find_rendered_text_shapes(&out, "alpha.md")
        .into_iter()
        .find_map(|(_, ts)| {
            (ts.pos.y > 50.0 && ts.pos.y < 140.0).then_some(ts.pos + egui::vec2(14.0, 6.0))
        })
        .expect("alpha.md tab must render in tab bar");
    let _ = right_click_at(&ctx, &mut app, &mut t, tab0_pos);
    assert!(app.context_menu.is_open());
    assert!(matches!(
        app.context_menu.target,
        Some(ContextMenuTarget::Tab { tab_idx: 0, .. })
    ));
    let tab_items = app.build_active_context_menu_items();
    for required in [
        "Close Tab",
        "Close Other Tabs",
        "Close Tabs to the Right",
        "Save",
        "Copy File Path",
        "Reveal File in Explorer",
    ] {
        assert!(menu_contains_label(&tab_items, required), "missing {required}");
    }

    app.execute_context_menu_action(&ctx, ContextMenuAction::CopyFilePath(file_a.clone()));
    assert_eq!(
        app.last_copied_text.as_deref(),
        Some(file_a.to_string_lossy().as_ref())
    );
    app.execute_context_menu_action(&ctx, ContextMenuAction::RevealInFileManager(file_a.clone()));
    assert_eq!(app.last_revealed_path.as_deref(), Some(file_a.as_path()));
    app.context_menu.close();

    // Close Tabs to the Right of tab 1 -> closes tab 2 cleanly; Close Other Tabs leaves 1 tab
    app.execute_context_menu_action(&ctx, ContextMenuAction::CloseTabsToRight(1));
    assert_eq!(app.tab_count(), 2);
    app.execute_context_menu_action(&ctx, ContextMenuAction::CloseOtherTabs(0));
    assert_eq!(app.tab_count(), 1);

    // 2. Right-click File Explorer item ("alpha.md") in left sidebar (y > 140)
    let out_exp = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;
    let exp_alpha_pos = find_rendered_text_shapes(&out_exp, "alpha.md")
        .into_iter()
        .find_map(|(_, ts)| {
            (ts.galley.text() == "alpha.md"
                && ts.pos.y > 160.0
                && ts.pos.y < 700.0
                && ts.pos.x < 260.0)
                .then_some(ts.pos + egui::vec2(12.0, 6.0))
        })
        .expect("alpha.md row must render in Explorer");
    let _ = right_click_at(&ctx, &mut app, &mut t, exp_alpha_pos);
    assert!(app.context_menu.is_open());
    assert!(matches!(
        app.context_menu.target,
        Some(ContextMenuTarget::Explorer { is_dir: false, .. })
    ));
    let exp_items = app.build_active_context_menu_items();
    for required in [
        "Open File",
        "Open in New Tab",
        "Copy File Path",
        "Copy Relative Path",
        "Reveal in OS File Manager",
        "Rename...",
        "Delete...",
        "Refresh Folder",
    ] {
        assert!(menu_contains_label(&exp_items, required), "missing {required}");
    }
    app.context_menu.close();

    // Open in New Tab + Copy Relative Path
    app.execute_context_menu_action(&ctx, ContextMenuAction::ExplorerOpenInNewTab(file_b.clone()));
    assert_eq!(app.tab_count(), 2);
    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::ExplorerCopyRelativePath(file_b.clone()),
    );
    assert_eq!(app.last_copied_text.as_deref(), Some("sub/beta.md"));

    // Workspace security: reject rename/delete outside root
    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::ExplorerStartRename(outside_file.clone()),
    );
    assert!(!app.explorer_rename_modal.open);
    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::ExplorerRequestDelete(outside_file.clone()),
    );
    assert!(!app.explorer_delete_modal.open);
    assert!(outside_file.exists());

    // Rename beta.md -> delta.md via modal and verify disk + active tab updated
    app.execute_context_menu_action(&ctx, ContextMenuAction::ExplorerStartRename(file_b.clone()));
    assert!(app.explorer_rename_modal.open);
    app.explorer_rename_modal.draft = "delta.md".to_string();
    let _ = press_key(&ctx, &mut app, &mut t, Key::Enter, Modifiers::NONE);
    let rename_out = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;
    let rename_btn = find_rendered_text_shapes(&rename_out, "Rename")
        .into_iter()
        .find_map(|(_, ts)| (ts.pos.y > 200.0).then_some(ts.pos + egui::vec2(12.0, 6.0)))
        .expect("Rename modal button must render");
    let _ = click_at(&ctx, &mut app, &mut t, rename_btn, Modifiers::NONE);
    let renamed_file = sub_dir.join("delta.md");
    assert!(renamed_file.exists(), "renamed file must exist on disk");
    assert!(!file_b.exists(), "old file name must be gone");
    assert_eq!(
        app.buffer.file_path.as_deref(),
        Some(renamed_file.as_path())
    );

    // Delete with confirmation: Cancel keeps file, Delete removes file
    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::ExplorerRequestDelete(renamed_file.clone()),
    );
    assert!(app.explorer_delete_modal.open);
    let _ = press_key(&ctx, &mut app, &mut t, Key::Escape, Modifiers::NONE);
    assert!(!app.explorer_delete_modal.open);
    assert!(renamed_file.exists(), "Cancel/Esc must keep file intact");

    app.execute_context_menu_action(
        &ctx,
        ContextMenuAction::ExplorerRequestDelete(renamed_file.clone()),
    );
    let del_out = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;
    let del_btn = find_rendered_text_shapes(&del_out, "Delete")
        .into_iter()
        .rfind(|(_, ts)| ts.pos.y > 200.0 && ts.galley.text().trim() == "Delete")
        .map(|(_, ts)| ts.pos + egui::vec2(12.0, 6.0))
        .expect("Delete confirmation button must render");
    let _ = click_at(&ctx, &mut app, &mut t, del_btn, Modifiers::NONE);
    assert!(!renamed_file.exists(), "Confirming delete must remove file");
}

#[test]
fn test_context_menu_themes_arabic_rtl_keyboard_nav_dismissal_and_shortcuts() {
    use markdown_studio::ui::theme::ThemeMode;

    let ctx = egui::Context::default();
    let (_dir, mut app) = new_isolated_app(&ctx);
    app.buffer = DocumentBuffer::from_str("مرحبا بالعالم الجميل\nسطر ثاني للمعاينة\n");
    app.dispatch_action(&ctx, ToolbarAction::SetViewMode(ViewMode::Source));

    let mut t = 1.0_f64;
    let _ = ui_step(&ctx, &mut app, t, vec![]);

    // Select Arabic word "بالعالم" (chars 6..13) and right-click near bottom-right edge
    app.buffer.selection_anchor = Some(6);
    app.buffer.cursor_char = 13;
    app.buffer.cache_current_selection();

    for mode in [ThemeMode::Light, ThemeMode::Sepia, ThemeMode::Dark] {
        app.dispatch_action(&ctx, ToolbarAction::SetThemeMode(mode));
        app.context_menu.open_at(
            Pos2::new(1240.0, 760.0),
            markdown_studio::ui::context_menu::ContextMenuTarget::Editor {
                from_wysiwyg: false,
                has_selection: true,
                selected_text: Some(app.buffer.selected_text()),
                link_url: None,
                cursor_line: 0,
            },
            ctx.cumulative_pass_nr(),
        );
        let _ = ui_step(&ctx, &mut app, t + 0.02, vec![]);
        t += 0.02;
        let theme_out = ui_step(&ctx, &mut app, t + 0.02, vec![]);
        t += 0.02;
        assert!(app.context_menu.is_open());
        assert!(!find_rendered_text_shapes(&theme_out, "Undo").is_empty());
        assert!(!find_rendered_text_shapes(&theme_out, "Formatting").is_empty());
        for rect in &app.context_menu.panel_rects {
            assert!(
                rect.right() <= 1280.0 && rect.bottom() <= 800.0 && rect.left() >= 0.0 && rect.top() >= 0.0,
                "menu panel must stay inside viewport bounds for {mode:?}: {rect:?}"
            );
        }
        app.context_menu.close();
    }

    // Arabic Find Selected Text
    app.execute_context_menu_action(
        &ctx,
        markdown_studio::ui::context_menu::ContextMenuAction::FindSelectedText(
            "بالعالم".to_string(),
        ),
    );
    assert!(app.find_bar.open);
    assert_eq!(app.find_bar.query, "بالعالم");
    assert_eq!(app.find_bar.matches.len(), 1);
    app.find_bar.open = false;

    // Keyboard navigation: ArrowUp wraps to "Formatting", ArrowRight opens submenu,
    // ArrowLeft closes submenu, ArrowDown/Enter executes, Escape closes root menu
    app.context_menu.open_at(
        Pos2::new(300.0, 200.0),
        markdown_studio::ui::context_menu::ContextMenuTarget::Editor {
            from_wysiwyg: false,
            has_selection: true,
            selected_text: Some("بالعالم".to_string()),
            link_url: None,
            cursor_line: 0,
        },
        ctx.cumulative_pass_nr(),
    );
    let _ = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;

    // ArrowUp selects last enabled item ("Formatting" submenu)
    let _ = press_key(&ctx, &mut app, &mut t, Key::ArrowUp, Modifiers::NONE);
    assert_eq!(app.context_menu.focus_path.len(), 1);
    // ArrowRight opens "Formatting" submenu and focuses "Bold"
    let _ = press_key(&ctx, &mut app, &mut t, Key::ArrowRight, Modifiers::NONE);
    assert_eq!(app.context_menu.open_submenu_path.len(), 1);
    // ArrowLeft closes "Formatting" submenu
    let _ = press_key(&ctx, &mut app, &mut t, Key::ArrowLeft, Modifiers::NONE);
    assert!(app.context_menu.open_submenu_path.is_empty());
    // Re-open "Formatting" and press Enter on Bold -> wraps Arabic selection in **...**
    let _ = press_key(&ctx, &mut app, &mut t, Key::ArrowRight, Modifiers::NONE);
    let _ = press_key(&ctx, &mut app, &mut t, Key::Enter, Modifiers::NONE);
    assert!(!app.context_menu.is_open(), "Enter must close menu after executing action");
    assert!(
        app.buffer.as_string().contains("**بالعالم**"),
        "expected bold Arabic text via keyboard submenu navigation, got: {}",
        app.buffer.as_string()
    );

    // Escape & outside-click dismissal
    app.context_menu.open_at(
        Pos2::new(300.0, 200.0),
        markdown_studio::ui::context_menu::ContextMenuTarget::Editor {
            from_wysiwyg: false,
            has_selection: false,
            selected_text: None,
            link_url: None,
            cursor_line: 0,
        },
        ctx.cumulative_pass_nr(),
    );
    let _ = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;
    let _ = press_key(&ctx, &mut app, &mut t, Key::Escape, Modifiers::NONE);
    assert!(!app.context_menu.is_open(), "Escape must dismiss context menu");

    app.context_menu.open_at(
        Pos2::new(300.0, 200.0),
        markdown_studio::ui::context_menu::ContextMenuTarget::Editor {
            from_wysiwyg: false,
            has_selection: false,
            selected_text: None,
            link_url: None,
            cursor_line: 0,
        },
        ctx.cumulative_pass_nr(),
    );
    let _ = ui_step(&ctx, &mut app, t + 0.02, vec![]);
    t += 0.02;
    let _ = click_at(&ctx, &mut app, &mut t, Pos2::new(50.0, 750.0), Modifiers::NONE);
    assert!(
        !app.context_menu.is_open(),
        "clicking outside menu panel must dismiss context menu"
    );

    // Existing keyboard shortcuts (Ctrl+Z undo, Ctrl+F find) still work after dismissal
    let _ = press_key(&ctx, &mut app, &mut t, Key::Z, Modifiers::COMMAND);
    assert!(!app.buffer.as_string().contains("**بالعالم**"));
    let _ = press_key(&ctx, &mut app, &mut t, Key::F, Modifiers::COMMAND);
    assert!(app.find_bar.open);
}
