mod app;
mod config;
mod grid;
mod input;
mod parser;
mod pty;
mod renderer;

fn main() {
    env_logger::init();

    let config = config::Config::load();
    log::info!("黒板kokuban starting: {}x{} terminal", config.window.columns, config.window.rows);

    app::launch(config);
}
