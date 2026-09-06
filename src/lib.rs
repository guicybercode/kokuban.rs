//! Android NativeActivity entry point. Desktop keeps its existing binary entry.
//! Both entries compile the same terminal sources; Android only replaces the
//! platform window and font discovery/rasterization.
#![cfg(target_os = "android")]
#![allow(dead_code)]

// Keep the Android input correction isolated from the desktop dependency.
extern crate winit_android as winit;

mod android_controls;
mod android_images;
mod android_ime;
mod android_input;
mod android_metrics;
mod android_runtime;
mod android_window;
mod config;
#[path = "android_glyph_atlas.rs"]
mod glyph_atlas;
mod graphics;
mod grid;
mod input;
mod layout;
mod parser;
mod pty;
mod render_scene;
mod selection;
use android_images as renderer;
mod software_graphics;
mod software_raster;
mod terminal_colors;
mod terminal_decoder;
mod terminal_reader;
mod terminal_writer;
mod window_title;

use winit::platform::android::activity::AndroidApp;

#[no_mangle]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_tag("Kokuban")
            .with_max_level(log::LevelFilter::Info),
    );
    if let Err(error) = android_window::launch(app) {
        log::error!("Android application failed: {error}");
    }
}
