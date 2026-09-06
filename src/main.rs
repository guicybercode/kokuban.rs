#[cfg(target_os = "macos")]
mod app;
mod config;
mod graphics;
#[cfg(not(target_os = "android"))]
mod glyph_atlas;
#[cfg(target_os = "android")]
#[path = "android_glyph_atlas.rs"]
mod glyph_atlas;
#[cfg(test)]
mod android_runtime;
#[cfg(all(test, not(target_os = "android")))]
mod android_glyph_atlas;
mod grid;
mod input;
mod layout;
#[cfg(target_os = "linux")]
mod linux_window;
#[cfg(target_os = "macos")]
mod pane;
mod parser;
mod pty;
mod render_scene;
mod selection;
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod software_raster;
mod terminal_decoder;
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod terminal_reader;
mod terminal_writer;
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod terminal_colors;
#[cfg(target_os = "macos")]
mod renderer;
mod window_title;

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    env_logger::init();

    let config = config::Config::load();
    log::info!("黒板kokuban starting: {}x{} terminal", config.window.columns, config.window.rows);

    match app::launch(config) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kokuban: failed to initialize macOS app: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    env_logger::init();

    let config = config::Config::load();
    log::info!("黒板kokuban starting: {}x{} terminal", config.window.columns, config.window.rows);

    match linux_window::launch(config) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kokuban: failed to initialize Linux window: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "android")]
fn main() {
    // Android loads the cdylib via NativeActivity; build APKs with --lib.
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
compile_error!("kokuban supports macOS, Linux and Android targets");
