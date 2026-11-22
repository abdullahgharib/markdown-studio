use std::fs;
use std::io::{Cursor, Read};
use std::time::{Duration, Instant};

use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader as XmlReader;
use tempfile::tempdir;
use zip::ZipArchive;

use markdown_studio::document::ast::parse_to_ir;
use markdown_studio::document::DocumentBuffer;
use markdown_studio::export::docx::{export_docx_to_file, export_docx_to_file_with_base_dir};
use markdown_studio::export::html::{
    export_html_to_file_with_base_dir, render_html_string_with_base_dir,
};
use markdown_studio::export::json::{export_json_to_file, render_json_string};
use markdown_studio::export::pdf::{
    export_pdf_to_file, export_pdf_to_file_with_options, export_pdf_to_file_with_options_and_base_dir,
    PdfLayoutOptions,
};
use markdown_studio::export::print::{
    parse_page_range, PageOrientation, PaperSize, PrintJobOptions,
};
use markdown_studio::export::{BackgroundExporter, ExportEvent, ExportTarget};

fn sample_export_markdown(repeat_sections: usize) -> String {
    let mut md = String::from(
        r#"# Quarterly Engineering Report

## Executive Summary

This report contains **bold metrics**, *italicized commentary*, ~~deprecated items~~, `inline_code()`, and a [reference link](https://example.org).

> Critical Architecture Note: All exports run asynchronously on background worker threads.

### Checklist

- [x] Rope B-tree buffer verified
- [ ] Release tag signed

1. Parse Markdown AST via `comrak`
2. Compile vector PDF and OpenXML Word artifacts

```rust
pub fn verify_export() -> bool {
    true
}
```

### Benchmark Table

| Subsystem | Complexity | Target |
| :--- | :---: | ---: |
| Rope Edit | O(log n) | < 50 us |
| Viewport Layout | O(log B) | < 8 ms |
| Vector PDF | Streaming | Validated |

---
"#,
    );

    for i in 0..repeat_sections {
        md.push_str(&format!(
            "\n#### Appendix Section {i}\n\nParagraph {i} verifying multi-page pagination, margins, and table continuity across page boundaries.\n\n| Key | Value |\n| --- | --- |\n| Item {i}A | Ok |\n| Item {i}B | Pass |\n"
        ));
    }

    md
}

#[test]
fn pdf_export_produces_valid_multipage_lopdf_document() {
    let dir = tempdir().unwrap();
    let pdf_path = dir.path().join("report.pdf");
    let md = sample_export_markdown(14);
    let blocks = parse_to_ir(&md);

    let mut last_progress = 0.0_f32;
    export_pdf_to_file(&blocks, "Quarterly Engineering Report", &pdf_path, |p| {
        assert!(p >= last_progress);
        last_progress = p;
    })
    .expect("PDF export must succeed");
    assert!((last_progress - 1.0).abs() < 1e-4);

    let bytes = fs::read(&pdf_path).expect("PDF file must exist");
    assert!(bytes.starts_with(b"%PDF-1."));

    let doc = lopdf::Document::load_mem(&bytes).expect("lopdf must parse generated PDF");
    let pages = doc.get_pages();
    assert!(
        pages.len() >= 2,
        "expected multi-page PDF, got {} page(s)",
        pages.len()
    );
    assert!(
        !doc.reference_table.entries.is_empty(),
        "PDF cross-reference table must be populated"
    );

    // Confirm catalog and /Pages tree exist and every page has a content stream.
    let catalog = doc.catalog().expect("PDF must have /Catalog");
    assert!(catalog.get(b"Pages").is_ok());
    for (&page_num, &page_id) in &pages {
        let content = doc
            .get_page_content(page_id)
            .expect("each page must have valid content stream");
        assert!(
            !content.is_empty(),
            "page {page_num} content stream must not be empty"
        );
    }
}

#[test]
fn docx_export_produces_valid_zip_and_well_formed_document_xml() {
    let dir = tempdir().unwrap();
    let docx_path = dir.path().join("report.docx");
    let md = sample_export_markdown(2);
    let blocks = parse_to_ir(&md);

    export_docx_to_file(&blocks, &docx_path, |_| {}).expect("DOCX export must succeed");

    let bytes = fs::read(&docx_path).expect("DOCX file must exist");
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).expect("DOCX must be a valid ZIP archive");

    for required in [
        "[Content_Types].xml",
        "_rels/.rels",
        "word/document.xml",
        "word/styles.xml",
        "word/numbering.xml",
    ] {
        assert!(
            archive.by_name(required).is_ok(),
            "missing required OpenXML entry: {required}"
        );
    }

    let mut doc_xml = String::new();
    archive
        .by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut doc_xml)
        .expect("word/document.xml must be valid UTF-8");

    // Strictly validate XML well-formedness and element balance via quick-xml.
    let mut reader = XmlReader::from_str(&doc_xml);
    let mut buf = Vec::new();
    let mut saw_document = false;
    let mut saw_body = false;
    let mut saw_table = false;
    let mut saw_bold = false;
    let mut saw_italic = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Start(e)) | Ok(XmlEvent::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_str() {
                    "w:document" => saw_document = true,
                    "w:body" => saw_body = true,
                    "w:tbl" => saw_table = true,
                    "w:b" => saw_bold = true,
                    "w:i" => saw_italic = true,
                    _ => {}
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(e) => panic!("malformed XML in word/document.xml: {e:?}"),
            _ => {}
        }
        buf.clear();
    }

    assert!(saw_document, "missing <w:document>");
    assert!(saw_body, "missing <w:body>");
    assert!(saw_table, "missing <w:tbl> for GFM table");
    assert!(saw_bold, "missing <w:b> for bold run");
    assert!(saw_italic, "missing <w:i> for italic run");
    assert!(doc_xml.contains("Quarterly Engineering Report"));
    assert!(doc_xml.contains("Rope Edit"));
}

#[test]
fn background_exporter_and_print_spool_pipeline() {
    let md = sample_export_markdown(1);
    let buf = DocumentBuffer::from_str(&md);
    let mut exporter = BackgroundExporter::new();

    exporter.start_job(
        buf.rope().clone(),
        "Spool Test".to_string(),
        ExportTarget::Print { spool_only: true },
    );
    assert!(exporter.busy);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut spool_path = None;
    while Instant::now() < deadline {
        for ev in exporter.poll() {
            match ev {
                ExportEvent::Completed { output_path, .. } => {
                    spool_path = output_path;
                }
                ExportEvent::Failed { error } => panic!("background print spool failed: {error}"),
                _ => {}
            }
        }
        if !exporter.busy {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let path = spool_path.expect("background print job should produce a temporary spool PDF");
    let doc = lopdf::Document::load(&path).expect("spool PDF must parse cleanly with lopdf");
    assert!(!doc.get_pages().is_empty());
    let _ = fs::remove_file(path);
}

#[test]
fn pdf_layout_options_and_page_range_validation() {
    let dir = tempdir().unwrap();
    let pdf_path = dir.path().join("letter_landscape.pdf");
    let md = sample_export_markdown(4);
    let blocks = parse_to_ir(&md);

    let opts = PrintJobOptions {
        paper_size: PaperSize::Letter,
        orientation: PageOrientation::Landscape,
        copies: 2,
        ..Default::default()
    };
    let layout = opts.layout_options();
    assert!((layout.page_w_mm - 279.4).abs() < 0.1);
    assert!((layout.page_h_mm - 215.9).abs() < 0.1);

    export_pdf_to_file_with_options(&blocks, "Landscape Letter", &pdf_path, layout, |_| {})
        .expect("custom paper size PDF export must succeed");
    let doc = lopdf::Document::load(&pdf_path).expect("custom PDF must load with lopdf");
    assert!(!doc.get_pages().is_empty());

    assert_eq!(parse_page_range("1-2, 4", Some(5)).unwrap(), vec![1, 2, 4]);
    assert!(parse_page_range("0", Some(5)).is_err());
    assert!(parse_page_range("4-2", Some(5)).is_err());
}

#[test]
fn pdf_export_unicode_arabic_alignment_footnotes_and_image() {
    let dir = tempdir().unwrap();
    let img_path = dir.path().join("sample.png");
    let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([220, 40, 40, 200]));
    img.save(&img_path).expect("save sample PNG");

    let mut md = String::from(
        "<!-- align:center --># Unicode Title — سلام\n\n\
         <!-- align:right,indent:1 -->مرحبا (بالعالم) with <u>underline</u> and <ins>ins</ins>[^1]\n\n\
         ![redbox](sample.png)\n\n\
         [^1]: Footnote explanation text.\n\n\
         | Column A | Column B |\n| :--- | ---: |\n",
    );
    for i in 0..55 {
        md.push_str(&format!(
            "| Multi-line cell row {i} with extra wrapped words | Value {i} |\n"
        ));
    }

    let blocks = parse_to_ir(&md);
    let pdf_path = dir.path().join("unicode_and_img.pdf");
    export_pdf_to_file_with_options_and_base_dir(
        &blocks,
        "Unicode & Image",
        &pdf_path,
        PdfLayoutOptions::default(),
        Some(dir.path()),
        |_| {},
    )
    .expect("Unicode/image PDF export must succeed");

    let doc = lopdf::Document::load(&pdf_path).expect("Unicode PDF must parse cleanly");
    assert!(
        doc.get_pages().len() >= 2,
        "long table should paginate across multiple pages"
    );
}

#[test]
fn docx_export_parity_hyperlinks_bidi_lists_borders_footnotes_and_image() {
    let dir = tempdir().unwrap();
    let img_path = dir.path().join("logo.png");
    let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([30, 120, 220, 255]));
    img.save(&img_path).expect("save logo PNG");

    let md = "\
<!-- align:center --># Heading Center\n\n\
<!-- align:right,indent:1 -->مرحبا بالعالم [Example](https://example.org) line1<br>line2[^n1]\n\n\
1. First list one\n2. First list two\n\n\
Paragraph breaking list.\n\n\
5. Restarted five\n6. Restarted six\n\n\
> Quoted insight\n\n\
---\n\n\
![logo](logo.png)\n\n\
[^n1]: Footnote detail here.\n";

    let blocks = parse_to_ir(md);
    let docx_path = dir.path().join("parity.docx");
    export_docx_to_file_with_base_dir(&blocks, &docx_path, Some(dir.path()), |_| {})
        .expect("DOCX parity export must succeed");

    let bytes = fs::read(&docx_path).unwrap();
    let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();

    let mut doc_xml = String::new();
    zip.by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut doc_xml)
        .unwrap();
    let mut num_xml = String::new();
    zip.by_name("word/numbering.xml")
        .unwrap()
        .read_to_string(&mut num_xml)
        .unwrap();
    let mut rels_xml = String::new();
    zip.by_name("word/_rels/document.xml.rels")
        .unwrap()
        .read_to_string(&mut rels_xml)
        .unwrap();

    assert!(doc_xml.contains("w:hyperlink"), "expected real hyperlink");
    assert!(
        rels_xml.contains("https://example.org"),
        "expected external URL rel"
    );
    assert!(doc_xml.contains("w:bidi"), "expected RTL paragraph bidi");
    assert!(doc_xml.contains("w:br"), "expected soft/hard line break");
    assert!(doc_xml.contains("w:pBdr"), "expected blockquote/hr borders");
    assert!(doc_xml.contains("w:drawing"), "expected embedded image drawing");
    assert!(
        num_xml.contains("startOverride") && num_xml.contains("5"),
        "expected restarted ordered list startOverride=5"
    );

    let has_media = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .any(|name| name.starts_with("word/media/"));
    assert!(has_media, "expected image stored under word/media/");
}

fn decode_pdf_outline_title(obj: &lopdf::Object) -> String {
    let bytes = match obj {
        lopdf::Object::String(b, _) => b.as_slice(),
        other => panic!("expected String for outline /Title, got {other:?}"),
    };
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_be_bytes(c))
            .collect();
        String::from_utf16(&units).expect("valid UTF-16BE outline title")
    } else {
        String::from_utf8(bytes.to_vec()).expect("valid ASCII outline title")
    }
}

#[test]
fn pdf_export_embeds_hierarchical_document_outline_bookmarks() {
    let dir = tempdir().unwrap();
    let mut md = String::from("# Title H1\n\n## Child H2 A\n\n### Grandchild H3\n\n");
    for i in 0..55 {
        md.push_str(&format!(
            "Filler paragraph {i} to guarantee multi-page PDF pagination across headings.\n\n"
        ));
    }
    md.push_str("## مرحبا بالعالم\n\nClosing section body.\n");

    let blocks = parse_to_ir(&md);
    let pdf_path = dir.path().join("bookmarked.pdf");
    export_pdf_to_file_with_options(&blocks, "Bookmarked", &pdf_path, PdfLayoutOptions::default(), |_| {})
        .expect("bookmarked PDF export must succeed");

    let doc = lopdf::Document::load(&pdf_path).expect("load exported PDF via lopdf");
    let pages = doc.get_pages();
    assert!(pages.len() >= 2, "expected multiple pages, got {}", pages.len());
    let page1_id = pages[&1];
    let last_page_id = *pages.values().last().unwrap();
    assert_ne!(page1_id, last_page_id);

    let catalog_id = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
    let catalog = doc.get_object(catalog_id).unwrap().as_dict().unwrap();
    assert_eq!(catalog.get(b"PageMode").unwrap().as_name().unwrap(), b"UseOutlines");

    let outlines_id = catalog.get(b"Outlines").unwrap().as_reference().unwrap();
    let outlines = doc.get_object(outlines_id).unwrap().as_dict().unwrap();
    assert_eq!(outlines.get(b"Count").unwrap().as_i64().unwrap(), 4);

    let h1_id = outlines.get(b"First").unwrap().as_reference().unwrap();
    let h1 = doc.get_object(h1_id).unwrap().as_dict().unwrap();
    assert_eq!(decode_pdf_outline_title(h1.get(b"Title").unwrap()), "Title H1");
    assert_eq!(h1.get(b"Count").unwrap().as_i64().unwrap(), 3);

    let h2_a_id = h1.get(b"First").unwrap().as_reference().unwrap();
    let h2_a = doc.get_object(h2_a_id).unwrap().as_dict().unwrap();
    assert_eq!(
        decode_pdf_outline_title(h2_a.get(b"Title").unwrap()),
        "Child H2 A"
    );
    assert_eq!(h2_a.get(b"Parent").unwrap().as_reference().unwrap(), h1_id);
    assert_eq!(h2_a.get(b"Count").unwrap().as_i64().unwrap(), 1);

    let h3_id = h2_a.get(b"First").unwrap().as_reference().unwrap();
    let h3 = doc.get_object(h3_id).unwrap().as_dict().unwrap();
    assert_eq!(
        decode_pdf_outline_title(h3.get(b"Title").unwrap()),
        "Grandchild H3"
    );
    assert_eq!(h3.get(b"Parent").unwrap().as_reference().unwrap(), h2_a_id);

    let h2_b_id = h2_a.get(b"Next").unwrap().as_reference().unwrap();
    let h2_b = doc.get_object(h2_b_id).unwrap().as_dict().unwrap();
    assert_eq!(
        decode_pdf_outline_title(h2_b.get(b"Title").unwrap()),
        "مرحبا بالعالم"
    );
    assert_eq!(h2_b.get(b"Prev").unwrap().as_reference().unwrap(), h2_a_id);
    assert_eq!(h1.get(b"Last").unwrap().as_reference().unwrap(), h2_b_id);

    let parse_dest = |dict: &lopdf::Dictionary| -> (lopdf::ObjectId, f32) {
        let dest = dict.get(b"Dest").unwrap().as_array().unwrap();
        assert_eq!(dest.len(), 5);
        assert_eq!(dest[1].as_name().unwrap(), b"XYZ");
        let page_ref = dest[0].as_reference().unwrap();
        let y_pt = dest[3].as_float().unwrap();
        (page_ref, y_pt)
    };
    let (h1_pg, h1_y) = parse_dest(h1);
    let (h2_a_pg, h2_a_y) = parse_dest(h2_a);
    let (h2_b_pg, _) = parse_dest(h2_b);
    assert_eq!(h1_pg, page1_id);
    assert_eq!(h2_a_pg, page1_id);
    assert!(h1_y > h2_a_y, "H1 y ({h1_y}) must exceed H2 y ({h2_a_y}) on page 1");
    assert_eq!(h2_b_pg, last_page_id, "final Arabic H2 must point to final page");
}

#[test]
fn pdf_and_docx_export_resolve_spaced_and_angle_bracket_image_paths() {
    let dir = tempdir().unwrap();
    let img_path = dir.path().join("my test image.png");
    let img = image::RgbaImage::from_pixel(6, 6, image::Rgba([20, 140, 210, 255]));
    img.save(&img_path).expect("save spaced PNG");

    let md = "![first](<my test image.png>)\n\n![second](my%20test%20image.png)\n";
    let blocks = parse_to_ir(md);

    let pdf_path = dir.path().join("spaced_imgs.pdf");
    export_pdf_to_file_with_options_and_base_dir(
        &blocks,
        "Spaced Images",
        &pdf_path,
        PdfLayoutOptions::default(),
        Some(dir.path()),
        |_| {},
    )
    .expect("PDF export with spaced image names must succeed");
    let doc = lopdf::Document::load(&pdf_path).unwrap();
    let image_xobj_count = doc
        .objects
        .values()
        .filter(|obj| {
            obj.as_stream().ok().is_some_and(|st| {
                st.dict
                    .get(b"Subtype")
                    .and_then(lopdf::Object::as_name)
                    .ok()
                    == Some(b"Image")
            })
        })
        .count();
    assert!(image_xobj_count >= 2, "expected 2 embedded Image XObjects in PDF, got {image_xobj_count}");

    let docx_path = dir.path().join("spaced_imgs.docx");
    export_docx_to_file_with_base_dir(&blocks, &docx_path, Some(dir.path()), |_| {})
        .expect("DOCX export with spaced image names must succeed");
    let bytes = fs::read(&docx_path).unwrap();
    let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut doc_xml = String::new();
    zip.by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut doc_xml)
        .unwrap();
    assert_eq!(doc_xml.matches("<w:drawing>").count(), 2, "expected 2 embedded drawings in DOCX");
}

#[test]
fn pdf_and_docx_export_render_math_mermaid_and_horizontal_rule() {
    let dir = tempdir().unwrap();
    let md = r#"# Mathematical & Visual Report

Simple inline math $E = mc^2$, inline fraction $\frac{a+b}{c}$, and inline radical \(\sqrt{x^2 + y^2}\).

$$
\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}
$$

\[
A = \begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}
\]

```math
\sum_{i=1}^n i = \frac{n(n+1)}{2}
```

---

```mermaid
flowchart LR
    Start([Start]) --> Check{Valid?}
    Check -->|Yes| Save[(DB)]
    Check -->|No| Fix[Fix]
```

```mermaid
sequenceDiagram
    Client->>Server: Request
    Server-->>Client: Response
```

```mermaid
stateDiagram-v2
    [*] --> Ready
    Ready --> Done : finish
```

```mermaid
classDiagram
    Base <|-- Sub
    class Base {
        +id: int
        +run()
    }
```

```mermaid
erDiagram
    CUSTOMER ||--o{ ORDER : places
```

```mermaid
pie showData
    title Share
    "Core" : 75
    "Plugins" : 25
```
"#;

    let blocks = parse_to_ir(md);

    // 1. PDF Export verification
    let pdf_path = dir.path().join("math_mermaid.pdf");
    export_pdf_to_file(&blocks, "Math & Mermaid Report", &pdf_path, |_| {})
        .expect("PDF export with Math and Mermaid must succeed");
    let pdf_bytes = fs::read(&pdf_path).unwrap();
    assert!(pdf_bytes.starts_with(b"%PDF-1."));
    let doc = lopdf::Document::load_mem(&pdf_bytes).expect("PDF must parse cleanly");
    assert!(!doc.get_pages().is_empty());
    let image_xobj_count = doc
        .objects
        .values()
        .filter(|obj| {
            obj.as_stream().ok().is_some_and(|st| {
                st.dict
                    .get(b"Subtype")
                    .and_then(lopdf::Object::as_name)
                    .ok()
                    == Some(b"Image")
            })
        })
        .count();
    assert!(
        image_xobj_count >= 9,
        "expected >= 9 embedded Image XObjects (3 display math + 6 Mermaid), got {image_xobj_count}"
    );

    // 2. DOCX Export verification
    let docx_path = dir.path().join("math_mermaid.docx");
    export_docx_to_file(&blocks, &docx_path, |_| {})
        .expect("DOCX export with Math and Mermaid must succeed");
    let docx_bytes = fs::read(&docx_path).unwrap();
    let mut zip = ZipArchive::new(Cursor::new(docx_bytes)).unwrap();
    let mut doc_xml = String::new();
    zip.by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut doc_xml)
        .unwrap();

    // Validate well-formed XML via quick-xml
    let mut reader = XmlReader::from_str(&doc_xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Eof) => break,
            Err(e) => panic!("malformed DOCX XML: {e:?}"),
            _ => {}
        }
        buf.clear();
    }

    assert!(
        doc_xml.contains("Cambria Math"),
        "expected Cambria Math font for inline equations"
    );
    assert!(
        doc_xml.contains("w:pBdr"),
        "expected horizontal rule paragraph border"
    );
    let drawing_count = doc_xml.matches("<w:drawing>").count();
    assert!(
        drawing_count >= 11,
        "expected >= 11 embedded drawings (2 inline 2D math + 3 display math + 6 Mermaid), got {drawing_count}"
    );
    let media_count = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|name| name.starts_with("word/media/"))
        .count();
    assert!(
        media_count >= 11,
        "expected >= 11 media files under word/media/, got {media_count}"
    );
}

#[test]
fn pdf_export_embeds_clickable_outline_and_link_annotations() {
    let dir = tempdir().unwrap();
    let pdf_path = dir.path().join("clickable_links.pdf");

    let mut md = String::from(
        r#"# Main Document

Jump to [Target Section](#target-section), visit [Example Site](https://example.org/docs), or check note[^ref1].

Also jump to duplicate [Second Copy](#repeat-heading-1) and skip [Unsafe](javascript:alert(1)).

"#,
    );
    // Push Target Section onto page 2+
    for i in 0..45 {
        md.push_str(&format!(
            "Filler paragraph {i} pushing headings onto subsequent pages so destinations target page 2 or later.\n\n"
        ));
    }
    md.push_str(
        r#"## Target Section

Content under target section.

## Repeat Heading

First copy.

## Repeat Heading

Second copy.

[^ref1]: Footnote explanation on a later page.
"#,
    );

    let blocks = parse_to_ir(&md);
    export_pdf_to_file(&blocks, "Clickable Links", &pdf_path, |_| {})
        .expect("PDF export with links & outline must succeed");

    let bytes = fs::read(&pdf_path).unwrap();
    let doc = lopdf::Document::load_mem(&bytes).expect("PDF must parse via lopdf");
    let pages = doc.get_pages();
    assert!(pages.len() >= 2, "expected at least 2 pages, got {}", pages.len());

    // 1. Verify /Outlines tree and /PageMode /UseOutlines
    let catalog = doc.catalog().expect("catalog missing");
    assert_eq!(
        catalog.get(b"PageMode").and_then(lopdf::Object::as_name).ok(),
        Some(b"UseOutlines".as_slice())
    );
    let outlines_ref = catalog
        .get(b"Outlines")
        .and_then(lopdf::Object::as_reference)
        .expect("Outlines reference missing");
    let outlines_dict = doc.get_dictionary(outlines_ref).expect("Outlines dict missing");
    assert!(
        outlines_dict.get(b"Count").and_then(lopdf::Object::as_i64).unwrap_or(0) >= 4,
        "expected >= 4 outline entries"
    );

    // 2. Inspect page 1 /Annots for clickable /Link annotations
    let page1_id = pages[&1];
    let page1_dict = doc.get_dictionary(page1_id).expect("page 1 dict missing");
    let annots = page1_dict
        .get(b"Annots")
        .ok()
        .and_then(|o| match o {
            lopdf::Object::Array(arr) => Some(arr.clone()),
            lopdf::Object::Reference(r) => doc
                .get_object(*r)
                .and_then(lopdf::Object::as_array)
                .ok()
                .cloned(),
            _ => None,
        })
        .expect("page 1 must contain /Annots array");

    let mut saw_external_uri = false;
    let mut internal_dest_pages = Vec::new();
    for annot_ref in &annots {
        let annot_id = annot_ref.as_reference().expect("annot must be indirect ref");
        let annot = doc.get_dictionary(annot_id).expect("annot dict missing");
        assert_eq!(
            annot.get(b"Subtype").and_then(lopdf::Object::as_name).ok(),
            Some(b"Link".as_slice())
        );
        if let Ok(action_obj) = annot.get(b"A") {
            let action = match action_obj {
                lopdf::Object::Dictionary(d) => d.clone(),
                lopdf::Object::Reference(r) => doc.get_dictionary(*r).unwrap().clone(),
                _ => panic!("unexpected /A type"),
            };
            if action.get(b"S").and_then(lopdf::Object::as_name).ok() == Some(b"URI".as_slice()) {
                let uri_bytes = action
                    .get(b"URI")
                    .and_then(lopdf::Object::as_str)
                    .expect("URI string missing");
                let uri_str = String::from_utf8_lossy(uri_bytes);
                if uri_str == "https://example.org/docs" {
                    saw_external_uri = true;
                }
            }
        }
        if let Ok(dest) = annot.get(b"Dest").and_then(lopdf::Object::as_array) {
            let target_page_id = dest[0].as_reference().expect("Dest[0] must be page ref");
            internal_dest_pages.push(target_page_id);
        }
    }

    assert!(saw_external_uri, "expected external URI link annotation");
    assert!(
        internal_dest_pages.len() >= 3,
        "expected >= 3 internal /Dest link annotations (#target-section, #repeat-heading-1, [^ref1]), got {}",
        internal_dest_pages.len()
    );
    let page2_or_later_ids: Vec<lopdf::ObjectId> = pages
        .iter()
        .filter(|(&num, _)| num >= 2)
        .map(|(_, &id)| id)
        .collect();
    assert!(
        internal_dest_pages
            .iter()
            .all(|pid| page2_or_later_ids.contains(pid)),
        "internal section & footnote links should jump to page 2+"
    );
}

#[test]
fn html_export_generates_clickable_outline_anchors_links_tables_and_diagrams() {
    let dir = tempdir().unwrap();
    let md = r#"# Overview & Architecture

Jump to [Target Section](#target-section), [Second Repeat](#repeat-title-1), or [Web Docs](https://example.org).

Also skip [Unsafe Link](javascript:alert('xss')) and check note[^fn1].

## Target Section

| Feature | Status |
| :--- | ---: |
| HTML | Ready |

## Repeat Title

First section.

## Repeat Title

Second section.

$$
E = mc^2
$$

```mermaid
flowchart LR
    A[Start] --> B[Done]
```

[^fn1]: Footnote body text.
"#;

    let blocks = parse_to_ir(md);
    let html = render_html_string_with_base_dir(&blocks, "Architecture & Spec <v1>", Some(dir.path()));

    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("<title>Architecture &amp; Spec &lt;v1&gt;</title>"));
    assert!(html.contains("<nav class=\"outline\""));
    assert!(html.contains("href=\"#overview--architecture\""));
    assert!(html.contains("<h1 id=\"overview--architecture\">"));
    assert!(html.contains("href=\"#target-section\""));
    assert!(html.contains("<h2 id=\"target-section\">"));
    assert!(html.contains("href=\"#repeat-title-1\""));
    assert!(html.contains("<h2 id=\"repeat-title-1\">"));
    assert!(html.contains("href=\"https://example.org\""));
    assert!(html.contains("href=\"#fn-fn1\""));
    assert!(html.contains("id=\"fn-fn1\""));
    assert!(!html.contains("javascript:alert"));
    assert!(html.contains("Unsafe Link"));
    assert!(html.contains("<table>"));
    assert!(html.contains("data:image/png;base64,"));

    let html_path = dir.path().join("doc.html");
    let mut last_p = 0.0_f32;
    export_html_to_file_with_base_dir(&blocks, "Doc", &html_path, Some(dir.path()), |p| {
        assert!(p >= last_p);
        last_p = p;
    })
    .expect("HTML file export must succeed");
    assert!((last_p - 1.0).abs() < 1e-4);
    assert!(fs::read_to_string(&html_path).unwrap().contains("<nav class=\"outline\""));
}

#[test]
fn json_export_and_background_exporter_produce_structured_html_and_json() {
    let dir = tempdir().unwrap();
    let src_path = dir.path().join("notes.md");
    let md = "# Spec \"Alpha\" \\ v1\n\nSee [Details](#details), [Web](https://example.org), and note[^1].\n\n## Details\n\n- [x] Done\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n\n```rust\nfn ok() {}\n```\n\n[^1]: Note text.\n";
    let blocks = parse_to_ir(md);

    let json = render_json_string(&blocks, "Spec \"Alpha\"", Some(&src_path), md);
    assert!(json.contains("\"format\": \"markdown-studio-document\""));
    assert!(json.contains("\"title\": \"Spec \\\"Alpha\\\"\""));
    assert!(json.contains("\"heading_count\": 2"));
    assert!(json.contains("\"link_count\": 3"));
    assert!(json.contains("\"slug\": \"spec-alpha--v1\""));
    assert!(json.contains("\"slug\": \"details\""));
    assert!(json.contains("\"kind\": \"anchor\""));
    assert!(json.contains("\"kind\": \"external\""));
    assert!(json.contains("\"kind\": \"footnote\""));
    assert!(json.contains("\"target_slug\": \"details\""));
    assert!(json.contains("\"type\": \"table\""));
    assert!(json.contains("\"type\": \"code_block\""));

    // Verify balanced JSON braces/brackets and clean escaping (no unescaped newline inside strings)
    let mut in_string = false;
    let mut escaped = false;
    let mut depth = 0i32;
    for ch in json.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            } else if ch == '\n' || ch == '\r' {
                panic!("unescaped newline inside JSON string literal");
            }
        } else {
            match ch {
                '"' => in_string = true,
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            }
        }
        assert!(depth >= 0, "JSON structure underflow");
    }
    assert!(!in_string, "unterminated JSON string");
    assert_eq!(depth, 0, "unbalanced JSON braces/brackets");

    let direct_json = dir.path().join("direct.json");
    export_json_to_file(&blocks, "Spec", Some(&src_path), md, &direct_json, |_| {})
        .expect("direct JSON export must succeed");
    assert!(fs::read_to_string(&direct_json).unwrap().contains("\"blocks\":"));

    // BackgroundExporter end-to-end for HTML and JSON
    let buf = DocumentBuffer::from_str(md);
    let mut exporter = BackgroundExporter::new();

    let bg_html = dir.path().join("bg.html");
    exporter.start_job_with_base_dir(
        buf.rope().clone(),
        "BG HTML".to_string(),
        ExportTarget::Html(bg_html.clone()),
        Some(dir.path().to_path_buf()),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while exporter.busy && Instant::now() < deadline {
        for ev in exporter.poll() {
            if let ExportEvent::Failed { error } = ev {
                panic!("background HTML export failed: {error}");
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!exporter.busy);
    assert!(fs::read_to_string(&bg_html).unwrap().contains("href=\"#details\""));

    let bg_json = dir.path().join("bg.json");
    exporter.start_job_with_base_dir(
        buf.rope().clone(),
        "BG JSON".to_string(),
        ExportTarget::Json {
            path: bg_json.clone(),
            source_path: Some(src_path),
        },
        Some(dir.path().to_path_buf()),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while exporter.busy && Instant::now() < deadline {
        for ev in exporter.poll() {
            if let ExportEvent::Failed { error } = ev {
                panic!("background JSON export failed: {error}");
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!exporter.busy);
    assert!(fs::read_to_string(&bg_json).unwrap().contains("\"outline\":"));
}

#[test]
fn docx_export_has_clickable_internal_external_footnote_links_bookmarks_and_clean_formatting() {
    let dir = tempdir().unwrap();
    let docx_path = dir.path().join("links_and_style.docx");

    let md = r#"# Document Root

See [Target Section](#target-section), [Duplicate Section](#repeat-title-1), [Web Portal](www.example.org/guide), and [Unsafe Link](javascript:alert(1)), plus note[^fn1].

## Target Section

| Feature | Link |
| :--- | :--- |
| Anchor | [Go Repeat](#repeat-title) |

```rust
fn clean_code() {}
```

## Repeat Title

First section.

## Repeat Title

Second section.

[^fn1]: Footnote target explanation.
"#;

    let blocks = parse_to_ir(md);
    export_docx_to_file(&blocks, &docx_path, |_| {})
        .expect("DOCX export with links & formatting must succeed");

    let bytes = fs::read(&docx_path).unwrap();
    let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();

    let mut doc_xml = String::new();
    zip.by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut doc_xml)
        .unwrap();
    let mut rels_xml = String::new();
    zip.by_name("word/_rels/document.xml.rels")
        .unwrap()
        .read_to_string(&mut rels_xml)
        .unwrap();

    // Validate well-formed OpenXML via quick-xml
    let mut reader = XmlReader::from_str(&doc_xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Eof) => break,
            Err(e) => panic!("malformed DOCX document.xml: {e:?}"),
            _ => {}
        }
        buf.clear();
    }

    assert!(doc_xml.contains("<w:bookmarkStart"), "headings/footnotes must emit Word bookmarks");
    assert!(doc_xml.contains("<w:outlineLvl"), "headings must emit Word outline levels");
    assert!(doc_xml.contains("w:anchor=\"md_target_section\""), "expected internal section anchor link");
    assert!(doc_xml.contains("w:anchor=\"md_repeat_title_1\""), "expected deduplicated heading anchor link");
    assert!(doc_xml.contains("w:anchor=\"md_fn_fn1\""), "expected clickable footnote anchor link");
    assert!(doc_xml.contains("r:id="), "expected external hyperlink r:id");
    assert!(rels_xml.contains("https://www.example.org/guide"), "expected normalized external URL in rels");
    assert!(!rels_xml.contains("#target-section"), "internal anchor must NOT be written as an external rel");
    assert!(!rels_xml.contains("javascript:"), "unsafe javascript: scheme must never appear in rels");
    assert!(!doc_xml.contains("javascript:"), "unsafe javascript: scheme must never appear in document.xml");
    assert!(doc_xml.contains("<w:tblBorders"), "clean tables & code blocks must declare borders");
    assert!(doc_xml.contains("<w:tblCellMar"), "clean tables & code blocks must declare cell margins");
    assert!(doc_xml.contains("<w:pgMar"), "page margins must be configured");
}

#[test]
fn pdf_export_renders_custom_multi_page_headers_and_footers() {
    use markdown_studio::export::pdf::{render_pdf_bytes_with_options, PdfHeaderFooterOptions};

    let dir = tempdir().unwrap();
    let pdf_path = dir.path().join("headers_footers.pdf");
    // Generate enough blocks to span >= 2 PDF pages
    let md = sample_export_markdown(45);
    let blocks = parse_to_ir(&md);

    let baseline_layout = PdfLayoutOptions {
        header_footer: PdfHeaderFooterOptions {
            show_header: false,
            show_footer: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let baseline_bytes =
        render_pdf_bytes_with_options(&blocks, "IgnoredTitle", baseline_layout, |_| {})
            .expect("baseline PDF must compile");
    let baseline_doc = lopdf::Document::load_mem(&baseline_bytes).unwrap();
    let baseline_pages = baseline_doc.get_pages();
    let base_p1_len = baseline_doc.get_page_content(baseline_pages[&1]).unwrap().len();

    let layout = PdfLayoutOptions {
        header_footer: PdfHeaderFooterOptions {
            show_header: true,
            header_template: "HEADER_DOC_{title}_{date}".to_string(),
            show_footer: true,
            footer_template: "FOOTER_P{page}_OF_{pages}".to_string(),
            date_text: "2026-11-22".to_string(),
            title_text: "SpecTitle".to_string(),
        },
        ..Default::default()
    };

    let bytes = render_pdf_bytes_with_options(&blocks, "IgnoredTitle", layout, |_| {})
        .expect("multi-page PDF with headers/footers must compile");
    fs::write(&pdf_path, &bytes).unwrap();

    let doc = lopdf::Document::load(&pdf_path).expect("PDF must load in lopdf");
    let pages = doc.get_pages();
    assert!(
        pages.len() >= 2,
        "expected >= 2 pages for header/footer test, got {}",
        pages.len()
    );
    let total_pages = pages.len();

    let p1_content = doc.get_page_content(pages[&1]).unwrap();
    let last_content = doc.get_page_content(pages[&(total_pages as u32)]).unwrap();
    let p1_str = String::from_utf8_lossy(&p1_content);
    let last_str = String::from_utf8_lossy(&last_content);
    assert!(
        p1_content.len() > base_p1_len + 100,
        "expected header+footer drawing ops added to page 1 content stream (base={base_p1_len}, got={})",
        p1_content.len()
    );
    assert!(
        p1_str.contains("TJ") || p1_str.contains("Tj"),
        "expected text operators on page 1"
    );
    assert!(
        last_str.contains("TJ") || last_str.contains("Tj"),
        "expected text operators on last page"
    );
}






