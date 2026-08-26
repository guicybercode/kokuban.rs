pub mod confirm;
pub mod window;

use crate::config::{ColorConfig, Config};
use crate::graphics::{ImageId, ImagePlacement, InlineRenderSize, PlacementMode};
use crate::glyph_atlas::{GlyphAtlas, GlyphAtlasError};
use crate::grid::{Grid, TerminalEvent};
use crate::input::keybind::KeybindMap;
use crate::layout::PixelRect;
use crate::pane::pane::Pane;
use crate::pane::PaneTree;
use crate::parser::ansi::GraphicsSupport;
use crate::parser::sixel::SixelImage;
use crate::render_scene::ChromeColors;
use crate::renderer::image_store::{ImageFormat, ImageStore};
use crate::renderer::kitty_handler::KittyHandlerOptions;
use crate::window_title::WINDOW_TITLE;

use objc2::{MainThreadMarker, Message};
use objc2_app_kit::*;
use objc2_foundation::*;
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use window::create_terminal_view;

enum CleanupCommand<T> {
    Retire(Box<T>),
    Shutdown,
}

#[derive(Clone)]
pub(super) struct PaneCleanup {
    sender: mpsc::Sender<CleanupCommand<Pane>>,
    fallback: Arc<Mutex<Vec<Pane>>>,
}

impl PaneCleanup {
    fn spawn() -> (Self, JoinHandle<()>) {
        let (sender, receiver) = mpsc::channel();
        let fallback = Arc::new(Mutex::new(Vec::new()));
        let handle = std::thread::Builder::new()
            .name("pane-cleanup".to_string())
            .spawn(move || run_cleanup(receiver, Pane::retire))
            .expect("Failed to spawn pane cleanup thread");

        (Self { sender, fallback }, handle)
    }

    pub(super) fn retire(&self, pane: Pane) {
        if let Err(error) = self
            .sender
            .send(CleanupCommand::Retire(Box::new(pane)))
        {
            let CleanupCommand::Retire(pane) = error.0 else {
                unreachable!("retire sends only retire commands");
            };
            log::error!("Pane cleanup worker is unavailable; deferring local cleanup");
            match self.fallback.lock() {
                Ok(mut fallback) => fallback.push(*pane),
                Err(poisoned) => poisoned.into_inner().push(*pane),
            }
        }
    }

    fn shutdown(&self) {
        if self.sender.send(CleanupCommand::Shutdown).is_err() {
            log::warn!("Pane cleanup worker stopped before shutdown");
        }
    }

    fn take_fallback(&self) -> Vec<Pane> {
        let mut fallback = match self.fallback.lock() {
            Ok(fallback) => fallback,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *fallback)
    }
}

fn run_cleanup<T, F>(receiver: mpsc::Receiver<CleanupCommand<T>>, mut retire: F)
where
    F: FnMut(T),
{
    while let Ok(command) = receiver.recv() {
        match command {
            CleanupCommand::Retire(value) => retire(*value),
            CleanupCommand::Shutdown => break,
        }
    }
}

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

fn process_sixel_event<F>(
    grid: &mut Grid,
    image: &SixelImage,
    cursor: (usize, usize),
    cell_size: (f32, f32),
    store_image: F,
)
where
    F: FnOnce(&SixelImage) -> Option<ImageId>,
{
    let display_cols = ((image.width as f32) / cell_size.0).ceil() as u32;
    let display_rows = ((image.height as f32) / cell_size.1).ceil() as u32;
    if let Some(image_id) = store_image(image) {
        grid.image_placements.push(ImagePlacement {
            image_id,
            placement_id: 0,
            client_placement_id: None,
            mode: PlacementMode::Inline {
                row: cursor.0,
                col: cursor.1,
                cols: display_cols,
                rows: display_rows,
                x_offset: 0,
                y_offset: 0,
                render_size: InlineRenderSize::CellAnchored,
            },
            z_index: 0,
        });
    }
    for _ in 0..display_rows {
        grid.newline();
    }
}

pub fn launch(config: Config) -> Result<(), GlyphAtlasError> {
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
    let kitty_options = KittyHandlerOptions::from_megabytes(
        config.images.kitty.max_image_size_mb,
        config.images.kitty.allow_file_transfer,
    );
    let graphics_support = GraphicsSupport {
        kitty: config.images.kitty_graphics_enabled(),
        sixel: config.images.sixel_graphics_enabled(),
    };

    let pane_tree = Arc::new(Mutex::new(
        PaneTree::new(cols, rows, scrollback_max, kitty_options, graphics_support)
            .expect("Failed to create initial pane"),
    ));

    let scale_factor = 2.0f32;
    let atlas = Arc::new(Mutex::new(GlyphAtlas::new(
        &config.font.family,
        config.font.size,
        scale_factor,
    )?));

    let dirty = Arc::new(AtomicBool::new(true));
    let window_title = Arc::new(window::WindowTitleMailbox::new());
    let should_close = Arc::new(AtomicBool::new(false));
    let (pane_cleanup, pane_cleanup_handle) = PaneCleanup::spawn();

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

    let prompt_indicator_color = if config.prompt_marks.enabled && config.prompt_marks.show_indicator {
        Some(ColorConfig::parse_hex(&config.prompt_marks.indicator_color))
    } else {
        None
    };

    let view = create_terminal_view(
        mtm,
        &device,
        pane_tree.clone(),
        pane_cleanup.clone(),
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
        window_title.clone(),
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
    let reader_window_title = window_title.clone();
    let reader_pane_cleanup = pane_cleanup.clone();

    let reader_handle = std::thread::Builder::new()
        .name("pty-reader".to_string())
        .spawn(move || {
            let mut buf = [0u8; 4096];

            loop {
                if reader_should_close.load(Ordering::Relaxed) {
                    break;
                }

                let mut any_data = false;
                let mut dead_panes = Vec::new();

                {
                    // Lock atlas FIRST (canonical order: atlas → tree → image_store).
                    // Keep it locked until the tree snapshot belongs to the same metric epoch.
                    let ((cell_w, cell_h), mut tree) = snapshot_then_lock(
                        reader_atlas.as_ref(),
                        reader_tree.as_ref(),
                        |atlas| (atlas.cell_width, atlas.cell_height),
                    );
                    let pane_ids = tree.pane_ids();

                    for id in pane_ids {
                        if tree.pane(id).is_some_and(|pane| pane.input_failed()) {
                            dead_panes.push(id);
                            continue;
                        }
                        let title_revision_before = tree
                            .pane(id)
                            .map(|pane| pane.grid.title_revision())
                            .unwrap_or_default();
                        let read_result = match tree.pane_mut(id) {
                            Some(pane) => pane.pty.read(&mut buf),
                            None => continue,
                        };

                        match read_result {
                            Ok(0) => {
                                log::info!("PTY EOF for pane {id}");
                                dead_panes.push(id);
                            }
                            Ok(n) => {
                                let mut parsed_bytes = 0;
                                while parsed_bytes < n {
                                    let (consumed, terminal_events) = {
                                        let Some(pane) = tree.pane_mut(id) else {
                                            break;
                                        };
                                        let step = pane.decoder.feed_until_event(
                                            &buf[parsed_bytes..n],
                                            &mut pane.grid,
                                        );
                                        (step.consumed, step.events)
                                    };
                                    debug_assert!(consumed > 0);
                                    parsed_bytes += consumed;

                                    for event in terminal_events {
                                        match event {
                                            TerminalEvent::Response(response) => {
                                                if let Some(pane) = tree.pane(id) {
                                                    pane.queue_input(response);
                                                }
                                            }
                                            TerminalEvent::KittyGraphics {
                                                command,
                                                cursor_row,
                                                cursor_col,
                                            } => {
                                                if !kitty_enabled {
                                                    continue;
                                                }
                                                let mut hard_delete_candidates = {
                                                    let Some(pane) = tree.pane_mut(id) else {
                                                        continue;
                                                    };
                                                    let grid_cols = pane.grid.cols();
                                                    let grid_rows = pane.grid.rows();
                                                    let outcome = {
                                                        let mut store =
                                                            reader_image_store.lock().unwrap();
                                                        pane.kitty_handler.process(
                                                            command,
                                                            &mut store,
                                                            cursor_row,
                                                            cursor_col,
                                                            cell_w,
                                                            cell_h,
                                                            grid_cols,
                                                            grid_rows,
                                                            &mut pane.grid.image_placements,
                                                        )
                                                    };
                                                    if let Some(image_id) =
                                                        outcome.retransmitted_image_id
                                                    {
                                                        pane.grid
                                                            .remove_hidden_primary_kitty_placements(
                                                                image_id,
                                                            );
                                                    }
                                                    if let Some(response) = outcome.response {
                                                        pane.queue_input(response);
                                                    }
                                                    // Advance cursor for inline images
                                                    if let Some(adv) = outcome.advance {
                                                        pane.grid.advance_image_cursor(
                                                            adv.cols,
                                                            adv.rows,
                                                        );
                                                    }
                                                    outcome.hard_delete_candidates
                                                };

                                                if !hard_delete_candidates.is_empty() {
                                                    tree.retain_unreferenced_image_ids(
                                                        &mut hard_delete_candidates,
                                                    );
                                                    if !hard_delete_candidates.is_empty() {
                                                        let mut store =
                                                            reader_image_store.lock().unwrap();
                                                        for image_id in hard_delete_candidates {
                                                            store.remove(image_id);
                                                        }
                                                    }
                                                }
                                            }
                                            TerminalEvent::SixelGraphics {
                                                image,
                                                cursor_row,
                                                cursor_col,
                                            } => {
                                                if !sixel_enabled {
                                                    continue;
                                                }
                                                let Some(pane) = tree.pane_mut(id) else {
                                                    continue;
                                                };
                                                process_sixel_event(
                                                    &mut pane.grid,
                                                    &image,
                                                    (cursor_row, cursor_col),
                                                    (cell_w, cell_h),
                                                    |image| {
                                                        let mut store =
                                                            reader_image_store.lock().unwrap();
                                                        let image_id = store.next_id();
                                                        store.store(
                                                            &image.pixels,
                                                            image.width,
                                                            image.height,
                                                            ImageFormat::Rgba,
                                                            Some(image_id),
                                                        )
                                                    },
                                                );
                                            }
                                        }
                                    }
                                }

                                let focused_title_changed = id == tree.focused
                                    && tree.pane(id).is_some_and(|pane| {
                                        pane.grid.title_revision() != title_revision_before
                                    });
                                if focused_title_changed {
                                    window::publish_focused_window_title(
                                        &tree,
                                        reader_window_title.as_ref(),
                                    );
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

                    let mut retired_panes = Vec::new();
                    // Close dead panes
                    for id in dead_panes {
                        let previous_focus = tree.focused;
                        let outcome = tree.close(id);
                        if let Some(pane) = outcome.closed_pane {
                            retired_panes.push(pane);
                        }
                        if tree.focused != previous_focus {
                            window::publish_focused_window_title(
                                &tree,
                                reader_window_title.as_ref(),
                            );
                        }
                        if outcome.should_terminate {
                            reader_should_close.store(true, Ordering::Relaxed);
                            break;
                        }
                    }

                    drop(tree);
                    for pane in retired_panes {
                        reader_pane_cleanup.retire(pane);
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
    // Keep the NSTimer block sendable; recover the main-thread-only window by number per update.
    let timer_window_number = window.windowNumber();

    unsafe {
        let interval = 1.0 / 60.0;
        let timer_block = block2::RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            if timer_should_close.load(Ordering::Acquire) {
                let mtm = MainThreadMarker::new().unwrap();
                let app = NSApplication::sharedApplication(mtm);
                app.terminate(None);
                return;
            }
            window::sync_window_title(timer_window_number);
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

    should_close.store(true, Ordering::Release);
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

#[cfg(test)]
mod tests {
    use super::{process_sixel_event, run_cleanup, snapshot_then_lock, CleanupCommand};
    use crate::graphics::PlacementMode;
    use crate::grid::Grid;
    use crate::parser::sixel::SixelImage;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    struct LockingDropProbe {
        lock: Arc<Mutex<()>>,
        started: mpsc::Sender<std::thread::ThreadId>,
        finished: mpsc::Sender<()>,
    }

    impl Drop for LockingDropProbe {
        fn drop(&mut self) {
            self.started
                .send(std::thread::current().id())
                .expect("cleanup test should observe the retire thread");
            let _guard = self.lock.lock().unwrap();
            self.finished
                .send(())
                .expect("cleanup test should observe retirement completion");
        }
    }

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

    #[test]
    fn sixel_placement_uses_the_event_cursor_snapshot() {
        let mut grid = Grid::new(12, 6, 0);
        grid.cursor_row = 4;
        grid.cursor_col = 7;
        let image = SixelImage {
            width: 1,
            height: 6,
            pixels: vec![0; 24],
        };

        process_sixel_event(&mut grid, &image, (1, 2), (8.0, 16.0), |_| Some(9));

        assert_eq!((grid.cursor_row, grid.cursor_col), (5, 7));
        let placement = grid.image_placements.pop().unwrap();
        match placement.mode {
            PlacementMode::Inline {
                row,
                col,
                cols,
                rows,
                ..
            } => {
                assert_eq!((row, col), (1, 2));
                assert_ne!((row, col), (grid.cursor_row, grid.cursor_col));
                assert_eq!((cols, rows), (1, 1));
            }
        }
    }

    #[test]
    fn sixel_store_rejection_still_advances_the_protocol_cursor() {
        let mut grid = Grid::new(12, 6, 0);
        grid.cursor_row = 1;
        grid.cursor_col = 2;
        let image = SixelImage {
            width: 1,
            height: 6,
            pixels: vec![0; 24],
        };

        process_sixel_event(&mut grid, &image, (1, 2), (8.0, 16.0), |_| None);

        assert!(grid.image_placements.is_empty());
        assert_eq!((grid.cursor_row, grid.cursor_col), (2, 2));
    }

    #[test]
    fn cleanup_retires_before_shutdown_without_blocking_the_callers_lock() {
        let (sender, receiver) = mpsc::channel();
        let (started_sender, started_receiver) = mpsc::channel();
        let (finished_sender, finished_receiver) = mpsc::channel();
        let lock = Arc::new(Mutex::new(()));
        let caller_guard = lock.lock().unwrap();
        let caller_thread = std::thread::current().id();
        let worker = std::thread::spawn(move || run_cleanup(receiver, drop));

        sender
            .send(CleanupCommand::Retire(Box::new(LockingDropProbe {
                lock: lock.clone(),
                started: started_sender,
                finished: finished_sender,
            })))
            .unwrap();
        sender.send(CleanupCommand::Shutdown).unwrap();

        let retire_thread = started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("retirement should start on the cleanup worker");
        assert_ne!(retire_thread, caller_thread);
        assert!(finished_receiver.try_recv().is_err());

        drop(caller_guard);
        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("retirement should finish after the caller releases its lock");
        worker.join().expect("cleanup worker should process shutdown");
    }
}
