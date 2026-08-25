#[cfg(target_os = "macos")]
mod app;
mod config;
mod graphics;
mod grid;
mod input;
#[cfg(target_os = "linux")]
mod linux_window;
#[cfg(target_os = "macos")]
mod pane;
mod parser;
mod pty;
mod selection;
#[cfg(target_os = "macos")]
mod renderer;

#[cfg(target_os = "macos")]
fn main() {
    env_logger::init();

    let config = config::Config::load();
    log::info!("黒板kokuban starting: {}x{} terminal", config.window.columns, config.window.rows);

    app::launch(config);
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
