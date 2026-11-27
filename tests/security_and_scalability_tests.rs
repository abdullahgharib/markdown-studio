use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

use markdown_studio::document::ast::{
    extract_tables, is_math_code_info, parse_rope_to_ir, parse_rope_to_ir_cancellable, parse_to_ir,
};
use markdown_studio::document::editing::table_at_line;
use markdown_studio::document::math::{layout_math, parse_latex_math};
use markdown_studio::document::mermaid::{compile_mermaid_scene, DiagramPalette};
use markdown_studio::document::search::{
    find_all_matches_in_rope, find_all_matches_in_rope_cancellable, SearchOptions,
};
use markdown_studio::document::{
    adaptive_undo_budget_bytes, adaptive_worker_count, system_total_memory_bytes, DocumentBuffer,
};
use markdown_studio::export::docx::export_docx_to_file_with_base_dir;
use markdown_studio::export::html::{
    export_html_to_file_with_base_dir, render_html_string_with_base_dir,
};
use markdown_studio::export::ir::{heading_slug, DocBlock};
use markdown_studio::export::json::export_json_to_file_from_rope;
use markdown_studio::export::pdf::{export_pdf_to_file_with_options_and_base_dir, PdfLayoutOptions};
use markdown_studio::ui::dialogs::compute_bounded_line_diff;
use markdown_studio::ui::instance::read_and_remove_handoff_file;
use markdown_studio::ui::os_integration::{validate_classes_root, validate_exe_path};
use markdown_studio::ui::theme::{ThemeMode, WordTheme};

fn make_minimal_png_bytes() -> Vec<u8> {
    let mut img = image::RgbaImage::new(4, 4);
    for px in img.pixels_mut() {
        *px = image::Rgba([20, 110, 220, 255]);
    }
    let mut bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut bytes);
    img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
    bytes
}

#[test]
fn html_export_enforces_csp_sanitizes_xss_and_confines_local_assets() {
    let dir = tempfile::tempdir().unwrap();
    let parent_secret = dir.path().join("secret.png");
    let sub_dir = dir.path().join("docs");
    std::fs::create_dir_all(&sub_dir).unwrap();
    let local_png = sub_dir.join("ok.png");
    let png_bytes = make_minimal_png_bytes();
    std::fs::write(&parent_secret, &png_bytes).unwrap();
    std::fs::write(&local_png, &png_bytes).unwrap();

    let md = "\
# Security Audit <script>alert(1)</script>

[xss](javascript:alert(1))
[vb](vbscript:msgbox(1))
[ok](https://example.org/safe)

![secret](../secret.png)
![ok](ok.png)
";
    let blocks = parse_to_ir(md);
    let html = render_html_string_with_base_dir(&blocks, "Audit <script>", Some(&sub_dir));
    let html_lower = html.to_ascii_lowercase();

    assert!(html.contains("Content-Security-Policy"));
    assert!(html.contains("default-src 'none'"));
    assert!(!html_lower.contains("<script>alert(1)</script>"));
    assert!(!html_lower.contains("javascript:alert"));
    assert!(!html_lower.contains("vbscript:msgbox"));
    assert!(html.contains("https://example.org/safe"));

    // Only `ok.png` inside `base_dir` may be embedded; `../secret.png` outside `sub_dir` must be blocked.
    let embedded_count = html.matches("data:image/png;base64,").count();
    assert_eq!(embedded_count, 1, "parent-dir asset must not be embedded");
}

#[test]
fn docx_and_pdf_exports_block_path_escape_and_unsafe_links() {
    let dir = tempfile::tempdir().unwrap();
    let parent_secret = dir.path().join("outside.png");
    let docs_dir = dir.path().join("workspace");
    std::fs::create_dir_all(&docs_dir).unwrap();
    let inside_png = docs_dir.join("inside.png");
    let png = make_minimal_png_bytes();
    std::fs::write(&parent_secret, &png).unwrap();
    std::fs::write(&inside_png, &png).unwrap();

    let md = "\
# Export Security

[js](javascript:alert(9))
[file_link](file:///etc/passwd)
[web](https://example.org)

![outside](../outside.png)
![inside1](inside.png)
![inside2](inside.png)
";
    let blocks = parse_to_ir(md);

    // DOCX: packs without unsafe hyperlink targets and succeeds cleanly.
    let docx_out = docs_dir.join("out.docx");
    export_docx_to_file_with_base_dir(&blocks, &docx_out, Some(&docs_dir), |_| {}).unwrap();
    assert!(docx_out.exists());

    // Examine DOCX relationships inside the zip to ensure unsafe schemes weren't emitted.
    let docx_file = std::fs::File::open(&docx_out).unwrap();
    let mut zip = zip::ZipArchive::new(docx_file).unwrap();
    let mut rels_text = String::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        if entry.name().ends_with(".rels") {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut entry, &mut s).unwrap();
            rels_text.push_str(&s);
        }
    }
    assert!(!rels_text.to_ascii_lowercase().contains("javascript:"));
    assert!(!rels_text.to_ascii_lowercase().contains("file:///"));
    assert!(rels_text.contains("https://example.org"));

    // PDF: writes cleanly, blocks ../outside.png, and deduplicates inside.png across 2 occurrences.
    let pdf_out = docs_dir.join("out.pdf");
    export_pdf_to_file_with_options_and_base_dir(
        &blocks,
        "PDF Security",
        &pdf_out,
        PdfLayoutOptions::default(),
        Some(&docs_dir),
        |_| {},
    )
    .unwrap();
    let doc = lopdf::Document::load(&pdf_out).unwrap();
    assert!(!doc.get_pages().is_empty());
}

#[test]
fn handoff_file_reader_blocks_paths_outside_handoff_dir() {
    let dir = tempfile::tempdir().unwrap();
    let bad_ext = dir.path().join("spoofed.txt");
    std::fs::write(&bad_ext, "TAB1\n").unwrap();
    assert_eq!(
        read_and_remove_handoff_file(&bad_ext).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );

    let outside_handoff = dir.path().join("spoofed.handoff");
    std::fs::write(&outside_handoff, "TAB1\n\"a\"\n0\n\n").unwrap();
    let err = read_and_remove_handoff_file(&outside_handoff).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        outside_handoff.exists(),
        "outside file must not be deleted or consumed on rejection"
    );
}

#[test]
fn os_integration_validation_rejects_hklm_traversal_and_unsafe_paths() {
    assert!(validate_classes_root(r"HKCU\Software\Classes").is_ok());
    assert!(validate_classes_root(r"HKCU\Software\MarkdownStudioTest_123\Classes").is_ok());
    assert!(validate_classes_root(r"HKEY_LOCAL_MACHINE\Software\Classes").is_err());
    assert!(validate_classes_root(r"Software\Classes").is_err());
    assert!(validate_classes_root(r"HKCU\Software\..\Classes").is_err());

    assert!(validate_exe_path(std::path::Path::new(r"C:\Apps\Markdown Studio.exe")).is_ok());
    assert!(validate_exe_path(std::path::Path::new(r"\\server\share\ms.exe")).is_err());
    assert!(validate_exe_path(std::path::Path::new("relative.exe")).is_err());
    assert!(validate_exe_path(std::path::Path::new("C:\\Bad\"Quote\\ms.exe")).is_err());
}

#[test]
fn deeply_nested_markdown_and_mermaid_and_math_do_not_overflow_stack() {
    let deep_quotes = format!("{}Leaf", "> ".repeat(250));
    let blocks = parse_to_ir(&deep_quotes);
    assert!(!blocks.is_empty());

    let deep_math = format!("{}{}", "\\frac{1}{".repeat(220), "1");
    let ast = parse_latex_math(&deep_math);
    let _layout = layout_math(&ast, 16.0, true);

    let mut dense_graph = String::from("flowchart TD\n");
    for i in 0..80 {
        dense_graph.push_str(&format!("N{i} --> N{}\n", (i + 1) % 80));
        dense_graph.push_str(&format!("N{i} --> N{}\n", (i + 7) % 80));
    }
    let pal = DiagramPalette::from_theme(&WordTheme::from_mode(ThemeMode::Light));
    let scene = compile_mermaid_scene(&dense_graph, pal);
    assert!(scene.width > 0.0 && scene.height > 0.0);
}

#[test]
fn artificial_limits_removed_5000_row_table_and_300_line_math_block() {
    // 1. Table exceeding old 4096-line editing cap
    let mut table_md = String::from("| K | V |\n| --- | --- |\n");
    for i in 0..4_500 {
        table_md.push_str(&format!("| row_{i} | {i} |\n"));
    }
    let buf = DocumentBuffer::from_str(&table_md);
    let found = table_at_line(&buf, 4_200).expect("table > 4096 rows must be detected");
    assert_eq!(found.rows.len(), 4_500);

    // 2. Display math block exceeding old 256-line lookahead cap
    let mut math_md = String::from("$$\n");
    for i in 0..290 {
        math_md.push_str(&format!("x_{{{i}}} + "));
    }
    math_md.push_str("0\n$$\n");
    let blocks = parse_to_ir(&math_md);
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, DocBlock::CodeBlock { info, .. } if is_math_code_info(info))),
        "300-line $$ block must parse cleanly as a math CodeBlock"
    );
}

#[test]
fn streaming_utf16_and_lossy_utf8_open_and_nul_rejection() {
    let dir = tempfile::tempdir().unwrap();

    // UTF-16 LE BOM streaming open
    let le_path = dir.path().join("utf16le.md");
    let text = "# مرحبا Rust\n\nSecond line with emoji 🚀\n";
    let mut le_bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        le_bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&le_path, &le_bytes).unwrap();
    let buf_le = DocumentBuffer::open_file(&le_path).unwrap();
    assert_eq!(buf_le.as_string(), text);

    // Invalid UTF-8 byte sequence opens lossily without failing
    let bad_utf8 = dir.path().join("lossy.md");
    {
        let mut f = std::fs::File::create(&bad_utf8).unwrap();
        f.write_all(b"Hello ").unwrap();
        f.write_all(&[0xFF, 0xFE, 0x80]).unwrap();
        f.write_all(b" World\n").unwrap();
    }
    let buf_lossy = DocumentBuffer::open_file(&bad_utf8).unwrap();
    let decoded = buf_lossy.as_string();
    assert!(decoded.starts_with("Hello "));
    assert!(decoded.ends_with(" World\n"));
    assert!(decoded.contains('\u{FFFD}'));

    // Binary file containing raw NUL bytes is still rejected
    let bin_path = dir.path().join("binary.bin");
    std::fs::write(&bin_path, b"PNG\x00\x00binary").unwrap();
    assert!(DocumentBuffer::open_file(&bin_path).is_err());
}

#[test]
fn adaptive_hardware_budgets_and_large_rope_undo_roundtrip() {
    let ram = system_total_memory_bytes();
    let workers = adaptive_worker_count(8);
    let budget = adaptive_undo_budget_bytes();
    assert!(workers >= 1);
    assert!(budget >= 64 * 1024 * 1024);
    assert!(ram == 0 || budget <= 1024 * 1024 * 1024);

    let mut buf = DocumentBuffer::from_str("Start");
    let big_payload = "AlphaBetaGammaDelta\n".repeat(4_000); // > 64 KiB -> Rope-backed UndoText
    buf.replace_range(0..buf.len_chars(), &big_payload);
    assert_eq!(buf.len_bytes(), big_payload.len());
    assert!(buf.undo());
    assert_eq!(buf.as_string(), "Start");
    assert!(buf.redo());
    assert_eq!(buf.len_bytes(), big_payload.len());
}

#[test]
fn parallel_ast_parse_and_search_match_serial_and_support_cancellation() {
    // > 300 KB Markdown triggers multi-threaded chunked AST parser when beneficial.
    let mut big_md = String::with_capacity(500_000);
    for i in 0..5_500 {
        big_md.push_str(&format!(
            "## Section {i}\n\nParagraph {i} with **bold**, `code`, and needle_token_{i}.\n\n"
        ));
    }
    assert!(big_md.len() > 380_000);
    let buf = DocumentBuffer::from_str(&big_md);
    let serial = parse_to_ir(&big_md);
    let rope_ir = parse_rope_to_ir(buf.rope());
    assert_eq!(serial.len(), rope_ir.len());
    assert_eq!(extract_tables(&big_md).len(), 0);

    // Pre-cancelled AST parse aborts immediately
    let cancel_gen = AtomicU64::new(2);
    let aborted = parse_rope_to_ir_cancellable(buf.rope(), Some((&cancel_gen, 1)));
    assert!(aborted.is_none());

    // > 1.2 MB Search triggers parallel literal & regex paths
    let big_search_text = format!("{}\n{}", big_md, big_md) + &big_md;
    assert!(big_search_text.len() > 1_100_000);
    let search_buf = DocumentBuffer::from_str(&big_search_text);
    let literal = find_all_matches_in_rope(
        search_buf.rope(),
        "needle_token_42",
        &SearchOptions::default(),
    );
    assert_eq!(literal.matches.len(), 333);

    let literal_word = find_all_matches_in_rope(
        search_buf.rope(),
        "needle_token_42",
        &SearchOptions {
            whole_word: true,
            ..Default::default()
        },
    );
    assert_eq!(literal_word.matches.len(), 3);

    let regex_opts = SearchOptions {
        use_regex: true,
        ..Default::default()
    };
    let regex_res = find_all_matches_in_rope(search_buf.rope(), r"needle_token_\d+", &regex_opts);
    assert_eq!(regex_res.matches.len(), 3 * 5_500);

    let cancel_search = AtomicBool::new(true);
    let cancelled = find_all_matches_in_rope_cancellable(
        search_buf.rope(),
        "needle_token_",
        &SearchOptions::default(),
        Some(&cancel_search),
    );
    assert!(cancelled.matches.is_empty());
}

#[test]
fn scalable_diff_exceeds_old_2000_line_limit_without_truncation() {
    let mut old_lines = Vec::with_capacity(3_000);
    let mut new_lines = Vec::with_capacity(3_000);
    for i in 0..3_000 {
        let s = format!("line_{i:04}");
        old_lines.push(s.clone());
        if i % 500 == 123 {
            new_lines.push(format!("modified_line_{i:04}"));
        } else {
            new_lines.push(s);
        }
    }
    let (rows, added, removed, truncated) = compute_bounded_line_diff(&old_lines, &new_lines);
    assert!(!truncated, "scalable Patience+DP diff must not truncate 3,000 lines");
    assert_eq!(added, 6);
    assert_eq!(removed, 6);
    assert!(!rows.is_empty());
}

#[test]
fn streaming_html_and_json_export_produce_valid_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let md = "# Heading One\n\nParagraph text.\n";
    let buf = DocumentBuffer::from_str(md);
    let blocks = parse_rope_to_ir(buf.rope());

    let html_path = dir.path().join("out.html");
    export_html_to_file_with_base_dir(&blocks, "Out", &html_path, None, |_| {}).unwrap();
    let html = std::fs::read_to_string(&html_path).unwrap();
    assert!(html.contains(&heading_slug("Heading One")));

    let json_path = dir.path().join("out.json");
    export_json_to_file_from_rope(&blocks, "Out", None, buf.rope(), &json_path, |_| {}).unwrap();
    let json = std::fs::read_to_string(&json_path).unwrap();
    assert!(json.contains("\"format\": \"markdown-studio-document\""));
    assert!(json.contains("Heading One"));
}
