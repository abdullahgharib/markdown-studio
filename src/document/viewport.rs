use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use ropey::Rope;

use crate::document::ast::{
    is_math_code_info, is_mermaid_code_info, parse_rope_to_ir_cancellable, parse_to_ir,
};
use crate::export::ir::DocBlock;

const BLOCK_VERTICAL_GAP: f32 = 10.0;
const DOCUMENT_TOP_PAD: f32 = 16.0;

#[derive(Debug, Clone, Default)]
struct FenwickTree {
    bit: Vec<f64>,
}

impl FenwickTree {
    fn from_strides(strides: &[f64]) -> Self {
        let n = strides.len();
        let mut bit = vec![0.0_f64; n + 1];
        for (i, &val) in strides.iter().enumerate() {
            let one = i + 1;
            bit[one] += val;
            let parent = one + one.isolate_lowest_one();
            if parent <= n {
                bit[parent] += bit[one];
            }
        }
        Self { bit }
    }

    #[inline]
    fn len(&self) -> usize {
        self.bit.len().saturating_sub(1)
    }

    #[inline]
    fn add(&mut self, idx: usize, delta: f64) {
        let mut i = idx + 1;
        let n = self.bit.len();
        while i < n {
            self.bit[i] += delta;
            i += i.isolate_lowest_one();
        }
    }

    #[inline]
    fn prefix_sum(&self, count: usize) -> f64 {
        let mut i = count.min(self.len());
        let mut sum = 0.0_f64;
        while i > 0 {
            sum += self.bit[i];
            i -= i.isolate_lowest_one();
        }
        sum
    }

    #[inline]
    fn total(&self) -> f64 {
        self.prefix_sum(self.len())
    }

    fn count_prefix_lt(&self, target: f64) -> usize {
        if target <= 0.0 {
            return 0;
        }
        let n = self.len();
        if n == 0 {
            return 0;
        }
        let mut pos = 0usize;
        let mut acc = 0.0_f64;
        let mut step = 1usize << (usize::BITS - 1 - n.leading_zeros());
        while step > 0 {
            let next = pos + step;
            if next <= n && acc + self.bit[next] < target {
                acc += self.bit[next];
                pos = next;
            }
            step >>= 1;
        }
        pos
    }

    fn count_prefix_le(&self, target: f64) -> usize {
        if target < 0.0 {
            return 0;
        }
        let n = self.len();
        if n == 0 {
            return 0;
        }
        let mut pos = 0usize;
        let mut acc = 0.0_f64;
        let mut step = 1usize << (usize::BITS - 1 - n.leading_zeros());
        while step > 0 {
            let next = pos + step;
            if next <= n && acc + self.bit[next] <= target {
                acc += self.bit[next];
                pos = next;
            }
            step >>= 1;
        }
        pos
    }
}

/// A single block positioned vertically in a virtualized scrollable document.
#[derive(Debug, Clone)]
pub struct IndexedBlock {
    pub block: DocBlock,
    pub y_start: f32,
    pub height: f32,
}

/// Virtualized layout index supporting O(log B) visible-slice lookup and height updates over multi-megabyte documents.
#[derive(Debug, Clone)]
pub struct ViewportBlockIndex {
    pub blocks: Vec<IndexedBlock>,
    pub total_height: f32,
    pub content_width: f32,
    pub version: u64,
    pub doc_id: u64,
    stride_tree: FenwickTree,
    non_virtual_y_override: bool,
}

impl Default for ViewportBlockIndex {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            total_height: 0.0,
            content_width: 720.0,
            version: 0,
            doc_id: 0,
            stride_tree: FenwickTree::default(),
            non_virtual_y_override: false,
        }
    }
}

impl ViewportBlockIndex {
    /// Creates an empty `ViewportBlockIndex` tagged with `doc_id`.
    pub fn empty_for_doc(doc_id: u64) -> Self {
        Self {
            doc_id,
            ..Self::default()
        }
    }

    /// Builds a virtualized block index from parsed IR blocks using fast heuristic heights.
    pub fn from_blocks(blocks: Vec<DocBlock>, content_width: f32, version: u64) -> Self {
        Self::from_blocks_with_doc(blocks, content_width, version, 0)
    }

    /// Builds a virtualized block index tagged with the originating document session id.
    pub fn from_blocks_with_doc(
        blocks: Vec<DocBlock>,
        content_width: f32,
        version: u64,
        doc_id: u64,
    ) -> Self {
        let width = content_width.max(240.0);
        let mut indexed = Vec::with_capacity(blocks.len());
        let mut strides = Vec::with_capacity(blocks.len());
        let mut y = DOCUMENT_TOP_PAD as f64;
        for block in blocks {
            let h = estimate_block_height(&block, width).max(0.0);
            let stride = (h + BLOCK_VERTICAL_GAP) as f64;
            indexed.push(IndexedBlock {
                block,
                y_start: y as f32,
                height: h,
            });
            strides.push(stride);
            y += stride;
        }
        let stride_tree = FenwickTree::from_strides(&strides);
        Self {
            blocks: indexed,
            total_height: (y as f32).max(100.0),
            content_width: width,
            version,
            doc_id,
            stride_tree,
            non_virtual_y_override: false,
        }
    }

    /// Returns the live vertical `y_start` offset of block `idx` in O(log B) time.
    #[inline]
    pub fn block_y_start(&self, idx: usize) -> f32 {
        if self.blocks.is_empty() {
            return DOCUMENT_TOP_PAD;
        }
        if idx >= self.blocks.len() {
            return self.total_height;
        }
        if self.non_virtual_y_override {
            return self.blocks[idx].y_start;
        }
        DOCUMENT_TOP_PAD + self.stride_tree.prefix_sum(idx) as f32
    }

    /// Returns the `[start, end)` block indices intersecting the viewport `[scroll_y - overscan, scroll_y + viewport_h + overscan]` in O(log B) time.
    pub fn visible_range(
        &self,
        scroll_y: f32,
        viewport_height: f32,
        overscan_px: f32,
    ) -> Range<usize> {
        let n = self.blocks.len();
        if n == 0 {
            return 0..0;
        }
        let top = (scroll_y - overscan_px).max(0.0);
        let bottom = scroll_y + viewport_height.max(1.0) + overscan_px;

        if self.non_virtual_y_override {
            let start = self
                .blocks
                .partition_point(|b| b.y_start + b.height < top)
                .min(n);
            let end = self.blocks[start..]
                .partition_point(|b| b.y_start <= bottom)
                .saturating_add(start)
                .min(n);
            return start..end;
        }

        // Block i ends at DOCUMENT_TOP_PAD + prefix_sum(i+1) - BLOCK_VERTICAL_GAP.
        let end_threshold = (top - (DOCUMENT_TOP_PAD - BLOCK_VERTICAL_GAP)) as f64;
        let start = if end_threshold <= 0.0 {
            0
        } else {
            self.stride_tree.count_prefix_lt(end_threshold).min(n)
        };

        // Block i starts at DOCUMENT_TOP_PAD + prefix_sum(i).
        let start_threshold = (bottom - DOCUMENT_TOP_PAD) as f64;
        let end = if start_threshold < 0.0 {
            0
        } else {
            let m = self.stride_tree.count_prefix_le(start_threshold);
            if m >= n {
                n
            } else {
                m + 1
            }
        }
        .max(start);

        start..end
    }

    /// Updates a single block's measured height in O(log B) time.
    pub fn set_measured_height(&mut self, idx: usize, measured: f32) {
        if idx >= self.blocks.len() {
            return;
        }
        let clamped = measured.max(0.0);
        let delta = clamped - self.blocks[idx].height;
        if delta.abs() < 1.0 {
            return;
        }
        self.blocks[idx].height = clamped;
        if self.non_virtual_y_override {
            for b in &mut self.blocks[idx + 1..] {
                b.y_start += delta;
            }
            self.total_height = (self.total_height + delta).max(100.0);
            return;
        }
        self.stride_tree.add(idx, delta as f64);
        self.total_height = (DOCUMENT_TOP_PAD + self.stride_tree.total() as f32).max(100.0);
    }

    /// Applies multiple measured block height updates in O(K log B) time (where K = `updates.len()`).
    pub fn set_measured_heights_batch(&mut self, updates: &[(usize, f32)]) {
        if updates.is_empty() || self.blocks.is_empty() {
            return;
        }
        let n = self.blocks.len();
        let any_changed = updates.iter().any(|&(idx, measured)| {
            idx < n && (measured.max(0.0) - self.blocks[idx].height).abs() >= 1.0
        });
        if !any_changed {
            return;
        }

        if !self.non_virtual_y_override {
            let strictly_sorted_valid = updates.windows(2).all(|w| w[0].0 < w[1].0)
                && updates.last().is_some_and(|w| w.0 < n);
            if strictly_sorted_valid {
                for &(idx, measured) in updates {
                    let latest = measured.max(0.0);
                    let delta = latest - self.blocks[idx].height;
                    if delta.abs() >= 1.0 {
                        self.blocks[idx].height = latest;
                        self.stride_tree.add(idx, delta as f64);
                    }
                }
                self.total_height =
                    (DOCUMENT_TOP_PAD + self.stride_tree.total() as f32).max(100.0);
                return;
            }
        }

        if self.non_virtual_y_override {
            let mut sorted: Vec<(usize, f32)> = updates
                .iter()
                .copied()
                .filter(|&(idx, _)| idx < n)
                .collect();
            sorted.sort_by_key(|&(idx, _)| idx);
            let mut running_delta = 0.0_f32;
            let mut cursor = 0usize;
            let first_idx = sorted[0].0;
            for (i, block) in self.blocks.iter_mut().enumerate().skip(first_idx) {
                block.y_start += running_delta;
                let mut latest_measured = None;
                while cursor < sorted.len() && sorted[cursor].0 == i {
                    latest_measured = Some(sorted[cursor].1.max(0.0));
                    cursor += 1;
                }
                if let Some(measured) = latest_measured {
                    let delta = measured - block.height;
                    if delta.abs() >= 1.0 {
                        block.height = measured;
                        running_delta += delta;
                    }
                }
            }
            if running_delta.abs() > 0.0 {
                self.total_height = (self.total_height + running_delta).max(100.0);
            }
            return;
        }

        let mut sorted: Vec<(usize, f32)> = updates
            .iter()
            .copied()
            .filter(|&(idx, _)| idx < n)
            .collect();
        sorted.sort_by_key(|&(idx, _)| idx);

        let mut i = 0usize;
        while i < sorted.len() {
            let idx = sorted[i].0;
            let mut latest = sorted[i].1.max(0.0);
            i += 1;
            while i < sorted.len() && sorted[i].0 == idx {
                latest = sorted[i].1.max(0.0);
                i += 1;
            }
            let delta = latest - self.blocks[idx].height;
            if delta.abs() >= 1.0 {
                self.blocks[idx].height = latest;
                self.stride_tree.add(idx, delta as f64);
            }
        }
        self.total_height = (DOCUMENT_TOP_PAD + self.stride_tree.total() as f32).max(100.0);
    }

    /// Synchronizes exact measured `(idx, y_start, height)` values when rendering small non-virtualized documents (`<= 48` blocks).
    pub fn sync_non_virtual_positions(&mut self, updates: &[(usize, f32, f32)]) {
        if updates.is_empty() || self.blocks.is_empty() {
            return;
        }
        for &(idx, y_start, measured) in updates {
            if let Some(b) = self.blocks.get_mut(idx) {
                b.y_start = y_start;
                b.height = measured.max(0.0);
            }
        }
        self.non_virtual_y_override = true;
        if let Some(last) = self.blocks.last() {
            self.total_height = (last.y_start + last.height).max(100.0);
        }
    }

    /// Finds the block index nearest to a 0-based source line in O(log B) time.
    pub fn block_for_source_line(&self, line_idx: usize) -> Option<usize> {
        if self.blocks.is_empty() {
            return None;
        }
        let pos = self
            .blocks
            .partition_point(|b| b.block.source_lines().end <= line_idx);
        Some(pos.min(self.blocks.len() - 1))
    }
}

/// Fast, allocation-free heuristic height estimation for a `DocBlock` at `content_width`.
pub fn estimate_block_height(block: &DocBlock, content_width: f32) -> f32 {
    let approx_chars_per_line = (content_width / 8.2).max(20.0);
    match block {
        DocBlock::Heading { level, runs, .. } => {
            let text_len = runs.iter().map(|r| r.text.len()).sum::<usize>().max(1) as f32;
            let font_px = match level {
                1 => 28.0,
                2 => 23.0,
                3 => 19.0,
                4 => 17.0,
                _ => 15.0,
            };
            let chars_per_line = (content_width / (font_px * 0.54)).max(12.0);
            let lines = (text_len / chars_per_line).ceil().max(1.0);
            lines * (font_px * 1.35) + 10.0
        }
        DocBlock::Paragraph { runs, .. } => {
            let text_len = runs.iter().map(|r| r.text.len()).sum::<usize>().max(1) as f32;
            let lines = (text_len / approx_chars_per_line).ceil().max(1.0);
            lines * 22.0 + 4.0
        }
        DocBlock::BlockQuote { blocks, .. } => {
            let inner: f32 = blocks
                .iter()
                .map(|b| estimate_block_height(b, (content_width - 24.0).max(120.0)) + 6.0)
                .sum();
            inner.max(26.0) + 8.0
        }
        DocBlock::CodeBlock { info, code, .. } => {
            let nl_count = memchr::memchr_iter(b'\n', code.as_bytes()).count();
            let trailing = if code.is_empty() || code.ends_with('\n') {
                0
            } else {
                1
            };
            let line_count = (nl_count + trailing).max(1) as f32;
            if is_mermaid_code_info(info) {
                (160.0 + line_count * 18.0).clamp(220.0, 360.0)
            } else if is_math_code_info(info) {
                (72.0 + line_count * 28.0).min(420.0)
            } else {
                line_count * 19.0 + 24.0
            }
        }
        DocBlock::List { items, .. } => {
            let mut sum = 0.0_f32;
            for item in items {
                let item_h: f32 = item
                    .blocks
                    .iter()
                    .map(|b| estimate_block_height(b, (content_width - 28.0).max(140.0)))
                    .sum();
                sum += item_h.max(22.0) + 4.0;
            }
            sum.max(24.0)
        }
        DocBlock::Table { header, rows, .. } => {
            let cols = header.len().max(1);
            let col_w = (content_width / cols as f32).max(60.0);
            let chars_per_cell_line = (col_w / 8.0).max(8.0);
            let mut h = 34.0_f32;
            for row in rows {
                    let max_lines = row
                    .iter()
                    .map(|cell| {
                        let len = cell.iter().map(|r| r.text.len()).sum::<usize>().max(1) as f32;
                        (len / chars_per_cell_line).ceil().max(1.0)
                    })
                    .fold(1.0_f32, f32::max);
                h += max_lines * 21.0 + 10.0;
            }
            h + 8.0
        }
        DocBlock::ThematicBreak { .. } => 18.0,
        DocBlock::FootnoteDefinition { blocks, .. } => {
            let inner: f32 = blocks
                .iter()
                .map(|b| estimate_block_height(b, (content_width - 28.0).max(140.0)) + 4.0)
                .sum();
            inner.max(24.0) + 6.0
        }
    }
}

/// Parses only a small bounded window of a large `Rope` for instant local preview while full AST builds.
pub fn parse_rope_line_window(rope: &Rope, start_line: usize, line_count: usize) -> Vec<DocBlock> {
    const MAX_LOCAL_PREVIEW_BYTES: usize = 256 * 1024;
    let total = rope.len_lines();
    if total == 0 {
        return Vec::new();
    }
    let s = start_line.min(total - 1);
    let e = (s + line_count).min(total);
    let c_start = rope.line_to_char(s);
    let mut c_end = if e >= total {
        rope.len_chars()
    } else {
        rope.line_to_char(e)
    };
    let b_start = rope.char_to_byte(c_start);
    let b_end = rope.char_to_byte(c_end);
    if b_end.saturating_sub(b_start) > MAX_LOCAL_PREVIEW_BYTES {
        let capped_byte = (b_start + MAX_LOCAL_PREVIEW_BYTES).min(rope.len_bytes());
        c_end = rope.byte_to_char(capped_byte).max(c_start);
    }
    let slice_str = rope.slice(c_start..c_end).to_string();
    parse_to_ir(&slice_str)
}

struct ParseRequest {
    rope: Rope,
    content_width: f32,
    version: u64,
    doc_id: u64,
    epoch: u64,
}

#[derive(Default)]
struct ParseMailboxInner {
    pending: Option<ParseRequest>,
    latest_result: Option<ViewportBlockIndex>,
    closed: bool,
}

struct SharedParseMailbox {
    state: Mutex<ParseMailboxInner>,
    cv: Condvar,
    cancel_epoch: Arc<AtomicU64>,
    worker_alive: AtomicBool,
}

/// Non-blocking, latest-wins cancellable background `comrak` parser that keeps the 120 FPS UI thread stutter-free.
pub struct BackgroundAstParser {
    mailbox: Arc<SharedParseMailbox>,
}

impl Default for BackgroundAstParser {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for BackgroundAstParser {
    fn drop(&mut self) {
        self.mailbox.cancel_epoch.fetch_add(1, Ordering::Release);
        if let Ok(mut guard) = self.mailbox.state.lock() {
            guard.closed = true;
            guard.pending = None;
        }
        self.mailbox.cv.notify_all();
    }
}

impl BackgroundAstParser {
    pub fn new() -> Self {
        Self::spawn_with_ctx(None)
    }

    pub fn with_repaint_ctx(ctx: egui::Context) -> Self {
        Self::spawn_with_ctx(Some(ctx))
    }

    fn spawn_with_ctx(repaint_ctx: Option<egui::Context>) -> Self {
        let mailbox = Arc::new(SharedParseMailbox {
            state: Mutex::new(ParseMailboxInner::default()),
            cv: Condvar::new(),
            cancel_epoch: Arc::new(AtomicU64::new(0)),
            worker_alive: AtomicBool::new(true),
        });
        let worker_box = Arc::clone(&mailbox);

        if thread::Builder::new()
            .name("md-ast-parser".to_string())
            .spawn(move || {
                loop {
                    let req = {
                        let Ok(mut guard) = worker_box.state.lock() else {
                            break;
                        };
                        while guard.pending.is_none() && !guard.closed {
                            let Ok(next) = worker_box.cv.wait(guard) else {
                                return;
                            };
                            guard = next;
                        }
                        if guard.closed {
                            break;
                        }
                        guard.pending.take()
                    };
                    let Some(req) = req else {
                        continue;
                    };
                    let Some(blocks) = parse_rope_to_ir_cancellable(
                        &req.rope,
                        Some((&worker_box.cancel_epoch, req.epoch)),
                    ) else {
                        continue;
                    };
                    if worker_box.cancel_epoch.load(Ordering::Acquire) != req.epoch {
                        continue;
                    }
                    let index = ViewportBlockIndex::from_blocks_with_doc(
                        blocks,
                        req.content_width,
                        req.version,
                        req.doc_id,
                    );
                    if let Ok(mut guard) = worker_box.state.lock() {
                        if guard.closed {
                            break;
                        }
                        guard.latest_result = Some(index);
                    }
                    if let Some(ref c) = repaint_ctx {
                        c.request_repaint();
                    }
                }
                worker_box.worker_alive.store(false, Ordering::Release);
            })
            .is_err()
        {
            mailbox.worker_alive.store(false, Ordering::Release);
        }

        Self { mailbox }
    }

    /// Queues an O(1) cloned `Rope` snapshot for background AST parsing, cancelling stale work.
    pub fn request_parse(&self, rope: Rope, content_width: f32, version: u64) {
        self.request_parse_for_doc(rope, content_width, version, 0);
    }

    /// Queues an O(1) cloned `Rope` snapshot tagged with a document session id, cancelling stale work.
    pub fn request_parse_for_doc(
        &self,
        rope: Rope,
        content_width: f32,
        version: u64,
        doc_id: u64,
    ) {
        let epoch = self
            .mailbox
            .cancel_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let req = ParseRequest {
            rope,
            content_width,
            version,
            doc_id,
            epoch,
        };
        if !self.mailbox.worker_alive.load(Ordering::Acquire) {
            if let Some(blocks) = parse_rope_to_ir_cancellable(&req.rope, None) {
                let index = ViewportBlockIndex::from_blocks_with_doc(
                    blocks,
                    req.content_width,
                    req.version,
                    req.doc_id,
                );
                if let Ok(mut guard) = self.mailbox.state.lock() {
                    guard.latest_result = Some(index);
                }
            }
            return;
        }
        if let Ok(mut guard) = self.mailbox.state.lock()
            && !guard.closed
        {
            guard.pending = Some(req);
            self.mailbox.cv.notify_one();
        }
    }

    /// Polls for the most recently completed `ViewportBlockIndex` without blocking.
    pub fn poll_latest(&self) -> Option<ViewportBlockIndex> {
        let mut guard = self.mailbox.state.lock().ok()?;
        guard.latest_result.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::unicode::{BlockAlignment, TextDirection};
    use crate::export::ir::InlineRun;

    fn make_dummy_blocks(count: usize) -> Vec<DocBlock> {
        (0..count)
            .map(|i| DocBlock::Paragraph {
                runs: vec![InlineRun::plain(format!("Paragraph {i}"))],
                alignment: BlockAlignment::Start,
                direction: TextDirection::Ltr,
                indent_level: 0,
                source_lines: i..(i + 1),
            })
            .collect()
    }

    #[test]
    fn fenwick_viewport_index_handles_10k_blocks_and_updates_accurately() {
        let n = 10_000usize;
        let blocks = make_dummy_blocks(n);
        let mut idx = ViewportBlockIndex::from_blocks(blocks, 720.0, 1);

        // Verify initial block_y_start matches precomputed y_start for every block
        for i in [0, 1, 7, 511, 4096, n - 1] {
            assert!(
                (idx.block_y_start(i) - idx.blocks[i].y_start).abs() < 0.01,
                "initial block_y_start mismatch at {i}"
            );
        }

        // Single height mutation at index 500
        let old_h = idx.blocks[500].height;
        idx.set_measured_height(500, old_h + 100.0);
        assert!(
            (idx.block_y_start(501) - (idx.blocks[501].y_start + 100.0)).abs() < 0.01,
            "subsequent block_y_start must shift by +100"
        );
        assert!(
            (idx.block_y_start(499) - idx.blocks[499].y_start).abs() < 0.01,
            "preceding block_y_start must stay unchanged"
        );

        // Batch height mutations including duplicate indices (last wins)
        idx.set_measured_heights_batch(&[(10, 50.0), (10, 80.0), (9999, 120.0)]);
        let expected_total: f32 =
            DOCUMENT_TOP_PAD + idx.blocks.iter().map(|b| b.height + BLOCK_VERTICAL_GAP).sum::<f32>();
        assert!((idx.total_height - expected_total).abs() < 0.1);
        assert!(
            (idx.block_y_start(n - 1) + idx.blocks[n - 1].height + BLOCK_VERTICAL_GAP
                - idx.total_height)
                .abs()
                < 0.1
        );

        // Verify visible_range binary lifting matches linear reference across sample scroll positions
        for &scroll_y in &[0.0_f32, 15.0, 500.0, idx.total_height * 0.5, idx.total_height - 400.0] {
            let vis = idx.visible_range(scroll_y, 600.0, 120.0);
            let top = (scroll_y - 120.0).max(0.0);
            let bottom = scroll_y + 600.0 + 120.0;
            let expected_start = (0..n)
                .position(|i| idx.block_y_start(i) + idx.blocks[i].height >= top)
                .unwrap_or(n);
            let expected_end = (expected_start..n)
                .find(|&i| idx.block_y_start(i) > bottom)
                .unwrap_or(n);
            assert_eq!(vis, expected_start..expected_end, "scroll_y={scroll_y}");
        }
    }
}
