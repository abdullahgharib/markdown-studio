use std::collections::HashMap;
use std::ops::Range;

use crate::document::unicode::{BlockAlignment, TextDirection};

/// Horizontal alignment for a GFM table column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableAlign {
    #[default]
    None,
    Left,
    Center,
    Right,
}

/// A styled inline text span within a paragraph, heading, or table cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub link_url: Option<String>,
    pub image_url: Option<String>,
    pub is_footnote_ref: bool,
    pub is_math: bool,
}

impl InlineRun {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            code: false,
            link_url: None,
            image_url: None,
            is_footnote_ref: false,
            is_math: false,
        }
    }

    pub fn math(latex: impl Into<String>) -> Self {
        Self {
            text: latex.into(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            code: false,
            link_url: None,
            image_url: None,
            is_footnote_ref: false,
            is_math: true,
        }
    }
}

/// A single item inside a bulleted, numbered, or task list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    /// `None` for standard list items, `Some(false)` for `[ ]`, `Some(true)` for `[x]`.
    pub checked: Option<bool>,
    pub blocks: Vec<DocBlock>,
}

/// Top-level or nested block element extracted from a `comrak` AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocBlock {
    Heading {
        level: u8,
        runs: Vec<InlineRun>,
        alignment: BlockAlignment,
        indent_level: u8,
        direction: TextDirection,
        source_lines: Range<usize>,
    },
    Paragraph {
        runs: Vec<InlineRun>,
        alignment: BlockAlignment,
        indent_level: u8,
        direction: TextDirection,
        source_lines: Range<usize>,
    },
    BlockQuote {
        blocks: Vec<DocBlock>,
        indent_level: u8,
        direction: TextDirection,
        source_lines: Range<usize>,
    },
    CodeBlock {
        info: String,
        code: String,
        source_lines: Range<usize>,
    },
    List {
        ordered: bool,
        start: usize,
        items: Vec<ListItem>,
        indent_level: u8,
        direction: TextDirection,
        source_lines: Range<usize>,
    },
    Table {
        alignments: Vec<TableAlign>,
        header: Vec<Vec<InlineRun>>,
        rows: Vec<Vec<Vec<InlineRun>>>,
        source_lines: Range<usize>,
    },
    ThematicBreak {
        source_lines: Range<usize>,
    },
    FootnoteDefinition {
        name: String,
        blocks: Vec<DocBlock>,
        source_lines: Range<usize>,
    },
}

impl DocBlock {
    /// Returns the 0-based `[start, end)` source line range in the underlying document.
    pub fn source_lines(&self) -> Range<usize> {
        match self {
            DocBlock::Heading { source_lines, .. }
            | DocBlock::Paragraph { source_lines, .. }
            | DocBlock::BlockQuote { source_lines, .. }
            | DocBlock::CodeBlock { source_lines, .. }
            | DocBlock::List { source_lines, .. }
            | DocBlock::Table { source_lines, .. }
            | DocBlock::ThematicBreak { source_lines }
            | DocBlock::FootnoteDefinition { source_lines, .. } => source_lines.clone(),
        }
    }
}

/// Joins inline runs into a plain UTF-8 string (useful for height estimation and search).
pub fn runs_to_plain_text(runs: &[InlineRun]) -> String {
    let total_len: usize = runs.iter().map(|r| r.text.len()).sum();
    let mut out = String::with_capacity(total_len);
    for r in runs {
        out.push_str(&r.text);
    }
    out
}

/// Generates a GitHub-style heading slug (`#architecture--data-flow`) for internal anchor links.
pub fn heading_slug(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for ch in title.trim().chars() {
        if ch == ' ' {
            slug.push('-');
        } else if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            for lc in ch.to_lowercase() {
                slug.push(lc);
            }
        }
    }
    slug
}

/// Deduplicates heading anchor slugs across a document (`intro`, `intro-1`, `intro-2`, ...).
#[derive(Debug, Default, Clone)]
pub struct SlugTracker {
    counts: HashMap<String, usize>,
}

impl SlugTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_slug(&mut self, title: &str) -> String {
        let base = heading_slug(title);
        let count = self.counts.entry(base.clone()).or_insert(0);
        let slug = if *count == 0 {
            base
        } else {
            format!("{base}-{}", *count)
        };
        *count += 1;
        slug
    }
}
