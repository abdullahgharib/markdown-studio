#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use markdown_studio::ui::instance::{
    default_instance_lock_path, try_forward_files_to_running_instance, LaunchArgs,
};
use markdown_studio::ui::MarkdownApp;

fn main() -> eframe::Result<()> {
    let launch = LaunchArgs::from_args(std::env::args().skip(1));
    if !launch.new_window && !launch.files.is_empty() {
        let lock_path = default_instance_lock_path();
        if let Ok(true) = try_forward_files_to_running_instance(&lock_path, &launch.files) {
            return Ok(());
        }
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1360.0, 880.0])
        .with_min_inner_size([480.0, 360.0])
        .with_title(format!("Markdown Studio v{}", markdown_studio::APP_VERSION))
        .with_icon(Arc::new(build_app_icon()));
    if let Some((x, y)) = launch.window_pos {
        viewport = viewport.with_position([x, y]);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Markdown Studio",
        native_options,
        Box::new(move |cc| Ok(Box::new(MarkdownApp::new_with_launch(cc, launch)))),
    )
}

/// Generates a crisp 64x64 RGBA Markdown `M↓` badge icon.
fn build_app_icon() -> egui::IconData {
    let size = 64u32;
    let rgba = markdown_studio::icon::render_app_icon_rgba(size as usize);

    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}
