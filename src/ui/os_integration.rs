use std::path::{Path, PathBuf};

use egui::{Align2, Color32, CornerRadius, Key, Margin, RichText, Stroke, Vec2};

use crate::ui::settings::AppSettings;
use crate::ui::theme::WordTheme;

pub const DEFAULT_CLASSES_ROOT: &str = r"HKCU\Software\Classes";
pub const PROG_ID: &str = "MarkdownStudio.md";
pub const SHELL_KEY_NAME: &str = "MarkdownStudio";
pub const LEGACY_PROG_ID: &str = "MarkdownWordStudio.md";
pub const LEGACY_SHELL_KEY_NAME: &str = "MarkdownWordStudio";

#[cfg(target_os = "windows")]
const APP_DISPLAY_NAME: &str = "Markdown Studio";
#[cfg(target_os = "windows")]
const LEGACY_APP_EXE_NAMES: &[&str] = &["Markdown Word Studio.exe", "md_word.exe"];
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Live Windows Explorer integration state under per-user `HKCU\Software\Classes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OsIntegrationStatus {
    pub shell_new_enabled: bool,
    pub open_with_enabled: bool,
    pub registered_exe_path: Option<PathBuf>,
}

/// Formats `"\"<exe>\" \"%1\""` for Windows Shell open commands.
pub fn format_open_command(exe_path: &Path) -> String {
    format!("\"{}\" \"%1\"", exe_path.display())
}

/// Formats `"\"<exe>\",0"` for Windows Shell icon values.
pub fn format_icon_value(exe_path: &Path) -> String {
    format!("\"{}\",0", exe_path.display())
}

/// Extracts the executable path from a Windows Shell command string like `"C:\App\markdown_studio.exe" "%1"`.
pub fn extract_exe_from_command(cmd: &str) -> Option<PathBuf> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('"') {
        let end_quote = rest.find('"')?;
        let path = rest[..end_quote].trim();
        if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path))
        }
    } else {
        let token = trimmed.split_whitespace().next()?;
        if token.is_empty() {
            None
        } else {
            Some(PathBuf::from(token))
        }
    }
}

pub fn paths_match(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

#[cfg(target_os = "windows")]
fn win_reg_exe_path() -> PathBuf {
    if let Some(root) = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("WINDIR")) {
        let candidate = PathBuf::from(root).join("System32").join("reg.exe");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("reg.exe")
}

/// Validates that `classes_root` strictly targets a per-user `HKCU\` hive (never `HKLM`).
pub fn validate_classes_root(classes_root: &str) -> Result<(), String> {
    let valid_prefix = classes_root
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case(r"HKCU\"));
    if !valid_prefix
        || classes_root.chars().any(|c| c.is_control())
        || classes_root.contains("..")
    {
        return Err(format!(
            "Refusing registry operation outside HKCU\\: '{classes_root}'"
        ));
    }
    Ok(())
}

/// Validates that `exe_path` is a safe, absolute, non-UNC `.exe` path without quote/control injection.
pub fn validate_exe_path(exe_path: &Path) -> Result<(), String> {
    let raw = exe_path.to_string_lossy();
    if raw.trim().is_empty()
        || raw.chars().any(|c| c.is_control() || c == '"')
        || raw.ends_with('\\')
        || raw.ends_with('/')
    {
        return Err("Executable path contains invalid characters or trailing slash.".to_string());
    }
    let is_non_verbatim_unc = raw.starts_with(r"\\") && !raw.starts_with(r"\\?\");
    let is_verbatim_unc = raw
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case(r"\\?\UNC\"));
    if is_non_verbatim_unc || is_verbatim_unc || raw.starts_with("//") {
        return Err("Network/UNC executable paths are not permitted.".to_string());
    }
    let bytes = raw.as_bytes();
    let has_windows_drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    if !exe_path.is_absolute() && !has_windows_drive {
        return Err("Executable path must be absolute.".to_string());
    }
    let is_exe = exe_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if !is_exe {
        return Err("Executable path must point to a .exe file.".to_string());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn run_reg(args: &[&str]) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new(win_reg_exe_path())
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Failed to invoke reg.exe: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Err(if !err.is_empty() { err } else { stdout })
    }
}

#[cfg(target_os = "windows")]
fn reg_add_default(key: &str, value: &str) -> Result<(), String> {
    run_reg(&["add", key, "/ve", "/t", "REG_SZ", "/d", value, "/f"]).map(|_| ())
}

#[cfg(target_os = "windows")]
fn reg_add_named(key: &str, name: &str, value: &str) -> Result<(), String> {
    run_reg(&["add", key, "/v", name, "/t", "REG_SZ", "/d", value, "/f"]).map(|_| ())
}

#[cfg(target_os = "windows")]
fn reg_delete_tree(key: &str) {
    let _ = run_reg(&["delete", key, "/f"]);
}

#[cfg(target_os = "windows")]
fn reg_key_exists(key: &str) -> bool {
    run_reg(&["query", key]).is_ok()
}

#[cfg(target_os = "windows")]
fn reg_query_value(key: &str, value_name: Option<&str>) -> Option<String> {
    let args = match value_name {
        Some(name) => vec!["query", key, "/v", name],
        None => vec!["query", key, "/ve"],
    };
    let out = run_reg(&args).ok()?;
    for line in out.lines() {
        let trimmed = line.trim();
        if let Some(pos) = trimmed.find("REG_SZ") {
            let val = trimmed[pos + "REG_SZ".len()..].trim();
            return Some(val.to_string());
        }
        if let Some(pos) = trimmed.find("REG_EXPAND_SZ") {
            let val = trimmed[pos + "REG_EXPAND_SZ".len()..].trim();
            return Some(val.to_string());
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn notify_shell_assoc_changed() {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHChangeNotify(wEventId: i32, uFlags: u32, dwItem1: *const u8, dwItem2: *const u8);
    }
    const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
    const SHCNF_IDLIST: u32 = 0x0000;
    // SAFETY: Null item pointers are valid with SHCNE_ASSOCCHANGED | SHCNF_IDLIST.
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED,
            SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
}

#[cfg(target_os = "windows")]
fn exe_file_name(exe_path: &Path) -> String {
    exe_path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Markdown Studio.exe")
        .to_string()
}

#[cfg(target_os = "windows")]
fn mui_cache_key(classes_root: &str) -> String {
    format!(r"{classes_root}\Local Settings\Software\Microsoft\Windows\Shell\MuiCache")
}

#[cfg(target_os = "windows")]
fn mui_friendly_value_name(exe_path: &Path) -> String {
    format!("{}.FriendlyAppName", exe_path.display())
}

#[cfg(target_os = "windows")]
fn has_legacy_keys_under(classes_root: &str) -> bool {
    let md_default = reg_query_value(&format!(r"{classes_root}\.md"), None);
    md_default.as_deref() == Some(LEGACY_PROG_ID)
        || reg_key_exists(&format!(r"{classes_root}\{LEGACY_PROG_ID}"))
        || reg_key_exists(&format!(r"{classes_root}\*\shell\{LEGACY_SHELL_KEY_NAME}"))
        || reg_key_exists(&format!(
            r"{classes_root}\SystemFileAssociations\.md\shell\{LEGACY_SHELL_KEY_NAME}"
        ))
        || LEGACY_APP_EXE_NAMES
            .iter()
            .any(|name| reg_key_exists(&format!(r"{classes_root}\Applications\{name}")))
}

#[cfg(target_os = "windows")]
fn needs_friendly_name_refresh_under(
    classes_root: &str,
    exe_path: &Path,
    status: &OsIntegrationStatus,
) -> bool {
    if status.shell_new_enabled {
        let app_name = reg_query_value(
            &format!(r"{classes_root}\{PROG_ID}\Application"),
            Some("ApplicationName"),
        );
        if app_name.as_deref() != Some(APP_DISPLAY_NAME) {
            return true;
        }
    }
    let exe_name = exe_file_name(exe_path);
    let app_friendly = reg_query_value(
        &format!(r"{classes_root}\Applications\{exe_name}"),
        Some("FriendlyAppName"),
    );
    if app_friendly.as_deref() != Some(APP_DISPLAY_NAME) {
        return true;
    }
    let mui_val = reg_query_value(
        &mui_cache_key(classes_root),
        Some(&mui_friendly_value_name(exe_path)),
    );
    mui_val.as_deref() != Some(APP_DISPLAY_NAME)
}

#[cfg(target_os = "windows")]
fn cleanup_legacy_keys_under(classes_root: &str) {
    let md_key = format!(r"{classes_root}\.md");
    if reg_query_value(&md_key, None).as_deref() == Some(LEGACY_PROG_ID) {
        let _ = run_reg(&["delete", &md_key, "/ve", "/f"]);
    }
    reg_delete_tree(&format!(r"{classes_root}\{LEGACY_PROG_ID}"));
    reg_delete_tree(&format!(r"{classes_root}\*\shell\{LEGACY_SHELL_KEY_NAME}"));
    reg_delete_tree(&format!(
        r"{classes_root}\SystemFileAssociations\.md\shell\{LEGACY_SHELL_KEY_NAME}"
    ));
    for exe_name in LEGACY_APP_EXE_NAMES {
        reg_delete_tree(&format!(r"{classes_root}\Applications\{exe_name}"));
    }
}

/// Queries Windows Explorer integration status under the given `classes_root` (e.g. `HKCU\Software\Classes`).
pub fn query_integration_status_under(classes_root: &str) -> OsIntegrationStatus {
    if validate_classes_root(classes_root).is_err() {
        return OsIntegrationStatus::default();
    }
    #[cfg(target_os = "windows")]
    {
        let shell_new_key = format!(r"{classes_root}\.md\ShellNew");
        let prog_cmd_key = format!(r"{classes_root}\{PROG_ID}\shell\open\command");
        let star_cmd_key = format!(r"{classes_root}\*\shell\{SHELL_KEY_NAME}\command");
        let md_cmd_key =
            format!(r"{classes_root}\SystemFileAssociations\.md\shell\{SHELL_KEY_NAME}\command");

        let legacy_prog_cmd_key = format!(r"{classes_root}\{LEGACY_PROG_ID}\shell\open\command");
        let legacy_star_cmd_key =
            format!(r"{classes_root}\*\shell\{LEGACY_SHELL_KEY_NAME}\command");
        let legacy_md_cmd_key = format!(
            r"{classes_root}\SystemFileAssociations\.md\shell\{LEGACY_SHELL_KEY_NAME}\command"
        );

        let md_default = reg_query_value(&format!(r"{classes_root}\.md"), None);
        let shell_new_exists = reg_key_exists(&shell_new_key);
        let new_shell_new = shell_new_exists
            && md_default.as_deref() == Some(PROG_ID)
            && reg_key_exists(&prog_cmd_key);
        let legacy_shell_new = shell_new_exists
            && md_default.as_deref() == Some(LEGACY_PROG_ID)
            && reg_key_exists(&legacy_prog_cmd_key);
        let shell_new_enabled = new_shell_new || legacy_shell_new;

        let new_open_with = reg_key_exists(&star_cmd_key) || reg_key_exists(&md_cmd_key);
        let legacy_open_with =
            reg_key_exists(&legacy_star_cmd_key) || reg_key_exists(&legacy_md_cmd_key);
        let open_with_enabled = new_open_with || legacy_open_with;

        let registered_exe_path = reg_query_value(&prog_cmd_key, None)
            .or_else(|| reg_query_value(&star_cmd_key, None))
            .or_else(|| reg_query_value(&md_cmd_key, None))
            .or_else(|| reg_query_value(&legacy_prog_cmd_key, None))
            .or_else(|| reg_query_value(&legacy_star_cmd_key, None))
            .or_else(|| reg_query_value(&legacy_md_cmd_key, None))
            .and_then(|cmd| extract_exe_from_command(&cmd));

        OsIntegrationStatus {
            shell_new_enabled,
            open_with_enabled,
            registered_exe_path,
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = classes_root;
        OsIntegrationStatus::default()
    }
}

pub fn query_integration_status() -> OsIntegrationStatus {
    query_integration_status_under(DEFAULT_CLASSES_ROOT)
}

/// Registers or updates Windows Explorer right-click integration under `classes_root` (strictly per-user, zero admin rights).
pub fn register_integration_under(
    classes_root: &str,
    exe_path: &Path,
    enable_shell_new: bool,
    enable_open_with: bool,
) -> Result<String, String> {
    validate_classes_root(classes_root)?;
    if !enable_shell_new && !enable_open_with {
        return unregister_integration_under(classes_root);
    }
    validate_exe_path(exe_path)?;

    #[cfg(target_os = "windows")]
    {
        cleanup_legacy_keys_under(classes_root);

        let open_cmd = format_open_command(exe_path);
        let icon_val = format_icon_value(exe_path);

        let md_key = format!(r"{classes_root}\.md");
        let shell_new_key = format!(r"{classes_root}\.md\ShellNew");
        let prog_key = format!(r"{classes_root}\{PROG_ID}");
        let prog_icon = format!(r"{classes_root}\{PROG_ID}\DefaultIcon");
        let prog_cmd = format!(r"{classes_root}\{PROG_ID}\shell\open\command");

        if enable_shell_new {
            reg_add_default(&md_key, PROG_ID)?;
            reg_add_named(&md_key, "Content Type", "text/markdown")?;
            reg_add_named(&md_key, "PerceivedType", "text")?;
            reg_add_named(&shell_new_key, "NullFile", "")?;

            reg_add_default(&prog_key, "Markdown Document")?;
            reg_add_default(&prog_icon, &icon_val)?;
            reg_add_default(&prog_cmd, &open_cmd)?;

            let prog_app = format!(r"{prog_key}\Application");
            reg_add_named(&prog_app, "ApplicationName", APP_DISPLAY_NAME)?;
            reg_add_named(&prog_app, "ApplicationDescription", APP_DISPLAY_NAME)?;
            reg_add_named(&prog_app, "ApplicationIcon", &icon_val)?;
        } else {
            reg_delete_tree(&shell_new_key);
            if reg_query_value(&md_key, None).as_deref() == Some(PROG_ID) {
                let _ = run_reg(&["delete", &md_key, "/ve", "/f"]);
            }
            reg_delete_tree(&prog_key);
        }

        let app_key = format!(r"{classes_root}\Applications\{}", exe_file_name(exe_path));
        reg_add_named(&app_key, "FriendlyAppName", APP_DISPLAY_NAME)?;
        reg_add_default(&format!(r"{app_key}\DefaultIcon"), &icon_val)?;
        reg_add_default(&format!(r"{app_key}\shell\open\command"), &open_cmd)?;

        reg_add_named(
            &mui_cache_key(classes_root),
            &mui_friendly_value_name(exe_path),
            APP_DISPLAY_NAME,
        )?;

        let star_shell = format!(r"{classes_root}\*\shell\{SHELL_KEY_NAME}");
        let star_cmd = format!(r"{classes_root}\*\shell\{SHELL_KEY_NAME}\command");
        let md_assoc_shell =
            format!(r"{classes_root}\SystemFileAssociations\.md\shell\{SHELL_KEY_NAME}");
        let md_assoc_cmd =
            format!(r"{classes_root}\SystemFileAssociations\.md\shell\{SHELL_KEY_NAME}\command");

        if enable_open_with {
            for (shell_k, cmd_k) in [(&star_shell, &star_cmd), (&md_assoc_shell, &md_assoc_cmd)] {
                reg_add_default(shell_k, "Open with Markdown Studio")?;
                reg_add_named(shell_k, "Icon", &icon_val)?;
                reg_add_default(cmd_k, &open_cmd)?;
            }
        } else {
            reg_delete_tree(&star_shell);
            reg_delete_tree(&md_assoc_shell);
        }

        if classes_root.eq_ignore_ascii_case(DEFAULT_CLASSES_ROOT) {
            notify_shell_assoc_changed();
        }
        Ok(format!(
            "Updated Windows Explorer menu -> {}",
            exe_path.display()
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (classes_root, exe_path);
        Err("Windows Explorer integration is only supported on Windows.".to_string())
    }
}

pub fn register_integration(
    exe_path: &Path,
    enable_shell_new: bool,
    enable_open_with: bool,
) -> Result<String, String> {
    register_integration_under(
        DEFAULT_CLASSES_ROOT,
        exe_path,
        enable_shell_new,
        enable_open_with,
    )
}

/// Removes Markdown Studio entries from Windows Explorer menus under `classes_root`.
pub fn unregister_integration_under(classes_root: &str) -> Result<String, String> {
    validate_classes_root(classes_root)?;
    #[cfg(target_os = "windows")]
    {
        let current_status = query_integration_status_under(classes_root);
        let md_key = format!(r"{classes_root}\.md");
        reg_delete_tree(&format!(r"{classes_root}\.md\ShellNew"));
        if reg_query_value(&md_key, None).as_deref() == Some(PROG_ID) {
            let _ = run_reg(&["delete", &md_key, "/ve", "/f"]);
        }
        reg_delete_tree(&format!(r"{classes_root}\{PROG_ID}"));
        reg_delete_tree(&format!(r"{classes_root}\*\shell\{SHELL_KEY_NAME}"));
        reg_delete_tree(&format!(
            r"{classes_root}\SystemFileAssociations\.md\shell\{SHELL_KEY_NAME}"
        ));
        reg_delete_tree(&format!(r"{classes_root}\Applications\Markdown Studio.exe"));
        if let Some(reg_exe) = current_status.registered_exe_path {
            let exe_name = exe_file_name(&reg_exe);
            if !exe_name.is_empty() && !exe_name.eq_ignore_ascii_case("Markdown Studio.exe") {
                reg_delete_tree(&format!(r"{classes_root}\Applications\{exe_name}"));
            }
        }
        if let Ok(cur_exe) = std::env::current_exe() {
            let exe_name = exe_file_name(&cur_exe);
            if !exe_name.is_empty() && !exe_name.eq_ignore_ascii_case("Markdown Studio.exe") {
                reg_delete_tree(&format!(r"{classes_root}\Applications\{exe_name}"));
            }
        }
        cleanup_legacy_keys_under(classes_root);
        if classes_root.eq_ignore_ascii_case(DEFAULT_CLASSES_ROOT) {
            notify_shell_assoc_changed();
        }
        Ok("Removed Markdown Studio from Windows Explorer menus.".to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = classes_root;
        Err("Windows Explorer integration is only supported on Windows.".to_string())
    }
}

pub fn unregister_integration() -> Result<String, String> {
    unregister_integration_under(DEFAULT_CLASSES_ROOT)
}

/// If Windows Explorer integration is currently active under `classes_root` and either legacy
/// `MarkdownWordStudio` keys remain or the registered `.exe` location changed, updates and
/// migrates it automatically to `exe_path`. Returns `Ok(true)` if updated.
pub fn sync_exe_path_if_registered_under(
    classes_root: &str,
    exe_path: &Path,
) -> Result<bool, String> {
    let status = query_integration_status_under(classes_root);
    if !status.shell_new_enabled && !status.open_with_enabled {
        return Ok(false);
    }
    #[cfg(target_os = "windows")]
    let needs_refresh = has_legacy_keys_under(classes_root)
        || needs_friendly_name_refresh_under(classes_root, exe_path, &status);
    #[cfg(not(target_os = "windows"))]
    let needs_refresh = false;

    if !needs_refresh
        && status
            .registered_exe_path
            .as_deref()
            .is_some_and(|p| paths_match(p, exe_path))
    {
        return Ok(false);
    }
    register_integration_under(
        classes_root,
        exe_path,
        status.shell_new_enabled,
        status.open_with_enabled,
    )?;
    Ok(true)
}

pub fn sync_exe_path_if_registered(exe_path: &Path) -> Result<bool, String> {
    sync_exe_path_if_registered_under(DEFAULT_CLASSES_ROOT, exe_path)
}

/// Interactive Windows Explorer Context Menu & Executable Path modal.
#[derive(Debug, Default)]
pub struct OsIntegrationModal {
    pub open: bool,
    pub exe_path_input: String,
    pub enable_shell_new: bool,
    pub enable_open_with: bool,
    pub auto_sync_exe_path: bool,
    pub status: OsIntegrationStatus,
    pub feedback: Option<(bool, String)>,
}

impl OsIntegrationModal {
    pub fn open_with_settings(&mut self, settings: &AppSettings) {
        self.open = true;
        self.status = query_integration_status();
        let effective = settings.effective_exe_path();
        self.exe_path_input = effective.to_string_lossy().to_string();
        self.auto_sync_exe_path = settings.auto_sync_exe_path;
        if self.status.shell_new_enabled || self.status.open_with_enabled {
            self.enable_shell_new = self.status.shell_new_enabled;
            self.enable_open_with = self.status.open_with_enabled;
        } else {
            self.enable_shell_new = true;
            self.enable_open_with = true;
        }
        self.feedback = None;
    }

    /// Renders the modal window and optionally returns a status bar message when an OS change occurred.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: &WordTheme,
        settings: &mut AppSettings,
        settings_path: &Path,
    ) -> Option<String> {
        if !self.open {
            return None;
        }

        let mut keep_open = true;
        let mut status_out: Option<String> = None;

        egui::Window::new("Windows Explorer & Context Menu Setup")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(540.0)
            .show(ctx, |ui| {
                status_out = self.show_contents(ui, theme, settings, settings_path, false);
            });

        if !keep_open {
            self.open = false;
        }
        status_out
    }

    /// Renders the inner OS Integration panel contents (used standalone or embedded in Settings).
    pub fn show_contents(
        &mut self,
        ui: &mut egui::Ui,
        theme: &WordTheme,
        settings: &mut AppSettings,
        settings_path: &Path,
        embedded: bool,
    ) -> Option<String> {
        let mut status_out: Option<String> = None;

        if embedded {
            ui.label(
                RichText::new("Windows Explorer & Context Menu Setup")
                    .strong()
                    .size(13.5)
                    .color(theme.text_primary),
            );
            ui.add_space(4.0);
        }

        ui.label(
            RichText::new(
                "Add Markdown Studio to your Windows Right-Click menu (per-user HKCU, no admin required).",
            )
            .size(12.5)
            .color(theme.text_muted),
        );
        #[cfg(not(target_os = "windows"))]
        {
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Note: Windows Registry integration is inactive on Linux/macOS; desktop file associations follow your OS desktop environment.",
                )
                .size(11.5)
                .color(theme.text_muted),
            );
        }
        ui.add_space(8.0);

        // Current registry status banner
        let is_registered = self.status.shell_new_enabled || self.status.open_with_enabled;
        let (pill_lbl, pill_col) = if is_registered {
            ("Registered in Windows Menu", theme.success_green)
        } else {
            ("Not Registered Yet", theme.text_muted)
        };
        egui::Frame::new()
            .fill(theme.ribbon_bg)
            .stroke(Stroke::new(1.0_f32, theme.border))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(pill_lbl).strong().size(12.0).color(pill_col));
                    if let Some(ref reg_exe) = self.status.registered_exe_path {
                        let exists_mark = if reg_exe.exists() { "found" } else { "missing" };
                        ui.label(
                            RichText::new(format!("({exists_mark}: {})", reg_exe.display()))
                                .size(11.5)
                                .color(theme.text_muted),
                        );
                    }
                });
            });

        ui.add_space(10.0);
        ui.label(RichText::new("Target Executable (.exe) Path:").strong().size(12.5));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.exe_path_input)
                    .desired_width(355.0)
                    .hint_text(r"C:\Path\To\Markdown Studio.exe"),
            );
            if ui
                .button("Current .exe")
                .on_hover_text("Use the currently running executable path")
                .clicked()
                && let Ok(cur) = std::env::current_exe() {
                    self.exe_path_input = cur.to_string_lossy().to_string();
                    self.feedback = None;
                }
            if ui
                .button("Browse...")
                .on_hover_text("Pick Markdown Studio.exe from disk")
                .clicked()
                && let Some(picked) = rfd::FileDialog::new()
                    .add_filter("Windows Executable", &["exe"])
                    .pick_file()
                {
                    self.exe_path_input = picked.to_string_lossy().to_string();
                    self.feedback = None;
                }
        });

        let typed_path = PathBuf::from(self.exe_path_input.trim());
        if !self.exe_path_input.trim().is_empty() && !typed_path.exists() {
            ui.label(
                RichText::new("Note: That path does not exist on disk yet — verify before applying.")
                    .size(11.5)
                    .color(theme.warning_amber),
            );
        }

        ui.add_space(8.0);
        if ui
            .checkbox(
                &mut self.auto_sync_exe_path,
                "Auto-update Windows Registry on startup if the .exe moves",
            )
            .changed()
        {
            settings.auto_sync_exe_path = self.auto_sync_exe_path;
            let _ = settings.save_to(settings_path);
        }

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("Windows Right-Click Options:").strong().size(12.5));
        ui.checkbox(
            &mut self.enable_shell_new,
            "Add \"Markdown Document (.md)\" to Right-Click -> New menu",
        );
        ui.checkbox(
            &mut self.enable_open_with,
            "Add \"Open with Markdown Studio\" to file Right-Click menu",
        );

        if let Some((is_err, ref msg)) = self.feedback {
            ui.add_space(6.0);
            let col = if is_err {
                theme.warning_amber
            } else {
                theme.success_green
            };
            ui.label(RichText::new(msg).size(12.0).color(col));
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            let apply_btn = egui::Button::new(
                RichText::new("Apply / Update Menu")
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(theme.accent)
            .stroke(Stroke::NONE);
            if ui.add(apply_btn).clicked() {
                let trimmed = self.exe_path_input.trim();
                let target = PathBuf::from(trimmed);
                if let Err(err) = validate_exe_path(&target) {
                    self.feedback = Some((true, err.clone()));
                    status_out = Some(err);
                } else {
                    settings.custom_exe_path = Some(target.clone());
                    settings.auto_sync_exe_path = self.auto_sync_exe_path;
                    let _ = settings.save_to(settings_path);
                    match register_integration(
                        &target,
                        self.enable_shell_new,
                        self.enable_open_with,
                    ) {
                        Ok(msg) => {
                            self.status = query_integration_status();
                            self.feedback = Some((false, msg.clone()));
                            status_out = Some(msg);
                        }
                        Err(err) => {
                            self.feedback = Some((true, err.clone()));
                            status_out = Some(err);
                        }
                    }
                }
            }

            if ui.button("Remove From Menu").clicked() {
                match unregister_integration() {
                    Ok(msg) => {
                        self.status = query_integration_status();
                        self.enable_shell_new = false;
                        self.enable_open_with = false;
                        self.feedback = Some((false, msg.clone()));
                        status_out = Some(msg);
                    }
                    Err(err) => {
                        self.feedback = Some((true, err.clone()));
                        status_out = Some(err);
                    }
                }
            }

            if !embedded {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        self.open = false;
                    }
                });
            }
        });

        status_out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_command_and_icon_formatting_roundtrips() {
        let exe = PathBuf::from(r"D:\Portable Apps\Markdown Studio.exe");
        let cmd = format_open_command(&exe);
        assert_eq!(cmd, r#""D:\Portable Apps\Markdown Studio.exe" "%1""#);
        assert_eq!(extract_exe_from_command(&cmd), Some(exe.clone()));

        let icon = format_icon_value(&exe);
        assert_eq!(icon, r#""D:\Portable Apps\Markdown Studio.exe",0"#);

        // Unquoted command fallback
        assert_eq!(
            extract_exe_from_command(r"C:\Tools\markdown_studio.exe %1"),
            Some(PathBuf::from(r"C:\Tools\markdown_studio.exe"))
        );
        assert_eq!(extract_exe_from_command("   "), None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn registry_register_relocate_sync_and_unregister_under_isolated_test_key() {
        let test_root = format!(
            r"HKCU\Software\MarkdownStudioTest_{}\Classes",
            std::process::id()
        );
        let test_base = format!(r"HKCU\Software\MarkdownStudioTest_{}", std::process::id());
        let exe_v1 = PathBuf::from(r"C:\Temp\v1\Markdown Studio.exe");
        let exe_v2 = PathBuf::from(r"D:\Moved\v2\Markdown Studio.exe");

        // Clean start
        reg_delete_tree(&test_base);

        // 1. Register both ShellNew and OpenWith pointing to exe_v1
        register_integration_under(&test_root, &exe_v1, true, true)
            .expect("register in HKCU test tree");
        let st1 = query_integration_status_under(&test_root);
        assert!(st1.shell_new_enabled);
        assert!(st1.open_with_enabled);
        assert_eq!(st1.registered_exe_path.as_ref(), Some(&exe_v1));

        // 2. Sync with same path does nothing
        assert!(!sync_exe_path_if_registered_under(&test_root, &exe_v1).unwrap());

        // 3. Relocate exe -> sync automatically updates registry commands
        assert!(sync_exe_path_if_registered_under(&test_root, &exe_v2).unwrap());
        let st2 = query_integration_status_under(&test_root);
        assert!(st2.shell_new_enabled);
        assert!(st2.open_with_enabled);
        assert_eq!(st2.registered_exe_path.as_ref(), Some(&exe_v2));

        // 4. Unregister removes all keys cleanly
        unregister_integration_under(&test_root).unwrap();
        let st3 = query_integration_status_under(&test_root);
        assert!(!st3.shell_new_enabled);
        assert!(!st3.open_with_enabled);
        assert_eq!(st3.registered_exe_path, None);

        reg_delete_tree(&test_base);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn migrates_legacy_markdown_word_studio_registry_keys_cleanly() {
        let test_base = format!(
            r"HKCU\Software\MarkdownStudioLegacyTest_{}",
            std::process::id()
        );
        let test_root = format!(r"{test_base}\Classes");
        let old_exe = PathBuf::from(r"C:\Temp\Old\Markdown Word Studio.exe");
        let new_exe = PathBuf::from(r"C:\Temp\New\Markdown Studio.exe");

        reg_delete_tree(&test_base);

        // Seed legacy MarkdownWordStudio keys
        let old_cmd = format_open_command(&old_exe);
        let old_icon = format_icon_value(&old_exe);
        reg_add_default(&format!(r"{test_root}\.md"), LEGACY_PROG_ID).unwrap();
        reg_add_named(&format!(r"{test_root}\.md\ShellNew"), "NullFile", "").unwrap();
        reg_add_default(&format!(r"{test_root}\{LEGACY_PROG_ID}"), "Markdown Document").unwrap();
        reg_add_default(
            &format!(r"{test_root}\{LEGACY_PROG_ID}\DefaultIcon"),
            &old_icon,
        )
        .unwrap();
        reg_add_default(
            &format!(r"{test_root}\{LEGACY_PROG_ID}\shell\open\command"),
            &old_cmd,
        )
        .unwrap();
        reg_add_default(
            &format!(r"{test_root}\*\shell\{LEGACY_SHELL_KEY_NAME}"),
            "Open with Markdown Word Studio",
        )
        .unwrap();
        reg_add_default(
            &format!(r"{test_root}\*\shell\{LEGACY_SHELL_KEY_NAME}\command"),
            &old_cmd,
        )
        .unwrap();

        let st_before = query_integration_status_under(&test_root);
        assert!(st_before.shell_new_enabled);
        assert!(st_before.open_with_enabled);
        assert_eq!(st_before.registered_exe_path.as_ref(), Some(&old_exe));
        assert!(has_legacy_keys_under(&test_root));

        // Sync migrates legacy keys to MarkdownStudio and removes legacy keys
        assert!(sync_exe_path_if_registered_under(&test_root, &new_exe).unwrap());
        assert!(!has_legacy_keys_under(&test_root));

        let st_after = query_integration_status_under(&test_root);
        assert!(st_after.shell_new_enabled);
        assert!(st_after.open_with_enabled);
        assert_eq!(st_after.registered_exe_path.as_ref(), Some(&new_exe));
        assert_eq!(
            reg_query_value(&format!(r"{test_root}\.md"), None).as_deref(),
            Some(PROG_ID)
        );

        reg_delete_tree(&test_base);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn refreshes_stale_muicache_and_application_friendly_name() {
        let test_base = format!(
            r"HKCU\Software\MarkdownStudioMuiTest_{}",
            std::process::id()
        );
        let test_root = format!(r"{test_base}\Classes");
        let exe = PathBuf::from(r"D:\Production\Markdown Studio\Markdown Studio.exe");

        reg_delete_tree(&test_base);

        // Register first, then simulate Windows MuiCache holding stale "Markdown Word Studio"
        // plus stale Applications\Markdown Word Studio.exe and missing ApplicationName subkey.
        register_integration_under(&test_root, &exe, true, true).unwrap();
        reg_delete_tree(&format!(r"{test_root}\{PROG_ID}\Application"));
        reg_add_named(
            &mui_cache_key(&test_root),
            &mui_friendly_value_name(&exe),
            "Markdown Word Studio",
        )
        .unwrap();
        reg_add_named(
            &format!(r"{test_root}\Applications\Markdown Word Studio.exe"),
            "FriendlyAppName",
            "Markdown Word Studio",
        )
        .unwrap();

        assert!(sync_exe_path_if_registered_under(&test_root, &exe).unwrap());
        assert!(!sync_exe_path_if_registered_under(&test_root, &exe).unwrap());

        assert_eq!(
            reg_query_value(
                &mui_cache_key(&test_root),
                Some(&mui_friendly_value_name(&exe)),
            )
            .as_deref(),
            Some("Markdown Studio")
        );
        assert_eq!(
            reg_query_value(
                &format!(r"{test_root}\{PROG_ID}\Application"),
                Some("ApplicationName"),
            )
            .as_deref(),
            Some("Markdown Studio")
        );
        assert_eq!(
            reg_query_value(
                &format!(r"{test_root}\Applications\Markdown Studio.exe"),
                Some("FriendlyAppName"),
            )
            .as_deref(),
            Some("Markdown Studio")
        );
        assert!(!reg_key_exists(&format!(
            r"{test_root}\Applications\Markdown Word Studio.exe"
        )));

        reg_delete_tree(&test_base);
    }
}
