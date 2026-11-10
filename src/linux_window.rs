use crate::config::{ColorConfig, Config};
use crate::glyph_atlas::GlyphAtlas;
use crate::graphics::SixelPlacement;
use crate::grid::cell::{CellFlags, Color};
use crate::grid::{Grid, MouseTracking};
use crate::input::keybind::{platform_table, KeybindMap, PaneAction};
use crate::input::linux::{
    encode_key_press, key_modifiers_from_winit, key_name_from_winit, key_press_from_winit,
    scrollback_action_from_winit, ImeCommitPayload,
};
use crate::input::paste::{encode_paste, MAX_PASTE_BYTES};
use crate::layout::{DividerInfo, PaneId, PixelRect, SplitDirection, DIVIDER_THICKNESS};
use crate::linux_clipboard::{ClipboardEvent, LinuxClipboard, MAX_CLIPBOARD_TEXT_BYTES};
use crate::notifications::NotificationController;
use crate::omarchy_theme::{watch_current_theme, ThemeWatcher};
use crate::palette::PaletteOverrides;
use crate::pane::actions::{apply_pane_action, PaneActionHost, ZoomDirection};
use crate::pane::cleanup::PaneCleanup;
use crate::pane::focus::{dispatch_pane_focus_transition_locked, publish_focused_window_title};
use crate::pane::pane::Pane;
use crate::pane::PaneTree;
use crate::pane_reader::{run_reader, GraphicsPolicy, PaneReaderHost, ReaderShared, ReaderWake};
use crate::parser::ansi::GraphicsSupport;
use crate::renderer::image_store::ImageStore;
use crate::renderer::kitty_handler::KittyHandlerOptions;
use crate::search::{MatchKind, SearchKey, SearchKeyOutcome, SearchState};
use crate::selection::GridPoint;
use crate::software_graphics::{
    advance_animations, snapshot_images, ImageSnapshot, MAX_PLACEMENTS,
};
use crate::software_raster::fill_rect;
use crate::terminal_colors::TerminalColors;
use crate::window_title::{
    normalized_window_title, sync_window_title_with, WindowTitleMailbox, WINDOW_TITLE,
};
use softbuffer::{Context, Surface};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Instant;
use thiserror::Error;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{DeviceId, ElementState, Ime, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::platform::x11::WindowAttributesExtX11;
use winit::window::{Icon, ImePurpose, UserAttentionType, Window, WindowId};

mod clipboard;
mod close_confirm;
mod damage;
mod dispatch;
mod draw;
mod ime;
mod overlay;
mod pointer;
mod sizing;
mod status;
#[cfg(test)]
mod test_support;

use clipboard::{
    apply_selection_to_snapshot, clipboard_action, selection_point_at_pointer, sync_selection,
    ClipboardAction, SelectionContext,
};
use close_confirm::{
    close_answer, pane_close_prompt, window_close_prompt, CloseAnswer, ClosePrompt, CloseTarget,
};
use damage::{frame_damage, scene_for_buffer_age, PresentedScene, MAX_PRESENTED_SCENES};
use dispatch::{
    dispatch_encoded_terminal_input_locked, dispatch_focus_event_locked,
    dispatch_keyboard_input_locked, dispatch_mouse_button_and_motion_locked,
    dispatch_mouse_motion_locked, dispatch_mouse_wheel_locked, modifiers_after_focus_change,
    record_window_focus_change, KeyboardInputError, KeyboardInputOutcome,
    MouseMotionDispatchOutcome,
};
use draw::{
    draw_grid_snapshot_in_band, draw_ime_preedit, paint_pane_viewport, rgb_to_xrgb,
    snapshot_locked_grid, GridSnapshot, PaneViewport,
};
use ime::{
    ime_cursor_area, ime_payload_from_event, ime_preedit_payload, layout_ime_preedit_for_snapshot,
    replace_ime_preedit, reveal_ime_input_viewport_locked, sync_ime_cursor_area_with,
    ImeCursorArea, ImePreedit, ImePreeditPayload,
};
use overlay::{
    close_prompt_segments, keep_match_clear_of_strip, paint_strip, search_prompt, search_strip_row,
    StripColors,
};
use pointer::{mouse_button_code_from_winit, PointerRouteState};
use sizing::{
    atlas_cell_dimensions, configured_terminal_dimensions, create_glyph_atlas, drawable_dimensions,
    immediate_surface_size_to_reconcile, initial_window_dimensions, is_current_surface_size,
    physical_size_for_terminal, replace_glyph_atlas_for_scale,
    terminal_dimensions_from_locked_grid, GlyphAtlasRebuildError, TerminalDimensions,
    INITIAL_CELL_HEIGHT, INITIAL_CELL_WIDTH, MAX_TERMINAL_COLUMNS, MAX_TERMINAL_ROWS,
};
use status::{combine_launch_results, terminal_accepts_input};

const EXIT_AFTER_FIRST_FRAME_ENV: &str = "KOKUBAN_EXIT_AFTER_FIRST_FRAME";

type SoftwareSurface = Surface<Arc<Window>, Arc<Window>>;

fn application_icon() -> Option<Icon> {
    // Keep the X11 property small; the full-resolution PNG stays embedded for macOS.
    const MAX_EDGE: u32 = 128;
    const MAX_DECODE_BYTES: usize = 8 * 1024 * 1024;
    let Some((rgba, width, height)) =
        crate::renderer::image_decode::decode_png(crate::app_icon::PNG, MAX_DECODE_BYTES)
    else {
        log::warn!("Could not decode the application icon");
        return None;
    };
    let longest_edge = width.max(height);
    let scaled_edge = longest_edge.min(MAX_EDGE);
    let icon_width =
        (u64::from(width) * u64::from(scaled_edge) / u64::from(longest_edge)).max(1) as u32;
    let icon_height =
        (u64::from(height) * u64::from(scaled_edge) / u64::from(longest_edge)).max(1) as u32;
    let mut icon_rgba = Vec::with_capacity((icon_width * icon_height * 4) as usize);
    for y in 0..icon_height {
        let source_y = ((u64::from(y) * 2 + 1) * u64::from(height)) / (u64::from(icon_height) * 2);
        for x in 0..icon_width {
            let source_x =
                ((u64::from(x) * 2 + 1) * u64::from(width)) / (u64::from(icon_width) * 2);
            let source = ((source_y * u64::from(width) + source_x) * 4) as usize;
            icon_rgba.extend_from_slice(&rgba[source..source + 4]);
        }
    }
    match Icon::from_rgba(icon_rgba, icon_width, icon_height) {
        Ok(icon) => Some(icon),
        Err(error) => {
            log::warn!("Could not create the application icon: {error}");
            None
        }
    }
}

#[derive(Debug)]
enum LinuxEvent {
    Clipboard(ClipboardEvent),
    ThemeChanged,
    GridUpdated,
    WindowTitleChanged,
    UserAttention,
    /// OSC 52 asked to put this text on the system clipboard.
    ClipboardWrite(String),
    /// The reader closed this pane because its shell exited or its input
    /// failed; when it was the last one, `should_close` is already set.
    PaneExited(PaneId),
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
enum GridAccessError {
    #[error("could not access the Linux pane tree because its lock is poisoned")]
    Poisoned,
    #[error("terminal grid dimensions {columns}x{rows} exceed the Linux PTY limit")]
    DimensionsOutOfRange { columns: usize, rows: usize },
}

/// Cell size the reader uses for image placements, published by the main
/// thread whenever the glyph atlas changes (width in the high half).
fn pack_cell_dimensions(cell_dimensions: (u16, u16)) -> u32 {
    (u32::from(cell_dimensions.0) << 16) | u32::from(cell_dimensions.1)
}

fn unpack_cell_dimensions(packed: u32) -> (f32, f32) {
    (f32::from((packed >> 16) as u16), f32::from(packed as u16))
}

/// Reader-side effects that reach the event loop as `LinuxEvent`s.
struct LinuxReaderHost {
    proxy: EventLoopProxy<LinuxEvent>,
    redraw_pending: Arc<AtomicBool>,
    cell_dimensions: Arc<AtomicU32>,
    pane_cleanup: PaneCleanup,
}

impl PaneReaderHost for LinuxReaderHost {
    fn cell_size_then_lock<'a>(
        &self,
        tree: &'a Mutex<PaneTree>,
    ) -> ((f32, f32), MutexGuard<'a, PaneTree>) {
        let tree = tree.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        (
            unpack_cell_dimensions(self.cell_dimensions.load(Ordering::Acquire)),
            tree,
        )
    }

    fn request_frame(&self) {
        signal_grid_update(&self.proxy, self.redraw_pending.as_ref());
    }

    fn pane_closed(&self, id: PaneId) {
        let _ = self.proxy.send_event(LinuxEvent::PaneExited(id));
    }

    fn title_changed(&self) {
        let _ = self.proxy.send_event(LinuxEvent::WindowTitleChanged);
    }

    fn user_attention(&self) {
        let _ = self.proxy.send_event(LinuxEvent::UserAttention);
    }

    fn clipboard_write(&self, text: String) {
        let _ = self.proxy.send_event(LinuxEvent::ClipboardWrite(text));
    }

    fn pane_retired(&self, pane: Pane) {
        self.pane_cleanup.retire(pane);
    }
}

pub(crate) fn launch(
    config: Config,
    options: crate::launch_options::LaunchOptions,
) -> Result<(), String> {
    if config.window.opacity < 1.0 {
        eprintln!(
            "kokuban: warning: window.opacity={} is not supported by the Linux software renderer; using an opaque window",
            config.window.opacity
        );
    }

    let event_loop = EventLoop::<LinuxEvent>::with_user_event()
        .build()
        .map_err(|error| format!("could not connect to an X11 or Wayland display: {error}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let terminal_dimensions =
        configured_terminal_dimensions(config.window.columns, config.window.rows);
    if terminal_dimensions.columns != config.window.columns
        || terminal_dimensions.rows != config.window.rows
    {
        eprintln!(
            "kokuban: warning: configured Linux terminal size {}x{} is outside the supported \
             1..={MAX_TERMINAL_COLUMNS} by 1..={MAX_TERMINAL_ROWS} range; using {terminal_dimensions}",
            config.window.columns, config.window.rows
        );
    }
    let columns = terminal_dimensions.columns;
    let rows = terminal_dimensions.rows;
    let theme_proxy = event_loop.create_proxy();
    let (theme_watcher, initial_theme) = watch_current_theme(config.omarchy.enabled, move || {
        theme_proxy.send_event(LinuxEvent::ThemeChanged).is_ok()
    });
    let palette_overrides = PaletteOverrides::from_config(&config);
    let colors = palette_overrides.resolve(initial_theme);
    let background = colors.default_background();
    let foreground = colors.resolve_foreground(Color::Default, false);
    let initial_size = initial_window_dimensions(columns, rows);
    let exit_after_first_frame = std::env::var(EXIT_AFTER_FIRST_FRAME_ENV).as_deref() == Ok("1");
    let graphics_support = GraphicsSupport {
        kitty: config.images.kitty_graphics_enabled(),
        sixel: config.images.sixel_graphics_enabled(),
    };
    let kitty_options = KittyHandlerOptions::from_megabytes(
        config.images.kitty.max_image_size_mb,
        config.images.kitty.allow_file_transfer,
    );

    let reader_wake =
        Arc::new(ReaderWake::new().map_err(|error| {
            format!("could not create the Linux PTY reader wake socket: {error}")
        })?);
    let input_failure_wake = reader_wake.clone();
    let mut tree = PaneTree::new(
        columns,
        rows,
        config.window.scrollback_lines,
        kitty_options,
        graphics_support,
        Arc::new(move || input_failure_wake.wake()),
        &options.command,
    )
    .map_err(|error| format!("could not start the Linux terminal process: {error}"))?;
    if let Some(pane) = tree.focused_pane_mut() {
        pane.grid.default_fg_hex = terminal_color_query_value(foreground);
        pane.grid.default_bg_hex = terminal_color_query_value(background);
        if let Some(title) = &options.title {
            pane.grid.set_title(title);
        }
    }
    let panes = Arc::new(Mutex::new(tree));
    let image_store = Arc::new(Mutex::new(ImageStore::new(config.images.max_memory_mb)));
    let window_title = Arc::new(WindowTitleMailbox::new());
    if let Some(title) = &options.title {
        window_title.set_initial_title(title);
    }
    let window_focused = Arc::new(AtomicBool::new(false));
    let should_close = Arc::new(AtomicBool::new(false));
    let notifications = Arc::new(NotificationController::new(
        config.notifications.clone(),
        window_focused.clone(),
    ));
    let (pane_cleanup, pane_cleanup_handle) = PaneCleanup::spawn();
    let redraw_pending = Arc::new(AtomicBool::new(false));
    let shared_cell_dimensions = Arc::new(AtomicU32::new(pack_cell_dimensions((
        INITIAL_CELL_WIDTH as u16,
        INITIAL_CELL_HEIGHT as u16,
    ))));
    let event_proxy = event_loop.create_proxy();
    let clipboard_proxy = event_proxy.clone();
    let clipboard = LinuxClipboard::new(move |event| {
        let _ = clipboard_proxy.send_event(LinuxEvent::Clipboard(event));
    })
    .map_err(|error| log::warn!("Linux clipboard worker unavailable: {error}"))
    .ok();

    let reader_shared = ReaderShared {
        image_store: image_store.clone(),
        window_title: window_title.clone(),
        window_is_key: window_focused.clone(),
        should_close: should_close.clone(),
        graphics: GraphicsPolicy {
            kitty_enabled: graphics_support.kitty,
            sixel_enabled: graphics_support.sixel,
            sixel_placement: SixelPlacement::NativePixels,
            placement_limit: Some(MAX_PLACEMENTS),
        },
        notifications: notifications.clone(),
        osc52_write: config.clipboard.osc52_write,
    };
    let reader_host = LinuxReaderHost {
        proxy: event_proxy.clone(),
        redraw_pending: redraw_pending.clone(),
        cell_dimensions: shared_cell_dimensions.clone(),
        pane_cleanup: pane_cleanup.clone(),
    };
    let reader_tree = panes.clone();
    let reader_thread_wake = reader_wake.clone();
    let reader_handle = std::thread::Builder::new()
        // Tooling (scripts/compare-terminal-performance.py) finds the reader by name.
        .name("terminal-reader".to_string())
        .spawn(move || {
            run_reader(
                reader_tree.as_ref(),
                reader_thread_wake.as_ref(),
                &reader_shared,
                &reader_host,
            );
        })
        .map_err(|error| format!("could not start the Linux terminal reader: {error}"))?;

    let chrome = &config.theme.chrome;
    let divider_colors = DividerColors {
        focused: xrgb_from_hex(&chrome.hanko_red),
        idle: xrgb_from_hex(&chrome.sumi_ghost),
    };
    let mut application = LinuxWindow::new(LinuxWindowParams {
        terminal_dimensions,
        colors,
        palette_overrides,
        font: FontSettings {
            family: config.font.family.clone(),
            size: config.font.size,
            zoom_step: config.font.zoom_step,
            min_size: config.font.min_size,
            max_size: config.font.max_size,
        },
        initial_size,
        exit_after_first_frame,
        panes,
        image_store,
        reader_wake,
        reader_handle,
        should_close,
        window_title,
        pane_cleanup: pane_cleanup.clone(),
        keybinds: KeybindMap::from_platform_config(&config),
        resize_step: platform_table(&config).resize_step(),
        divider_colors,
        shared_cell_dimensions,
        redraw_pending,
        notifications,
        window_focused,
        confirm_on_close_pane: config.confirm.on_close_pane,
        confirm_on_quit: config.confirm.on_quit,
    });
    application.clipboard = clipboard;
    application.theme_watcher = theme_watcher;
    if crate::update::startup_check_enabled(config.update.check_on_startup) {
        // Linux has no status bar yet; the notice goes into the window title.
        let update_check_proxy = event_proxy.clone();
        crate::update::spawn_check(application.update_status.clone(), false, move || {
            let _ = update_check_proxy.send_event(LinuxEvent::WindowTitleChanged);
        });
    }
    application.app_id = options
        .app_id
        .unwrap_or_else(|| crate::app_icon::APP_ID.to_string());

    let run_result = event_loop
        .run_app(&mut application)
        .map_err(|error| format!("Linux event loop failed: {error}"));
    application.request_terminal_shutdown();
    let reader_join_result = application.join_reader();
    let finish_result = application.finish();

    let remaining_panes = application
        .panes
        .lock()
        .map(|mut tree| tree.take_all_panes())
        .unwrap_or_else(|poisoned| poisoned.into_inner().take_all_panes());
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

    combine_launch_results(run_result, reader_join_result, finish_result)
}

fn xrgb_components(color: u32) -> (u8, u8, u8) {
    (
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
    )
}

fn xrgb_from_hex(hex: &str) -> u32 {
    let (red, green, blue) = ColorConfig::parse_hex(hex);
    rgb_to_xrgb(red, green, blue)
}

/// Divider colors from `[theme.chrome]`, packed for the software surface.
#[derive(Debug, Clone, Copy)]
struct DividerColors {
    /// `hanko_red`: the divider touches the focused pane or one asking for attention.
    focused: u32,
    /// `sumi_ghost`: every other divider.
    idle: u32,
}

#[derive(Debug, Clone)]
struct FontSettings {
    family: String,
    size: f32,
    zoom_step: f32,
    min_size: f32,
    max_size: f32,
}

struct LinuxWindowParams {
    terminal_dimensions: TerminalDimensions,
    colors: TerminalColors,
    palette_overrides: PaletteOverrides,
    font: FontSettings,
    initial_size: LogicalSize<u32>,
    exit_after_first_frame: bool,
    panes: Arc<Mutex<PaneTree>>,
    image_store: Arc<Mutex<ImageStore>>,
    reader_wake: Arc<ReaderWake>,
    reader_handle: JoinHandle<()>,
    should_close: Arc<AtomicBool>,
    window_title: Arc<WindowTitleMailbox>,
    pane_cleanup: PaneCleanup,
    keybinds: KeybindMap,
    resize_step: f32,
    divider_colors: DividerColors,
    shared_cell_dimensions: Arc<AtomicU32>,
    redraw_pending: Arc<AtomicBool>,
    notifications: Arc<NotificationController>,
    window_focused: Arc<AtomicBool>,
    /// `[confirm] on_close_pane` / `on_quit`: ask before closing a pane or
    /// the window while a job runs in the foreground.
    confirm_on_close_pane: bool,
    confirm_on_quit: bool,
}

/// A clipboard paste requested while `pane` was focused; applied only if the
/// pane still shows the same screen in the same paste mode.
#[derive(Debug, Clone, Copy)]
struct PendingPaste {
    pane: PaneId,
    bracketed: bool,
    revision: u64,
}

/// One pane ready to paint: its rectangle, cells and visible images.
struct PaneFrame {
    viewport: PaneViewport,
    snapshot: GridSnapshot,
    images: Vec<ImageSnapshot>,
}

struct LinuxWindow {
    app_id: String,
    clipboard: Option<LinuxClipboard>,
    paste_request: u64,
    pending_pastes: HashMap<u64, PendingPaste>,
    /// Selection bookkeeping per pane (revision and evicted rows).
    selection_contexts: HashMap<PaneId, SelectionContext>,
    /// Pointer device dragging a selection, and the pane it started in.
    selection_drag: Option<(DeviceId, PaneId)>,
    /// Pane that received the last button press; motion with a held button
    /// keeps reporting to it.
    pointer_pane: Option<PaneId>,
    // Drop the surface and its display connection before releasing the window.
    surface: Option<SoftwareSurface>,
    context: Option<Context<Arc<Window>>>,
    window: Option<Arc<Window>>,
    glyph_atlas: Option<GlyphAtlas>,
    atlas_scale_factor: Option<f64>,
    cell_dimensions: Option<(u16, u16)>,
    shared_cell_dimensions: Arc<AtomicU32>,
    /// `(columns, rows, pixel width, pixel height)` last applied to each PTY.
    applied_pty_sizes: HashMap<PaneId, (u16, u16, u16, u16)>,
    presented_scenes: std::collections::VecDeque<PresentedScene>,
    background: u32,
    colors: TerminalColors,
    palette_overrides: PaletteOverrides,
    theme_watcher: Option<ThemeWatcher>,
    font: FontSettings,
    base_font_size: f32,
    /// `[window] columns`/`rows`: the first pane's grid and window size.
    terminal_dimensions: TerminalDimensions,
    initial_size: LogicalSize<u32>,
    exit_after_first_frame: bool,
    first_frame_presented: bool,
    /// Focused pane's title, before the update notice is appended.
    base_window_title: String,
    applied_window_title: String,
    panes: Arc<Mutex<PaneTree>>,
    image_store: Arc<Mutex<ImageStore>>,
    reader_wake: Arc<ReaderWake>,
    reader_handle: Option<JoinHandle<()>>,
    should_close: Arc<AtomicBool>,
    window_title: Arc<WindowTitleMailbox>,
    pane_cleanup: PaneCleanup,
    keybinds: KeybindMap,
    resize_step: f32,
    /// Reserved for the status bar (Frente B2); panes already leave it free.
    status_bar_height: f32,
    divider_colors: DividerColors,
    redraw_pending: Arc<AtomicBool>,
    notifications: Arc<NotificationController>,
    window_focused: Arc<AtomicBool>,
    update_status: crate::update::SharedUpdateStatus,
    /// The last pane closed; the window exits after presenting one more frame.
    terminal_closed: bool,
    confirm_on_close_pane: bool,
    confirm_on_quit: bool,
    /// Close waiting for y/N; keys and clicks go nowhere else meanwhile.
    close_prompt: Option<ClosePrompt>,
    modifiers: ModifiersState,
    last_window_focus: Option<bool>,
    occluded: bool,
    pointer_route: PointerRouteState<DeviceId>,
    ime_active: bool,
    ime_preedit: Option<ImePreedit>,
    last_ime_cursor_area: Option<ImeCursorArea>,
    error: Option<String>,
}

/// Window geometry the pane shortcuts need; zoom is applied by the window
/// after the action returns.
struct LinuxPaneActionHost {
    viewport: PixelRect,
    cell_size: (f32, f32),
    status_bar_height: f32,
    resize_step: f32,
    zoom: Option<ZoomDirection>,
    /// Ask before closing a pane with a foreground job.
    confirm_close_pane: bool,
    close_prompt: Option<ClosePrompt>,
}

impl PaneActionHost for LinuxPaneActionHost {
    fn viewport(&self) -> PixelRect {
        self.viewport
    }

    fn cell_size(&self) -> (f32, f32) {
        self.cell_size
    }

    fn status_bar_height(&self) -> f32 {
        self.status_bar_height
    }

    fn resize_step(&self) -> f32 {
        self.resize_step
    }

    fn zoom(&mut self, _tree: &mut PaneTree, direction: ZoomDirection) {
        // The atlas lives outside the tree lock; the window rebuilds it next.
        self.zoom = Some(direction);
    }

    fn confirm_close(&mut self, id: PaneId, pane: Option<&Pane>) -> bool {
        self.close_prompt = pane_close_prompt(self.confirm_close_pane, id, pane);
        self.close_prompt.is_some()
    }
}

/// Lock the tree through the field alone, so other window fields stay
/// borrowable while the guard lives.
fn lock_tree(panes: &Mutex<PaneTree>) -> Result<MutexGuard<'_, PaneTree>, GridAccessError> {
    panes.lock().map_err(|_| GridAccessError::Poisoned)
}

/// Area the panes share, clamped so no pane exceeds the PTY limits.
fn layout_viewport(size: PhysicalSize<u32>, cell_dimensions: (u16, u16)) -> PixelRect {
    let max_width = u32::from(MAX_TERMINAL_COLUMNS) * u32::from(cell_dimensions.0);
    let max_height = u32::from(MAX_TERMINAL_ROWS) * u32::from(cell_dimensions.1);
    PixelRect {
        x: 0.0,
        y: 0.0,
        width: size.width.min(max_width) as f32,
        height: size.height.min(max_height) as f32,
    }
}

/// Pane rectangle in whole physical pixels.
fn pane_viewport(rect: PixelRect) -> PaneViewport {
    let edge = |value: f32| value.max(0.0).round() as u32;
    PaneViewport {
        origin: (edge(rect.x), edge(rect.y)),
        size: (edge(rect.width), edge(rect.height)),
    }
}

/// Pixel rectangle of a divider line of `DIVIDER_THICKNESS` centred on it.
fn divider_rectangle(divider: &DividerInfo) -> ((i32, i32), (u32, u32)) {
    let half = DIVIDER_THICKNESS / 2.0;
    let (x, y, width, height) = match divider.direction {
        SplitDirection::Vertical => (
            divider.x0 - half,
            divider.y0,
            DIVIDER_THICKNESS,
            divider.y1 - divider.y0,
        ),
        SplitDirection::Horizontal => (
            divider.x0,
            divider.y0 - half,
            divider.x1 - divider.x0,
            DIVIDER_THICKNESS,
        ),
    };
    (
        (x.round() as i32, y.round() as i32),
        (
            width.max(0.0).round() as u32,
            height.max(0.0).round() as u32,
        ),
    )
}

/// Pane under `position` and the position relative to its rectangle; the
/// focused pane when the pointer is on a divider or outside every pane.
fn pane_under_pointer(
    tree: &PaneTree,
    position: PhysicalPosition<f64>,
) -> Option<(PaneId, PhysicalPosition<f64>)> {
    let id = tree
        .pane_at(position.x as f32, position.y as f32)
        .unwrap_or(tree.focused);
    let rect = tree.pane(id)?.rect;
    Some((id, pane_local_position(rect, position)))
}

fn pane_local_position(rect: PixelRect, position: PhysicalPosition<f64>) -> PhysicalPosition<f64> {
    PhysicalPosition::new(
        position.x - f64::from(rect.x),
        position.y - f64::from(rect.y),
    )
}

/// Key for the search prompt of the focused pane, when it is open.
fn search_key_from_winit(logical_key: &Key, modifiers: ModifiersState) -> SearchKey {
    let chord = modifiers.control_key() || modifiers.alt_key() || modifiers.super_key();
    match logical_key {
        Key::Named(NamedKey::Escape) => SearchKey::Escape,
        Key::Named(NamedKey::Enter) if modifiers.shift_key() => SearchKey::ShiftEnter,
        Key::Named(NamedKey::Enter) => SearchKey::Enter,
        Key::Named(NamedKey::Backspace) => SearchKey::Backspace,
        Key::Named(NamedKey::Space) if !chord => SearchKey::Char(' '),
        Key::Character(text) if !chord => text
            .chars()
            .next()
            .filter(|character| !character.is_control())
            .map_or(SearchKey::Ignore, SearchKey::Char),
        _ => SearchKey::Ignore,
    }
}

/// Give a key to the pane's open search, then keep the current match out
/// from under the search strip. `None` when no search is open.
fn handle_search_key(pane: &mut Pane, key: SearchKey) -> Option<SearchKeyOutcome> {
    let outcome = pane.handle_search_key(key)?;
    if let Some(search) = pane.search.as_ref() {
        keep_match_clear_of_strip(search, &mut pane.grid);
    }
    Some(outcome)
}

/// Append committed IME text to the open search query; true when it changed.
fn type_into_search(pane: &mut Pane, bytes: &[u8]) -> bool {
    let mut changed = false;
    for character in String::from_utf8_lossy(bytes).chars() {
        if character.is_control() {
            continue;
        }
        changed |= matches!(
            handle_search_key(pane, SearchKey::Char(character)),
            Some(SearchKeyOutcome::Changed)
        );
    }
    changed
}

/// Shortcuts that must not fire again while the key is held.
fn pane_action_ignores_repeat(action: PaneAction) -> bool {
    matches!(
        action,
        PaneAction::SplitVertical
            | PaneAction::SplitHorizontal
            | PaneAction::ClosePane
            | PaneAction::Search
    )
}

/// Paint search matches over the snapshot: the selection colors for every
/// match, the hanko accent behind the current one.
fn apply_search_to_snapshot(
    snapshot: &mut GridSnapshot,
    search: &SearchState,
    grid: &Grid,
    colors: &TerminalColors,
    current_background: u32,
) {
    if search.matches.is_empty() {
        return;
    }
    let foreground = colors.selection_foreground();
    let background = colors.selection_background();
    let current = (
        ((current_background >> 16) & 0xff) as u8,
        ((current_background >> 8) & 0xff) as u8,
        (current_background & 0xff) as u8,
    );
    for row in 0..snapshot.rows {
        for column in 0..snapshot.columns {
            let Some(kind) = search.contains_cell(grid, row, column) else {
                continue;
            };
            let cell = &mut snapshot.cells[row * snapshot.columns + column];
            cell.fg = Color::Rgb(foreground.0, foreground.1, foreground.2);
            cell.bg = match kind {
                MatchKind::Current => Color::Rgb(current.0, current.1, current.2),
                MatchKind::Other => Color::Rgb(background.0, background.1, background.2),
            };
            cell.flags.remove(CellFlags::REVERSE | CellFlags::FAINT);
        }
    }
}

impl LinuxWindow {
    fn new(params: LinuxWindowParams) -> Self {
        let background = params.colors.default_background();
        Self {
            app_id: crate::app_icon::APP_ID.to_string(),
            clipboard: None,
            paste_request: 0,
            pending_pastes: HashMap::new(),
            selection_contexts: HashMap::new(),
            selection_drag: None,
            pointer_pane: None,
            surface: None,
            context: None,
            window: None,
            glyph_atlas: None,
            atlas_scale_factor: None,
            cell_dimensions: None,
            shared_cell_dimensions: params.shared_cell_dimensions,
            applied_pty_sizes: HashMap::new(),
            presented_scenes: std::collections::VecDeque::new(),
            background: rgb_to_xrgb(background.0, background.1, background.2),
            colors: params.colors,
            palette_overrides: params.palette_overrides,
            theme_watcher: None,
            base_font_size: params.font.size,
            font: params.font,
            terminal_dimensions: params.terminal_dimensions,
            initial_size: params.initial_size,
            exit_after_first_frame: params.exit_after_first_frame,
            first_frame_presented: false,
            base_window_title: WINDOW_TITLE.to_string(),
            applied_window_title: WINDOW_TITLE.to_string(),
            panes: params.panes,
            image_store: params.image_store,
            reader_wake: params.reader_wake,
            reader_handle: Some(params.reader_handle),
            should_close: params.should_close,
            window_title: params.window_title,
            pane_cleanup: params.pane_cleanup,
            keybinds: params.keybinds,
            resize_step: params.resize_step,
            status_bar_height: 0.0,
            divider_colors: params.divider_colors,
            redraw_pending: params.redraw_pending,
            notifications: params.notifications,
            window_focused: params.window_focused,
            update_status: Arc::new(Mutex::new(crate::update::UpdateStatus::Idle)),
            terminal_closed: false,
            confirm_on_close_pane: params.confirm_on_close_pane,
            confirm_on_quit: params.confirm_on_quit,
            close_prompt: None,
            modifiers: ModifiersState::empty(),
            last_window_focus: None,
            occluded: false,
            pointer_route: PointerRouteState::default(),
            ime_active: false,
            ime_preedit: None,
            last_ime_cursor_area: None,
            error: None,
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn title_with_update_notice(&self, title: &str) -> String {
        crate::update::title_with_notice(title, &crate::update::read_status(&self.update_status))
    }

    /// Apply the mailbox title (plus the update notice) to the native window.
    fn apply_window_title(&mut self) {
        if let Some(title) = self.window_title.take_pending_title() {
            self.base_window_title = title;
        }
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let title = self.title_with_update_notice(&self.base_window_title);
        sync_window_title_with(&mut self.applied_window_title, &title, |title| {
            window.set_title(title)
        });
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        if let Some(title) = self.window_title.take_pending_title() {
            self.base_window_title = title;
        }
        let initial_title = self.title_with_update_notice(&self.base_window_title);
        let attributes = Window::default_attributes()
            .with_title(normalized_window_title(&initial_title).into_owned())
            .with_window_icon(application_icon())
            .with_transparent(false)
            .with_inner_size(self.initial_size);
        let attributes =
            WindowAttributesExtWayland::with_name(attributes, self.app_id.clone(), "kokuban");
        let attributes =
            WindowAttributesExtX11::with_name(attributes, self.app_id.clone(), "kokuban");
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| format!("could not create an opaque window: {error}"))?,
        );
        sync_window_title_with(&mut self.applied_window_title, &initial_title, |title| {
            window.set_title(title)
        });
        window.set_ime_purpose(ImePurpose::Terminal);
        window.set_ime_allowed(true);
        let context = Context::new(window.clone())
            .map_err(|error| format!("could not initialize the software renderer: {error}"))?;
        let surface = Surface::new(&context, window.clone())
            .map_err(|error| format!("could not create the software-rendering surface: {error}"))?;
        let scale_factor = window.scale_factor();
        let glyph_atlas = self.create_glyph_atlas(scale_factor)?;
        let cell_dimensions = atlas_cell_dimensions(&glyph_atlas)?;
        // The configured grid, not the pane's: a `-e` command can finish (and
        // its pane close) before the compositor delivers `resumed`; the window
        // still opens, presents its frame and exits cleanly.
        let terminal_dimensions = self.terminal_dimensions;
        let requested_inner_size = physical_size_for_terminal(terminal_dimensions, cell_dimensions)
            .ok_or_else(|| {
                format!(
                    "could not calculate the Linux window size for terminal {terminal_dimensions}"
                )
            })?;

        self.presented_scenes.clear();
        self.surface = Some(surface);
        self.context = Some(context);
        self.window = Some(window.clone());
        self.glyph_atlas = Some(glyph_atlas);
        self.atlas_scale_factor = Some(scale_factor);
        self.set_cell_dimensions(cell_dimensions);
        let applied_inner_size = immediate_surface_size_to_reconcile(
            window.request_inner_size(requested_inner_size),
            window.inner_size(),
        );
        self.relayout_panes(applied_inner_size)?;
        window.request_redraw();
        Ok(())
    }

    fn create_glyph_atlas(&self, scale_factor: f64) -> Result<GlyphAtlas, String> {
        create_glyph_atlas(&self.font.family, self.font.size, scale_factor)
    }

    fn set_cell_dimensions(&mut self, cell_dimensions: (u16, u16)) {
        self.cell_dimensions = Some(cell_dimensions);
        self.shared_cell_dimensions
            .store(pack_cell_dimensions(cell_dimensions), Ordering::Release);
    }

    fn rebuild_glyph_atlas(&mut self, scale_factor: f64) -> Result<bool, GlyphAtlasRebuildError> {
        let mut replacement_dimensions = None;
        let changed = replace_glyph_atlas_for_scale(
            &mut self.glyph_atlas,
            &mut self.atlas_scale_factor,
            &self.font.family,
            self.font.size,
            scale_factor,
            |_, cell_dimensions| {
                replacement_dimensions = Some(cell_dimensions);
                Ok(())
            },
        )?;
        if changed {
            self.presented_scenes.clear();
            if let Some(cell_dimensions) = replacement_dimensions {
                self.set_cell_dimensions(cell_dimensions);
            }
        }
        Ok(changed)
    }

    /// Replace the atlas for a new font size at the current scale.
    fn apply_zoom(&mut self, direction: ZoomDirection) -> Result<bool, String> {
        let next_size = match direction {
            ZoomDirection::In => (self.font.size + self.font.zoom_step).min(self.font.max_size),
            ZoomDirection::Out => (self.font.size - self.font.zoom_step).max(self.font.min_size),
            ZoomDirection::Reset => self.base_font_size,
        };
        if (next_size - self.font.size).abs() < 0.01 {
            return Ok(false);
        }
        let scale_factor = self.atlas_scale_factor.unwrap_or(1.0);
        let atlas = create_glyph_atlas(&self.font.family, next_size, scale_factor)?;
        let cell_dimensions = atlas_cell_dimensions(&atlas)?;
        self.font.size = next_size;
        self.glyph_atlas = Some(atlas);
        self.set_cell_dimensions(cell_dimensions);
        self.presented_scenes.clear();
        Ok(true)
    }

    /// Fit every pane into the surface and size its grid and PTY.
    fn relayout_panes(&mut self, size: PhysicalSize<u32>) -> Result<(), String> {
        let cell_dimensions = self.cell_dimensions.ok_or_else(|| {
            "could not lay out the Linux panes before glyph metrics were ready".to_string()
        })?;
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let viewport = layout_viewport(size, cell_dimensions);
        let mut tree = lock_tree(&self.panes).map_err(|error| error.to_string())?;
        tree.relayout(
            viewport,
            f32::from(cell_dimensions.0),
            f32::from(cell_dimensions.1),
            self.status_bar_height,
        );
        sync_pane_metrics(&mut tree, cell_dimensions, &mut self.applied_pty_sizes)
    }

    /// Current pane area, for the shortcuts and hit testing.
    fn current_viewport(&self) -> Option<PixelRect> {
        let window = self.window.as_ref()?;
        let cell_dimensions = self.cell_dimensions?;
        let size = window.inner_size();
        (size.width > 0 && size.height > 0).then(|| layout_viewport(size, cell_dimensions))
    }

    fn present_frame(&mut self) -> Result<Option<bool>, String> {
        let window =
            self.window.as_ref().cloned().ok_or_else(|| {
                "redraw requested before the Linux window was created".to_string()
            })?;
        let size = window.inner_size();
        let Some((width, height)) = drawable_dimensions(size).map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let cell_dimensions = self
            .cell_dimensions
            .ok_or_else(|| "redraw requested before Linux glyph metrics were ready".to_string())?;
        let frame_size = (width.get(), height.get());
        let strip_colors = StripColors::new(
            self.colors.default_background(),
            self.colors.resolve_foreground(Color::Default, false),
            xrgb_components(self.divider_colors.focused),
        );
        let (pane_frames, dividers, focused_index) = {
            // Match the reader's lock order: pane tree, then image store.
            let mut tree = lock_tree(&self.panes).map_err(|error| error.to_string())?;
            let store = self
                .image_store
                .lock()
                .map_err(|_| "image cache lock is poisoned".to_string())?;
            let (layouts, dividers) = tree.layout_info(layout_viewport(size, cell_dimensions));
            let focused = tree.focused;
            let mut pane_frames = Vec::with_capacity(layouts.len());
            let mut focused_index = None;
            for (id, rect) in layouts {
                let Some(pane) = tree.pane_mut(id) else {
                    continue;
                };
                if let Some(search) = pane.search.as_mut() {
                    if search.needs_recompute(&pane.grid) {
                        search.recompute(&pane.grid);
                    }
                }
                let context = self.selection_contexts.entry(id).or_default();
                sync_selection(&mut pane.selection, context, &pane.grid);
                let mut snapshot = snapshot_locked_grid(&pane.grid);
                apply_selection_to_snapshot(
                    &mut snapshot,
                    &pane.selection,
                    &pane.grid,
                    &self.colors,
                );
                if let Some(search) = pane.search.as_mut() {
                    let row = search_strip_row(search, &pane.grid);
                    apply_search_to_snapshot(
                        &mut snapshot,
                        search,
                        &pane.grid,
                        &self.colors,
                        self.divider_colors.focused,
                    );
                    let prompt = search_prompt(search, snapshot.columns, strip_colors);
                    paint_strip(
                        &mut snapshot,
                        row,
                        &prompt.segments,
                        strip_colors.background,
                    );
                    if id == focused {
                        // Typed text and IME candidates belong to the query.
                        snapshot.cursor = None;
                        snapshot.input_cursor = (
                            row,
                            prompt.caret_column.min(snapshot.columns.saturating_sub(1)),
                        );
                    }
                }
                if let Some(prompt) = self
                    .close_prompt
                    .as_ref()
                    .filter(|prompt| prompt.shown_in(focused) == id)
                {
                    let segments = close_prompt_segments(
                        prompt.question(),
                        &prompt.running,
                        snapshot.columns,
                        strip_colors,
                    );
                    let row = snapshot.rows.saturating_sub(1);
                    paint_strip(&mut snapshot, row, &segments, strip_colors.background);
                }
                let images =
                    snapshot_images(&pane.kitty_handler, &store, &pane.grid, cell_dimensions);
                if id == focused {
                    focused_index = Some(pane_frames.len());
                }
                pane_frames.push(PaneFrame {
                    viewport: pane_viewport(rect),
                    snapshot,
                    images,
                });
            }
            (pane_frames, dividers, focused_index)
        };
        let focused_frame = focused_index.and_then(|index| pane_frames.get(index));
        let preedit_layout = self
            .ime_preedit
            .as_ref()
            .zip(focused_frame)
            .map(|(preedit, frame)| layout_ime_preedit_for_snapshot(preedit, &frame.snapshot));
        let next_ime_cursor_area = focused_frame.and_then(|frame| {
            let ime_cursor = preedit_layout
                .as_ref()
                .and_then(|layout| layout.caret_cell)
                .unwrap_or(frame.snapshot.input_cursor);
            let area = ime_cursor_area(
                ime_cursor,
                (frame.snapshot.columns, frame.snapshot.rows),
                cell_dimensions,
                frame.viewport.size,
            )?;
            let (origin_x, origin_y) = frame.viewport.origin;
            Some(ImeCursorArea {
                position: PhysicalPosition::new(
                    area.position
                        .x
                        .saturating_add(i32::try_from(origin_x).ok()?),
                    area.position
                        .y
                        .saturating_add(i32::try_from(origin_y).ok()?),
                ),
                size: area.size,
            })
        });
        let surface = self
            .surface
            .as_mut()
            .ok_or_else(|| "redraw requested before the software renderer was ready".to_string())?;
        let glyph_atlas = self
            .glyph_atlas
            .as_mut()
            .ok_or_else(|| "redraw requested before the Linux glyph atlas was ready".to_string())?;

        surface
            .resize(width, height)
            .map_err(|error| format!("could not resize the software-rendering surface: {error}"))?;
        let mut buffer = surface
            .buffer_mut()
            .map_err(|error| format!("could not acquire the software-rendering buffer: {error}"))?;
        let background = self.background;
        let colors = &self.colors;
        // The band (damage) path needs the single pane to cover the frame so
        // the buffer history describes the same pixels; split layouts repaint.
        let single_pane = match pane_frames.as_slice() {
            [frame] if frame.viewport.covers_frame(frame_size) => Some(frame),
            _ => None,
        };
        let has_overlays;
        if let Some(frame) = single_pane {
            has_overlays = !frame.images.is_empty() || preedit_layout.is_some();
            let damage = frame_damage(
                scene_for_buffer_age(&self.presented_scenes, buffer.age()),
                &frame.snapshot,
                frame_size,
                cell_dimensions,
                has_overlays,
                glyph_atlas,
            );
            paint_pane_viewport(
                &mut buffer,
                frame_size,
                frame.viewport,
                |canvas, canvas_size| {
                    if !damage.is_empty() {
                        let start = damage.start as usize * canvas_size.0 as usize;
                        let end = damage.end as usize * canvas_size.0 as usize;
                        canvas[start..end].fill(background);
                        draw_grid_snapshot_in_band(
                            canvas,
                            canvas_size,
                            glyph_atlas,
                            colors,
                            cell_dimensions,
                            &frame.snapshot,
                            preedit_layout.is_none(),
                            &frame.images,
                            damage,
                        );
                    }
                    if let Some(layout) = preedit_layout.as_ref() {
                        draw_ime_preedit(
                            canvas,
                            canvas_size,
                            glyph_atlas,
                            colors,
                            cell_dimensions,
                            &frame.snapshot,
                            layout,
                        );
                    }
                },
            );
        } else {
            has_overlays = true;
            buffer.fill(background);
            for (index, frame) in pane_frames.iter().enumerate() {
                let preedit_layout = preedit_layout
                    .as_ref()
                    .filter(|_| focused_index == Some(index));
                paint_pane_viewport(
                    &mut buffer,
                    frame_size,
                    frame.viewport,
                    |canvas, canvas_size| {
                        canvas.fill(background);
                        draw_grid_snapshot_in_band(
                            canvas,
                            canvas_size,
                            glyph_atlas,
                            colors,
                            cell_dimensions,
                            &frame.snapshot,
                            preedit_layout.is_none(),
                            &frame.images,
                            0..canvas_size.1,
                        );
                        if let Some(layout) = preedit_layout {
                            draw_ime_preedit(
                                canvas,
                                canvas_size,
                                glyph_atlas,
                                colors,
                                cell_dimensions,
                                &frame.snapshot,
                                layout,
                            );
                        }
                    },
                );
            }
            for divider in &dividers {
                let color = if divider.touches_focused || divider.touches_attention {
                    self.divider_colors.focused
                } else {
                    self.divider_colors.idle
                };
                let (origin, size) = divider_rectangle(divider);
                fill_rect(&mut buffer, frame_size, origin, size, color, u8::MAX);
            }
        }
        let terminal_content_visible =
            !self.exit_after_first_frame || buffer.iter().any(|&pixel| pixel != background);
        window.pre_present_notify();
        buffer
            .present()
            .map_err(|error| format!("could not present a Linux frame: {error}"))?;
        match single_pane {
            Some(_) => {
                let frame = pane_frames
                    .into_iter()
                    .next()
                    .expect("the single pane frame was just painted");
                self.presented_scenes.push_front(PresentedScene {
                    snapshot: frame.snapshot,
                    frame_size,
                    cell_dimensions,
                    has_overlays,
                });
                self.presented_scenes.truncate(MAX_PRESENTED_SCENES);
            }
            None => self.presented_scenes.clear(),
        }
        sync_ime_cursor_area_with(
            self.ime_active,
            &mut self.last_ime_cursor_area,
            next_ime_cursor_area,
            |area| window.set_ime_cursor_area(area.position, area.size),
        );
        Ok(Some(terminal_content_visible))
    }

    /// Clear the focused pane's selection after input reached its shell.
    fn clear_focused_selection(&mut self) {
        let cleared = lock_tree(&self.panes).is_ok_and(|mut tree| {
            tree.focused_pane_mut().is_some_and(|pane| {
                let active = pane.selection.is_active();
                pane.selection.clear();
                active
            })
        });
        if cleared {
            self.request_redraw();
        }
    }

    /// OSC 8 target under the pointer, in retained-grid coordinates.
    fn hyperlink_at_pointer(
        &self,
        position: PhysicalPosition<f64>,
    ) -> Result<Option<String>, GridAccessError> {
        let Some(cell_size) = self.cell_dimensions else {
            return Ok(None);
        };
        let tree = lock_tree(&self.panes)?;
        let Some((id, local)) = pane_under_pointer(&tree, position) else {
            return Ok(None);
        };
        let Some(pane) = tree.pane(id) else {
            return Ok(None);
        };
        let Some(point) = selection_point_at_pointer(&pane.grid, local, cell_size) else {
            return Ok(None);
        };
        let Ok(retained_row) = usize::try_from(point.row) else {
            return Ok(None);
        };
        Ok(pane
            .grid
            .hyperlink_at_retained(retained_row, point.col)
            .map(str::to_owned))
    }

    /// Start or extend the selection of `pane` at a window position.
    fn select_at_pointer(
        &mut self,
        pane_id: PaneId,
        position: PhysicalPosition<f64>,
        start: bool,
    ) -> Result<bool, GridAccessError> {
        let Some(cell_size) = self.cell_dimensions else {
            return Ok(false);
        };
        let mut tree = lock_tree(&self.panes)?;
        let Some(pane) = tree.pane_mut(pane_id) else {
            return Ok(false);
        };
        let context = self.selection_contexts.entry(pane_id).or_default();
        sync_selection(&mut pane.selection, context, &pane.grid);
        if !start && !pane.selection.is_active() {
            return Ok(false);
        }
        let local = pane_local_position(pane.rect, position);
        let Some(point) = selection_point_at_pointer(&pane.grid, local, cell_size) else {
            return Ok(false);
        };
        if start {
            pane.selection.start(point);
        } else {
            pane.selection.update(point);
        }
        drop(tree);
        self.request_redraw();
        Ok(true)
    }

    /// Focus the pane under a button press, with focus reports and title.
    fn focus_pane_at_pointer(
        &mut self,
        position: PhysicalPosition<f64>,
    ) -> Result<(), GridAccessError> {
        let mut tree = lock_tree(&self.panes)?;
        let Some((id, _)) = pane_under_pointer(&tree, position) else {
            return Ok(());
        };
        if id == tree.focused {
            return Ok(());
        }
        let previous_focus = tree.focused_pane().map(|pane| pane.id);
        tree.focused = id;
        dispatch_pane_focus_transition_locked(
            &tree,
            self.window_focused.as_ref(),
            previous_focus,
            None,
        );
        publish_focused_window_title(&tree, self.window_title.as_ref());
        tree.clear_focused_attention();
        drop(tree);
        self.presented_scenes.clear();
        self.last_ime_cursor_area = None;
        self.apply_window_title();
        self.request_redraw();
        Ok(())
    }

    fn handle_clipboard_action(&mut self, action: ClipboardAction) -> Result<(), GridAccessError> {
        let mut tree = lock_tree(&self.panes)?;
        let focused = tree.focused;
        let Some(pane) = tree.pane_mut(focused) else {
            return Ok(());
        };
        let context = self.selection_contexts.entry(focused).or_default();
        sync_selection(&mut pane.selection, context, &pane.grid);
        let grid = &pane.grid;
        match action {
            ClipboardAction::SelectAll => {
                let first_row = if grid.using_alt_screen {
                    grid.scrollback_len()
                } else {
                    0
                };
                pane.selection.start(GridPoint {
                    row: first_row as i64,
                    col: 0,
                });
                pane.selection.update(GridPoint {
                    row: (grid.retained_rows() - 1) as i64,
                    col: grid.cols() - 1,
                });
                drop(tree);
                self.request_redraw();
            }
            ClipboardAction::Copy => {
                if pane.selection.is_active() {
                    match pane
                        .selection
                        .get_text_with_limit(grid, MAX_CLIPBOARD_TEXT_BYTES)
                    {
                        Ok(text) => {
                            if let Some(clipboard) = &self.clipboard {
                                if let Err(error) = clipboard.copy_text(text) {
                                    log::warn!("could not copy selection: {error}");
                                }
                            }
                        }
                        Err(_) => log::warn!("selection exceeds the clipboard byte limit"),
                    }
                }
            }
            ClipboardAction::Paste => {
                // Keep the target pane, mode and revision with this request.
                // Clipboard IO runs asynchronously and may finish after a
                // screen transition. Bound replies waiting in the UI event
                // queue, as well as the clipboard worker's own bounded queue.
                if self.pending_pastes.len() >= 8 {
                    log::warn!("clipboard paste queue is full");
                    return Ok(());
                }
                self.paste_request = self.paste_request.wrapping_add(1);
                while self.pending_pastes.contains_key(&self.paste_request) {
                    self.paste_request = self.paste_request.wrapping_add(1);
                }
                if let Some(clipboard) = &self.clipboard {
                    match clipboard.request_paste(self.paste_request) {
                        Ok(()) => {
                            self.pending_pastes.insert(
                                self.paste_request,
                                PendingPaste {
                                    pane: focused,
                                    bracketed: grid.bracketed_paste,
                                    revision: grid.screen_revision(),
                                },
                            );
                        }
                        Err(error) => log::warn!("could not request clipboard paste: {error}"),
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_clipboard_event(&mut self, event: ClipboardEvent) -> Result<(), GridAccessError> {
        match event {
            ClipboardEvent::CopyFinished(result) => {
                if let Err(error) = result {
                    log::warn!("could not copy selection: {error}");
                }
            }
            ClipboardEvent::Paste { request_id, result } => {
                let Some(pending) = self.pending_pastes.remove(&request_id) else {
                    return Ok(());
                };
                let text = match result {
                    Ok(text) => text,
                    Err(error) => {
                        log::warn!("could not read clipboard: {error}");
                        return Ok(());
                    }
                };
                let mut tree = lock_tree(&self.panes)?;
                let Some(pane) = tree.pane_mut(pending.pane) else {
                    log::warn!("clipboard paste cancelled because its pane closed");
                    return Ok(());
                };
                let bytes = match encode_paste(
                    &text,
                    pending.bracketed,
                    pane.max_nonfatal_input_bytes().min(MAX_PASTE_BYTES),
                ) {
                    Ok(bytes) if !bytes.is_empty() => bytes,
                    Ok(_) => return Ok(()),
                    Err(error) => {
                        log::warn!("could not encode clipboard paste: {error}");
                        return Ok(());
                    }
                };
                if pane.grid.bracketed_paste != pending.bracketed
                    || pane.grid.screen_revision() != pending.revision
                {
                    log::warn!("clipboard paste cancelled because the terminal screen or paste mode changed");
                    return Ok(());
                }
                match pane.queue_paste_input(bytes) {
                    Ok(()) => {
                        pane.grid.scroll_offset = 0;
                        pane.selection.clear();
                        drop(tree);
                        self.request_redraw();
                    }
                    Err(error) => log::warn!("clipboard paste was not queued: {error}"),
                }
            }
        }
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: String) {
        if self.error.is_none() {
            self.error = Some(error);
        }
        self.request_terminal_shutdown();
        event_loop.exit();
    }

    fn update_ime_preedit(&mut self, next: Option<ImePreedit>) -> bool {
        if !replace_ime_preedit(&mut self.ime_preedit, next) {
            return false;
        }
        self.request_redraw();
        true
    }

    fn handle_terminal_input_result(
        &mut self,
        event_loop: &ActiveEventLoop,
        result: Result<KeyboardInputOutcome, KeyboardInputError>,
    ) {
        match result {
            Ok(outcome) => {
                if outcome.viewport_changed() {
                    self.request_redraw();
                }
            }
            Err(error) => self.fail(event_loop, error.to_string()),
        }
    }

    fn handle_mouse_motion_result(
        &mut self,
        event_loop: &ActiveEventLoop,
        result: Result<MouseMotionDispatchOutcome, KeyboardInputError>,
    ) {
        match result {
            Ok(outcome) => self.pointer_route.motion_state.record_dispatch(outcome),
            Err(error) => self.fail(event_loop, error.to_string()),
        }
    }

    /// Apply a `[keybind.linux]` shortcut to the pane tree, then the effects
    /// the tree leaves to the window: focus reports, title, atlas for zoom,
    /// PTY sizes, retiring the closed pane and waking the reader.
    fn handle_pane_action(&mut self, event_loop: &ActiveEventLoop, action: PaneAction) {
        self.apply_pane_action(event_loop, action, self.confirm_on_close_pane);
    }

    /// `ask_before_close` is false once the user confirmed the close prompt.
    fn apply_pane_action(
        &mut self,
        event_loop: &ActiveEventLoop,
        action: PaneAction,
        ask_before_close: bool,
    ) {
        let (Some(viewport), Some(cell_dimensions)) =
            (self.current_viewport(), self.cell_dimensions)
        else {
            return;
        };
        let mut host = LinuxPaneActionHost {
            viewport,
            cell_size: (f32::from(cell_dimensions.0), f32::from(cell_dimensions.1)),
            status_bar_height: self.status_bar_height,
            resize_step: self.resize_step,
            zoom: None,
            confirm_close_pane: ask_before_close,
            close_prompt: None,
        };
        // Hold the guard through a local handle: zoom rebuilds the atlas
        // (and other window fields) while the tree stays locked.
        let panes = Arc::clone(&self.panes);
        let locked = lock_tree(&panes).map_err(|error| error.to_string());
        let mut tree = match locked {
            Ok(tree) => tree,
            Err(error) => {
                self.fail(event_loop, error);
                return;
            }
        };
        let outcome = apply_pane_action(&mut tree, action, &mut host);
        if action == PaneAction::Search {
            // A reopened search may land on a match under the strip.
            if let Some(pane) = tree.focused_pane_mut() {
                if let Some(search) = pane.search.as_ref() {
                    keep_match_clear_of_strip(search, &mut pane.grid);
                }
            }
        }
        if let Some(prompt) = host.close_prompt.take() {
            drop(tree);
            self.close_prompt = Some(prompt);
            self.request_redraw();
            return;
        }
        if outcome.terminate {
            dispatch_pane_focus_transition_locked(
                &tree,
                self.window_focused.as_ref(),
                outcome.focus_changed_from,
                outcome.closed_pane.as_ref(),
            );
            drop(tree);
            if let Some(pane) = outcome.closed_pane {
                self.pane_cleanup.retire(pane);
            }
            self.request_terminal_shutdown();
            event_loop.exit();
            return;
        }
        if let Some(previous_focus) = outcome.focus_changed_from {
            dispatch_pane_focus_transition_locked(
                &tree,
                self.window_focused.as_ref(),
                Some(previous_focus),
                outcome.closed_pane.as_ref(),
            );
            publish_focused_window_title(&tree, self.window_title.as_ref());
        }
        let mut relayout = outcome.needs_relayout;
        if let Some(direction) = host.zoom {
            match self.apply_zoom(direction) {
                Ok(true) => {
                    if let Some(cell_dimensions) = self.cell_dimensions {
                        tree.relayout(
                            viewport,
                            f32::from(cell_dimensions.0),
                            f32::from(cell_dimensions.1),
                            self.status_bar_height,
                        );
                    }
                    relayout = true;
                }
                Ok(false) => return,
                Err(error) => {
                    log::error!("{error}; keeping font size {}", self.font.size);
                    return;
                }
            }
        } else if !outcome.render {
            return;
        }
        tree.clear_focused_attention();
        if relayout {
            if let Some(cell_dimensions) = self.cell_dimensions {
                if let Err(error) =
                    sync_pane_metrics(&mut tree, cell_dimensions, &mut self.applied_pty_sizes)
                {
                    drop(tree);
                    self.fail(event_loop, error);
                    return;
                }
            }
        }
        drop(tree);
        if let Some(pane) = outcome.closed_pane {
            self.forget_pane(pane.id);
            self.pane_cleanup.retire(pane);
        }
        // Splits and closes change the PTYs the reader must poll.
        self.reader_wake.wake();
        self.presented_scenes.clear();
        self.last_ime_cursor_area = None;
        self.apply_window_title();
        self.request_redraw();
    }

    /// Answer the open close prompt; a pane prompt only closes the pane it
    /// was raised for, and only while that pane still has the focus.
    fn answer_close_prompt(&mut self, event_loop: &ActiveEventLoop, answer: CloseAnswer) {
        if answer == CloseAnswer::Ignore {
            return;
        }
        let Some(prompt) = self.close_prompt.take() else {
            return;
        };
        self.request_redraw();
        if answer == CloseAnswer::Cancel {
            return;
        }
        match prompt.target {
            CloseTarget::Window => {
                self.request_terminal_shutdown();
                event_loop.exit();
            }
            CloseTarget::Pane(id) => match lock_tree(&self.panes).map(|tree| tree.focused == id) {
                Ok(true) => self.apply_pane_action(event_loop, PaneAction::ClosePane, false),
                Ok(false) => {}
                Err(error) => self.fail(event_loop, error.to_string()),
            },
        }
    }

    /// Drop the window's bookkeeping for a pane that left the tree.
    fn forget_pane(&mut self, id: PaneId) {
        self.selection_contexts.remove(&id);
        self.applied_pty_sizes.remove(&id);
        self.pending_pastes.retain(|_, paste| paste.pane != id);
        if self.selection_drag.is_some_and(|(_, pane)| pane == id) {
            self.selection_drag = None;
        }
        if self.pointer_pane == Some(id) {
            self.pointer_pane = None;
        }
    }

    fn request_terminal_shutdown(&mut self) {
        self.theme_watcher.take();
        self.ime_preedit = None;
        self.should_close.store(true, Ordering::Release);
        self.reader_wake.wake();
    }

    fn join_reader(&mut self) -> Result<(), String> {
        let Some(reader) = self.reader_handle.take() else {
            return Ok(());
        };
        reader
            .join()
            .map_err(|_| "Linux terminal reader thread panicked".to_string())
    }

    fn finish(&mut self) -> Result<(), String> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        if self.exit_after_first_frame && !self.first_frame_presented {
            return Err("smoke mode exited before the first Linux frame was presented".to_string());
        }
        Ok(())
    }

    fn finish_terminal_exit_after_frame(&mut self, event_loop: &ActiveEventLoop) {
        if !self.terminal_closed {
            return;
        }
        self.request_terminal_shutdown();
        event_loop.exit();
    }

    /// Window size that keeps the terminal content at a new scale: the single
    /// pane's grid in the new cell size, or every pane scaled proportionally.
    fn preserved_physical_size(&self, scale_ratio: f64) -> Result<PhysicalSize<u32>, String> {
        let cell_dimensions = self.cell_dimensions.ok_or_else(|| {
            "could not size the Linux window before glyph metrics were ready".to_string()
        })?;
        let tree = lock_tree(&self.panes).map_err(|error| error.to_string())?;
        let single_pane = match tree.pane_ids().as_slice() {
            [id] => tree.pane(*id),
            _ => None,
        };
        if let Some(pane) = single_pane {
            let terminal_dimensions = terminal_dimensions_from_locked_grid(&pane.grid)
                .map_err(|error| error.to_string())?;
            return physical_size_for_terminal(terminal_dimensions, cell_dimensions).ok_or_else(
                || {
                    format!(
                        "could not calculate the Linux window size for terminal {terminal_dimensions}"
                    )
                },
            );
        }
        drop(tree);
        let size = self
            .window
            .as_ref()
            .map(|window| window.inner_size())
            .unwrap_or_default();
        let scale = |value: u32| (f64::from(value) * scale_ratio).round().max(1.0) as u32;
        Ok(PhysicalSize::new(scale(size.width), scale(size.height)))
    }
}

/// Publish the cell size to every pane grid and keep each PTY's window size
/// (cells and pixels) in step with its grid, including panes the tree
/// created or whose cell size changed without a column change.
fn sync_pane_metrics(
    tree: &mut PaneTree,
    cell_dimensions: (u16, u16),
    applied_pty_sizes: &mut HashMap<PaneId, (u16, u16, u16, u16)>,
) -> Result<(), String> {
    let ids = tree.pane_ids();
    applied_pty_sizes.retain(|id, _| ids.contains(id));
    for id in ids {
        let Some(pane) = tree.pane_mut(id) else {
            continue;
        };
        pane.grid.cell_pixel_width = cell_dimensions.0;
        pane.grid.cell_pixel_height = cell_dimensions.1;
        let dimensions =
            terminal_dimensions_from_locked_grid(&pane.grid).map_err(|error| error.to_string())?;
        let pixels = |cells: u16, cell: u16| {
            u16::try_from(u32::from(cells) * u32::from(cell)).unwrap_or(u16::MAX)
        };
        let target = (
            dimensions.columns,
            dimensions.rows,
            pixels(dimensions.columns, cell_dimensions.0),
            pixels(dimensions.rows, cell_dimensions.1),
        );
        if applied_pty_sizes.get(&id) == Some(&target) {
            continue;
        }
        pane.pty
            .resize_with_pixels(target.0, target.1, target.2, target.3)
            .map_err(|error| format!("could not resize the PTY of pane {id}: {error}"))?;
        applied_pty_sizes.insert(id, target);
    }
    Ok(())
}

impl Drop for LinuxWindow {
    fn drop(&mut self) {
        // An exceptional unwind can bypass `launch`'s ordered joins. Signal the
        // reader before the fields drop so it is not left blocked in poll.
        self.request_terminal_shutdown();
    }
}

impl ApplicationHandler<LinuxEvent> for LinuxWindow {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Wait);
        if event_loop.exiting() || self.occluded || self.surface.is_none() {
            return;
        }
        let (Some(window), Some(cell_size)) = (&self.window, self.cell_dimensions) else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let result = (|| {
            // Preserve the PTY reader's pane tree -> image store lock order.
            let tree = lock_tree(&self.panes).map_err(|error| error.to_string())?;
            let mut store = self
                .image_store
                .lock()
                .map_err(|_| "image cache lock is poisoned".to_string())?;
            let grids = tree
                .pane_ids()
                .into_iter()
                .filter_map(|id| tree.pane(id).map(|pane| &pane.grid))
                .collect::<Vec<_>>();
            Ok::<_, String>(advance_animations(
                &mut store,
                grids,
                cell_size,
                Instant::now(),
            ))
        })();
        match result {
            Ok(update) => {
                if update.changed {
                    window.request_redraw();
                }
                if let Some(deadline) = update.next_deadline {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
            }
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(error) = self.create_window(event_loop) {
            self.fail(event_loop, error);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window.as_ref().map(|window| window.id()) != Some(window_id) {
            return;
        }
        let accepts_input = terminal_accepts_input(event_loop.exiting(), self.terminal_closed);

        match event {
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if !occluded {
                    self.request_redraw();
                }
            }
            WindowEvent::CloseRequested => {
                let prompt = if accepts_input {
                    lock_tree(&self.panes)
                        .ok()
                        .and_then(|tree| window_close_prompt(self.confirm_on_quit, &tree))
                } else {
                    None
                };
                match prompt {
                    Some(prompt) => {
                        self.close_prompt = Some(prompt);
                        self.request_redraw();
                    }
                    None => {
                        self.request_terminal_shutdown();
                        event_loop.exit();
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::CursorEntered { device_id } => {
                self.pointer_route.cursor_entered(device_id);
            }
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => {
                if !self.pointer_route.cursor_moved(device_id, position) || !accepts_input {
                    return;
                }
                let active_button = self.pointer_route.active_motion_button();
                if let Some((_, pane_id)) = self
                    .selection_drag
                    .filter(|(drag_device, _)| *drag_device == device_id)
                {
                    if let Err(error) = self.select_at_pointer(pane_id, position, false) {
                        self.fail(event_loop, error.to_string());
                    }
                    return;
                }
                let cell_dimensions = self.cell_dimensions;
                let modifiers = self.modifiers;
                let last_cell = self.pointer_route.motion_state.last_cell;
                let held_pane = active_button.and(self.pointer_pane);
                let result = lock_tree(&self.panes)
                    .map_err(KeyboardInputError::Grid)
                    .and_then(|tree| {
                        let target = match held_pane {
                            Some(id) => tree
                                .pane(id)
                                .map(|pane| (id, pane_local_position(pane.rect, position))),
                            None => pane_under_pointer(&tree, position),
                        };
                        let Some((id, local)) = target else {
                            return Ok(MouseMotionDispatchOutcome::Ignored);
                        };
                        let Some(pane) = tree.pane(id) else {
                            return Ok(MouseMotionDispatchOutcome::Ignored);
                        };
                        dispatch_mouse_motion_locked(
                            &pane.grid,
                            local,
                            cell_dimensions,
                            active_button,
                            modifiers,
                            last_cell,
                            |bytes| pane.input().queue_nonfatal_input(bytes),
                        )
                    });
                self.handle_mouse_motion_result(event_loop, result);
            }
            WindowEvent::CursorLeft { device_id } => {
                self.pointer_route.cursor_left(device_id);
            }
            WindowEvent::MouseInput {
                device_id,
                state,
                button,
            } => {
                if !accepts_input {
                    return;
                }
                let button_code = mouse_button_code_from_winit(button);
                if !self
                    .pointer_route
                    .record_physical_button_event(device_id, state, button_code)
                {
                    return;
                }
                let cell_dimensions = self.pointer_route.button_cell_dimensions_for(
                    device_id,
                    state,
                    button_code,
                    self.cell_dimensions,
                );
                if self.close_prompt.is_some() && state.is_pressed() {
                    // A click must not move the focus or reach a program while
                    // the close prompt waits for its answer.
                    self.pointer_route.record_button_dispatch(
                        device_id,
                        state,
                        button_code,
                        false,
                        cell_dimensions,
                    );
                    return;
                }
                let position = self.pointer_route.position_for(device_id);
                if state.is_pressed() {
                    if let Some(position) = position {
                        if let Err(error) = self.focus_pane_at_pointer(position) {
                            self.fail(event_loop, error.to_string());
                            return;
                        }
                        self.pointer_pane = lock_tree(&self.panes)
                            .ok()
                            .and_then(|tree| pane_under_pointer(&tree, position).map(|(id, _)| id));
                    }
                }
                // Ctrl+click opens an OSC 8 hyperlink under the pointer instead
                // of reporting the press or starting a selection.
                if button == MouseButton::Left
                    && state.is_pressed()
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                {
                    if let Some(position) = position {
                        match self.hyperlink_at_pointer(position) {
                            Ok(Some(url)) => {
                                if let Err(error) = crate::notifications::open_hyperlink(&url) {
                                    log::warn!("Failed to open hyperlink: {error}");
                                }
                                self.pointer_route.record_button_dispatch(
                                    device_id,
                                    state,
                                    button_code,
                                    false,
                                    cell_dimensions,
                                );
                                return;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                self.fail(event_loop, error.to_string());
                                return;
                            }
                        }
                    }
                }
                let local_press = if button == MouseButton::Left && state.is_pressed() {
                    match lock_tree(&self.panes) {
                        Ok(tree) => {
                            tree.focused_pane()
                                .is_some_and(|pane| pane.grid.mouse_tracking == MouseTracking::None)
                                || self.modifiers.shift_key()
                        }
                        Err(_) => {
                            // Drop the poisoned guard before calling the error path.
                            false
                        }
                    }
                } else {
                    false
                };
                let local_release = button == MouseButton::Left
                    && state == ElementState::Released
                    && self
                        .selection_drag
                        .is_some_and(|(drag_device, _)| drag_device == device_id);
                if local_press || local_release {
                    let drag_pane = self
                        .selection_drag
                        .map(|(_, pane)| pane)
                        .or(self.pointer_pane);
                    if let Some((position, pane_id)) = position.zip(drag_pane) {
                        match self.select_at_pointer(pane_id, position, local_press) {
                            Ok(true) if local_press => {
                                self.selection_drag = Some((device_id, pane_id))
                            }
                            Ok(_) => {}
                            Err(error) => {
                                self.fail(event_loop, error.to_string());
                                return;
                            }
                        }
                    }
                    if local_release {
                        self.selection_drag = None;
                    }
                    self.pointer_route.record_button_dispatch(
                        device_id,
                        state,
                        button_code,
                        false,
                        cell_dimensions,
                    );
                    return;
                }
                let modifiers = self.modifiers;
                let target_pane = self.pointer_pane;
                let motion_state = &mut self.pointer_route.motion_state;
                let result = self
                    .panes
                    .lock()
                    .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))
                    .and_then(|mut tree| {
                        let id = target_pane.unwrap_or(tree.focused);
                        let Some(pane) = tree.pane_mut(id) else {
                            return Ok(KeyboardInputOutcome::Ignored {
                                viewport_changed: false,
                            });
                        };
                        let rect = pane.rect;
                        let (grid, input) = pane.grid_and_input();
                        dispatch_mouse_button_and_motion_locked(
                            grid,
                            position.map(|position| pane_local_position(rect, position)),
                            cell_dimensions,
                            state,
                            button,
                            modifiers,
                            motion_state,
                            |bytes| {
                                input.queue_input(bytes);
                                Ok(())
                            },
                        )
                    });
                let forwarded = matches!(
                    &result,
                    Ok(KeyboardInputOutcome::Forwarded {
                        viewport_changed: false
                    })
                );
                self.pointer_route.record_button_dispatch(
                    device_id,
                    state,
                    button_code,
                    forwarded,
                    cell_dimensions,
                );
                self.handle_terminal_input_result(event_loop, result);
            }
            WindowEvent::MouseWheel {
                device_id,
                delta,
                phase,
            } => {
                if !accepts_input {
                    return;
                }
                let Some(pointer_position) = self.pointer_route.select_wheel_device(device_id)
                else {
                    return;
                };
                let cell_dimensions = self.cell_dimensions;
                let modifiers = self.modifiers;
                let wheel_state = &mut self.pointer_route.wheel_state;
                let result = self
                    .panes
                    .lock()
                    .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))
                    .and_then(|mut tree| {
                        let target = pointer_position
                            .and_then(|position| pane_under_pointer(&tree, position))
                            .map_or((tree.focused, None), |(id, local)| (id, Some(local)));
                        let Some(pane) = tree.pane_mut(target.0) else {
                            return Ok(KeyboardInputOutcome::Ignored {
                                viewport_changed: false,
                            });
                        };
                        let (grid, input) = pane.grid_and_input();
                        dispatch_mouse_wheel_locked(
                            grid,
                            target.1,
                            cell_dimensions,
                            delta,
                            phase,
                            modifiers,
                            wheel_state,
                            |bytes| {
                                input.queue_input(bytes);
                                Ok(())
                            },
                        )
                    });
                self.handle_terminal_input_result(event_loop, result);
            }
            WindowEvent::Focused(focused) => {
                self.modifiers = modifiers_after_focus_change(self.modifiers, focused);
                self.window_focused.store(focused, Ordering::Release);
                if focused {
                    self.notifications.on_focus_gained();
                }
                if !focused {
                    self.update_ime_preedit(None);
                }
                let focus_changed =
                    record_window_focus_change(&mut self.last_window_focus, focused);
                if focus_changed && accepts_input {
                    let result = lock_tree(&self.panes)
                        .map_err(KeyboardInputError::Grid)
                        .and_then(|tree| {
                            let Some(pane) = tree.focused_pane() else {
                                return Ok(KeyboardInputOutcome::Ignored {
                                    viewport_changed: false,
                                });
                            };
                            dispatch_focus_event_locked(&pane.grid, focused, |bytes| {
                                pane.queue_input(bytes);
                                Ok(())
                            })
                        });
                    self.handle_terminal_input_result(event_loop, result);
                }
                if focused {
                    // Watching the focused pane clears its attention mark.
                    let cleared =
                        lock_tree(&self.panes).is_ok_and(|mut tree| tree.clear_focused_attention());
                    if cleared {
                        self.presented_scenes.clear();
                        self.request_redraw();
                    }
                }
            }
            WindowEvent::Ime(Ime::Enabled) => {
                self.ime_active = true;
                self.update_ime_preedit(None);
                self.last_ime_cursor_area = None;
                self.request_redraw();
            }
            WindowEvent::Ime(Ime::Disabled) => {
                self.ime_active = false;
                self.update_ime_preedit(None);
                self.last_ime_cursor_area = None;
            }
            WindowEvent::Ime(Ime::Preedit(text, cursor)) => {
                if !self.ime_active || !accepts_input {
                    return;
                }
                let next = match ime_preedit_payload(text, cursor) {
                    ImePreeditPayload::Clear => None,
                    ImePreeditPayload::Text(preedit) => {
                        let revealed = lock_tree(&self.panes).map(|mut tree| {
                            tree.focused_pane_mut().is_some_and(|pane| {
                                reveal_ime_input_viewport_locked(&mut pane.grid)
                            })
                        });
                        let viewport_changed = match revealed {
                            Ok(changed) => changed,
                            Err(error) => {
                                self.fail(event_loop, error.to_string());
                                return;
                            }
                        };
                        if viewport_changed {
                            self.request_redraw();
                        }
                        Some(preedit)
                    }
                    ImePreeditPayload::TooManyBytes { byte_count, limit } => {
                        log::warn!(
                            "dropping Linux IME preedit of {byte_count} bytes; limit is {limit}"
                        );
                        None
                    }
                    ImePreeditPayload::TooManyRenderCells { cell_count, limit } => {
                        log::warn!(
                            "dropping Linux IME preedit requiring {cell_count} render cells; limit is {limit}"
                        );
                        None
                    }
                };
                self.update_ime_preedit(next);
            }
            WindowEvent::Ime(event) => {
                self.update_ime_preedit(None);
                if !accepts_input {
                    return;
                }
                let Some(payload) = ime_payload_from_event(event) else {
                    return;
                };
                let bytes = match payload {
                    ImeCommitPayload::Empty => return,
                    ImeCommitPayload::Bytes(bytes) => bytes,
                    ImeCommitPayload::TooLarge { byte_count, limit } => {
                        log::warn!(
                            "dropping Linux IME commit of {byte_count} bytes; limit is {limit}"
                        );
                        return;
                    }
                };
                if self.close_prompt.is_some() {
                    return;
                }
                let result = lock_tree(&self.panes)
                    .map_err(KeyboardInputError::Grid)
                    .and_then(|mut tree| {
                        let Some(pane) = tree.focused_pane_mut() else {
                            return Ok(KeyboardInputOutcome::Ignored {
                                viewport_changed: false,
                            });
                        };
                        if pane.search.is_some() {
                            // Composed text extends the query, never the shell.
                            let changed = type_into_search(pane, &bytes);
                            return Ok(KeyboardInputOutcome::Ignored {
                                viewport_changed: changed,
                            });
                        }
                        let (grid, input) = pane.grid_and_input();
                        dispatch_encoded_terminal_input_locked(grid, Some(bytes), |bytes| {
                            input.queue_input(bytes);
                            Ok(())
                        })
                    });
                if matches!(result, Ok(KeyboardInputOutcome::Forwarded { .. })) {
                    self.clear_focused_selection();
                }
                self.handle_terminal_input_result(event_loop, result);
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if !accepts_input {
                    return;
                }
                if self.close_prompt.is_some() {
                    if !is_synthetic && event.state.is_pressed() && !event.repeat {
                        let answer = close_answer(&event.logical_key, self.modifiers);
                        self.answer_close_prompt(event_loop, answer);
                    }
                    return;
                }
                if !is_synthetic && event.state.is_pressed() {
                    if let Some(action) =
                        clipboard_action(event.logical_key.as_ref(), self.modifiers)
                    {
                        if !event.repeat {
                            if let Err(error) = self.handle_clipboard_action(action) {
                                self.fail(event_loop, error.to_string());
                            }
                        }
                        return;
                    }
                    let pane_action =
                        key_name_from_winit(&event.key_without_modifiers(), event.physical_key)
                            .and_then(|key| {
                                self.keybinds
                                    .lookup(key, key_modifiers_from_winit(self.modifiers))
                            });
                    if let Some(action) = pane_action {
                        if !(event.repeat && pane_action_ignores_repeat(action)) {
                            self.handle_pane_action(event_loop, action);
                        }
                        return;
                    }
                    let search_outcome = lock_tree(&self.panes).map(|mut tree| {
                        tree.focused_pane_mut().and_then(|pane| {
                            handle_search_key(
                                pane,
                                search_key_from_winit(&event.logical_key, self.modifiers),
                            )
                        })
                    });
                    match search_outcome {
                        Ok(Some(outcome)) => {
                            if !matches!(outcome, SearchKeyOutcome::Unchanged) {
                                self.request_redraw();
                            }
                            return;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            self.fail(event_loop, error.to_string());
                            return;
                        }
                    }
                }
                let scrollback_action =
                    scrollback_action_from_winit(&event, is_synthetic, self.modifiers);
                let key_press = key_press_from_winit(&event, is_synthetic, self.modifiers);
                if scrollback_action.is_none() && key_press.is_none() {
                    return;
                }
                let result = lock_tree(&self.panes)
                    .map_err(KeyboardInputError::Grid)
                    .and_then(|mut tree| {
                        let Some(pane) = tree.focused_pane_mut() else {
                            return Ok(KeyboardInputOutcome::Ignored {
                                viewport_changed: false,
                            });
                        };
                        let (grid, input) = pane.grid_and_input();
                        dispatch_keyboard_input_locked(
                            grid,
                            scrollback_action,
                            |application_cursor_keys, kitty_disambiguate| {
                                key_press.and_then(|key_press| {
                                    encode_key_press(
                                        key_press,
                                        application_cursor_keys,
                                        kitty_disambiguate,
                                    )
                                })
                            },
                            |bytes| {
                                input.queue_input(bytes);
                                Ok(())
                            },
                        )
                    });
                if matches!(result, Ok(KeyboardInputOutcome::Forwarded { .. })) {
                    self.clear_focused_selection();
                }
                self.handle_terminal_input_result(event_loop, result);
            }
            WindowEvent::Resized(size) => {
                if let Some(current_size) = self.window.as_ref().map(|window| window.inner_size()) {
                    if !is_current_surface_size(size, current_size) {
                        log::debug!(
                            "ignoring stale Linux resize event {size:?}; current surface is \
                             {current_size:?}"
                        );
                        return;
                    }
                }
                if let Err(error) = self.relayout_panes(size) {
                    self.fail(event_loop, error);
                    return;
                }
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged {
                scale_factor,
                mut inner_size_writer,
            } => {
                self.pointer_route
                    .scale_factor_changed(self.cell_dimensions);
                self.last_ime_cursor_area = None;
                let previous_scale = self.atlas_scale_factor.unwrap_or(1.0);
                match self.rebuild_glyph_atlas(scale_factor) {
                    Ok(changed) => {
                        if changed {
                            log::info!("Linux scale factor changed to {scale_factor}");
                        }
                        let requested_inner_size =
                            match self.preserved_physical_size(scale_factor / previous_scale) {
                                Ok(size) => size,
                                Err(error) => {
                                    self.fail(event_loop, error);
                                    return;
                                }
                            };
                        match inner_size_writer.request_inner_size(requested_inner_size) {
                            Ok(()) => {
                                if let Err(error) = self.relayout_panes(requested_inner_size) {
                                    self.fail(event_loop, error);
                                    return;
                                }
                            }
                            Err(error) => {
                                log::warn!(
                                    "could not preserve the Linux terminal grid at scale \
                                     {scale_factor}: {error}; using the current window size"
                                );
                                // The atlas has changed even if the window cannot resize.
                                // No subsequent Resized event is guaranteed in this case.
                                if let Some(size) =
                                    self.window.as_ref().map(|window| window.inner_size())
                                {
                                    if let Err(error) = self.relayout_panes(size) {
                                        self.fail(event_loop, error);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Err(GlyphAtlasRebuildError::Atlas(error)) => log::error!(
                        "{error}; keeping scale {}",
                        self.atlas_scale_factor.unwrap_or(1.0)
                    ),
                    Err(GlyphAtlasRebuildError::Grid(error)) => {
                        self.fail(event_loop, error.to_string());
                        return;
                    }
                }
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                begin_grid_redraw(self.redraw_pending.as_ref());
                match self.present_frame() {
                    Ok(Some(terminal_content_visible)) => {
                        self.first_frame_presented = true;
                        if self.exit_after_first_frame {
                            if terminal_content_visible {
                                self.request_terminal_shutdown();
                                event_loop.exit();
                            } else {
                                self.fail(
                                    event_loop,
                                    "smoke mode presented a Linux frame without visible terminal content"
                                        .to_string(),
                                );
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => self.fail(event_loop, error),
                }
                self.finish_terminal_exit_after_frame(event_loop);
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: LinuxEvent) {
        match event {
            LinuxEvent::ThemeChanged => {
                let theme = self
                    .theme_watcher
                    .as_ref()
                    .and_then(ThemeWatcher::take_update);
                if let Some(theme) = theme {
                    let colors = self.palette_overrides.resolve(Some(theme));
                    if colors != self.colors {
                        let foreground = colors.resolve_foreground(Color::Default, false);
                        let background = colors.default_background();
                        let result = lock_tree(&self.panes).map(|mut tree| {
                            // OSC queries and the next frame observe the same palette.
                            for id in tree.pane_ids() {
                                if let Some(pane) = tree.pane_mut(id) {
                                    pane.grid.default_fg_hex =
                                        terminal_color_query_value(foreground);
                                    pane.grid.default_bg_hex =
                                        terminal_color_query_value(background);
                                }
                            }
                        });
                        if let Err(error) = result {
                            self.fail(event_loop, error.to_string());
                            return;
                        }
                        self.colors = colors;
                        self.background = rgb_to_xrgb(background.0, background.1, background.2);
                        // Palette changes affect unchanged cells and buffer-age history.
                        self.presented_scenes.clear();
                        self.request_redraw();
                    }
                }
            }
            LinuxEvent::Clipboard(event) => {
                if terminal_accepts_input(event_loop.exiting(), self.terminal_closed) {
                    if let Err(error) = self.handle_clipboard_event(event) {
                        self.fail(event_loop, error.to_string());
                    }
                }
            }
            LinuxEvent::ClipboardWrite(text) => {
                if let Some(clipboard) = &self.clipboard {
                    if let Err(error) = clipboard.copy_text(text) {
                        log::warn!("could not apply OSC 52 clipboard write: {error}");
                    }
                }
            }
            LinuxEvent::UserAttention => {
                if self.notifications.take_attention_request() {
                    if let Some(window) = self.window.as_ref() {
                        window.request_user_attention(Some(UserAttentionType::Informational));
                    }
                }
                // Attention marks recolor dividers.
                self.request_redraw();
            }
            LinuxEvent::GridUpdated => {
                self.request_redraw();
            }
            LinuxEvent::WindowTitleChanged => {
                self.apply_window_title();
            }
            LinuxEvent::PaneExited(id) => {
                if self
                    .close_prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.asks_to_close_pane(id))
                {
                    // Its job and shell are gone: nothing is left to confirm.
                    self.close_prompt = None;
                }
                self.forget_pane(id);
                self.update_ime_preedit(None);
                self.last_ime_cursor_area = None;
                self.presented_scenes.clear();
                if self.should_close.load(Ordering::Acquire) {
                    // Present the final frame, then leave the event loop.
                    self.terminal_closed = true;
                } else {
                    // The tree already relaid out the survivors.
                    if let Some(cell_dimensions) = self.cell_dimensions {
                        let synced = lock_tree(&self.panes)
                            .map_err(|error| error.to_string())
                            .and_then(|mut tree| {
                                sync_pane_metrics(
                                    &mut tree,
                                    cell_dimensions,
                                    &mut self.applied_pty_sizes,
                                )
                            });
                        if let Err(error) = synced {
                            self.fail(event_loop, error);
                            return;
                        }
                    }
                    self.apply_window_title();
                }
                self.request_redraw();
            }
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.request_terminal_shutdown();
    }
}

fn terminal_color_query_value(color: (u8, u8, u8)) -> String {
    format!("{:02x}{:02x}{:02x}", color.0, color.1, color.2)
}

fn signal_grid_update(
    event_proxy: &winit::event_loop::EventLoopProxy<LinuxEvent>,
    redraw_pending: &AtomicBool,
) {
    if arm_grid_redraw(redraw_pending) && event_proxy.send_event(LinuxEvent::GridUpdated).is_err() {
        redraw_pending.store(false, Ordering::Release);
    }
}

fn arm_grid_redraw(redraw_pending: &AtomicBool) -> bool {
    redraw_pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

fn begin_grid_redraw(redraw_pending: &AtomicBool) {
    redraw_pending.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::draw::snapshot_grid;
    use super::{
        apply_search_to_snapshot, arm_grid_redraw, begin_grid_redraw, divider_rectangle,
        layout_viewport, pack_cell_dimensions, pane_action_ignores_repeat, pane_local_position,
        pane_viewport, search_key_from_winit, sync_pane_metrics, terminal_color_query_value,
        type_into_search, unpack_cell_dimensions, xrgb_components, GridAccessError,
        MAX_TERMINAL_COLUMNS, MAX_TERMINAL_ROWS,
    };
    use crate::grid::cell::Color;
    use crate::grid::Grid;
    use crate::input::keybind::PaneAction;
    use crate::layout::{DividerInfo, PixelRect, SplitDirection};
    use crate::linux_window::draw::PaneViewport;
    use crate::linux_window::test_support::test_colors;
    use crate::pane::PaneTree;
    use crate::parser::ansi::GraphicsSupport;
    use crate::renderer::kitty_handler::KittyHandlerOptions;
    use crate::search::{SearchKey, SearchState};
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use winit::dpi::{PhysicalPosition, PhysicalSize};
    use winit::keyboard::{Key, ModifiersState, NamedKey, SmolStr};

    #[test]
    fn terminal_query_colors_are_canonical_six_digit_hex() {
        assert_eq!(terminal_color_query_value((0, 10, 255)), "000aff");
    }

    #[test]
    fn poisoned_grid_returns_a_typed_error() {
        let grid = Arc::new(Mutex::new(Grid::new(1, 1, 0)));
        let poison_target = grid.clone();
        let result = std::thread::spawn(move || {
            let _guard = poison_target
                .lock()
                .expect("fresh test grid should lock before poisoning");
            panic!("poison the test grid");
        })
        .join();
        assert!(result.is_err());

        assert_eq!(
            snapshot_grid(grid.as_ref()).expect_err("poisoned grid should not be recovered"),
            GridAccessError::Poisoned
        );
    }

    #[test]
    fn grid_updates_coalesce_until_redraw_begins_then_rearm() {
        let redraw_pending = AtomicBool::new(false);

        assert!(arm_grid_redraw(&redraw_pending));
        assert!(!arm_grid_redraw(&redraw_pending));

        begin_grid_redraw(&redraw_pending);

        assert!(arm_grid_redraw(&redraw_pending));
        assert!(!arm_grid_redraw(&redraw_pending));
    }

    #[test]
    fn shared_cell_dimensions_round_trip_through_one_atomic() {
        assert_eq!(
            unpack_cell_dimensions(pack_cell_dimensions((9, 21))),
            (9.0, 21.0)
        );
        assert_eq!(
            unpack_cell_dimensions(pack_cell_dimensions((u16::MAX, 1))),
            (f32::from(u16::MAX), 1.0)
        );
    }

    #[test]
    fn layout_viewport_starts_at_the_origin_and_clamps_to_the_pty_limits() {
        let viewport = layout_viewport(PhysicalSize::new(800, 600), (10, 20));
        assert_eq!(
            viewport,
            PixelRect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0
            }
        );
        let clamped = layout_viewport(PhysicalSize::new(u32::MAX, u32::MAX), (10, 20));
        assert_eq!(clamped.width, f32::from(MAX_TERMINAL_COLUMNS) * 10.0);
        assert_eq!(clamped.height, f32::from(MAX_TERMINAL_ROWS) * 20.0);
    }

    #[test]
    fn pane_viewports_round_to_whole_pixels_and_positions_become_pane_local() {
        let rect = PixelRect {
            x: 403.0,
            y: 0.0,
            width: 396.6,
            height: 600.0,
        };
        assert_eq!(
            pane_viewport(rect),
            PaneViewport {
                origin: (403, 0),
                size: (397, 600)
            }
        );
        let local = pane_local_position(rect, PhysicalPosition::new(410.5, 12.0));
        assert_eq!((local.x, local.y), (7.5, 12.0));
    }

    #[test]
    fn divider_rectangles_are_centred_on_the_split_line() {
        let vertical = DividerInfo {
            x0: 400.0,
            y0: 0.0,
            x1: 400.0,
            y1: 600.0,
            direction: SplitDirection::Vertical,
            touches_focused: true,
            touches_attention: false,
        };
        assert_eq!(divider_rectangle(&vertical), ((397, 0), (6, 600)));
        let horizontal = DividerInfo {
            x0: 0.0,
            y0: 300.0,
            x1: 800.0,
            y1: 300.0,
            direction: SplitDirection::Horizontal,
            touches_focused: false,
            touches_attention: false,
        };
        assert_eq!(divider_rectangle(&horizontal), ((0, 297), (800, 6)));
    }

    #[test]
    fn packed_colors_unpack_to_their_components() {
        assert_eq!(xrgb_components(0x00b5_312c), (0xb5, 0x31, 0x2c));
        assert_eq!(xrgb_components(0xff00_0000), (0, 0, 0));
    }

    #[test]
    fn ime_commits_extend_only_an_open_search() {
        let command: Vec<std::ffi::OsString> = ["/bin/sh", "-c", "read value"]
            .into_iter()
            .map(Into::into)
            .collect();
        let mut tree = PaneTree::new(
            20,
            4,
            100,
            KittyHandlerOptions::from_megabytes(1, false),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            std::sync::Arc::new(|| {}),
            &command,
        )
        .expect("IME search test should spawn its PTY");
        let pane = tree.focused_pane_mut().unwrap();
        assert!(!type_into_search(pane, "x".as_bytes()));
        assert!(pane.search.is_none());

        pane.search = Some(SearchState::new(String::new()));
        assert!(type_into_search(pane, "日本\n\u{7}".as_bytes()));
        assert_eq!(pane.search.as_ref().unwrap().query, "日本");
        assert!(!type_into_search(pane, "\r".as_bytes()));
    }

    #[test]
    fn search_keys_map_edit_keys_text_and_swallow_chords() {
        let none = ModifiersState::empty();
        assert_eq!(
            search_key_from_winit(&Key::Named(NamedKey::Escape), none),
            SearchKey::Escape
        );
        assert_eq!(
            search_key_from_winit(&Key::Named(NamedKey::Enter), none),
            SearchKey::Enter
        );
        assert_eq!(
            search_key_from_winit(&Key::Named(NamedKey::Enter), ModifiersState::SHIFT),
            SearchKey::ShiftEnter
        );
        assert_eq!(
            search_key_from_winit(&Key::Named(NamedKey::Backspace), none),
            SearchKey::Backspace
        );
        assert_eq!(
            search_key_from_winit(&Key::Named(NamedKey::Space), none),
            SearchKey::Char(' ')
        );
        assert_eq!(
            search_key_from_winit(&Key::Character(SmolStr::new("日")), ModifiersState::SHIFT),
            SearchKey::Char('日')
        );
        assert_eq!(
            search_key_from_winit(&Key::Character(SmolStr::new("c")), ModifiersState::CONTROL),
            SearchKey::Ignore
        );
        assert_eq!(
            search_key_from_winit(&Key::Named(NamedKey::Tab), none),
            SearchKey::Ignore
        );
    }

    #[test]
    fn only_splits_close_and_search_ignore_key_repeat() {
        assert!(pane_action_ignores_repeat(PaneAction::SplitVertical));
        assert!(pane_action_ignores_repeat(PaneAction::ClosePane));
        assert!(pane_action_ignores_repeat(PaneAction::Search));
        assert!(!pane_action_ignores_repeat(PaneAction::ResizeLeft));
        assert!(!pane_action_ignores_repeat(PaneAction::FocusDown));
        assert!(!pane_action_ignores_repeat(PaneAction::ZoomIn));
    }

    #[test]
    fn search_matches_take_selection_colors_and_the_current_one_the_accent() {
        let mut grid = Grid::new(8, 2, 0);
        for (column, character) in "ab ab".chars().enumerate() {
            grid.set_cursor_pos(0, column);
            grid.put_char(character);
        }
        let mut search = SearchState::new("ab".to_string());
        search.recompute(&grid);
        assert_eq!(search.matches.len(), 2);
        let colors = test_colors();
        let mut snapshot = snapshot_locked_grid_for_test(&grid);
        apply_search_to_snapshot(&mut snapshot, &search, &grid, &colors, 0x00b5_312c);
        let selection_bg = colors.selection_background();
        let selection_fg = colors.selection_foreground();
        let backgrounds: Vec<_> = (0..5).map(|column| snapshot.cells[column].bg).collect();
        let accent = Color::Rgb(0xb5, 0x31, 0x2c);
        let other = Color::Rgb(selection_bg.0, selection_bg.1, selection_bg.2);
        assert!(backgrounds.contains(&accent));
        assert!(backgrounds.contains(&other));
        assert_eq!(backgrounds[2], Color::Default);
        assert_eq!(
            snapshot.cells[0].fg,
            Color::Rgb(selection_fg.0, selection_fg.1, selection_fg.2)
        );
    }

    fn snapshot_locked_grid_for_test(grid: &Grid) -> super::GridSnapshot {
        super::snapshot_locked_grid(grid)
    }

    #[test]
    fn pane_metrics_reach_every_grid_and_pty_once_per_change() {
        let mut tree = PaneTree::new(
            80,
            24,
            100,
            KittyHandlerOptions::from_megabytes(1, false),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            Arc::new(|| {}),
            &[],
        )
        .expect("pane metrics test should spawn its shell");
        let viewport = layout_viewport(PhysicalSize::new(800, 480), (10, 20));
        tree.relayout(viewport, 10.0, 20.0, 0.0);
        let second = tree
            .split(SplitDirection::Vertical, 10.0, 20.0)
            .expect("split should spawn its shell");
        tree.relayout(viewport, 10.0, 20.0, 0.0);
        let mut applied = HashMap::new();

        sync_pane_metrics(&mut tree, (10, 20), &mut applied).expect("PTY sizes should apply");

        for id in [1, second] {
            let pane = tree.pane(id).expect("both panes should exist");
            assert_eq!(
                (pane.grid.cell_pixel_width, pane.grid.cell_pixel_height),
                (10, 20)
            );
            let (columns, rows) = (pane.grid.cols() as u16, pane.grid.rows() as u16);
            assert_eq!(applied[&id], (columns, rows, columns * 10, rows * 20));
            assert!(columns < 80, "split panes share the viewport width");
        }

        // A new cell size with the same grid still reaches the PTY.
        sync_pane_metrics(&mut tree, (12, 24), &mut applied).expect("PTY sizes should apply");
        let pane = tree.pane(second).unwrap();
        assert_eq!(applied[&second].2, pane.grid.cols() as u16 * 12);

        tree.close(second)
            .closed_pane
            .expect("the split pane should detach")
            .retire();
        sync_pane_metrics(&mut tree, (12, 24), &mut applied).expect("PTY sizes should apply");
        assert_eq!(applied.len(), 1, "closed panes leave the bookkeeping");
        tree.close(1)
            .closed_pane
            .expect("the last pane should detach")
            .retire();
    }
}
