pub mod confirm;
mod reader;
mod render_scheduler;
pub mod window;

use crate::config::{ColorConfig, Config};
use crate::glyph_atlas::{GlyphAtlas, GlyphAtlasError};
use crate::graphics::SixelPlacement;
use crate::input::keybind::{platform_table, KeybindMap};
use crate::launch_options::LaunchOptions;
use crate::layout::PixelRect;
use crate::pane::cleanup::PaneCleanup;
use crate::pane::PaneTree;
use crate::pane_reader::GraphicsPolicy;
use crate::parser::ansi::GraphicsSupport;
use crate::render_scene::{ChromeColors, StatusBarLayout};
use crate::renderer::image_store::ImageStore;
use crate::renderer::kitty_handler::KittyHandlerOptions;
use crate::window_title::{WindowTitleMailbox, WINDOW_TITLE};

use objc2::{runtime::ProtocolObject, MainThreadMarker, Message};
use objc2_app_kit::*;
use objc2_foundation::*;
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use window::{create_terminal_view, TerminalViewParams};

fn snapshot_then_lock<'a, First, Second, Snapshot, F>(
    first: &Mutex<First>,
    second: &'a Mutex<Second>,
    capture: F,
) -> (Snapshot, std::sync::MutexGuard<'a, Second>)
where
    F: FnOnce(&First) -> Snapshot,
{
    let first_guard = first.lock().unwrap();
    let snapshot = capture(&first_guard);
    let second_guard = second.lock().unwrap();
    drop(first_guard);
    (snapshot, second_guard)
}

pub fn launch(config: Config, options: LaunchOptions) -> Result<(), GlyphAtlasError> {
    let mtm = MainThreadMarker::new().expect("Must be called from the main thread");

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let icon_data = NSData::with_bytes(crate::app_icon::PNG);
    if let Some(icon) = NSImage::initWithData(mtm.alloc(), &icon_data) {
        // SAFETY: The application is accessed on the main thread and the image is non-null.
        unsafe { app.setApplicationIconImage(Some(&icon)) };
    } else {
        log::warn!("Could not load the application icon");
    }

    setup_menu_bar(&app, mtm);

    let device = MTLCreateSystemDefaultDevice()
        .expect("Failed to create Metal device — is this Apple Silicon?");
    log::info!("Metal device: {:?}", device.name());

    let cols = config.window.columns;
    let rows = config.window.rows;
    let scrollback_max = config.window.scrollback_lines;
    let bg_opacity = config.window.opacity.clamp(0.0, 1.0);
    let kitty_options = KittyHandlerOptions::from_megabytes(
        config.images.kitty.max_image_size_mb,
        config.images.kitty.allow_file_transfer,
    );
    let graphics_support = GraphicsSupport {
        kitty: config.images.kitty_graphics_enabled(),
        sixel: config.images.sixel_graphics_enabled(),
    };

    let reader_wake =
        Arc::new(reader::ReaderWake::new().expect("Failed to create PTY reader wake socket"));
    let input_failure_wake = reader_wake.clone();
    let pane_tree = Arc::new(Mutex::new(
        PaneTree::new(
            cols,
            rows,
            scrollback_max,
            kitty_options,
            graphics_support,
            Arc::new(move || input_failure_wake.wake()),
            &options.command,
        )
        .expect("Failed to create initial pane"),
    ));

    // Size the glyph atlas and the first window for the screen the app opens on;
    // `viewDidChangeBackingProperties` handles later moves between displays.
    let scale_factor = NSScreen::mainScreen(mtm)
        .map(|screen| screen.backingScaleFactor() as f32)
        .filter(|scale| scale.is_finite() && *scale > 0.0)
        .unwrap_or(2.0);
    let mut atlas = GlyphAtlas::new(&config.font.family, config.font.size, scale_factor)?;
    atlas.set_ligatures(config.font.ligatures);
    let atlas = Arc::new(Mutex::new(atlas));

    let dirty = Arc::new(AtomicBool::new(true));
    let window_title = Arc::new(WindowTitleMailbox::new());
    if let Some(title) = &options.title {
        window_title.set_initial_title(title);
    }
    let should_close = Arc::new(AtomicBool::new(false));
    let window_is_key = Arc::new(AtomicBool::new(false));
    let notifications = Arc::new(crate::notifications::NotificationController::new(
        config.notifications.clone(),
        window_is_key.clone(),
    ));
    let (pane_cleanup, pane_cleanup_handle) = PaneCleanup::spawn();
    let clipboard_write: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let terminal_colors = crate::palette::configured_colors(&config);
    let selection_fg = ColorConfig::parse_hex(&config.selection.foreground);
    let selection_bg = ColorConfig::parse_hex(&config.selection.background);

    let chrome = ChromeColors {
        sumi_dark: ColorConfig::parse_hex(&config.theme.chrome.sumi_dark),
        sumi_medium: ColorConfig::parse_hex(&config.theme.chrome.sumi_medium),
        sumi_light: ColorConfig::parse_hex(&config.theme.chrome.sumi_light),
        sumi_ghost: ColorConfig::parse_hex(&config.theme.chrome.sumi_ghost),
        hanko_red: ColorConfig::parse_hex(&config.theme.chrome.hanko_red),
    };

    let keybinds = KeybindMap::from_platform_config(&config);
    let resize_step = platform_table(&config).resize_step();

    // Create shared image store for Kitty/Sixel image rendering
    let image_store = Arc::new(Mutex::new(ImageStore::new(
        device.retain(),
        config.images.max_memory_mb,
    )));
    let kitty_enabled = graphics_support.kitty;
    let sixel_enabled = graphics_support.sixel;

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

    // SAFETY: NSWindow's designated initializer runs once on a fresh
    // allocation, on the main thread (`mtm`); rect, style mask and backing
    // type are plain values and the window is retained by the returned handle.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            window_rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };

    window.setTitle(&NSString::from_str(WINDOW_TITLE));
    window.setMinSize(NSSize::new(200.0, 150.0));
    window.setAcceptsMouseMovedEvents(true);

    // Window transparency
    if bg_opacity < 1.0 {
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(true);
    }

    // Frames run on the main queue only when requested (at most 60 Hz) and
    // draw only while the window is visible. The callback also applies window
    // titles and termination requested elsewhere, hidden or not.
    let frame_dirty = dirty.clone();
    let frame_should_close = should_close.clone();
    let frame_notifications = notifications.clone();
    let frame_clipboard_write = clipboard_write.clone();
    // Keep the callback sendable; recover the main-thread-only window by number per frame.
    let frame_window_number = window.windowNumber();
    let render_scheduler = render_scheduler::RenderScheduler::new(
        dirty.clone(),
        Box::new(move |mode| {
            if frame_should_close.load(Ordering::Acquire) {
                let mtm = MainThreadMarker::new().expect("frames run on the main thread");
                NSApplication::sharedApplication(mtm).terminate(None);
                return;
            }
            window::apply_pending_user_attention(frame_notifications.as_ref());
            window::apply_pending_clipboard_write(&frame_clipboard_write);
            window::sync_window_title(frame_window_number);
            if mode == render_scheduler::FrameMode::Draw {
                window::render_if_dirty(&frame_dirty);
            }
        }),
    );

    let update_status: crate::update::SharedUpdateStatus =
        Arc::new(Mutex::new(crate::update::UpdateStatus::Idle));

    let prompt_indicator_color =
        if config.prompt_marks.enabled && config.prompt_marks.show_indicator {
            Some(ColorConfig::parse_hex(&config.prompt_marks.indicator_color))
        } else {
            None
        };

    let view = create_terminal_view(TerminalViewParams {
        mtm,
        device: &device,
        pane_tree: pane_tree.clone(),
        pane_cleanup: pane_cleanup.clone(),
        atlas: atlas.clone(),
        dirty: dirty.clone(),
        render_scheduler: render_scheduler.clone(),
        should_close: should_close.clone(),
        reader_wake: reader_wake.clone(),
        window_is_key: window_is_key.clone(),
        terminal_colors,
        scale_factor,
        font_family: config.font.family.clone(),
        font_size: config.font.size,
        zoom_step: config.font.zoom_step,
        min_font_size: config.font.min_size,
        max_font_size: config.font.max_size,
        selection_fg,
        selection_bg,
        chrome,
        keybinds,
        bg_opacity,
        status_bar_height,
        status_bar_enabled: config.status_bar.enabled,
        option_as_alt: config.input.option_as_alt,
        status_bar_layout: StatusBarLayout {
            show_shell: config.status_bar.show_shell,
            show_cwd: config.status_bar.show_cwd,
            show_pane_index: config.status_bar.show_pane_index,
        },
        resize_step,
        prompt_indicator_color,
        image_store: image_store.clone(),
        window_title: window_title.clone(),
        confirm_on_close_pane: config.confirm.on_close_pane,
        confirm_on_quit: config.confirm.on_quit,
        update_status: update_status.clone(),
    });

    window.setContentView(Some(&view));
    // NSWindow keeps its delegate weakly; the content view retains TerminalView
    // for the full window lifetime.
    window.setDelegate(Some(ProtocolObject::from_ref(&*view)));
    window.makeFirstResponder(Some(&view));
    window.makeKeyAndOrderFront(None);
    window.center();

    // PTY reader thread: waits on ALL panes without periodic wakeups
    let reader_tree = pane_tree.clone();
    let reader_render_scheduler = render_scheduler.clone();
    let reader_atlas = atlas.clone();
    let reader_thread_wake = reader_wake.clone();
    let reader_shared = reader::ReaderShared {
        image_store: image_store.clone(),
        window_title: window_title.clone(),
        window_is_key: window_is_key.clone(),
        should_close: should_close.clone(),
        graphics: GraphicsPolicy {
            kitty_enabled,
            sixel_enabled,
            sixel_placement: SixelPlacement::CellAnchored,
            placement_limit: None,
        },
        notifications: notifications.clone(),
        osc52_write: config.clipboard.osc52_write,
    };
    let reader_host = reader::MacReaderHost {
        atlas: reader_atlas,
        render_scheduler: reader_render_scheduler,
        pane_cleanup: pane_cleanup.clone(),
        clipboard_write: clipboard_write.clone(),
    };

    let reader_handle = std::thread::Builder::new()
        .name("pty-reader".to_string())
        .spawn(move || {
            reader::run_reader(
                reader_tree.as_ref(),
                reader_thread_wake.as_ref(),
                &reader_shared,
                &reader_host,
            );
        })
        .expect("Failed to spawn PTY reader thread");

    render_scheduler.request_render();

    if crate::update::startup_check_enabled(config.update.check_on_startup) {
        let update_render_scheduler = render_scheduler.clone();
        crate::update::spawn_check(update_status, false, move || {
            update_render_scheduler.request_render()
        });
    }

    app.activate();
    log::info!("Starting application run loop");
    app.run();

    should_close.store(true, Ordering::Release);
    reader_wake.wake();
    if reader_handle.join().is_err() {
        log::error!("PTY reader thread panicked during shutdown");
    }

    let remaining_panes = {
        let mut tree = pane_tree.lock().unwrap();
        tree.take_all_panes()
    };
    for pane in remaining_panes {
        pane_cleanup.retire(pane);
    }
    pane_cleanup.shutdown();
    if pane_cleanup_handle.join().is_err() {
        log::error!("Pane cleanup thread panicked during shutdown");
    }
    for pane in pane_cleanup.take_fallback() {
        pane.retire();
    }

    Ok(())
}

fn setup_menu_bar(app: &NSApplication, mtm: MainThreadMarker) {
    // SAFETY: every call is an AppKit message send on the main thread (`mtm`)
    // with valid, retained NSString/NSMenu arguments. The selectors are
    // resolved by AppKit through the responder chain at click time, so no
    // dangling target is stored.
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
        // No target: AppKit routes these to the terminal view (first responder).
        for (title, action) in [
            ("Check for Updates…", objc2::sel!(checkForUpdates:)),
            ("Install Update…", objc2::sel!(installUpdate:)),
        ] {
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(""),
            );
            app_menu.addItem(&item);
        }
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        app_menu.addItem(&quit_item);
        app_menu_item.setSubmenu(Some(&app_menu));

        app.setMainMenu(Some(&menu_bar));
    }
}

#[cfg(test)]
mod tests {
    use super::snapshot_then_lock;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn metric_snapshot_stays_locked_until_the_tree_is_acquired() {
        let metric_epoch = Arc::new(Mutex::new(0_u64));
        let tree_epoch = Arc::new(Mutex::new(0_u64));
        let tree_blocker = tree_epoch.lock().unwrap();
        let (captured_sender, captured_receiver) = mpsc::channel();
        let (snapshot_sender, snapshot_receiver) = mpsc::channel();

        let reader_metric_epoch = metric_epoch.clone();
        let reader_tree_epoch = tree_epoch.clone();
        let reader = std::thread::spawn(move || {
            let (metric_epoch, tree_epoch) = snapshot_then_lock(
                reader_metric_epoch.as_ref(),
                reader_tree_epoch.as_ref(),
                |epoch| {
                    captured_sender.send(()).unwrap();
                    *epoch
                },
            );
            snapshot_sender.send((metric_epoch, *tree_epoch)).unwrap();
        });

        captured_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("reader should capture the metric before waiting for the tree");
        assert!(metric_epoch.try_lock().is_err());

        let (zoom_started_sender, zoom_started_receiver) = mpsc::channel();
        let (zoom_finished_sender, zoom_finished_receiver) = mpsc::channel();
        let zoom_metric_epoch = metric_epoch.clone();
        let zoom_tree_epoch = tree_epoch.clone();
        let zoom = std::thread::spawn(move || {
            zoom_started_sender.send(()).unwrap();
            let mut metric_epoch = zoom_metric_epoch.lock().unwrap();
            let mut tree_epoch = zoom_tree_epoch.lock().unwrap();
            *metric_epoch = 1;
            *tree_epoch = 1;
            zoom_finished_sender.send(()).unwrap();
        });

        zoom_started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("zoom should attempt to acquire the metric lock");
        assert!(zoom_finished_receiver.try_recv().is_err());
        drop(tree_blocker);

        assert_eq!(
            snapshot_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("reader should acquire the matching tree epoch"),
            (0, 0)
        );
        zoom_finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("zoom should proceed after the reader releases both epochs");
        reader.join().unwrap();
        zoom.join().unwrap();
        assert_eq!(*metric_epoch.lock().unwrap(), 1);
        assert_eq!(*tree_epoch.lock().unwrap(), 1);
    }

    #[test]
    fn metric_snapshot_observes_the_tree_epoch_when_zoom_wins() {
        let metric_epoch = Arc::new(Mutex::new(0_u64));
        let tree_epoch = Arc::new(Mutex::new(0_u64));
        let (zoom_updated_sender, zoom_updated_receiver) = mpsc::channel();
        let (release_zoom_sender, release_zoom_receiver) = mpsc::channel();
        let zoom_metric_epoch = metric_epoch.clone();
        let zoom_tree_epoch = tree_epoch.clone();
        let zoom = std::thread::spawn(move || {
            let mut metric_epoch = zoom_metric_epoch.lock().unwrap();
            let mut tree_epoch = zoom_tree_epoch.lock().unwrap();
            *metric_epoch = 1;
            *tree_epoch = 1;
            zoom_updated_sender.send(()).unwrap();
            release_zoom_receiver.recv().unwrap();
        });

        zoom_updated_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("zoom should install both epochs before the reader starts");

        let (reader_started_sender, reader_started_receiver) = mpsc::channel();
        let (snapshot_sender, snapshot_receiver) = mpsc::channel();
        let reader_metric_epoch = metric_epoch.clone();
        let reader_tree_epoch = tree_epoch.clone();
        let reader = std::thread::spawn(move || {
            reader_started_sender.send(()).unwrap();
            let (metric_epoch, tree_epoch) = snapshot_then_lock(
                reader_metric_epoch.as_ref(),
                reader_tree_epoch.as_ref(),
                |epoch| *epoch,
            );
            snapshot_sender.send((metric_epoch, *tree_epoch)).unwrap();
        });

        reader_started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("reader should attempt the metric snapshot");
        assert!(metric_epoch.try_lock().is_err());
        release_zoom_sender.send(()).unwrap();

        assert_eq!(
            snapshot_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("reader should observe the epochs installed by zoom"),
            (1, 1)
        );
        zoom.join().unwrap();
        reader.join().unwrap();
    }
}
