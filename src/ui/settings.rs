use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::ui::theme::ThemeMode;
use crate::ui::toolbar::ViewMode;

/// Reading page measure preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum PageWidth {
    Narrow,
    #[default]
    Standard,
    Wide,
}

impl PageWidth {
    pub fn max_px(self) -> f32 {
        match self {
            PageWidth::Narrow => 680.0,
            PageWidth::Standard => 820.0,
            PageWidth::Wide => 1040.0,
        }
    }
}

/// Editorial line height & vertical rhythm preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum LineSpacing {
    Compact,
    #[default]
    Comfortable,
    Relaxed,
}

impl LineSpacing {
    pub fn body_multiplier(self) -> f32 {
        match self {
            LineSpacing::Compact => 1.28,
            LineSpacing::Comfortable => 1.52,
            LineSpacing::Relaxed => 1.74,
        }
    }

    pub fn block_gap(self) -> f32 {
        match self {
            LineSpacing::Compact => 8.0,
            LineSpacing::Comfortable => 11.5,
            LineSpacing::Relaxed => 15.5,
        }
    }
}

/// Font family preference for Editor or Reader/WYSIWYG surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum FontPreference {
    #[default]
    SystemSans,
    Serif,
    Monospace,
}

impl FontPreference {
    pub fn as_str(self) -> &'static str {
        match self {
            FontPreference::SystemSans => "sans",
            FontPreference::Serif => "serif",
            FontPreference::Monospace => "mono",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sans" | "system" | "system_sans" => Some(FontPreference::SystemSans),
            "serif" => Some(FontPreference::Serif),
            "mono" | "monospace" => Some(FontPreference::Monospace),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FontPreference::SystemSans => "System Sans",
            FontPreference::Serif => "Serif",
            FontPreference::Monospace => "Monospace",
        }
    }

    pub fn to_font_family(self) -> egui::FontFamily {
        match self {
            FontPreference::SystemSans => egui::FontFamily::Proportional,
            FontPreference::Serif => egui::FontFamily::Name("serif".into()),
            FontPreference::Monospace => egui::FontFamily::Monospace,
        }
    }

    pub fn to_bold_font_family(self) -> egui::FontFamily {
        match self {
            FontPreference::SystemSans => egui::FontFamily::Name("bold".into()),
            FontPreference::Serif => egui::FontFamily::Name("serif_bold".into()),
            FontPreference::Monospace => egui::FontFamily::Monospace,
        }
    }

    pub fn resolved_font_family(self, use_custom: bool) -> egui::FontFamily {
        if use_custom {
            egui::FontFamily::Name("custom_reader".into())
        } else {
            self.to_font_family()
        }
    }

    pub fn resolved_bold_font_family(self, use_custom: bool) -> egui::FontFamily {
        if use_custom {
            egui::FontFamily::Name("custom_reader_bold".into())
        } else {
            self.to_bold_font_family()
        }
    }
}

/// Persisted user preferences stored locally in JSON format.
#[derive(Debug, Clone, PartialEq)]
pub struct AppSettings {
    pub dark_mode: bool,
    pub theme_mode: ThemeMode,
    pub page_width: PageWidth,
    pub line_spacing: LineSpacing,
    pub view_mode: ViewMode,
    pub show_markup: bool,
    pub outline_open: bool,
    pub split_ratio: f32,
    pub sync_scroll: bool,
    pub zoom_factor: f32,
    pub ui_font_size: f32,
    pub ui_font_name: Option<String>,
    pub editor_font_family: FontPreference,
    pub editor_font_size: f32,
    pub editor_font_name: Option<String>,
    pub reader_font_family: FontPreference,
    pub reader_font_size: f32,
    pub reader_font_name: Option<String>,
    pub allow_external_links: bool,
    pub allow_local_file_links: bool,
    pub allow_executable_links: bool,
    pub allow_unc_network_links: bool,
    pub allow_ntfs_ads_links: bool,
    pub allow_javascript_uri: bool,
    pub allow_vbscript_uri: bool,
    pub allow_data_uri: bool,
    pub allow_remote_images: bool,
    pub recent_files: Vec<PathBuf>,
    pub explorer_root: Option<PathBuf>,
    pub custom_exe_path: Option<PathBuf>,
    pub auto_sync_exe_path: bool,
    pub check_updates_on_startup: bool,
    pub skipped_update_version: Option<String>,
    pub auto_calc_math: bool,
    pub auto_calc_tables: bool,
    pub word_goal: usize,
    pub char_goal: usize,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            dark_mode: false,
            theme_mode: ThemeMode::Light,
            page_width: PageWidth::Standard,
            line_spacing: LineSpacing::Comfortable,
            view_mode: ViewMode::Split,
            show_markup: false,
            outline_open: false,
            split_ratio: 0.48,
            sync_scroll: true,
            zoom_factor: 1.0,
            ui_font_size: 12.5,
            ui_font_name: None,
            editor_font_family: FontPreference::Monospace,
            editor_font_size: 14.0,
            editor_font_name: None,
            reader_font_family: FontPreference::SystemSans,
            reader_font_size: 15.5,
            reader_font_name: None,
            allow_external_links: true,
            allow_local_file_links: true,
            allow_executable_links: false,
            allow_unc_network_links: false,
            allow_ntfs_ads_links: false,
            allow_javascript_uri: false,
            allow_vbscript_uri: false,
            allow_data_uri: false,
            allow_remote_images: true,
            recent_files: Vec::new(),
            explorer_root: None,
            custom_exe_path: None,
            auto_sync_exe_path: true,
            check_updates_on_startup: true,
            skipped_update_version: None,
            auto_calc_math: false,
            auto_calc_tables: true,
            word_goal: 0,
            char_goal: 0,
        }
    }
}

impl AppSettings {
    /// Returns the configured custom `.exe` path if set, or falls back to the currently running executable path.
    pub fn effective_exe_path(&self) -> PathBuf {
        if let Some(ref p) = self.custom_exe_path
            && !p.as_os_str().is_empty() {
                return p.clone();
            }
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("Markdown Studio.exe"))
    }
    /// Adds or promotes `path` to the front of `recent_files` (capped at 10 entries).
    pub fn record_recent_file(&mut self, path: &Path) {
        let normalized = path.to_path_buf();
        self.recent_files.retain(|existing| {
            #[cfg(windows)]
            {
                !existing
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&normalized.to_string_lossy())
            }
            #[cfg(not(windows))]
            {
                existing != &normalized
            }
        });
        self.recent_files.insert(0, normalized);
        self.recent_files.truncate(10);
    }
    /// Resolves the JSON preferences path (supports `MD_STUDIO_SETTINGS_PATH` / legacy `MD_WORD_SETTINGS_PATH`).
    pub fn default_path() -> PathBuf {
        if let Some(override_path) = custom_settings_path_override() {
            return override_path;
        }
        platform_settings_path("MarkdownStudio", ".markdown_studio")
    }

    /// Resolves the legacy `MarkdownWordStudio` settings path when no custom env override is set.
    pub fn legacy_default_path() -> Option<PathBuf> {
        if custom_settings_path_override().is_some() {
            return None;
        }
        Some(platform_settings_path(
            "MarkdownWordStudio",
            ".markdown_word_studio",
        ))
    }

    /// Migrates settings once from `legacy_path` to `new_path` if `new_path` does not yet exist.
    /// Clears stale `custom_exe_path` values pointing to a missing `Markdown Word Studio.exe`.
    pub fn migrate_legacy_settings_if_needed(new_path: &Path, legacy_path: &Path) -> bool {
        if new_path.exists() || !legacy_path.is_file() {
            return false;
        }
        let mut migrated = Self::load_from(legacy_path);
        if let Some(ref exe) = migrated.custom_exe_path {
            let exe_str = exe.to_string_lossy();
            let file_name = exe_str.rsplit(['/', '\\']).next().unwrap_or("");
            let is_legacy_exe = file_name.eq_ignore_ascii_case("Markdown Word Studio.exe");
            if is_legacy_exe && !exe.exists() {
                migrated.custom_exe_path = None;
            }
        }
        migrated.save_to(new_path).is_ok()
    }

    /// Loads preferences synchronously from `path`, falling back to defaults if absent or invalid.
    pub fn load_from(path: &Path) -> Self {
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        Self::from_json_str(&raw)
    }

    /// Persists preferences synchronously as UTF-8 JSON, creating parent directories if needed.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        fs::write(path, self.to_json_string().as_bytes())
    }

    pub fn to_json_string(&self) -> String {
        let vm = match self.view_mode {
            ViewMode::Wysiwyg => "wysiwyg",
            ViewMode::Split => "split",
            ViewMode::Reader => "reader",
            ViewMode::Source => "source",
        };
        let tm = match self.theme_mode {
            ThemeMode::Light => "light",
            ThemeMode::Sepia => "sepia",
            ThemeMode::Dark => "dark",
        };
        let pw = match self.page_width {
            PageWidth::Narrow => "narrow",
            PageWidth::Standard => "standard",
            PageWidth::Wide => "wide",
        };
        let ls = match self.line_spacing {
            LineSpacing::Compact => "compact",
            LineSpacing::Comfortable => "comfortable",
            LineSpacing::Relaxed => "relaxed",
        };
        let mut recent_json = String::from("[");
        for (i, path) in self.recent_files.iter().take(10).enumerate() {
            if i > 0 {
                recent_json.push(',');
            }
            recent_json.push_str("\n    \"");
            recent_json.push_str(&escape_json_str(&path.to_string_lossy()));
            recent_json.push('"');
        }
        if !self.recent_files.is_empty() {
            recent_json.push_str("\n  ");
        }
        recent_json.push(']');

        let exe_json = match &self.custom_exe_path {
            Some(p) if !p.as_os_str().is_empty() => {
                format!("\"{}\"", escape_json_str(&p.to_string_lossy()))
            }
            _ => "null".to_string(),
        };
        let explorer_json = match &self.explorer_root {
            Some(p) if !p.as_os_str().is_empty() => {
                format!("\"{}\"", escape_json_str(&p.to_string_lossy()))
            }
            _ => "null".to_string(),
        };

        let opt_str_json = |opt: &Option<String>| -> String {
            match opt {
                Some(v) if !v.trim().is_empty() => format!("\"{}\"", escape_json_str(v.trim())),
                _ => "null".to_string(),
            }
        };
        let ui_font_json = opt_str_json(&self.ui_font_name);
        let editor_font_json = opt_str_json(&self.editor_font_name);
        let reader_font_json = opt_str_json(&self.reader_font_name);
        let skipped_update_json = opt_str_json(&self.skipped_update_version);

        format!(
            "{{\n  \"dark_mode\": {},\n  \"theme_mode\": \"{}\",\n  \"page_width\": \"{}\",\n  \"line_spacing\": \"{}\",\n  \"view_mode\": \"{}\",\n  \"show_markup\": {},\n  \"outline_open\": {},\n  \"split_ratio\": {:.3},\n  \"sync_scroll\": {},\n  \"zoom_factor\": {:.2},\n  \"ui_font_size\": {:.1},\n  \"ui_font_name\": {},\n  \"editor_font_family\": \"{}\",\n  \"editor_font_size\": {:.1},\n  \"editor_font_name\": {},\n  \"reader_font_family\": \"{}\",\n  \"reader_font_size\": {:.1},\n  \"reader_font_name\": {},\n  \"allow_external_links\": {},\n  \"allow_local_file_links\": {},\n  \"allow_executable_links\": {},\n  \"allow_unc_network_links\": {},\n  \"allow_ntfs_ads_links\": {},\n  \"allow_javascript_uri\": {},\n  \"allow_vbscript_uri\": {},\n  \"allow_data_uri\": {},\n  \"allow_remote_images\": {},\n  \"recent_files\": {},\n  \"explorer_root\": {},\n  \"custom_exe_path\": {},\n  \"auto_sync_exe_path\": {},\n  \"check_updates_on_startup\": {},\n  \"skipped_update_version\": {},\n  \"auto_calc_math\": {},\n  \"auto_calc_tables\": {},\n  \"word_goal\": {},\n  \"char_goal\": {}\n}}\n",
            self.dark_mode || self.theme_mode == ThemeMode::Dark,
            tm,
            pw,
            ls,
            vm,
            self.show_markup,
            self.outline_open,
            self.split_ratio.clamp(0.25, 0.75),
            self.sync_scroll,
            self.zoom_factor.clamp(0.6, 2.0),
            self.ui_font_size.clamp(11.0, 16.0),
            ui_font_json,
            self.editor_font_family.as_str(),
            self.editor_font_size.clamp(11.0, 28.0),
            editor_font_json,
            self.reader_font_family.as_str(),
            self.reader_font_size.clamp(12.0, 28.0),
            reader_font_json,
            self.allow_external_links,
            self.allow_local_file_links,
            self.allow_executable_links,
            self.allow_unc_network_links,
            self.allow_ntfs_ads_links,
            self.allow_javascript_uri,
            self.allow_vbscript_uri,
            self.allow_data_uri,
            self.allow_remote_images,
            recent_json,
            explorer_json,
            exe_json,
            self.auto_sync_exe_path,
            self.check_updates_on_startup,
            skipped_update_json,
            self.auto_calc_math,
            self.auto_calc_tables,
            self.word_goal,
            self.char_goal
        )
    }

    pub fn from_json_str(raw: &str) -> Self {
        let mut s = Self::default();
        if let Some(v) = extract_bool_field(raw, "dark_mode") {
            s.dark_mode = v;
        }
        if let Some(tm) = extract_str_field(raw, "theme_mode") {
            s.theme_mode = match tm.as_str() {
                "light" => ThemeMode::Light,
                "sepia" => ThemeMode::Sepia,
                "dark" => ThemeMode::Dark,
                _ => s.theme_mode,
            };
            s.dark_mode = s.theme_mode == ThemeMode::Dark;
        } else {
            s.theme_mode = if s.dark_mode {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            };
        }
        if let Some(pw) = extract_str_field(raw, "page_width") {
            s.page_width = match pw.as_str() {
                "narrow" => PageWidth::Narrow,
                "standard" => PageWidth::Standard,
                "wide" => PageWidth::Wide,
                _ => s.page_width,
            };
        }
        if let Some(ls) = extract_str_field(raw, "line_spacing") {
            s.line_spacing = match ls.as_str() {
                "compact" => LineSpacing::Compact,
                "comfortable" => LineSpacing::Comfortable,
                "relaxed" => LineSpacing::Relaxed,
                _ => s.line_spacing,
            };
        }
        if let Some(v) = extract_bool_field(raw, "show_markup") {
            s.show_markup = v;
        }
        if let Some(v) = extract_bool_field(raw, "outline_open") {
            s.outline_open = v;
        }
        if let Some(vm) = extract_str_field(raw, "view_mode") {
            s.view_mode = match vm.as_str() {
                "wysiwyg" => ViewMode::Wysiwyg,
                "split" => ViewMode::Split,
                "reader" => ViewMode::Reader,
                "source" => ViewMode::Source,
                _ => s.view_mode,
            };
        }
        if let Some(r) = extract_f32_field(raw, "split_ratio") {
            s.split_ratio = r.clamp(0.25, 0.75);
        }
        if let Some(v) = extract_bool_field(raw, "sync_scroll") {
            s.sync_scroll = v;
        }
        if let Some(z) = extract_f32_field(raw, "zoom_factor") {
            s.zoom_factor = z.clamp(0.6, 2.0);
        }
        if let Some(sz) = extract_f32_field(raw, "ui_font_size") {
            s.ui_font_size = sz.clamp(11.0, 16.0);
        }
        if let Some(name) = extract_str_field(raw, "ui_font_name") {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                s.ui_font_name = Some(trimmed.to_string());
            }
        }
        if let Some(fam) = extract_str_field(raw, "editor_font_family")
            && let Some(parsed) = FontPreference::parse(&fam)
        {
            s.editor_font_family = parsed;
        }
        if let Some(sz) = extract_f32_field(raw, "editor_font_size") {
            s.editor_font_size = sz.clamp(11.0, 28.0);
        }
        if let Some(name) = extract_str_field(raw, "editor_font_name") {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                s.editor_font_name = Some(trimmed.to_string());
            }
        }
        if let Some(fam) = extract_str_field(raw, "reader_font_family")
            && let Some(parsed) = FontPreference::parse(&fam)
        {
            s.reader_font_family = parsed;
        }
        if let Some(sz) = extract_f32_field(raw, "reader_font_size") {
            s.reader_font_size = sz.clamp(12.0, 28.0);
        }
        if let Some(name) = extract_str_field(raw, "reader_font_name") {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                s.reader_font_name = Some(trimmed.to_string());
            }
        }
        if let Some(v) = extract_bool_field(raw, "allow_external_links") {
            s.allow_external_links = v;
        }
        if let Some(v) = extract_bool_field(raw, "allow_local_file_links") {
            s.allow_local_file_links = v;
        }
        if let Some(v) = extract_bool_field(raw, "allow_executable_links") {
            s.allow_executable_links = v;
        }
        if let Some(v) = extract_bool_field(raw, "allow_unc_network_links") {
            s.allow_unc_network_links = v;
        }
        if let Some(v) = extract_bool_field(raw, "allow_ntfs_ads_links") {
            s.allow_ntfs_ads_links = v;
        }
        if let Some(v) = extract_bool_field(raw, "allow_javascript_uri") {
            s.allow_javascript_uri = v;
        }
        if let Some(v) = extract_bool_field(raw, "allow_vbscript_uri") {
            s.allow_vbscript_uri = v;
        }
        if let Some(v) = extract_bool_field(raw, "allow_data_uri") {
            s.allow_data_uri = v;
        }
        if let Some(v) = extract_bool_field(raw, "allow_remote_images") {
            s.allow_remote_images = v;
        }
        if let Some(paths) = extract_str_array_field(raw, "recent_files") {
            s.recent_files = paths
                .into_iter()
                .filter(|p| !p.trim().is_empty())
                .take(10)
                .map(PathBuf::from)
                .collect();
        }
        if let Some(root) = extract_str_field(raw, "explorer_root") {
            let trimmed = root.trim();
            if !trimmed.is_empty() {
                s.explorer_root = Some(PathBuf::from(trimmed));
            }
        }
        if let Some(exe) = extract_str_field(raw, "custom_exe_path") {
            let trimmed = exe.trim();
            if !trimmed.is_empty() {
                s.custom_exe_path = Some(PathBuf::from(trimmed));
            }
        }
        if let Some(v) = extract_bool_field(raw, "auto_sync_exe_path") {
            s.auto_sync_exe_path = v;
        }
        if let Some(v) = extract_bool_field(raw, "check_updates_on_startup") {
            s.check_updates_on_startup = v;
        }
        if let Some(ver) = extract_str_field(raw, "skipped_update_version") {
            let trimmed = ver.trim();
            if !trimmed.is_empty() {
                s.skipped_update_version = Some(trimmed.to_string());
            }
        }
        if let Some(v) = extract_bool_field(raw, "auto_calc_math") {
            s.auto_calc_math = v;
        }
        if let Some(v) = extract_bool_field(raw, "auto_calc_tables") {
            s.auto_calc_tables = v;
        }
        if let Some(wg) = extract_usize_field(raw, "word_goal") {
            s.word_goal = wg;
        }
        if let Some(cg) = extract_usize_field(raw, "char_goal") {
            s.char_goal = cg;
        }
        s
    }
}

pub(crate) fn escape_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

pub(crate) fn parse_json_string_at(s: &str) -> Option<(String, &str)> {
    let mut chars = s.strip_prefix('"')?.char_indices();
    let mut out = String::new();
    let mut escaped = false;
    while let Some((idx, ch)) = chars.next() {
        if escaped {
            escaped = false;
            match ch {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{0008}'),
                'f' => out.push('\u{000C}'),
                'u' => {
                    let mut hex = String::with_capacity(4);
                    for _ in 0..4 {
                        if let Some((_, h)) = chars.next() {
                            hex.push(h);
                        }
                    }
                    if hex.len() == 4
                        && let Ok(cp) = u32::from_str_radix(&hex, 16)
                            && let Some(decoded) = char::from_u32(cp) {
                                out.push(decoded);
                            }
                }
                other => out.push(other),
            }
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            let after_quote = &s[1 + idx + 1..];
            return Some((out, after_quote));
        } else {
            out.push(ch);
        }
    }
    None
}

pub(crate) fn find_json_value_slice<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let pos = raw.find(&needle)?;
    let after_key = &raw[pos + needle.len()..];
    let colon = after_key.find(':')?;
    Some(after_key[colon + 1..].trim_start())
}

pub(crate) fn extract_bool_field(raw: &str, key: &str) -> Option<bool> {
    let v = find_json_value_slice(raw, key)?;
    if v.starts_with("true") {
        Some(true)
    } else if v.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

pub(crate) fn extract_str_field(raw: &str, key: &str) -> Option<String> {
    let v = find_json_value_slice(raw, key)?;
    parse_json_string_at(v).map(|(s, _)| s)
}

fn extract_str_array_field(raw: &str, key: &str) -> Option<Vec<String>> {
    let mut rest = find_json_value_slice(raw, key)?.strip_prefix('[')?;
    let mut items = Vec::new();
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
        if rest.starts_with(']') {
            return Some(items);
        }
        if rest.starts_with('"') {
            let (s, after) = parse_json_string_at(rest)?;
            items.push(s);
            rest = after;
        } else {
            return None;
        }
    }
}

pub(crate) fn extract_f32_field(raw: &str, key: &str) -> Option<f32> {
    let v = find_json_value_slice(raw, key)?;
    let num_str: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    num_str.parse::<f32>().ok()
}

pub(crate) fn extract_usize_field(raw: &str, key: &str) -> Option<usize> {
    let v = find_json_value_slice(raw, key)?;
    let num_str: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
    if num_str.is_empty() {
        None
    } else {
        num_str.parse::<usize>().ok()
    }
}

fn custom_settings_path_override() -> Option<PathBuf> {
    for key in ["MD_STUDIO_SETTINGS_PATH", "MD_WORD_SETTINGS_PATH"] {
        if let Ok(custom) = std::env::var(key) {
            let trimmed = custom.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    None
}

fn platform_settings_path(win_dir: &str, unix_dir: &str) -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA")
        && !appdata.trim().is_empty() {
            return PathBuf::from(appdata).join(win_dir).join("settings.json");
        }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))
        && !home.trim().is_empty() {
            return PathBuf::from(home).join(unix_dir).join("settings.json");
        }
    std::env::temp_dir().join(win_dir).join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_settings_and_clears_missing_old_exe_path() {
        let root = std::env::temp_dir().join(format!(
            "markdown_studio_settings_migrate_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let legacy_path = root.join("MarkdownWordStudio").join("settings.json");
        let new_path = root.join("MarkdownStudio").join("settings.json");

        let legacy = AppSettings {
            theme_mode: ThemeMode::Sepia,
            zoom_factor: 1.25,
            custom_exe_path: Some(PathBuf::from(r"Z:\NonExistent\Markdown Word Studio.exe")),
            recent_files: vec![PathBuf::from(r"C:\Docs\notes.md")],
            ..Default::default()
        };
        legacy.save_to(&legacy_path).unwrap();

        assert!(AppSettings::migrate_legacy_settings_if_needed(
            &new_path,
            &legacy_path
        ));
        assert!(new_path.is_file());

        let loaded = AppSettings::load_from(&new_path);
        assert_eq!(loaded.theme_mode, ThemeMode::Sepia);
        assert!((loaded.zoom_factor - 1.25).abs() < 0.01);
        assert_eq!(loaded.custom_exe_path, None);
        assert_eq!(loaded.recent_files, vec![PathBuf::from(r"C:\Docs\notes.md")]);

        // Second run must not overwrite existing new_path
        let mut updated = loaded.clone();
        updated.theme_mode = ThemeMode::Dark;
        updated.save_to(&new_path).unwrap();
        assert!(!AppSettings::migrate_legacy_settings_if_needed(
            &new_path,
            &legacy_path
        ));
        assert_eq!(AppSettings::load_from(&new_path).theme_mode, ThemeMode::Dark);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migration_is_noop_when_legacy_missing() {
        let root = std::env::temp_dir().join(format!(
            "markdown_studio_settings_noop_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let legacy_path = root.join("MarkdownWordStudio").join("settings.json");
        let new_path = root.join("MarkdownStudio").join("settings.json");

        assert!(!AppSettings::migrate_legacy_settings_if_needed(
            &new_path,
            &legacy_path
        ));
        assert!(!new_path.exists());
    }

    #[test]
    fn explorer_root_roundtrips_through_json() {
        let mut s = AppSettings::default();
        assert_eq!(s.explorer_root, None);
        let root = PathBuf::from(r"D:\Workspace\Notes");
        s.explorer_root = Some(root.clone());
        s.word_goal = 1500;
        s.char_goal = 8000;
        let parsed = AppSettings::from_json_str(&s.to_json_string());
        assert_eq!(parsed.explorer_root, Some(root));
        assert_eq!(parsed.word_goal, 1500);
        assert_eq!(parsed.char_goal, 8000);
    }

    #[test]
    fn font_and_security_settings_defaults_roundtrip_and_clamp() {
        let def = AppSettings::default();
        assert!((def.ui_font_size - 12.5).abs() < 0.01);
        assert_eq!(def.ui_font_name, None);
        assert_eq!(def.editor_font_family, FontPreference::Monospace);
        assert!((def.editor_font_size - 14.0).abs() < 0.01);
        assert_eq!(def.editor_font_name, None);
        assert_eq!(def.reader_font_family, FontPreference::SystemSans);
        assert!((def.reader_font_size - 15.5).abs() < 0.01);
        assert_eq!(def.reader_font_name, None);
        assert!(def.allow_external_links);
        assert!(def.allow_local_file_links);
        assert!(!def.allow_executable_links);
        assert!(!def.allow_unc_network_links);
        assert!(!def.allow_ntfs_ads_links);
        assert!(!def.allow_javascript_uri);
        assert!(!def.allow_vbscript_uri);
        assert!(!def.allow_data_uri);
        assert!(def.allow_remote_images);

        let custom = AppSettings {
            ui_font_size: 14.0,
            ui_font_name: Some("Segoe UI".to_string()),
            editor_font_family: FontPreference::Serif,
            editor_font_size: 16.5,
            editor_font_name: Some("Consolas".to_string()),
            reader_font_family: FontPreference::Monospace,
            reader_font_size: 18.0,
            reader_font_name: Some("Georgia".to_string()),
            allow_external_links: false,
            allow_local_file_links: false,
            allow_executable_links: true,
            allow_unc_network_links: true,
            allow_ntfs_ads_links: true,
            allow_javascript_uri: true,
            allow_vbscript_uri: true,
            allow_data_uri: true,
            allow_remote_images: false,
            check_updates_on_startup: false,
            skipped_update_version: Some("0.9.0".to_string()),
            ..Default::default()
        };
        let parsed = AppSettings::from_json_str(&custom.to_json_string());
        assert!(!parsed.check_updates_on_startup);
        assert_eq!(parsed.skipped_update_version.as_deref(), Some("0.9.0"));
        assert!((parsed.ui_font_size - 14.0).abs() < 0.01);
        assert_eq!(parsed.ui_font_name.as_deref(), Some("Segoe UI"));
        assert_eq!(parsed.editor_font_family, FontPreference::Serif);
        assert!((parsed.editor_font_size - 16.5).abs() < 0.01);
        assert_eq!(parsed.editor_font_name.as_deref(), Some("Consolas"));
        assert_eq!(parsed.reader_font_family, FontPreference::Monospace);
        assert!((parsed.reader_font_size - 18.0).abs() < 0.01);
        assert_eq!(parsed.reader_font_name.as_deref(), Some("Georgia"));
        assert!(!parsed.allow_external_links);
        assert!(!parsed.allow_local_file_links);
        assert!(parsed.allow_executable_links);
        assert!(parsed.allow_unc_network_links);
        assert!(parsed.allow_ntfs_ads_links);
        assert!(parsed.allow_javascript_uri);
        assert!(parsed.allow_vbscript_uri);
        assert!(parsed.allow_data_uri);
        assert!(!parsed.allow_remote_images);

        // Out-of-range font sizes are clamped cleanly
        let extreme = r#"{
            "ui_font_size": 40.0,
            "editor_font_size": 3.0,
            "reader_font_size": 99.0
        }"#;
        let clamped = AppSettings::from_json_str(extreme);
        assert!((clamped.ui_font_size - 16.0).abs() < 0.01);
        assert!((clamped.editor_font_size - 11.0).abs() < 0.01);
        assert!((clamped.reader_font_size - 28.0).abs() < 0.01);
    }
}
