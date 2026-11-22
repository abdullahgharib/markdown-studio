use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};

use markdown_studio::document::ast::parse_to_ir;
use markdown_studio::document::viewport::{estimate_block_height, ViewportBlockIndex};
use markdown_studio::document::DocumentBuffer;
use markdown_studio::export::ir::{runs_to_plain_text, DocBlock, InlineRun};

fn generate_10mb_markdown() -> String {
    let chunk = concat!(
        "## Section Heading\n\n",
        "This is a **benchmark paragraph** with *italic text*, `inline_code()`, and a [link](https://example.com) designed to simulate real-world multi-megabyte technical documentation.\n\n",
        "- First bullet point with **emphasis**\n",
        "- [x] Completed checklist item\n\n",
        "| Metric | Value | Status |\n",
        "| :--- | :---: | ---: |\n",
        "| Latency | 12 us | Pass |\n\n",
        "```rust\n",
        "fn compute(x: usize) -> usize { x * 2 }\n",
        "```\n\n"
    );

    let target_bytes = 10 * 1024 * 1024;
    let mut out = String::with_capacity(target_bytes + chunk.len());
    while out.len() < target_bytes {
        out.push_str(chunk);
    }
    out
}

fn bench_rope_edits_10mb(c: &mut Criterion) {
    let fixture = generate_10mb_markdown();
    assert!(fixture.len() >= 10 * 1024 * 1024);
    let base_buf = DocumentBuffer::from_str(&fixture);
    let mid_char = base_buf.len_chars() / 2;
    let mid_line = base_buf.len_lines() / 2;

    let mut group = c.benchmark_group("rope_10mb");

    group.bench_function("insert_and_delete_middle_char", |b| {
        b.iter_batched(
            || base_buf.clone(), // O(1) Arc B-tree clone
            |mut buf| {
                buf.insert(black_box(mid_char), "**inserted** ");
                buf.remove(black_box(mid_char)..black_box(mid_char + 13));
                black_box(buf.len_chars());
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("line_jump_and_slice_10mb", |b| {
        b.iter(|| {
            let ch = base_buf.line_to_char(black_box(mid_line));
            let line = base_buf.line_text(black_box(mid_line));
            black_box((ch, line.len()));
        });
    });

    group.finish();
}

fn bench_virtualized_viewport_10mb(c: &mut Criterion) {
    // Build a 10 MB Markdown document and index its blocks once, then benchmark per-frame viewport layout.
    let fixture = generate_10mb_markdown();
    let buf = DocumentBuffer::from_str(&fixture);
    let blocks = parse_to_ir(&fixture);
    let index = ViewportBlockIndex::from_blocks(blocks, 720.0, 1);

    let mut group = c.benchmark_group("viewport_10mb");

    group.bench_function("visible_blocks_lookup_and_layout", |b| {
        let mid_scroll_y = index.total_height * 0.5;
        b.iter(|| {
            let vis = index.visible_range(black_box(mid_scroll_y), black_box(900.0), 160.0);
            let mut rendered_chars = 0usize;
            for idx in vis {
                let blk = &index.blocks[idx].block;
                let _h = estimate_block_height(blk, 720.0);
                match blk {
                    DocBlock::Paragraph { runs, .. } | DocBlock::Heading { runs, .. } => {
                        rendered_chars += runs_to_plain_text(runs).len();
                    }
                    DocBlock::CodeBlock { code, .. } => {
                        rendered_chars += code.len();
                    }
                    DocBlock::Table { header, rows, .. } => {
                        for cell in header {
                            rendered_chars += runs_to_plain_text(cell).len();
                        }
                        for row in rows {
                            for cell in row {
                                rendered_chars += runs_to_plain_text(cell).len();
                            }
                        }
                    }
                    _ => {}
                }
            }
            black_box(rendered_chars);
        });
    });

    group.bench_function("visible_source_rows_45_lines", |b| {
        let mid_line = buf.len_lines() / 2;
        b.iter(|| {
            let mut total_bytes = 0usize;
            for l in mid_line..(mid_line + 45).min(buf.len_lines()) {
                let s = buf.line_text(black_box(l));
                total_bytes += s.len();
            }
            black_box(total_bytes);
        });
    });

    // Keep InlineRun import used in case of refactoring
    let _ = InlineRun::plain("");

    group.finish();
}

fn make_10mb_markdown_doc() -> String {
    let unit = "# Heading Section\n\nThis paragraph has **bold**, *italic*, `code`, and BookmarkTarget words with ID-4201 numbers for realistic Markdown Studio benchmarking.\n\n";
    let target_len = 10 * 1024 * 1024;
    let mut s = String::with_capacity(target_len + unit.len());
    while s.len() < target_len {
        s.push_str(unit);
    }
    s
}

fn bench_word_count_and_selection_10mb(c: &mut criterion::Criterion) {
    let text = make_10mb_markdown_doc();
    let mut buf = markdown_studio::document::DocumentBuffer::from_str(&text);

    let mut group = c.benchmark_group("doc_ops_10mb");
    group.bench_function("word_count_10mb_cached", |b| {
        b.iter(|| std::hint::black_box(buf.word_count()));
    });

    buf.selection_anchor = Some(0);
    buf.cursor_char = buf.len_chars();
    group.bench_function("query_active_format_10mb_ctrl_a", |b| {
        b.iter(|| std::hint::black_box(markdown_studio::document::editing::query_active_format(&buf)));
    });
    group.finish();
}

fn bench_search_10mb(c: &mut criterion::Criterion) {
    use markdown_studio::document::search::{find_all_matches, SearchOptions};

    let text = make_10mb_markdown_doc();
    let buf = markdown_studio::document::DocumentBuffer::from_str(&text);

    let mut group = c.benchmark_group("search_10mb");
    group.sample_size(20);

    let cs_opts = SearchOptions {
        case_sensitive: true,
        whole_word: false,
        use_regex: false,
        selection_scope: None,
    };
    group.bench_function("literal_case_sensitive", |b| {
        b.iter(|| std::hint::black_box(find_all_matches(&buf, "BookmarkTarget", &cs_opts)));
    });

    let ci_opts = SearchOptions {
        case_sensitive: false,
        whole_word: false,
        use_regex: false,
        selection_scope: None,
    };
    group.bench_function("case_insensitive", |b| {
        b.iter(|| std::hint::black_box(find_all_matches(&buf, "bookmarktarget", &ci_opts)));
    });

    let re_opts = SearchOptions {
        case_sensitive: true,
        whole_word: false,
        use_regex: true,
        selection_scope: None,
    };
    group.bench_function("regex", |b| {
        b.iter(|| std::hint::black_box(find_all_matches(&buf, r"ID-\d+", &re_opts)));
    });
    group.finish();
}

fn bench_parse_to_ir_1mb(c: &mut criterion::Criterion) {
    let unit = "## Benchmark Block\n\nParagraph with **strong** and *em* and [link](https://example.com).\n\n| Col A | Col B |\n|---|---|\n| 1 | 2 |\n\n- Item one\n- Item two\n\n";
    let target_len = 1024 * 1024;
    let mut md_1mb = String::with_capacity(target_len + unit.len());
    while md_1mb.len() < target_len {
        md_1mb.push_str(unit);
    }
    let rope_1mb = ropey::Rope::from_str(&md_1mb);

    let mut group = c.benchmark_group("ast_parse_1mb");
    group.sample_size(20);
    group.bench_function("parse_to_ir_1mb", |b| {
        b.iter(|| {
            std::hint::black_box(markdown_studio::document::ast::parse_to_ir(std::hint::black_box(
                &md_1mb,
            )))
        });
    });
    group.bench_function("parse_rope_to_ir_1mb", |b| {
        b.iter(|| {
            std::hint::black_box(markdown_studio::document::ast::parse_rope_to_ir(
                std::hint::black_box(&rope_1mb),
            ))
        });
    });
    group.finish();
}

fn bench_fenwick_viewport_50k_updates(c: &mut criterion::Criterion) {
    use markdown_studio::document::unicode::{BlockAlignment, TextDirection};

    let blocks: Vec<DocBlock> = (0..50_000)
        .map(|i| DocBlock::Paragraph {
            runs: vec![InlineRun::plain("Benchmark line")],
            alignment: BlockAlignment::Start,
            direction: TextDirection::Ltr,
            indent_level: 0,
            source_lines: i..(i + 1),
        })
        .collect();
    let base_idx = ViewportBlockIndex::from_blocks(blocks, 720.0, 1);
    let updates: Vec<(usize, f32)> = (0..32).map(|k| (k * 1500 + 7, 44.0 + k as f32)).collect();

    c.bench_function("fenwick_set_measured_heights_batch_50k", |b| {
        b.iter_batched(
            || base_idx.clone(),
            |mut idx| {
                idx.set_measured_heights_batch(std::hint::black_box(&updates));
                std::hint::black_box((idx.total_height, idx.visible_range(50_000.0, 900.0, 160.0)));
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_math_and_mermaid_layout(c: &mut criterion::Criterion) {
    use markdown_studio::document::math::{layout_math, parse_latex_math};
    use markdown_studio::document::mermaid::{compile_mermaid_scene, DiagramPalette};
    use markdown_studio::ui::theme::{ThemeMode, WordTheme};

    let expr = r"\int_{-\infty}^{\infty} \frac{e^{-x^2}}{\sqrt{\pi}} \, dx = \sum_{n=0}^{\infty} \frac{(-1)^n}{(2n+1)!}";
    let diagram = "flowchart LR\nA[Input Rope] --> B{AST Parser}\nB -->|Fast| C[Fenwick Viewport]\nB -->|Export| D[Vector PDF]\n";
    let pal = DiagramPalette::from_theme(&WordTheme::from_mode(ThemeMode::Light));

    let mut group = c.benchmark_group("math_and_mermaid");
    group.bench_function("parse_and_layout_latex_math", |b| {
        b.iter(|| {
            let ast = parse_latex_math(std::hint::black_box(expr));
            std::hint::black_box(layout_math(&ast, 18.0, true));
        });
    });
    group.bench_function("compile_mermaid_flowchart_scene", |b| {
        b.iter(|| {
            std::hint::black_box(compile_mermaid_scene(
                std::hint::black_box(diagram),
                pal,
            ));
        });
    });
    group.finish();
}

fn bench_bidi_shape_arabic_line(c: &mut criterion::Criterion) {
    use markdown_studio::document::unicode::{shape_bidi_for_display, TextDirection};

    let sample = "مرحبا بالعالم في Rust و Markdown Studio رقم 2026 مع نص عربي متكامل لاختبار التشكيل والربط";
    c.bench_function("bidi_shape_arabic_line", |b| {
        b.iter(|| {
            std::hint::black_box(shape_bidi_for_display(
                std::hint::black_box(sample),
                TextDirection::Rtl,
            ))
        });
    });
}

criterion_group!(
    benches,
    bench_rope_edits_10mb,
    bench_virtualized_viewport_10mb,
    bench_word_count_and_selection_10mb,
    bench_search_10mb,
    bench_parse_to_ir_1mb,
    bench_fenwick_viewport_50k_updates,
    bench_math_and_mermaid_layout,
    bench_bidi_shape_arabic_line
);
criterion_main!(benches);
