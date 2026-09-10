#[cfg(target_os = "macos")]
mod app;
mod app_icon;
mod config;
#[cfg(test)]
mod content_preservation_tests;
mod graphics;
mod glyph_atlas;
mod grid;
mod input;
mod layout;
#[cfg(any(target_os = "linux", test))]
mod launch_options;
#[cfg(target_os = "linux")]
mod linux_window;
#[cfg(target_os = "linux")]
mod linux_clipboard;
#[cfg(target_os = "macos")]
mod pane;
mod parser;
mod pty;
mod render_scene;
mod selection;
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod software_raster;
#[cfg(target_os = "linux")]
mod software_graphics;
mod terminal_decoder;
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod terminal_reader;
mod terminal_writer;
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod terminal_colors;
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
    let options = match launch_options::parse(std::env::args_os().skip(1)) {
        Ok(launch_options::LaunchAction::Run(options)) => options,
        Ok(launch_options::LaunchAction::Help) => {
            println!("{}", launch_options::HELP);
            return std::process::ExitCode::SUCCESS;
        }
        Ok(launch_options::LaunchAction::Version) => {
            println!("kokuban {}", env!("CARGO_PKG_VERSION"));
            return std::process::ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("kokuban: {error}\nTry 'kokuban --help' for usage.");
            return std::process::ExitCode::from(2);
        }
    };
    env_logger::init();
    let config = config::Config::load();
    if let Some(directory) = &options.working_directory {
        if let Err(error) = std::env::set_current_dir(directory) {
            eprintln!(
                "kokuban: could not open working directory {}: {error}",
                directory.display()
            );
            return std::process::ExitCode::FAILURE;
        }
        // Set before starting any renderer or terminal worker threads.
        if let Ok(directory) = std::env::current_dir() {
            std::env::set_var("PWD", directory);
        }
    }
    log::info!("黒板kokuban starting: {}x{} terminal", config.window.columns, config.window.rows);

    match linux_window::launch(config, options) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kokuban: failed to initialize Linux window: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("kokuban currently supports only macOS and Linux targets");
