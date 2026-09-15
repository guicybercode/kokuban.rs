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
#[cfg(target_os = "linux")]
mod omarchy_theme;
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

/// Apps opened from Finder, the Dock or Launchpad start in `/`. Use the home
/// directory instead so shells and `kokuban.toml` lookup behave as in a terminal.
#[cfg(any(target_os = "macos", test))]
fn launch_directory(
    current: &std::path::Path,
    home: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    let home = std::path::PathBuf::from(home?);
    (current == std::path::Path::new("/") && home.is_absolute() && home != current).then_some(home)
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    env_logger::init();

    if let Ok(current) = std::env::current_dir() {
        if let Some(home) = launch_directory(&current, std::env::var_os("HOME")) {
            match std::env::set_current_dir(&home) {
                // Set before starting any renderer or terminal worker threads.
                Ok(()) => std::env::set_var("PWD", &home),
                Err(error) => log::warn!("Could not start in {}: {error}", home.display()),
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::launch_directory;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    #[test]
    fn app_launched_from_root_starts_in_home() {
        assert_eq!(
            launch_directory(Path::new("/"), Some(OsString::from("/Users/kokuban"))),
            Some(PathBuf::from("/Users/kokuban"))
        );
    }

    #[test]
    fn explicit_directories_and_unusable_homes_are_kept() {
        let home = || Some(OsString::from("/Users/kokuban"));
        assert_eq!(launch_directory(Path::new("/tmp/project"), home()), None);
        assert_eq!(launch_directory(Path::new("/"), None), None);
        assert_eq!(launch_directory(Path::new("/"), Some(OsString::from("relative"))), None);
        assert_eq!(launch_directory(Path::new("/"), Some(OsString::from("/"))), None);
    }
}
