pub mod window;

use crate::config::Config;
use crate::grid::Grid;
use crate::parser::ansi::Utf8Parser;
use crate::pty::Pty;
use crate::renderer::atlas::GlyphAtlas;

use objc2::MainThreadMarker;
use objc2_app_kit::*;
use objc2_foundation::*;
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use window::create_terminal_view;

pub fn launch(config: Config) {
    let mtm = MainThreadMarker::new().expect("Must be called from the main thread");

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // Create menu bar
    setup_menu_bar(&app, mtm);

    // Create Metal device
    let device = MTLCreateSystemDefaultDevice()
        .expect("Failed to create Metal device — is this Apple Silicon?");
    log::info!("Metal device: {:?}", device.name());

    // Create grid
    let cols = config.window.columns as usize;
    let rows = config.window.rows as usize;
    let grid = Arc::new(Mutex::new(Grid::new(cols, rows)));

    // Default to 2.0 for Retina; viewDidChangeBackingProperties adjusts if needed
    let scale_factor = 2.0f32;
    let atlas = Arc::new(Mutex::new(GlyphAtlas::new(
        &config.font.family,
        config.font.size,
        scale_factor,
    )));

    // Spawn PTY
    let pty = Arc::new(
        Pty::spawn(config.window.columns, config.window.rows)
            .expect("Failed to spawn PTY"),
    );

    let dirty = Arc::new(AtomicBool::new(true));
    let should_close = Arc::new(AtomicBool::new(false));

    // Parse colors
    let default_fg = crate::config::ColorConfig::parse_hex(&config.colors.foreground);
    let default_bg = crate::config::ColorConfig::parse_hex(&config.colors.background);

    // Get cell dimensions for window sizing
    let (cell_w, cell_h) = {
        let atlas = atlas.lock().unwrap();
        (atlas.cell_width, atlas.cell_height)
    };

    // Create window
    let content_width = cell_w * cols as f32;
    let content_height = cell_h * rows as f32;
    let window_width = content_width / scale_factor;
    let window_height = content_height / scale_factor;

    let window_rect = NSRect::new(
        NSPoint::new(200.0, 200.0),
        NSSize::new(window_width as f64, window_height as f64),
    );

    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;

    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            window_rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };

    window.setTitle(&NSString::from_str("黒板kokuban"));
    window.setMinSize(NSSize::new(200.0, 150.0));
    window.setAcceptsMouseMovedEvents(true);

    // Create terminal view
    let view = create_terminal_view(
        mtm,
        &device,
        grid.clone(),
        atlas.clone(),
        pty.clone(),
        dirty.clone(),
        default_fg,
        default_bg,
        scale_factor,
        config.font.family.clone(),
        config.font.size,
    );

    window.setContentView(Some(&view));
    window.makeFirstResponder(Some(&view));
    window.makeKeyAndOrderFront(None);
    window.center();

    // Start PTY reader thread
    let reader_grid = grid.clone();
    let reader_dirty = dirty.clone();
    let reader_should_close = should_close.clone();
    let reader_pty = pty.clone();

    std::thread::Builder::new()
        .name("pty-reader".to_string())
        .spawn(move || {
            let mut parser = Utf8Parser::new();
            let mut buf = [0u8; 4096];

            loop {
                if reader_should_close.load(Ordering::Relaxed) {
                    break;
                }

                match reader_pty.read(&mut buf) {
                    Ok(0) => {
                        log::info!("PTY EOF — child exited");
                        reader_should_close.store(true, Ordering::Relaxed);
                        break;
                    }
                    Ok(n) => {
                        let mut grid = reader_grid.lock().unwrap();
                        parser.feed(&buf[..n], &mut grid);
                        drop(grid);
                        reader_dirty.store(true, Ordering::Relaxed);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(e) => {
                        log::error!("PTY read error: {e}");
                        reader_should_close.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }

            log::info!("PTY reader thread exiting");
        })
        .expect("Failed to spawn PTY reader thread");

    // Set up render timer (60fps)
    let timer_dirty = dirty.clone();
    let timer_should_close = should_close.clone();
    let view_for_timer = view.clone();

    unsafe {
        let interval = 1.0 / 60.0;
        let timer_block = block2::RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            if timer_should_close.load(Ordering::Relaxed) {
                let mtm = MainThreadMarker::new().unwrap();
                let app = NSApplication::sharedApplication(mtm);
                app.terminate(None);
                return;
            }

            if timer_dirty.load(Ordering::Relaxed) {
                view_for_timer.setNeedsDisplay(true);
            }
        });

        let _timer = NSTimer::scheduledTimerWithTimeInterval_repeats_block(
            interval,
            true,
            &timer_block,
        );
    }

    // Activate app
    app.activate();

    log::info!("Starting application run loop");
    app.run();
}

fn setup_menu_bar(app: &NSApplication, mtm: MainThreadMarker) {
    unsafe {
        let menu_bar = NSMenu::new(mtm);
        let app_menu_item = NSMenuItem::new(mtm);
        menu_bar.addItem(&app_menu_item);

        let app_menu = NSMenu::new(mtm);
        let quit_title = NSString::from_str("Quit 黒板kokuban");
        let quit_key = NSString::from_str("q");
        let quit_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &quit_title,
            Some(objc2::sel!(terminate:)),
            &quit_key,
        );
        app_menu.addItem(&quit_item);
        app_menu_item.setSubmenu(Some(&app_menu));

        app.setMainMenu(Some(&menu_bar));
    }
}
