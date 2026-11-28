use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, Margin, RichText, Stroke, Vec2};
use flate2::read::GzDecoder;

use crate::APP_VERSION;
use crate::ui::settings::{extract_str_field, find_json_value_slice, parse_json_string_at};
use crate::ui::theme::WordTheme;

pub const GITHUB_OWNER: &str = "abdullahgharib";
pub const GITHUB_REPO: &str = "markdown-studio";
pub const RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/abdullahgharib/markdown-studio/releases/latest";
pub const CHANGELOG_RAW_URL: &str =
    "https://raw.githubusercontent.com/abdullahgharib/markdown-studio/main/changelog.md";
pub const RELEASES_PAGE_URL: &str =
    "https://github.com/abdullahgharib/markdown-studio/releases/latest";

const MAX_RELEASE_JSON_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CHANGELOG_BYTES: u64 = 1024 * 1024;
const MAX_UPDATE_BINARY_BYTES: u64 = 150 * 1024 * 1024;

const TRUSTED_GITHUB_HOSTS: &[&str] = &[
    "api.github.com",
    "github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub version: String,
    pub title: String,
    pub body: String,
    pub html_url: String,
    pub published_at: String,
    pub asset: Option<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    Idle,
    Checking { silent: bool },
    UpToDate { checked_version: String },
    Available(ReleaseInfo),
    Downloading {
        info: ReleaseInfo,
        downloaded: u64,
        total: u64,
    },
    ReadyToInstall {
        info: ReleaseInfo,
        staged_path: PathBuf,
    },
    Error {
        message: String,
        open_releases_fallback: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateModalAction {
    None,
    DownloadAndInstall,
    InstallAndRestart,
    SkipVersion(String),
    OpenReleasePage(String),
}

enum UpdateEvent {
    CheckFinished {
        silent: bool,
        result: Result<CheckOutcome, String>,
    },
    DownloadProgress {
        info: ReleaseInfo,
        downloaded: u64,
        total: u64,
    },
    DownloadFinished {
        info: ReleaseInfo,
        result: Result<PathBuf, DownloadErr>,
    },
}

enum CheckOutcome {
    UpToDate(String),
    Available(ReleaseInfo),
}

struct DownloadErr {
    message: String,
    open_releases_fallback: bool,
}

/// Manages background GitHub update checks, release notes display, downloads, and modal state.
pub struct UpdateManager {
    pub status: UpdateStatus,
    pub modal_open: bool,
    pub auto_install_on_download: bool,
    rx: Option<Receiver<UpdateEvent>>,
}

impl Default for UpdateManager {
    fn default() -> Self {
        Self {
            status: UpdateStatus::Idle,
            modal_open: false,
            auto_install_on_download: false,
            rx: None,
        }
    }
}

impl UpdateManager {
    pub fn is_busy(&self) -> bool {
        matches!(
            self.status,
            UpdateStatus::Checking { .. } | UpdateStatus::Downloading { .. }
        )
    }

    pub fn available_info(&self) -> Option<&ReleaseInfo> {
        match &self.status {
            UpdateStatus::Available(info)
            | UpdateStatus::Downloading { info, .. }
            | UpdateStatus::ReadyToInstall { info, .. } => Some(info),
            _ => None,
        }
    }

    /// Returns the newer version string to show in the top-bar badge unless it was skipped.
    pub fn visible_badge_version(&self, skipped_version: Option<&str>) -> Option<&str> {
        let info = self.available_info()?;
        if let Some(skipped) = skipped_version
            && normalize_version_str(skipped) == normalize_version_str(&info.version)
        {
            return None;
        }
        Some(info.version.as_str())
    }

    /// Starts an asynchronous GitHub Releases check.
    pub fn start_check(&mut self, ctx: &egui::Context, silent: bool) {
        if self.is_busy() {
            return;
        }
        self.status = UpdateStatus::Checking { silent };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let repaint_ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = fetch_latest_release_checked();
            let _ = tx.send(UpdateEvent::CheckFinished { silent, result });
            repaint_ctx.request_repaint();
        });
    }

    /// Starts an asynchronous binary download for the currently available release.
    pub fn start_download(&mut self, ctx: &egui::Context) {
        let info = match &self.status {
            UpdateStatus::Available(info) => info.clone(),
            UpdateStatus::ReadyToInstall { .. } => return,
            _ => return,
        };
        let Some(asset) = info.asset.clone() else {
            let url = if info.html_url.is_empty() {
                RELEASES_PAGE_URL.to_string()
            } else {
                info.html_url.clone()
            };
            self.status = UpdateStatus::Error {
                message: format!(
                    "No compatible {} release asset found for v{}. Please download manually from GitHub Releases.",
                    std::env::consts::OS,
                    info.version
                ),
                open_releases_fallback: true,
            };
            let _ = url;
            return;
        };

        let dest_dir = match current_exe_dir() {
            Ok(d) => d,
            Err(err) => {
                self.status = UpdateStatus::Error {
                    message: err,
                    open_releases_fallback: true,
                };
                return;
            }
        };

        let total = asset.size;
        self.auto_install_on_download = true;
        self.status = UpdateStatus::Downloading {
            info: info.clone(),
            downloaded: 0,
            total,
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let repaint_ctx = ctx.clone();
        std::thread::spawn(move || {
            let tx_prog = tx.clone();
            let info_prog = info.clone();
            let ctx_prog = repaint_ctx.clone();
            let result = download_update_to_temp(&asset, &dest_dir, |downloaded, total_bytes| {
                let _ = tx_prog.send(UpdateEvent::DownloadProgress {
                    info: info_prog.clone(),
                    downloaded,
                    total: total_bytes,
                });
                ctx_prog.request_repaint();
            });
            let _ = tx.send(UpdateEvent::DownloadFinished { info, result });
            repaint_ctx.request_repaint();
        });
    }

    /// Polls background check/download events and returns `(status_msg, ready_to_auto_install)`.
    pub fn poll(&mut self) -> (Option<String>, bool) {
        let mut status_msg = None;
        let mut trigger_install = false;
        let Some(rx) = self.rx.take() else {
            return (None, false);
        };
        let mut keep_rx = true;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                UpdateEvent::CheckFinished { silent, result } => {
                    keep_rx = false;
                    match result {
                        Ok(CheckOutcome::UpToDate(ver)) => {
                            self.status = UpdateStatus::UpToDate {
                                checked_version: ver,
                            };
                            if !silent {
                                status_msg = Some(format!(
                                    "You're up to date (v{APP_VERSION})"
                                ));
                            }
                        }
                        Ok(CheckOutcome::Available(info)) => {
                            let ver = info.version.clone();
                            self.status = UpdateStatus::Available(info);
                            if !silent {
                                status_msg = Some(format!("Update v{ver} is available!"));
                            }
                        }
                        Err(err) => {
                            if silent {
                                self.status = UpdateStatus::Idle;
                            } else {
                                self.status = UpdateStatus::Error {
                                    message: err,
                                    open_releases_fallback: true,
                                };
                            }
                        }
                    }
                }
                UpdateEvent::DownloadProgress {
                    info,
                    downloaded,
                    total,
                } => {
                    self.status = UpdateStatus::Downloading {
                        info,
                        downloaded,
                        total,
                    };
                }
                UpdateEvent::DownloadFinished { info, result } => {
                    keep_rx = false;
                    match result {
                        Ok(staged_path) => {
                            let ver = info.version.clone();
                            self.status = UpdateStatus::ReadyToInstall { info, staged_path };
                            status_msg = Some(format!("Downloaded Markdown Studio v{ver}"));
                            if self.auto_install_on_download {
                                self.auto_install_on_download = false;
                                trigger_install = true;
                            }
                        }
                        Err(err) => {
                            self.auto_install_on_download = false;
                            self.status = UpdateStatus::Error {
                                message: err.message,
                                open_releases_fallback: err.open_releases_fallback,
                            };
                        }
                    }
                }
            }
        }
        if keep_rx {
            self.rx = Some(rx);
        }
        (status_msg, trigger_install)
    }

    /// Renders the dedicated Update Available / What's Changed modal window.
    pub fn show_modal(
        &mut self,
        ctx: &egui::Context,
        theme: &WordTheme,
        skipped_version: Option<&str>,
    ) -> UpdateModalAction {
        let mut action = UpdateModalAction::None;
        if !self.modal_open {
            return action;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.modal_open = false;
            return action;
        }

        let mut keep_open = true;
        egui::Window::new("Software Update")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(540.0)
            .show(ctx, |ui| match self.status.clone() {
                UpdateStatus::Idle | UpdateStatus::Checking { .. } => {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Checking GitHub Releases for updates...")
                                .size(13.5)
                                .color(theme.text_primary),
                        );
                    });
                    ui.add_space(10.0);
                }
                UpdateStatus::UpToDate { checked_version } => {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "You're running the latest version (v{APP_VERSION})."
                        ))
                        .strong()
                        .size(14.0)
                        .color(theme.text_primary),
                    );
                    if !checked_version.is_empty() {
                        ui.label(
                            RichText::new(format!("Latest GitHub release: v{checked_version}"))
                                .size(12.0)
                                .color(theme.text_muted),
                        );
                    }
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.modal_open = false;
                        }
                    });
                }
                UpdateStatus::Available(info) => {
                    render_release_header(ui, &info, theme);
                    render_release_notes_box(ui, &info.body, theme, 260.0);
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);
                    let already_skipped = skipped_version.is_some_and(|s| {
                        normalize_version_str(s) == normalize_version_str(&info.version)
                    });
                    ui.horizontal(|ui| {
                        let primary_btn = egui::Button::new(
                            RichText::new(if info.asset.is_some() {
                                "Download & Install Update"
                            } else {
                                "Open Release Page"
                            })
                            .strong()
                            .color(Color32::WHITE),
                        )
                        .fill(theme.accent)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(6));
                        if ui.add(primary_btn).clicked() {
                            if info.asset.is_some() {
                                action = UpdateModalAction::DownloadAndInstall;
                            } else {
                                let url = if info.html_url.is_empty() {
                                    RELEASES_PAGE_URL.to_string()
                                } else {
                                    info.html_url.clone()
                                };
                                action = UpdateModalAction::OpenReleasePage(url);
                            }
                        }

                        if !already_skipped && ui.button("Skip This Version").clicked() {
                            action = UpdateModalAction::SkipVersion(info.version.clone());
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Later").clicked() {
                                self.modal_open = false;
                            }
                            if info.asset.is_some() && ui.button("GitHub Page").clicked() {
                                let url = if info.html_url.is_empty() {
                                    RELEASES_PAGE_URL.to_string()
                                } else {
                                    info.html_url.clone()
                                };
                                action = UpdateModalAction::OpenReleasePage(url);
                            }
                        });
                    });
                }
                UpdateStatus::Downloading {
                    info,
                    downloaded,
                    total,
                } => {
                    render_release_header(ui, &info, theme);
                    ui.add_space(4.0);
                    let frac = if total > 0 {
                        (downloaded as f32 / total as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let mb_done = downloaded as f64 / (1024.0 * 1024.0);
                    let progress_text = if total > 0 {
                        let mb_tot = total as f64 / (1024.0 * 1024.0);
                        format!("Downloading v{}... {mb_done:.1} / {mb_tot:.1} MB", info.version)
                    } else {
                        format!("Downloading v{}... {mb_done:.1} MB", info.version)
                    };
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .text(progress_text)
                            .animate(true),
                    );
                    ui.add_space(8.0);
                }
                UpdateStatus::ReadyToInstall { info, .. } => {
                    render_release_header(ui, &info, theme);
                    ui.label(
                        RichText::new(format!(
                            "Markdown Studio v{} is downloaded and ready to install.",
                            info.version
                        ))
                        .size(13.0)
                        .color(theme.text_primary),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let install_btn = egui::Button::new(
                            RichText::new("Install & Restart Now")
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(theme.accent)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(6));
                        if ui.add(install_btn).clicked() {
                            action = UpdateModalAction::InstallAndRestart;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                self.modal_open = false;
                            }
                        });
                    });
                }
                UpdateStatus::Error {
                    message,
                    open_releases_fallback,
                } => {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Update could not be completed")
                            .strong()
                            .size(14.0)
                            .color(theme.warning_amber),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(message)
                            .size(12.5)
                            .color(theme.text_primary),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if open_releases_fallback
                            && ui.button("Open GitHub Releases").clicked()
                        {
                            action = UpdateModalAction::OpenReleasePage(
                                RELEASES_PAGE_URL.to_string(),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Close").clicked() {
                                self.modal_open = false;
                            }
                        });
                    });
                }
            });

        if !keep_open {
            self.modal_open = false;
        }
        action
    }
}

fn render_release_header(ui: &mut egui::Ui, info: &ReleaseInfo, theme: &WordTheme) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("Markdown Studio v{} is available", info.version))
                .strong()
                .size(15.0)
                .color(theme.text_primary),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("Current: v{APP_VERSION}"))
                    .size(11.5)
                    .color(theme.text_muted),
            );
        });
    });
    if !info.published_at.is_empty() {
        let date_only = info
            .published_at
            .split('T')
            .next()
            .unwrap_or(&info.published_at);
        let size_note = info
            .asset
            .as_ref()
            .filter(|a| a.size > 0)
            .map(|a| format!("  ·  {:.1} MB", a.size as f64 / (1024.0 * 1024.0)))
            .unwrap_or_default();
        ui.label(
            RichText::new(format!("Released {date_only}{size_note}"))
                .size(11.5)
                .color(theme.text_muted),
        );
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new("What's Changed")
            .strong()
            .size(12.5)
            .color(theme.accent),
    );
    ui.add_space(4.0);
}

/// Renders scrollable release notes inside Settings or the Update modal.
pub fn render_release_notes_box(
    ui: &mut egui::Ui,
    body: &str,
    theme: &WordTheme,
    max_height: f32,
) {
    let display_notes = if body.trim().is_empty() {
        "No release notes were provided for this release."
    } else {
        body.trim()
    };
    egui::Frame::new()
        .fill(theme.pill_bg)
        .stroke(Stroke::new(1.0_f32, theme.border_subtle))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .id_salt("update_whats_changed_scroll")
                .show(ui, |ui| {
                    for line in display_notes.lines() {
                        let trimmed = line.trim_end();
                        if trimmed.is_empty() {
                            ui.add_space(4.0);
                        } else if let Some(h) = trimmed.strip_prefix("### ") {
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new(h.trim())
                                    .strong()
                                    .size(12.5)
                                    .color(theme.accent),
                            );
                        } else if let Some(h) = trimmed.strip_prefix("## ") {
                            ui.add_space(3.0);
                            ui.label(
                                RichText::new(h.trim())
                                    .strong()
                                    .size(13.0)
                                    .color(theme.text_primary),
                            );
                        } else {
                            ui.label(
                                RichText::new(trimmed)
                                    .size(12.0)
                                    .color(theme.text_primary),
                            );
                        }
                    }
                });
        });
}

/// Removes leftover `<exe>.old` left behind from an earlier self-update on Windows.
pub fn cleanup_old_binary_on_startup() {
    let Ok(cur) = std::env::current_exe() else {
        return;
    };
    let old = old_backup_path_for(&cur);
    if old.exists() {
        let _ = fs::remove_file(&old);
    }
}

fn old_backup_path_for(exe: &Path) -> PathBuf {
    let mut name = exe
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "Markdown Studio.exe".into());
    name.push(".old");
    exe.with_file_name(name)
}

fn current_exe_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Could not determine current executable path: {e}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Current executable has no parent directory.".to_string())
}

pub fn normalize_version_str(raw: &str) -> &str {
    let t = raw.trim();
    t.strip_prefix('v')
        .or_else(|| t.strip_prefix('V'))
        .unwrap_or(t)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedSemver {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub prerelease: bool,
}

pub fn parse_semver(raw: &str) -> Option<ParsedSemver> {
    let clean = normalize_version_str(raw);
    let (core, prerelease) = match clean.split_once(['-', '+']) {
        Some((lhs, _rhs)) if clean.contains('-') => (lhs, true),
        Some((lhs, _)) => (lhs, false),
        None => (clean, false),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u32>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u32>().ok()?;
    Some(ParsedSemver {
        major,
        minor,
        patch,
        prerelease,
    })
}

pub fn is_newer_version(latest: &str, current: &str) -> bool {
    let Some(l) = parse_semver(latest) else {
        return false;
    };
    let Some(c) = parse_semver(current) else {
        return false;
    };
    (l.major, l.minor, l.patch, !l.prerelease) > (c.major, c.minor, c.patch, !c.prerelease)
}

pub fn is_trusted_github_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.len() < 8
        || !trimmed[..8].eq_ignore_ascii_case("https://")
        || trimmed.chars().any(|c| c.is_control())
    {
        return false;
    }
    let after_scheme = &trimmed[8..];
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let host = if let Some((h, port)) = authority.split_once(':') {
        if port != "443" {
            return false;
        }
        h
    } else {
        authority
    };
    let host_lower = host.to_ascii_lowercase();
    TRUSTED_GITHUB_HOSTS.contains(&host_lower.as_str())
}

fn updater_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .user_agent(format!("MarkdownStudio/{APP_VERSION}"))
        .build();
    ureq::Agent::new_with_config(config)
}

fn fetch_trusted_text(url: &str, max_bytes: u64, accept_header: Option<&str>) -> Result<String, String> {
    let agent = updater_agent();
    let mut current = url.trim().to_string();
    for _hop in 0..=5 {
        if !is_trusted_github_url(&current) {
            return Err(format!("Blocked untrusted URL host: {current}"));
        }
        let mut req = agent.get(&current);
        if let Some(acc) = accept_header {
            req = req.header("Accept", acc);
        }
        let resp = req
            .call()
            .map_err(|e| format!("Network request failed: {e}"))?;
        if resp.status().is_redirection() {
            let loc = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| "Redirect response missing Location header".to_string())?;
            let resolved = resolve_github_redirect(&current, loc)?;
            current = resolved;
            continue;
        }
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("GitHub returned HTTP {status}"));
        }
        let bytes = resp
            .into_body()
            .with_config()
            .limit(max_bytes)
            .read_to_vec()
            .map_err(|e| format!("Failed reading response body: {e}"))?;
        return String::from_utf8(bytes)
            .map_err(|_| "GitHub response was not valid UTF-8".to_string());
    }
    Err("Too many redirects while contacting GitHub".to_string())
}

fn resolve_github_redirect(current: &str, location: &str) -> Result<String, String> {
    let loc = location.trim();
    if is_trusted_github_url(loc) {
        return Ok(loc.to_string());
    }
    if loc.starts_with('/') {
        let after_scheme = current
            .get(8..)
            .ok_or_else(|| "Invalid redirect base URL".to_string())?;
        let authority = after_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default();
        let candidate = format!("https://{authority}{loc}");
        if is_trusted_github_url(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!("Blocked redirect to untrusted location: {loc}"))
}

fn fetch_latest_release_checked() -> Result<CheckOutcome, String> {
    let json_text = fetch_trusted_text(
        RELEASES_LATEST_URL,
        MAX_RELEASE_JSON_BYTES,
        Some("application/vnd.github+json"),
    )?;
    let mut info = parse_github_release_json(&json_text)?;
    if !is_newer_version(&info.version, APP_VERSION) {
        return Ok(CheckOutcome::UpToDate(info.version));
    }
    if info.body.trim().is_empty()
        && let Ok(changelog_md) = fetch_trusted_text(CHANGELOG_RAW_URL, MAX_CHANGELOG_BYTES, None)
        && let Some(notes) = extract_changelog_section(&changelog_md, &info.version)
    {
        info.body = notes;
    }
    Ok(CheckOutcome::Available(info))
}

/// Parses the GitHub Releases API JSON response without pulling in `serde`.
pub fn parse_github_release_json(raw: &str) -> Result<ReleaseInfo, String> {
    let fields = top_level_json_object_fields(raw)
        .ok_or_else(|| "Invalid GitHub Releases JSON object".to_string())?;

    let mut tag_name = String::new();
    let mut title = String::new();
    let mut body = String::new();
    let mut html_url = String::new();
    let mut published_at = String::new();
    let mut assets_slice: Option<&str> = None;

    for (key, val) in fields {
        match key.as_str() {
            "tag_name" => {
                if let Some((s, _)) = parse_json_string_at(val) {
                    tag_name = s;
                }
            }
            "name" => {
                if let Some((s, _)) = parse_json_string_at(val) {
                    title = s;
                }
            }
            "body" => {
                if let Some((s, _)) = parse_json_string_at(val) {
                    body = s;
                }
            }
            "html_url" => {
                if let Some((s, _)) = parse_json_string_at(val)
                    && is_trusted_github_url(&s)
                {
                    html_url = s;
                }
            }
            "published_at" => {
                if let Some((s, _)) = parse_json_string_at(val) {
                    published_at = s;
                }
            }
            "assets" => {
                assets_slice = Some(val);
            }
            _ => {}
        }
    }

    if tag_name.trim().is_empty() {
        return Err("GitHub release payload missing tag_name".to_string());
    }
    let version = normalize_version_str(&tag_name).to_string();
    if title.trim().is_empty() {
        title = format!("Markdown Studio v{version}");
    }
    if html_url.is_empty() {
        html_url = RELEASES_PAGE_URL.to_string();
    }

    let assets = assets_slice
        .map(parse_release_assets_array)
        .unwrap_or_default();
    let asset = select_platform_asset(&assets, std::env::consts::OS);

    Ok(ReleaseInfo {
        tag_name,
        version,
        title,
        body,
        html_url,
        published_at,
        asset,
    })
}

fn parse_release_assets_array(arr_slice: &str) -> Vec<ReleaseAsset> {
    let mut out = Vec::new();
    let mut rest = match arr_slice.trim_start().strip_prefix('[') {
        Some(r) => r,
        None => return out,
    };
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
        if rest.is_empty() || rest.starts_with(']') {
            break;
        }
        if rest.starts_with('{') {
            let Some(end_idx) = find_matching_bracket_end(rest, '{', '}') else {
                break;
            };
            let obj = &rest[..=end_idx];
            let name = extract_str_field(obj, "name").unwrap_or_default();
            let download_url =
                extract_str_field(obj, "browser_download_url").unwrap_or_default();
            let size = find_json_value_slice(obj, "size")
                .and_then(|v| {
                    let digits: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
                    digits.parse::<u64>().ok()
                })
                .unwrap_or(0);
            if !name.is_empty() && is_trusted_github_url(&download_url) {
                out.push(ReleaseAsset {
                    name,
                    download_url,
                    size,
                });
            }
            rest = &rest[end_idx + 1..];
        } else {
            break;
        }
    }
    out
}

pub fn select_platform_asset(assets: &[ReleaseAsset], target_os: &str) -> Option<ReleaseAsset> {
    let exact_name = match target_os {
        "windows" => "Markdown-Studio-windows-x86_64.exe",
        "linux" => "Markdown-Studio-linux-x86_64.tar.gz",
        "macos" => "Markdown-Studio-macos-universal.tar.gz",
        _ => return None,
    };
    if let Some(exact) = assets.iter().find(|a| a.name.eq_ignore_ascii_case(exact_name)) {
        return Some(exact.clone());
    }
    assets
        .iter()
        .find(|a| {
            let low = a.name.to_ascii_lowercase();
            match target_os {
                "windows" => low.ends_with(".exe") && low.contains("windows"),
                "linux" => low.ends_with(".tar.gz") && low.contains("linux"),
                "macos" => low.ends_with(".tar.gz") && low.contains("macos"),
                _ => false,
            }
        })
        .cloned()
}

/// Extracts the release notes block for `version` (or the latest top section) from `changelog.md`.
pub fn extract_changelog_section(changelog_md: &str, version: &str) -> Option<String> {
    let clean_ver = normalize_version_str(version);
    let mut lines = changelog_md.lines().peekable();
    let mut found_header = false;
    let mut fallback_lines: Option<Vec<&str>> = None;
    let mut collected = Vec::new();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            if found_header {
                break;
            }
            if trimmed.contains(clean_ver) {
                found_header = true;
                continue;
            }
            if fallback_lines.is_none() {
                let mut fb = Vec::new();
                while let Some(&next_l) = lines.peek() {
                    if next_l.trim().starts_with("## ") {
                        break;
                    }
                    fb.push(lines.next().unwrap_or_default());
                }
                fallback_lines = Some(fb);
            }
        } else if found_header {
            collected.push(line);
        }
    }

    let chosen = if found_header {
        collected
    } else {
        fallback_lines.unwrap_or_default()
    };
    let joined = chosen.join("\n").trim().to_string();
    (!joined.is_empty()).then_some(joined)
}

fn download_update_to_temp(
    asset: &ReleaseAsset,
    dest_dir: &Path,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<PathBuf, DownloadErr> {
    if !is_trusted_github_url(&asset.download_url) {
        return Err(DownloadErr {
            message: format!("Untrusted download URL: {}", asset.download_url),
            open_releases_fallback: true,
        });
    }
    let write_probe = dest_dir.join(format!(".md_studio_probe_{}.tmp", std::process::id()));
    match fs::File::create(&write_probe) {
        Ok(f) => {
            drop(f);
            let _ = fs::remove_file(&write_probe);
        }
        Err(e) => {
            return Err(DownloadErr {
                message: format!(
                    "Cannot write to application folder ({}): {e}. Download manually from GitHub Releases.",
                    dest_dir.display()
                ),
                open_releases_fallback: true,
            });
        }
    }

    let agent = updater_agent();
    let mut current = asset.download_url.clone();
    let resp = {
        let mut final_resp = None;
        for _hop in 0..=5 {
            if !is_trusted_github_url(&current) {
                return Err(DownloadErr {
                    message: format!("Blocked untrusted download host: {current}"),
                    open_releases_fallback: true,
                });
            }
            let r = agent.get(&current).call().map_err(|e| DownloadErr {
                message: format!("Download request failed: {e}"),
                open_releases_fallback: true,
            })?;
            if r.status().is_redirection() {
                let loc = r
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| DownloadErr {
                        message: "Download redirect missing Location header".to_string(),
                        open_releases_fallback: true,
                    })?;
                current = resolve_github_redirect(&current, loc).map_err(|msg| DownloadErr {
                    message: msg,
                    open_releases_fallback: true,
                })?;
                continue;
            }
            let code = r.status().as_u16();
            if !(200..300).contains(&code) {
                return Err(DownloadErr {
                    message: format!("GitHub download returned HTTP {code}"),
                    open_releases_fallback: true,
                });
            }
            final_resp = Some(r);
            break;
        }
        final_resp.ok_or_else(|| DownloadErr {
            message: "Too many redirects during update download".to_string(),
            open_releases_fallback: true,
        })?
    };

    let content_len = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(asset.size);
    if content_len > MAX_UPDATE_BINARY_BYTES {
        return Err(DownloadErr {
            message: "Release binary exceeds maximum allowed update size.".to_string(),
            open_releases_fallback: true,
        });
    }

    let mut body = resp.into_body();
    let mut body_reader = body
        .with_config()
        .limit(MAX_UPDATE_BINARY_BYTES)
        .reader();

    let mut raw_bytes = Vec::with_capacity((content_len as usize).min(64 * 1024 * 1024));
    let mut chunk = [0u8; 64 * 1024];
    let mut last_notify = Instant::now();
    loop {
        let n = body_reader.read(&mut chunk).map_err(|e| DownloadErr {
            message: format!("Error reading update stream: {e}"),
            open_releases_fallback: true,
        })?;
        if n == 0 {
            break;
        }
        if raw_bytes.len() as u64 + n as u64 > MAX_UPDATE_BINARY_BYTES {
            return Err(DownloadErr {
                message: "Downloaded update exceeded 150 MB safety limit.".to_string(),
                open_releases_fallback: true,
            });
        }
        raw_bytes.extend_from_slice(&chunk[..n]);
        if last_notify.elapsed() >= Duration::from_millis(50) {
            on_progress(raw_bytes.len() as u64, content_len);
            last_notify = Instant::now();
        }
    }
    on_progress(raw_bytes.len() as u64, content_len);

    if asset.size > 0 && raw_bytes.len() as u64 != asset.size {
        return Err(DownloadErr {
            message: format!(
                "Downloaded file size mismatch (got {} bytes, expected {} bytes).",
                raw_bytes.len(),
                asset.size
            ),
            open_releases_fallback: true,
        });
    }

    let exec_bytes = if asset.name.to_ascii_lowercase().ends_with(".tar.gz") {
        extract_binary_from_tar_gz(&raw_bytes).map_err(|msg| DownloadErr {
            message: msg,
            open_releases_fallback: true,
        })?
    } else {
        raw_bytes
    };

    if !validate_executable_magic(&exec_bytes, std::env::consts::OS) {
        return Err(DownloadErr {
            message: "Downloaded update file failed executable header verification.".to_string(),
            open_releases_fallback: true,
        });
    }

    let staged_path = dest_dir.join(format!(".markdown_studio_update_{}.tmp", std::process::id()));
    let write_res = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&staged_path)?;
        f.write_all(&exec_bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_res {
        let _ = fs::remove_file(&staged_path);
        return Err(DownloadErr {
            message: format!("Failed to stage update binary: {e}"),
            open_releases_fallback: true,
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o755));
    }

    Ok(staged_path)
}

pub fn validate_executable_magic(bytes: &[u8], target_os: &str) -> bool {
    match target_os {
        "windows" => bytes.len() >= 2 && &bytes[..2] == b"MZ",
        "linux" => bytes.len() >= 4 && &bytes[..4] == b"\x7fELF",
        "macos" => {
            bytes.len() >= 4
                && matches!(
                    &bytes[..4],
                    [0xFE, 0xED, 0xFA, 0xCF]
                        | [0xCF, 0xFA, 0xED, 0xFE]
                        | [0xCA, 0xFE, 0xBA, 0xBE]
                        | [0xBE, 0xBA, 0xFE, 0xCA]
                )
        }
        _ => false,
    }
}

/// Extracts the `markdown_studio` binary payload from a `.tar.gz` archive in memory.
pub fn extract_binary_from_tar_gz(tar_gz_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = GzDecoder::new(tar_gz_bytes);
    let mut tar_bytes = Vec::new();
    decoder
        .by_ref()
        .take(MAX_UPDATE_BINARY_BYTES + 1)
        .read_to_end(&mut tar_bytes)
        .map_err(|e| format!("Failed to decompress update archive: {e}"))?;
    if tar_bytes.len() as u64 > MAX_UPDATE_BINARY_BYTES {
        return Err("Decompressed update archive exceeded safety limit.".to_string());
    }

    let mut offset = 0usize;
    let mut fallback: Option<Vec<u8>> = None;
    while offset + 512 <= tar_bytes.len() {
        let header = &tar_bytes[offset..offset + 512];
        offset += 512;
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let name_raw = read_c_str(&header[0..100]);
        let size = parse_tar_octal(&header[124..136])
            .ok_or_else(|| "Corrupt tar entry size in update archive.".to_string())?;
        let typeflag = header[156];
        let aligned = size.div_ceil(512) * 512;
        if offset.saturating_add(aligned) > tar_bytes.len() {
            return Err("Truncated tar entry in update archive.".to_string());
        }
        let payload = &tar_bytes[offset..offset + size];
        offset += aligned;

        if typeflag == b'0' || typeflag == 0 {
            let base = name_raw.rsplit('/').next().unwrap_or(&name_raw);
            if base == "markdown_studio" || base == "markdown_studio.exe" {
                return Ok(payload.to_vec());
            }
            if fallback.is_none() && !payload.is_empty() {
                fallback = Some(payload.to_vec());
            }
        }
    }
    fallback.ok_or_else(|| "Update archive did not contain an executable binary.".to_string())
}

fn read_c_str(slice: &[u8]) -> String {
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    String::from_utf8_lossy(&slice[..end]).to_string()
}

fn parse_tar_octal(slice: &[u8]) -> Option<usize> {
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    let s = std::str::from_utf8(&slice[..end]).ok()?.trim();
    if s.is_empty() {
        return Some(0);
    }
    usize::from_str_radix(s, 8).ok()
}

/// Atomically swaps the running executable with `staged_path` and launches the new version.
pub fn apply_staged_update_and_relaunch(staged_path: &Path) -> Result<(), String> {
    if !staged_path.exists() {
        return Err("Downloaded update file is no longer present on disk.".to_string());
    }
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Could not resolve executable path: {e}"))?;
    let old_exe = old_backup_path_for(&current_exe);
    if old_exe.exists() {
        let _ = fs::remove_file(&old_exe);
    }

    fs::rename(&current_exe, &old_exe).map_err(|e| {
        format!(
            "Could not move running executable aside ({}): {e}",
            current_exe.display()
        )
    })?;

    if let Err(e) = fs::rename(staged_path, &current_exe) {
        let _ = fs::rename(&old_exe, &current_exe);
        return Err(format!("Could not install new executable: {e}"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755));
    }

    Command::new(&current_exe).spawn().map_err(|e| {
        format!(
            "Update installed, but relaunch failed ({}): {e}",
            current_exe.display()
        )
    })?;
    Ok(())
}

fn top_level_json_object_fields(raw: &str) -> Option<Vec<(String, &str)>> {
    let s = raw.trim_start();
    let bytes = s.as_bytes();
    if *bytes.first()? != b'{' {
        return None;
    }
    let mut fields = Vec::new();
    let mut i = 1usize;
    let n = s.len();

    while i < n {
        while i < n && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r' | b',') {
            i += 1;
        }
        if i >= n || bytes[i] == b'}' {
            break;
        }
        if bytes[i] != b'"' {
            return None;
        }
        let (key, after_key) = parse_json_string_at(&s[i..])?;
        i = n - after_key.len();
        while i < n && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= n || bytes[i] != b':' {
            return None;
        }
        i += 1;
        while i < n && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= n {
            break;
        }
        let val_slice = &s[i..];
        fields.push((key, val_slice));
        i += json_value_span_len(val_slice)?;
    }

    Some(fields)
}

fn json_value_span_len(s: &str) -> Option<usize> {
    let first = *s.as_bytes().first()?;
    match first {
        b'"' => {
            let (_, after) = parse_json_string_at(s)?;
            Some(s.len() - after.len())
        }
        b'{' => find_matching_bracket_end(s, '{', '}').map(|idx| idx + 1),
        b'[' => find_matching_bracket_end(s, '[', ']').map(|idx| idx + 1),
        _ => {
            let end = s
                .find([',', '}', ']', '\n', '\r'])
                .unwrap_or(s.len());
            Some(end)
        }
    }
}

fn find_matching_bracket_end(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in s.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
        } else if ch == open {
            depth += 1;
        } else if ch == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(idx);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    #[test]
    fn semver_parsing_and_comparison_handles_v_prefix_and_prerelease() {
        assert!(is_newer_version("0.8.8", "0.8.7"));
        assert!(is_newer_version("v1.0.0", "0.9.99"));
        assert!(!is_newer_version("v0.8.7", "0.8.7"));
        assert!(!is_newer_version("0.8.6", "0.8.7"));
        assert!(!is_newer_version("0.8.7-beta.1", "0.8.7"));
        assert!(is_newer_version("0.8.7", "0.8.7-beta.1"));
        assert!(!is_newer_version("invalid", "0.8.7"));
    }

    #[test]
    fn trusted_github_url_allowlist_blocks_spoofing_and_http() {
        assert!(is_trusted_github_url(
            "https://api.github.com/repos/abdullahgharib/markdown-studio/releases/latest"
        ));
        assert!(is_trusted_github_url(
            "https://objects.githubusercontent.com/github-production-release-asset/123"
        ));
        assert!(is_trusted_github_url(
            "https://release-assets.githubusercontent.com/foo/bar"
        ));
        assert!(!is_trusted_github_url("http://github.com/a/b"));
        assert!(!is_trusted_github_url("https://evil.com/github.com"));
        assert!(!is_trusted_github_url("https://user@github.com/a/b"));
        assert!(!is_trusted_github_url("https://github.com:8443/a/b"));
    }

    #[test]
    fn parses_github_release_json_ignoring_nested_author_fields() {
        let payload = r###"{
            "url": "https://api.github.com/repos/abdullahgharib/markdown-studio/releases/1",
            "author": {
                "name": "Wrong Nested Author Name",
                "html_url": "https://github.com/wrong"
            },
            "tag_name": "v0.9.0",
            "name": "Markdown Studio v0.9.0 — Great Update",
            "html_url": "https://github.com/abdullahgharib/markdown-studio/releases/tag/v0.9.0",
            "published_at": "2026-11-28T03:00:00Z",
            "body": "## What's Changed\n- Fixed WYSIWYG Enter\n- Added Auto-Updater",
            "assets": [
                {
                    "name": "Markdown-Studio-linux-x86_64.tar.gz",
                    "size": 4096,
                    "browser_download_url": "https://github.com/abdullahgharib/markdown-studio/releases/download/v0.9.0/Markdown-Studio-linux-x86_64.tar.gz"
                },
                {
                    "name": "Markdown-Studio-windows-x86_64.exe",
                    "size": 8192,
                    "browser_download_url": "https://github.com/abdullahgharib/markdown-studio/releases/download/v0.9.0/Markdown-Studio-windows-x86_64.exe"
                },
                {
                    "name": "Markdown-Studio-macos-universal.tar.gz",
                    "size": 6144,
                    "browser_download_url": "https://github.com/abdullahgharib/markdown-studio/releases/download/v0.9.0/Markdown-Studio-macos-universal.tar.gz"
                }
            ]
        }"###;

        let info = parse_github_release_json(payload).expect("release JSON must parse");
        assert_eq!(info.tag_name, "v0.9.0");
        assert_eq!(info.version, "0.9.0");
        assert_eq!(info.title, "Markdown Studio v0.9.0 — Great Update");
        assert_eq!(
            info.html_url,
            "https://github.com/abdullahgharib/markdown-studio/releases/tag/v0.9.0"
        );
        assert!(info.body.contains("Fixed WYSIWYG Enter"));
        let asset = info.asset.expect("current OS asset must match");
        match std::env::consts::OS {
            "windows" => assert_eq!(asset.name, "Markdown-Studio-windows-x86_64.exe"),
            "linux" => assert_eq!(asset.name, "Markdown-Studio-linux-x86_64.tar.gz"),
            "macos" => assert_eq!(asset.name, "Markdown-Studio-macos-universal.tar.gz"),
            _ => {}
        }
    }

    #[test]
    fn extract_changelog_section_reads_matching_version() {
        let md = "# Changelog\n\n## [0.9.0] - 2026-12-01\n### Fixed\n- Fast Enter\n\n## [0.8.8] - 2026-11-28\n- Old\n";
        let section = extract_changelog_section(md, "0.9.0").expect("should find section");
        assert!(section.contains("Fast Enter"));
        assert!(!section.contains("Old"));
    }

    #[test]
    fn validate_executable_magic_checks_platform_headers() {
        assert!(validate_executable_magic(b"MZ\x90\x00", "windows"));
        assert!(!validate_executable_magic(b"<!DOCTYPE html>", "windows"));
        assert!(validate_executable_magic(b"\x7fELF\x02\x01", "linux"));
        assert!(validate_executable_magic(&[0xCF, 0xFA, 0xED, 0xFE], "macos"));
    }

    #[test]
    fn extract_binary_from_tar_gz_recovers_executable() {
        let payload = b"\x7fELFmockbinarycontent";
        let mut tar = vec![0u8; 1024];
        let name = b"markdown_studio";
        tar[..name.len()].copy_from_slice(name);
        let size_oct = format!("{:011o}\0", payload.len());
        tar[124..136].copy_from_slice(size_oct.as_bytes());
        tar[156] = b'0';
        tar[512..512 + payload.len()].copy_from_slice(payload);
        tar.extend_from_slice(&[0u8; 1024]);

        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&tar).unwrap();
        let gz = enc.finish().unwrap();

        let extracted = extract_binary_from_tar_gz(&gz).unwrap();
        assert_eq!(extracted, payload);
    }
}
