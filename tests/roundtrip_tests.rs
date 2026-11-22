use markdown_studio::document::ast::{parse_to_ir, verify_roundtrip_invariant};
use markdown_studio::document::editing::{
    apply_table_edit, insert_code_block, insert_link, insert_table, query_active_format,
    set_heading, table_at_line, toggle_bold, toggle_inline_code, toggle_italic,
    toggle_line_prefix, toggle_strikethrough, toggle_task_checkbox_at_line, LineBlockKind,
};
use markdown_studio::document::search::{
    find_all_matches, replace_all_matches, replace_single_match_with_opts, SearchOptions,
};
use markdown_studio::document::unicode::{build_visual_bidi_line_map, shape_bidi_for_display, TextDirection};
use markdown_studio::document::viewport::ViewportBlockIndex;
use markdown_studio::document::DocumentBuffer;
use markdown_studio::export::ir::{runs_to_plain_text, DocBlock, TableAlign};

const COMPLEX_GFM: &str = r#"# Document Title

## Overview

This paragraph tests **bold**, *italic*, ~~strikethrough~~, `inline code`, and [links](https://example.com).

### Lists & Tasks

- Bullet one
- Bullet two

1. First ordered
2. Second ordered

- [ ] Unchecked item
- [x] Checked item

> A thoughtful blockquote
> spanning two lines.

#### Code Sample

```rust
fn add(a: i32, b: i32) -> i32 {
    a + b
}
```

##### Alignment Table

| Name | Role | Score |
| :--- | :---: | ---: |
| Alice | Engineer | 98 |
| Bob | Designer | 95 |

###### Level 6 Heading

---
"#;

#[test]
fn roundtrip_invariant_complex_gfm_document() {
    let serialized = markdown_studio::document::ast::roundtrip_markdown(COMPLEX_GFM);
    assert!(
        verify_roundtrip_invariant(COMPLEX_GFM),
        "parse(serialize(ast)) == ast failed on complex GFM document.\nSerialized:\n{serialized}"
    );
}

#[test]
fn toolbar_formatting_preserves_roundtrip_and_surrounding_text() {
    let prefix = "# Header Above\n\nUnrelated paragraph above.\n\n";
    let suffix = "\n\nUnrelated paragraph below.\n";
    let initial = format!("{prefix}target word here{suffix}");

    let mut buf = DocumentBuffer::from_str(&initial);

    // Select "target"
    let start = prefix.chars().count();
    let end = start + "target".chars().count();
    buf.selection_anchor = Some(start);
    buf.cursor_char = end;

    toggle_bold(&mut buf);
    assert!(buf.as_string().contains("**target** word here"));
    assert!(buf.as_string().starts_with(prefix));
    assert!(buf.as_string().ends_with(suffix));
    assert!(verify_roundtrip_invariant(&buf.as_string()));

    // Toggle bold back off
    toggle_bold(&mut buf);
    assert!(buf.as_string().contains("target word here"));

    // Italic, strikethrough, inline code
    toggle_italic(&mut buf);
    assert!(buf.as_string().contains("*target* word here"));
    assert!(verify_roundtrip_invariant(&buf.as_string()));
    toggle_italic(&mut buf);

    toggle_strikethrough(&mut buf);
    assert!(buf.as_string().contains("~~target~~ word here"));
    assert!(verify_roundtrip_invariant(&buf.as_string()));
    toggle_strikethrough(&mut buf);

    toggle_inline_code(&mut buf);
    assert!(buf.as_string().contains("`target` word here"));
    assert!(verify_roundtrip_invariant(&buf.as_string()));
    toggle_inline_code(&mut buf);

    // Heading levels H1..H6 and back to Normal
    buf.set_cursor(start, false);
    for lvl in 1..=6u8 {
        set_heading(&mut buf, Some(lvl));
        let line = buf.line_text(buf.char_to_line(buf.cursor_char));
        assert!(line.starts_with(&format!("{} ", "#".repeat(lvl as usize))));
        assert!(verify_roundtrip_invariant(&buf.as_string()));
    }
    set_heading(&mut buf, None);
    assert_eq!(buf.line_text(buf.char_to_line(buf.cursor_char)), "target word here");

    // Bullet, Numbered, Task, Blockquote toggles
    for kind in [
        LineBlockKind::BulletList,
        LineBlockKind::NumberedList,
        LineBlockKind::TaskList,
        LineBlockKind::BlockQuote,
    ] {
        toggle_line_prefix(&mut buf, kind);
        assert!(buf.as_string().starts_with(prefix));
        assert!(buf.as_string().ends_with(suffix));
        assert!(verify_roundtrip_invariant(&buf.as_string()));
        toggle_line_prefix(&mut buf, kind);
        assert_eq!(buf.line_text(buf.char_to_line(buf.cursor_char)), "target word here");
    }

    // Code block insertion + Undo/Redo integrity
    insert_code_block(&mut buf, "rust");
    assert!(buf.as_string().contains("```rust\ntarget word here\n```"));
    assert!(verify_roundtrip_invariant(&buf.as_string()));
    assert!(buf.undo());
    assert_eq!(buf.as_string(), initial);
    assert!(buf.redo());
    assert!(buf.as_string().contains("```rust\ntarget word here\n```"));
}

#[test]
fn table_insert_and_ast_editing_preserves_roundtrip() {
    let header_para = "# Report\n\nIntro paragraph stays untouched.\n\n";
    let footer_para = "\nOutro paragraph stays untouched.\n";
    let initial = format!("{header_para}\n{footer_para}");
    let mut buf = DocumentBuffer::from_str(&initial);

    // Place cursor on the blank line between intro and outro
    let blank_line = 4;
    buf.set_cursor(buf.line_to_char(blank_line), false);
    insert_table(&mut buf, 3, 3);

    let after_insert = buf.as_string();
    assert!(after_insert.contains("Intro paragraph stays untouched."));
    assert!(after_insert.contains("Outro paragraph stays untouched."));
    assert!(verify_roundtrip_invariant(&after_insert));

    let cur_line = buf.char_to_line(buf.cursor_char);
    let mut table = table_at_line(&buf, cur_line).expect("table should be detected at cursor");
    assert_eq!(table.header.len(), 3);
    assert_eq!(table.rows.len(), 2);

    // Edit cells, add row, add column, set alignments
    table.set_cell(None, 0, "Metric".to_string());
    table.set_cell(Some(0), 0, "**Latency**".to_string());
    table.set_cell(Some(0), 1, "12 us".to_string());
    table.add_row(None);
    table.set_cell(Some(2), 0, "Throughput".to_string());
    table.add_column(None);
    table.set_cell(None, 3, "Notes".to_string());
    table.set_alignment(1, TableAlign::Center);
    table.set_alignment(2, TableAlign::Right);
    table.delete_row(1);
    table.delete_column(2);

    apply_table_edit(&mut buf, &table);

    let updated = buf.as_string();
    assert!(updated.contains("Intro paragraph stays untouched."));
    assert!(updated.contains("Outro paragraph stays untouched."));
    assert!(verify_roundtrip_invariant(&updated));

    let ir = parse_to_ir(&updated);
    let found_table = ir
        .iter()
        .find_map(|b| match b {
            DocBlock::Table {
                alignments,
                header,
                rows,
                ..
            } => Some((alignments, header, rows)),
            _ => None,
        })
        .expect("updated IR must contain a Table");

    assert_eq!(found_table.1.len(), 3);
    assert_eq!(runs_to_plain_text(&found_table.1[0]), "Metric");
    assert_eq!(runs_to_plain_text(&found_table.1[2]), "Notes");
    assert_eq!(found_table.0[1], TableAlign::Center);
    assert_eq!(found_table.2.len(), 2);
    assert_eq!(runs_to_plain_text(&found_table.2[0][0]), "Latency");
    assert!(found_table.2[0][0][0].bold);
}

#[test]
fn task_checkbox_toggle_and_viewport_virtualization() {
    let md = "- [ ] First task\n- [x] Second task\n\nParagraph\n";
    let mut buf = DocumentBuffer::from_str(md);
    assert!(toggle_task_checkbox_at_line(&mut buf, 0));
    assert_eq!(buf.line_text(0), "- [x] First task");
    assert!(toggle_task_checkbox_at_line(&mut buf, 1));
    assert_eq!(buf.line_text(1), "- [ ] Second task");
    assert!(verify_roundtrip_invariant(&buf.as_string()));

    // Verify viewport virtualization binary search returns a tight slice
    let mut big_md = String::new();
    for i in 0..2000 {
        big_md.push_str(&format!("## Section {i}\n\nParagraph content for block {i}.\n\n"));
    }
    let blocks = parse_to_ir(&big_md);
    assert_eq!(blocks.len(), 4000);
    let idx = ViewportBlockIndex::from_blocks(blocks, 720.0, 1);
    let mid_y = idx.total_height * 0.5;
    let vis = idx.visible_range(mid_y, 800.0, 100.0);
    assert!(vis.len() < 40, "expected virtualized slice, got {}", vis.len());
    assert!(vis.start > 500 && vis.end < 3500);
}

#[test]
fn footnotes_br_and_marker_normalization_fixes() {
    let md = "Text with note[^ref1] and line1<br>line2 and ``a `b` ** c **`` and \\**lit **.\n\n[^ref1]: Footnote body.\n";
    let blocks = parse_to_ir(md);
    let para_runs = match &blocks[0] {
        DocBlock::Paragraph { runs, .. } => runs,
        other => panic!("expected Paragraph, got {other:?}"),
    };
    assert!(
        para_runs.iter().any(|r| r.is_footnote_ref && r.text == "[^ref1]"),
        "expected discrete footnote ref run"
    );
    assert!(
        para_runs.iter().any(|r| r.text.contains('\n')),
        "expected <br> converted to line break"
    );
    assert!(
        para_runs.iter().any(|r| r.code && r.text.contains("** c **")),
        "expected double-backtick code span preserved verbatim"
    );
    assert!(blocks.iter().any(|b| matches!(
        b,
        DocBlock::FootnoteDefinition { name, .. } if name == "ref1"
    )));
}

#[test]
fn unicode_persian_urdu_and_bidi_bracket_mirroring() {
    let persian = "پژوهش گچ ی";
    let shaped = shape_bidi_for_display(persian, TextDirection::Rtl);
    assert!(
        shaped.chars().any(|c| ('\u{FB50}'..='\u{FDCF}').contains(&c) || ('\u{FBFC}'..='\u{FBFF}').contains(&c)),
        "expected Persian/Urdu letters mapped to Presentation Forms-A: {shaped:?}"
    );
    assert_eq!(shape_bidi_for_display("لا", TextDirection::Rtl), "\u{FEFB}");
    assert_eq!(build_visual_bidi_line_map("لا").visual_text, "\u{FE8E}\u{FEDF}");

    let brackets = shape_bidi_for_display("مرحبا (عالم)", TextDirection::Rtl);
    assert!(
        brackets.contains('(') && brackets.contains(')'),
        "mirrored brackets must remain paired in visual RTL output: {brackets:?}"
    );
}

#[test]
fn document_buffer_atomic_save_vecdeque_eol_and_word_count() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("atomic.md");
    let mut buf = DocumentBuffer::from_str("hello world\r\nsecond line here\r\n");
    assert_eq!(buf.line_ending(), "\r\n");
    assert_eq!(buf.word_count(), 5);

    buf.save_to_file(&target).unwrap();
    let entries: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries, vec!["atomic.md".to_string()]);

    for i in 0..510 {
        buf.insert_at_cursor(if i % 2 == 0 { "a" } else { "b" });
    }
    assert!(buf.undo());
}

#[test]
fn search_regex_captures_char_class_ranges_and_dot_crlf() {
    let mut buf = DocumentBuffer::from_str("k1=10 [foo\\bar] k2=20\r\nend");
    let opts = SearchOptions {
        case_sensitive: true,
        whole_word: false,
        use_regex: true,
        selection_scope: None,
    };

    let dot_res = find_all_matches(&buf, "20.", &opts);
    assert!(dot_res.matches.is_empty(), "`.` must not match `\\r` in CRLF");

    let cls_res = find_all_matches(&buf, r"[\[-\]]+", &opts);
    assert_eq!(cls_res.matches.len(), 3, "escaped char-class range [\\[-\\]] must match [, \\, and ]");

    let kv_res = find_all_matches(&buf, r"(\w+)=(\d+)", &opts);
    assert_eq!(kv_res.matches.len(), 2);
    let rep_one = replace_single_match_with_opts(
        &mut buf,
        r"(\w+)=(\d+)",
        kv_res.matches[0].clone(),
        "$1:$2 ($$)",
        &opts,
    )
    .unwrap();
    assert_eq!(buf.rope().slice(rep_one).to_string(), "k1:10 ($)");

    let (count, _) = replace_all_matches(&mut buf, r"(\w+)=(\d+)", "$2->$1", &opts).unwrap();
    assert_eq!(count, 1);
    assert!(buf.as_string().contains("20->k2"));
}

#[test]
fn editing_link_task_plus_crlf_and_active_format_guards() {
    let mut buf = DocumentBuffer::from_str("+ [ ] Plus task\r\n**bold** item\r\n");
    assert!(toggle_task_checkbox_at_line(&mut buf, 0));
    assert!(buf.line_text(0).starts_with("+ [x] Plus task"));

    buf.selection_anchor = Some(17);
    buf.cursor_char = 25; // "**bold**"
    let fmt = query_active_format(&buf);
    assert!(fmt.bold);
    assert!(!fmt.italic, "**bold** selection must not report italic active");

    buf.selection_anchor = Some(26);
    buf.cursor_char = 30; // "item"
    insert_link(&mut buf, "", "https://rust-lang.org");
    assert!(buf.as_string().contains("[item](https://rust-lang.org)"));
    assert_eq!(buf.line_ending(), "\r\n");
}

#[test]
fn code_block_language_change_preserves_fence_crlf_cursor_and_undo() {
    use markdown_studio::document::editing::{parse_code_fence_line, set_code_block_language};

    assert_eq!(
        parse_code_fence_line("  ```rust  "),
        Some((2, '`', 3, "rust".to_string()))
    );
    assert_eq!(
        parse_code_fence_line("~~~~ python"),
        Some((0, '~', 4, "python".to_string()))
    );
    assert_eq!(parse_code_fence_line("    ```rust"), None);
    assert_eq!(parse_code_fence_line("```bad`lang"), None);
    assert_eq!(parse_code_fence_line("plain paragraph"), None);

    let mut buf = DocumentBuffer::from_str("  ```rust\r\nlet x = 1;\r\n  ```\r\n");
    let offset_of_x = buf.as_string().find("x = 1").unwrap();
    buf.set_cursor(offset_of_x, false);

    assert!(set_code_block_language(&mut buf, 0, "python"));
    assert_eq!(buf.line_text(0), "  ```python");
    assert_eq!(buf.line_text(1), "let x = 1;");
    assert_eq!(buf.line_ending(), "\r\n");
    let new_offset_of_x = buf.as_string().find("x = 1").unwrap();
    assert_eq!(buf.cursor_char, new_offset_of_x);

    assert!(buf.undo());
    assert_eq!(buf.line_text(0), "  ```rust");
    assert!(buf.redo());
    assert_eq!(buf.line_text(0), "  ```python");

    assert!(set_code_block_language(&mut buf, 0, "ts`bad\nline2"));
    assert_eq!(buf.line_text(0), "  ```tsbadline2");
    assert!(set_code_block_language(&mut buf, 0, ""));
    assert_eq!(buf.line_text(0), "  ```");

    let mut tilde_buf = DocumentBuffer::from_str("~~~~\nfn main() {}\n~~~~\n");
    assert!(set_code_block_language(&mut tilde_buf, 0, "zig"));
    assert_eq!(tilde_buf.line_text(0), "~~~~zig");
    assert_eq!(tilde_buf.line_text(1), "fn main() {}");
}

#[test]
fn link_classification_and_markdown_link_extraction_cases() {
    use markdown_studio::ui::reader::{classify_link_url, extract_markdown_link_at_char, LinkTarget};

    assert_eq!(
        classify_link_url("https://example.org/docs"),
        Some(LinkTarget::External("https://example.org/docs".to_string()))
    );
    assert_eq!(
        classify_link_url("http://localhost:8080/health"),
        Some(LinkTarget::External("http://localhost:8080/health".to_string()))
    );
    assert_eq!(
        classify_link_url("www.rust-lang.org/learn"),
        Some(LinkTarget::External("https://www.rust-lang.org/learn".to_string()))
    );
    assert_eq!(
        classify_link_url("mailto:dev@example.com"),
        Some(LinkTarget::External("mailto:dev@example.com".to_string()))
    );
    assert_eq!(
        classify_link_url("#My%20Heading"),
        Some(LinkTarget::Anchor("My Heading".to_string()))
    );
    assert_eq!(
        classify_link_url("./docs/guide.md#sec-two"),
        Some(LinkTarget::LocalFile {
            path: "./docs/guide.md".to_string(),
            fragment: Some("sec-two".to_string()),
        })
    );
    assert_eq!(
        classify_link_url("file:///C:/notes/my%20doc.md#part1"),
        Some(LinkTarget::LocalFile {
            path: "C:/notes/my doc.md".to_string(),
            fragment: Some("part1".to_string()),
        })
    );
    assert_eq!(classify_link_url("javascript:alert(1)"), None);
    assert_eq!(classify_link_url("data:text/html,<script>"), None);
    assert_eq!(classify_link_url("vbscript:msgbox"), None);
    assert_eq!(classify_link_url("   "), None);

    let line = "See [Alpha](https://a.test) and ![Img](pic.png) plus <https://b.test> or www.c.test end.";
    let alpha_col = line.find("Alpha").unwrap() + 2;
    let prose_col = line.find("and").unwrap() + 1;
    let img_col = line.find("Img").unwrap() + 1;
    let auto_col = line.find("b.test").unwrap() + 1;
    let bare_col = line.find("www.c.test").unwrap() + 4;

    assert_eq!(
        extract_markdown_link_at_char(line, alpha_col).as_deref(),
        Some("https://a.test")
    );
    assert_eq!(extract_markdown_link_at_char(line, prose_col), None);
    assert_eq!(extract_markdown_link_at_char(line, img_col), None);
    assert_eq!(
        extract_markdown_link_at_char(line, auto_col).as_deref(),
        Some("https://b.test")
    );
    assert_eq!(
        extract_markdown_link_at_char(line, bare_col).as_deref(),
        Some("www.c.test")
    );
}
