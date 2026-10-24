//! HurenTohn Plugin Manager: releases, silent installs and builds of main
//! for Jonas Grunau's audio plugins.

// No console window next to the app on Windows (release builds).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod auth;
mod build;
mod config;
mod github;
mod installed;
mod manifest;
mod platform;
mod privileged;
mod theme;

/// The window icon (Windows taskbar; macOS takes the bundle's .icns).
fn icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../assets/AppIcon.png")).map(|i| i.thumbnail(256, 256).to_rgba8());
    match img {
        Ok(i) => egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() },
        Err(_) => egui::IconData::default(),
    }
}

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("HurenTohn Plugin Manager")
        .with_app_id("de.grunau.hurentohn")
        .with_inner_size([theme::sheet::W, 850.0])
        .with_min_inner_size([660.0, 480.0])
        .with_icon(icon());
    // The app draws its own themed title bar. macOS keeps the system frame
    // under it (traffic lights, rounded corners, resizing) with the bar made
    // transparent; Windows gets no system frame and the app's own keys.
    #[cfg(target_os = "macos")]
    {
        viewport = viewport.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false);
    }
    #[cfg(windows)]
    {
        viewport = viewport.with_decorations(false);
    }
    if std::env::var_os("HURENTOHN_SNAPSHOT").is_some() {
        // A covered window gets no frames, so the screenshot would never come.
        viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnTop);
        // HURENTOHN_SNAPSHOT_SIZE=1600x1100 checks how the sheet holds up in a big window.
        if let Some((w, h)) = std::env::var("HURENTOHN_SNAPSHOT_SIZE").ok().and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))
        }) {
            viewport = viewport.with_inner_size([w, h]);
        }
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(90));
            eprintln!("snapshot: no frame settled within 90 s");
            std::process::exit(3);
        });
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("HurenTohn Plugin Manager", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
