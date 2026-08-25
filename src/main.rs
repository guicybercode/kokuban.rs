#[cfg(target_os = "macos")]
mod app;
mod config;
mod graphics;
mod glyph_atlas;
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
// Land the portable compositor independently from its Linux window call site.
#[allow(dead_code)]
mod software_raster;
#[cfg(target_os = "macos")]
mod renderer;

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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("kokuban currently supports only macOS and Linux targets");
