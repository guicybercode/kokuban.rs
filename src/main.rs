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
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod software_raster;
mod terminal_decoder;
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod terminal_reader;
#[cfg(target_os = "linux")]
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("kokuban currently supports only macOS and Linux targets");
