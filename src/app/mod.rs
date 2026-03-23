pub mod confirm;
pub mod selection;
pub mod window;

use crate::config::{ColorConfig, Config};
use crate::input::keybind::KeybindMap;
use crate::pane::layout::PixelRect;
use crate::pane::PaneTree;
use crate::renderer::atlas::GlyphAtlas;
use crate::renderer::image_store::{ImageFormat, ImageStore};
use crate::renderer::metal::ChromeColors;

use objc2::{MainThreadMarker, Message};
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

    setup_menu_bar(&app, mtm);

    let device = MTLCreateSystemDefaultDevice()
        .expect("Failed to create Metal device — is this Apple Silicon?");
    log::info!("Metal device: {:?}", device.name());

    let cols = config.window.columns;
    let rows = config.window.rows;
    let scrollback_max = config.window.scrollback_lines;
    let bg_opacity = config.window.opacity.clamp(0.0, 1.0);

    let pane_tree = Arc::new(Mutex::new(
        PaneTree::new(cols, rows, scrollback_max).expect("Failed to create initial pane"),
    ));

    let scale_factor = 2.0f32;
    let atlas = Arc::new(Mutex::new(GlyphAtlas::new(
        &config.font.family,
        config.font.size,
        scale_factor,
    )));

    let dirty = Arc::new(AtomicBool::new(true));
    let should_close = Arc::new(AtomicBool::new(false));

    let default_fg = ColorConfig::parse_hex(&config.colors.foreground);
    let default_bg = ColorConfig::parse_hex(&config.colors.background);
    let selection_fg = ColorConfig::parse_hex(&config.selection.foreground);
    let selection_bg = ColorConfig::parse_hex(&config.selection.background);

    let chrome = ChromeColors {
        sumi_dark: ColorConfig::parse_hex(&config.theme.chrome.sumi_dark),
        sumi_medium: ColorConfig::parse_hex(&config.theme.chrome.sumi_medium),
        sumi_light: ColorConfig::parse_hex(&config.theme.chrome.sumi_light),
        sumi_ghost: ColorConfig::parse_hex(&config.theme.chrome.sumi_ghost),
        hanko_red: ColorConfig::parse_hex(&config.theme.chrome.hanko_red),
        hanko_dim: ColorConfig::parse_hex(&config.theme.chrome.hanko_dim),
    };

    let keybinds = KeybindMap::from_config(&config.keybind);
    let resize_step = config.keybind.resize.step as f32;

    // Create shared image store for Kitty/Sixel image rendering
    let image_store = Arc::new(Mutex::new(ImageStore::new(
        device.retain(),
        config.images.max_memory_mb,
    )));
    let images_enabled = config.images.enabled;
    let kitty_enabled = config.images.kitty_enabled;
    let sixel_enabled = config.images.sixel_enabled;

    // Status bar height = 1.5× line height
    let status_bar_height = if config.status_bar.enabled {
        let a = atlas.lock().unwrap();
        (a.cell_height * 1.5).ceil()
    } else {
        0.0
    };

    // Cell dimensions for window sizing
    let (cell_w, cell_h) = {
        let a = atlas.lock().unwrap();
        (a.cell_width, a.cell_height)
    };

    // Set default colors and cell dimensions on initial pane
    {
        let mut tree = pane_tree.lock().unwrap();
        let fg_hex = config.colors.foreground.trim_start_matches('#').to_string();
        let bg_hex = config.colors.background.trim_start_matches('#').to_string();
        for id in tree.pane_ids() {
            if let Some(pane) = tree.pane_mut(id) {
                pane.grid.default_fg_hex = fg_hex.clone();
                pane.grid.default_bg_hex = bg_hex.clone();
                pane.grid.cell_pixel_width = cell_w as u16;
                pane.grid.cell_pixel_height = cell_h as u16;
            }
        }
    }

    // Initial layout
    {
        let mut tree = pane_tree.lock().unwrap();
        let pixel_w = cell_w * cols as f32;
        let pixel_h = cell_h * rows as f32 + status_bar_height;
        let viewport = PixelRect {
            x: 0.0,
            y: 0.0,
            width: pixel_w,
            height: pixel_h,
        };
        tree.relayout(viewport, cell_w, cell_h, status_bar_height);
    }

    let content_width = cell_w * cols as f32;
    let content_height = cell_h * rows as f32 + status_bar_height;
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

    // Window transparency
    if bg_opacity < 1.0 {
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(true);
    }

    let prompt_indicator_color = if config.prompt_marks.enabled && config.prompt_marks.show_indicator {
        Some(ColorConfig::parse_hex(&config.prompt_marks.indicator_color))
    } else {
        None
    };

    let view = create_terminal_view(
        mtm,
        &device,
        pane_tree.clone(),
        atlas.clone(),
        dirty.clone(),
        should_close.clone(),
        default_fg,
        default_bg,
        scale_factor,
        config.font.family.clone(),
        config.font.size,
        config.font.zoom_step,
        config.font.min_size,
        config.font.max_size,
        selection_fg,
        selection_bg,
        chrome,
        keybinds,
        bg_opacity,
        status_bar_height,
        config.status_bar.enabled,
        resize_step,
        prompt_indicator_color,
        image_store.clone(),
        config.confirm.on_close_pane,
        config.confirm.on_quit,
    );

    window.setContentView(Some(&view));
    window.makeFirstResponder(Some(&view));
    window.makeKeyAndOrderFront(None);
    window.center();

    // PTY reader thread: reads from ALL panes
    let reader_tree = pane_tree.clone();
    let reader_dirty = dirty.clone();
    let reader_should_close = should_close.clone();
    let reader_image_store = image_store.clone();
    let reader_atlas = atlas.clone();

    std::thread::Builder::new()
        .name("pty-reader".to_string())
        .spawn(move || {
            let mut buf = [0u8; 4096];

            loop {
                if reader_should_close.load(Ordering::Relaxed) {
                    break;
                }

                let mut any_data = false;
                let mut dead_panes = Vec::new();

                // Lock atlas FIRST (canonical order: atlas → tree → image_store)
                let (cell_w, cell_h) = {
                    let atlas = reader_atlas.lock().unwrap();
                    (atlas.cell_width, atlas.cell_height)
                };

                {
                    let mut tree = reader_tree.lock().unwrap();
                    let pane_ids = tree.pane_ids();

                    for id in pane_ids {
                        if let Some(pane) = tree.pane_mut(id) {
                            match pane.pty.read(&mut buf) {
                                Ok(0) => {
                                    log::info!("PTY EOF for pane {id}");
                                    dead_panes.push(id);
                                }
                                Ok(n) => {
                                    pane.parser.feed(&buf[..n], &mut pane.grid);
                                    // Drain and send any query responses back to PTY
                                    let responses = pane.grid.drain_responses();
                                    for resp in responses {
                                        pane.pty.write_all(&resp).ok();
                                    }

                                    // Process Kitty graphics commands
                                    if kitty_enabled || images_enabled {
                                        let kitty_cmds = pane.grid.drain_kitty_commands();
                                        if !kitty_cmds.is_empty() {
                                            let mut store = reader_image_store.lock().unwrap();
                                            for cmd in kitty_cmds {
                                                let cursor_row = pane.grid.cursor_row;
                                                let cursor_col = pane.grid.cursor_col;
                                                let grid_cols = pane.grid.cols();
                                                let grid_rows = pane.grid.rows();
                                                let (resp, advance) = pane.kitty_handler.process(
                                                    cmd,
                                                    &mut store,
                                                    cursor_row,
                                                    cursor_col,
                                                    cell_w,
                                                    cell_h,
                                                    grid_cols,
                                                    grid_rows,
                                                    &mut pane.grid.image_placements,
                                                );
                                                if let Some(resp) = resp {
                                                    pane.pty.write_all(&resp).ok();
                                                }
                                                // Advance cursor for inline images
                                                if let Some(adv) = advance {
                                                    for _ in 0..adv.rows {
                                                        pane.grid.newline();
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // Process Sixel images
                                    if sixel_enabled || images_enabled {
                                        let sixel_imgs = pane.grid.drain_sixel_images();
                                        if !sixel_imgs.is_empty() {
                                            let mut store = reader_image_store.lock().unwrap();
                                            for sixel_img in sixel_imgs {
                                                let img_id = store.next_id();
                                                if let Some(stored_id) = store.store(
                                                    &sixel_img.pixels,
                                                    sixel_img.width,
                                                    sixel_img.height,
                                                    ImageFormat::Rgba,
                                                    Some(img_id),
                                                ) {
                                                    // Place inline at cursor
                                                    let display_cols = ((sixel_img.width as f32) / cell_w).ceil() as u32;
                                                    let display_rows = ((sixel_img.height as f32) / cell_h).ceil() as u32;
                                                    pane.grid.image_placements.push(
                                                        crate::renderer::kitty_handler::ImagePlacement {
                                                            image_id: stored_id,
                                                            placement_id: 0,
                                                            mode: crate::renderer::kitty_handler::PlacementMode::Inline {
                                                                row: pane.grid.cursor_row,
                                                                col: pane.grid.cursor_col,
                                                                cols: display_cols,
                                                                rows: display_rows,
                                                            },
                                                            z_index: 0,
                                                        },
                                                    );
                                                    // Advance cursor past image
                                                    for _ in 0..display_rows {
                                                        pane.grid.newline();
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    any_data = true;
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                Err(e) => {
                                    log::error!("PTY read error for pane {id}: {e}");
                                    dead_panes.push(id);
                                }
                            }
                        }
                    }

                    // Close dead panes
                    for id in dead_panes {
                        let should_terminate = tree.close(id);
                        if should_terminate {
                            reader_should_close.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                }

                if any_data {
                    reader_dirty.store(true, Ordering::Relaxed);
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }

            log::info!("PTY reader thread exiting");
        })
        .expect("Failed to spawn PTY reader thread");

    // Render timer (60fps)
    let timer_dirty = dirty.clone();
    let timer_should_close = should_close.clone();

    unsafe {
        let interval = 1.0 / 60.0;
        let timer_block = block2::RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            if timer_should_close.load(Ordering::Relaxed) {
                let mtm = MainThreadMarker::new().unwrap();
                let app = NSApplication::sharedApplication(mtm);
                app.terminate(None);
                return;
            }
            window::render_if_dirty(&timer_dirty);
        });

        let _timer = NSTimer::scheduledTimerWithTimeInterval_repeats_block(
            interval,
            true,
            &timer_block,
        );
    }

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
