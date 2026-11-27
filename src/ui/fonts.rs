use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use egui::FontData;

/// Metadata for an installed OS font family discovered via `fontdb`.
#[derive(Debug, Clone)]
pub struct SystemFontEntry {
    pub family: String,
    pub family_lower: String,
    pub is_monospace: bool,
    pub regular_path: PathBuf,
    pub regular_index: u32,
    pub bold_path: Option<PathBuf>,
    pub bold_index: u32,
}

#[derive(Default)]
struct CandidateFaces {
    is_monospace: bool,
    regular_score: u16,
    regular_path: Option<PathBuf>,
    regular_index: u32,
    bold_score: u16,
    bold_path: Option<PathBuf>,
    bold_index: u32,
}

/// Enumerates all installed TrueType/OpenType/TTC fonts on the system cleanly once per process.
pub fn installed_system_fonts() -> &'static [SystemFontEntry] {
    static CATALOG: OnceLock<Vec<SystemFontEntry>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();

        let mut by_family: HashMap<String, CandidateFaces> = HashMap::new();
        for face in db.faces() {
            if face.style != resvg::usvg::fontdb::Style::Normal {
                continue;
            }
            let path = match &face.source {
                resvg::usvg::fontdb::Source::File(p)
                | resvg::usvg::fontdb::Source::SharedFile(p, _) => p.clone(),
                resvg::usvg::fontdb::Source::Binary(_) => continue,
            };
            let ext_ok = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| {
                    matches!(
                        ext.to_ascii_lowercase().as_str(),
                        "ttf" | "otf" | "ttc" | "otc"
                    )
                });
            if !ext_ok {
                continue;
            }
            let Some((primary_family, _)) = face.families.first() else {
                continue;
            };
            let fam = primary_family.trim();
            if fam.is_empty() || fam.starts_with('.') {
                continue;
            }

            let entry = by_family.entry(fam.to_string()).or_default();
            if face.monospaced {
                entry.is_monospace = true;
            }
            let weight = face.weight.0;
            // Regular score: prefer weight 400 (smaller diff = better, store 1000 - diff).
            let reg_dist = weight.abs_diff(400);
            let reg_score = 1000u16.saturating_sub(reg_dist);
            if weight < 600 && (entry.regular_path.is_none() || reg_score > entry.regular_score) {
                entry.regular_score = reg_score;
                entry.regular_path = Some(path.clone());
                entry.regular_index = face.index;
            }
            // Bold score: prefer weight 700 among weight >= 600.
            let bold_dist = weight.abs_diff(700);
            let bold_score = 1000u16.saturating_sub(bold_dist);
            if weight >= 600 && (entry.bold_path.is_none() || bold_score > entry.bold_score) {
                entry.bold_score = bold_score;
                entry.bold_path = Some(path.clone());
                entry.bold_index = face.index;
            }
        }

        let mut list: Vec<SystemFontEntry> = by_family
            .into_iter()
            .filter_map(|(family, c)| {
                let (regular_path, regular_index) = match c.regular_path {
                    Some(p) => (p, c.regular_index),
                    None => (c.bold_path.clone()?, c.bold_index),
                };
                let family_lower = family.to_lowercase();
                Some(SystemFontEntry {
                    family,
                    family_lower,
                    is_monospace: c.is_monospace,
                    regular_path,
                    regular_index,
                    bold_path: c.bold_path,
                    bold_index: c.bold_index,
                })
            })
            .collect();

        list.sort_by(|a, b| {
            a.family_lower
                .cmp(&b.family_lower)
                .then_with(|| a.family.cmp(&b.family))
        });
        list
    })
}

type FontPair = (Arc<FontData>, Option<Arc<FontData>>);

fn font_pair_cache() -> &'static Mutex<HashMap<String, FontPair>> {
    static CACHE: OnceLock<Mutex<HashMap<String, FontPair>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn read_font_data(path: &std::path::Path, index: u32) -> Option<Arc<FontData>> {
    let bytes = std::fs::read(path).ok()?;
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    Some(Arc::new(data))
}

/// Loads (and caches) regular + optional bold `FontData` for an installed system font family.
pub fn load_system_font_pair(family: &str) -> Option<FontPair> {
    let trimmed = family.trim();
    if trimmed.is_empty() {
        return None;
    }
    let key = trimmed.to_lowercase();
    {
        let guard = font_pair_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pair) = guard.get(&key) {
            return Some((Arc::clone(&pair.0), pair.1.as_ref().map(Arc::clone)));
        }
    }

    let catalog = installed_system_fonts();
    let entry = catalog
        .iter()
        .find(|e| e.family.eq_ignore_ascii_case(trimmed) || e.family_lower == key)?;

    let regular = read_font_data(&entry.regular_path, entry.regular_index)?;
    let bold = entry
        .bold_path
        .as_deref()
        .and_then(|p| read_font_data(p, entry.bold_index));

    let mut guard = font_pair_cache().lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() >= 32 {
        guard.clear();
    }
    guard.insert(key, (Arc::clone(&regular), bold.as_ref().map(Arc::clone)));
    Some((regular, bold))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_system_fonts_enumerates_and_loads_real_font() {
        let fonts = installed_system_fonts();
        if fonts.is_empty() {
            return;
        }
        assert!(
            fonts
                .windows(2)
                .all(|w| w[0].family_lower <= w[1].family_lower),
            "system font list should be sorted alphabetically"
        );
        let first = &fonts[0];
        let loaded = load_system_font_pair(&first.family);
        assert!(
            loaded.is_some(),
            "expected to load bytes for installed font {}",
            first.family
        );
    }
}
