use std::hash::Hash;
use std::ops::Range;
use std::sync::{Arc, Weak};

use rustc_hash::{FxHashMap, FxHasher};

use super::LinkTarget;
use crate::export::ir::InlineRun;
use crate::ui::settings::LineSpacing;
use crate::ui::theme::ThemeMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NavTarget {
    Link(String, LinkTarget),
    Footnote(String),
}

#[derive(Debug, Clone)]
pub(super) struct GalleyLinkSpan {
    pub char_range: Range<usize>,
    pub target: NavTarget,
}

pub(super) type EvaluatedTableGrid = (Vec<Vec<String>>, Vec<Vec<bool>>);

#[derive(Debug, Clone)]
pub(super) struct CachedMathPreview {
    pub text: String,
    pub result: Option<String>,
    pub last_tick: u64,
}

#[derive(Debug, Clone)]
pub(super) struct CachedGalley {
    pub galley: Arc<egui::Galley>,
    pub link_spans: Arc<Vec<GalleyLinkSpan>>,
    pub last_tick: u64,
}

pub(super) fn evict_oldest_half<K, V, F>(
    entries: &mut FxHashMap<K, V>,
    max_entries: usize,
    tick_of: F,
) where
    K: Eq + Hash,
    F: Fn(&V) -> u64,
{
    if entries.len() < max_entries {
        return;
    }
    let mut ticks: Vec<u64> = entries.values().map(&tick_of).collect();
    let mid = ticks.len() / 2;
    let (_, cutoff, _) = ticks.select_nth_unstable(mid);
    let cutoff_val = *cutoff;
    entries.retain(|_, v| tick_of(v) > cutoff_val);
    if entries.len() >= max_entries {
        entries.clear();
    }
}

#[derive(Debug, Clone)]
struct CachedTableEval {
    value: Option<Arc<EvaluatedTableGrid>>,
    last_tick: u64,
}

#[derive(Debug, Default)]
pub(super) struct TableEvalCache {
    tick: u64,
    entries: FxHashMap<usize, CachedTableEval>,
}

fn adaptive_cache_cap(per_gb: u64, min_cap: usize, max_cap: usize) -> usize {
    let ram_gb = (crate::document::system_total_memory_bytes() / (1024 * 1024 * 1024)).max(1);
    (ram_gb.saturating_mul(per_gb) as usize).clamp(min_cap, max_cap)
}

impl TableEvalCache {
    pub(super) fn max_entries() -> usize {
        adaptive_cache_cap(64, 256, 2_048)
    }

    pub(super) fn get(&mut self, start_line: usize) -> Option<Option<Arc<EvaluatedTableGrid>>> {
        let entry = self.entries.get_mut(&start_line)?;
        self.tick = self.tick.wrapping_add(1);
        entry.last_tick = self.tick;
        Some(entry.value.clone())
    }

    pub(super) fn insert_bounded(
        &mut self,
        start_line: usize,
        value: Option<Arc<EvaluatedTableGrid>>,
    ) {
        if !self.entries.contains_key(&start_line) {
            evict_oldest_half(&mut self.entries, Self::max_entries(), |e| e.last_tick);
        }
        self.tick = self.tick.wrapping_add(1);
        self.entries.insert(
            start_line,
            CachedTableEval {
                value,
                last_tick: self.tick,
            },
        );
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, Default)]
pub(super) struct ReaderGalleyCache {
    pub epoch: (u64, ThemeMode, LineSpacing, bool, u32, u64),
    pub atlas: Weak<egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>>,
    tick: u64,
    pub entries: FxHashMap<(usize, usize, u64), CachedGalley>,
}

pub(super) fn hash_inline_runs(runs: &[InlineRun], hasher: &mut FxHasher) {
    runs.len().hash(hasher);
    for r in runs {
        r.text.hash(hasher);
        r.bold.hash(hasher);
        r.italic.hash(hasher);
        r.underline.hash(hasher);
        r.strikethrough.hash(hasher);
        r.code.hash(hasher);
        r.is_math.hash(hasher);
        r.link_url.hash(hasher);
        r.image_url.hash(hasher);
        r.is_footnote_ref.hash(hasher);
    }
}

impl ReaderGalleyCache {
    pub(super) fn max_entries() -> usize {
        adaptive_cache_cap(512, 1_024, 8_192)
    }

    pub(super) fn get(&mut self, key: &(usize, usize, u64)) -> Option<CachedGalley> {
        self.tick = self.tick.wrapping_add(1);
        let tick = self.tick;
        let entry = self.entries.get_mut(key)?;
        entry.last_tick = tick;
        Some(entry.clone())
    }

    pub(super) fn insert_bounded(
        &mut self,
        key: (usize, usize, u64),
        galley: Arc<egui::Galley>,
        link_spans: Arc<Vec<GalleyLinkSpan>>,
    ) -> CachedGalley {
        if !self.entries.contains_key(&key) {
            evict_oldest_half(&mut self.entries, Self::max_entries(), |e| e.last_tick);
        }
        self.tick = self.tick.wrapping_add(1);
        let entry = CachedGalley {
            galley,
            link_spans,
            last_tick: self.tick,
        };
        self.entries.insert(key, entry.clone());
        entry
    }
}
