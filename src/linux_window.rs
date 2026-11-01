use crate::config::Config;
use crate::notifications::{NotificationController, NotificationRequest};
use crate::grid::TerminalEvent;
use crate::glyph_atlas::{GlyphAtlas, GlyphKey};
use crate::grid::cell::{Cell, CellFlags, Color, UnderlineStyle};
use crate::grid::{CursorShape, Grid, MouseTracking};
use crate::input::linux::{
    encode_key_press, key_press_from_winit, scrollback_action_from_winit, ImeCommitPayload,
};
use crate::input::paste::{encode_paste, MAX_PASTE_BYTES};
use crate::linux_clipboard::{ClipboardEvent, LinuxClipboard, MAX_CLIPBOARD_TEXT_BYTES};
use crate::omarchy_theme::{watch_current_theme, ThemeWatcher};
use crate::palette::PaletteOverrides;
use crate::selection::{GridPoint, SelectionState};
use crate::pty::Pty;
use crate::parser::ansi::GraphicsSupport;
use crate::renderer::box_drawing::{self, CellRect};
use crate::software_graphics::{ImageSnapshot, SoftwareGraphics};
use crate::software_raster::{draw_glyph_a8, draw_glyph_rgba, draw_image_rgba, fill_rect};
use crate::terminal_colors::TerminalColors;
use crate::terminal_reader::{ReaderExit, TerminalReader};
use crate::terminal_writer::{TerminalWriteQueueError, TerminalWriter};
use crate::window_title::{normalized_window_title, sync_window_title_with, WINDOW_TITLE};
use softbuffer::{Context, Surface};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use thiserror::Error;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{
    DeviceId, ElementState, Ime, MouseButton, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::platform::x11::WindowAttributesExtX11;
use winit::window::{Icon, ImePurpose, UserAttentionType, Window, WindowId};

mod damage;
mod status;
mod clipboard;
mod sizing;
mod pointer;
mod dispatch;
mod ime;
#[cfg(test)]
mod test_support;

use damage::{frame_damage, scene_for_buffer_age, PresentedScene, MAX_PRESENTED_SCENES};
use status::{
    classify_reader_exit, classify_writer_exit, combine_launch_results, reader_status_result,
    terminal_accepts_input, writer_status_result, ReaderStatus, WriterStatus,
};
use clipboard::{
    apply_selection_to_snapshot, clipboard_action, selection_point_at_pointer, sync_selection,
    ClipboardAction, SelectionContext,
};
use sizing::{
    atlas_cell_dimensions, configured_terminal_dimensions, create_glyph_atlas, drawable_dimensions,
    immediate_surface_size_to_reconcile, initial_window_dimensions, is_current_surface_size,
    physical_size_for_terminal, replace_glyph_atlas_for_scale, resize_terminal_with,
    set_grid_cell_dimensions, terminal_dimensions_from_grid, terminal_size_for_surface,
    GlyphAtlasRebuildError, TerminalDimensions,
    TerminalSize, MAX_TERMINAL_COLUMNS, MAX_TERMINAL_ROWS,
};
use pointer::{mouse_button_code_from_winit, terminal_cell_at_pointer, PointerRouteState};
use dispatch::{
    dispatch_encoded_terminal_input_with, dispatch_focus_event_with, dispatch_keyboard_input_with,
    dispatch_mouse_button_and_motion_with, dispatch_mouse_motion_with, dispatch_mouse_wheel_with,
    modifiers_after_focus_change, record_window_focus_change, KeyboardInputError,
    KeyboardInputOutcome, MouseMotionDispatchOutcome,
};
use ime::{
    ime_cursor_area, ime_payload_from_event, ime_preedit_payload, layout_ime_preedit_for_snapshot,
    replace_ime_preedit, reveal_ime_input_viewport, sync_ime_cursor_area_with, ImeCursorArea,
    ImePreedit, ImePreeditGlyph, ImePreeditLayout, ImePreeditPayload,
};

const EXIT_AFTER_FIRST_FRAME_ENV: &str = "KOKUBAN_EXIT_AFTER_FIRST_FRAME";
const CURSOR_THICKNESS: u32 = 2;
const CURSOR_ALPHA: u8 = 180;

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
    ReaderExited(ReaderStatus),
    WriterExited(WriterStatus),
}

#[derive(Clone, Copy)]
struct GlyphSource<'a> {
    pixels: &'a [u8],
    rgba_pixels: Option<&'a [u8]>,
    size: (u32, u32),
    ascent: f32,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
enum GridAccessError {
    #[error("could not access the Linux terminal grid because its lock is poisoned")]
    Poisoned,
    #[error("terminal grid dimensions {columns}x{rows} exceed the Linux PTY limit")]
    DimensionsOutOfRange { columns: usize, rows: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorSnapshot {
    row: usize,
    column: usize,
    shape: CursorShape,
}

#[derive(Debug)]
struct GridSnapshot {
    columns: usize,
    rows: usize,
    cells: Vec<Cell>,
    cursor: Option<CursorSnapshot>,
    input_cursor: (usize, usize),
    wrap_pending: bool,
    auto_wrap: bool,
}

impl GridSnapshot {
    fn cell(&self, row: usize, column: usize) -> Option<&Cell> {
        let index = row.checked_mul(self.columns)?.checked_add(column)?;
        self.cells.get(index)
    }

    fn rendered_cell(&self, row: usize, column: usize) -> Option<&Cell> {
        let cell = self.cell(row, column)?;
        if cell.flags.contains(CellFlags::WIDE_CONT) && column > 0 {
            let leader = self.cell(row, column - 1)?;
            if leader.flags.contains(CellFlags::WIDE) {
                return Some(leader);
            }
        }
        Some(cell)
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
    let mut grid = Grid::new(
        usize::from(columns),
        usize::from(rows),
        config.window.scrollback_lines,
    );
    grid.default_fg_hex = terminal_color_query_value(foreground);
    grid.default_bg_hex = terminal_color_query_value(background);
    if let Some(title) = &options.title {
        grid.set_title(title);
    }
    let grid = Arc::new(Mutex::new(grid));
    let graphics_support = GraphicsSupport {
        kitty: config.images.kitty_graphics_enabled(),
        sixel: config.images.sixel_graphics_enabled(),
    };
    let graphics = Arc::new(Mutex::new(SoftwareGraphics::new(&config.images)));
    let reader_graphics = graphics.clone();
    let window_focused = Arc::new(AtomicBool::new(false));
    let notifications = Arc::new(NotificationController::new(
        config.notifications.clone(),
        window_focused.clone(),
    ));
    let reader_notifications = notifications.clone();
    let pty = if options.command.is_empty() {
        Pty::spawn(columns, rows, graphics_support.kitty, graphics_support.sixel)
    } else {
        Pty::spawn_command(
            columns,
            rows,
            graphics_support.kitty,
            graphics_support.sixel,
            &options.command,
        )
    }
    .map_err(|error| format!("could not start the Linux terminal process: {error}"))?;
    let pty = Arc::new(pty);
    let redraw_pending = Arc::new(AtomicBool::new(false));
    let window_title_pending = Arc::new(AtomicBool::new(false));
    let event_proxy = event_loop.create_proxy();
    let clipboard_proxy = event_proxy.clone();
    let clipboard = LinuxClipboard::new(move |event| {
        let _ = clipboard_proxy.send_event(LinuxEvent::Clipboard(event));
    }).map_err(|error| log::warn!("Linux clipboard worker unavailable: {error}")).ok();
    let writer_proxy = event_proxy.clone();
    let mut writer = TerminalWriter::spawn(pty.clone(), move |exit| {
        let _ = writer_proxy.send_event(LinuxEvent::WriterExited(classify_writer_exit(exit)));
    })
    .map_err(|error| format!("could not start the Linux terminal writer: {error}"))?;
    let update_check_proxy = event_proxy.clone();
    let update_proxy = event_proxy.clone();
    let update_pending = redraw_pending.clone();
    let update_title_grid = grid.clone();
    let update_title_pending = window_title_pending.clone();
    let mut observed_window_title = WINDOW_TITLE.to_string();
    let attention_proxy = event_proxy.clone();
    let clipboard_write_proxy = event_proxy.clone();
    let osc52_write = config.clipboard.osc52_write;
    let reader = match TerminalReader::spawn_with_graphics(
        pty.clone(),
        grid.clone(),
        graphics_support,
        move |event, grid| {
            match event {
                TerminalEvent::Notification { title, body } => {
                    reader_notifications
                        .handle_notification(NotificationRequest::new(title, body));
                    Ok(None)
                }
                TerminalEvent::Bell => {
                    reader_notifications.handle_bell();
                    let _ = attention_proxy.send_event(LinuxEvent::UserAttention);
                    Ok(None)
                }
                TerminalEvent::ClipboardWrite(text) => {
                    if osc52_write {
                        let _ = clipboard_write_proxy.send_event(LinuxEvent::ClipboardWrite(text));
                    }
                    Ok(None)
                }
                event => {
                    let mut graphics =
                        reader_graphics.lock().map_err(|_| ReaderExit::GridPoisoned)?;
                    Ok(graphics.process(event, grid))
                }
            }
        },
        move || {
            signal_grid_update(&update_proxy, update_pending.as_ref());
            if changed_window_title(update_title_grid.as_ref(), &mut observed_window_title)
                .unwrap_or(false)
            {
                signal_window_title_update(&update_proxy, update_title_pending.as_ref());
            }
        },
        move |exit| {
            let _ = event_proxy.send_event(LinuxEvent::ReaderExited(classify_reader_exit(exit)));
        },
    ) {
        Ok(reader) => reader,
        Err(error) => {
            writer.request_shutdown();
            let cleanup_result = writer.shutdown_and_join();
            let error = format!("could not start the Linux terminal reader: {error}");
            return match cleanup_result {
                Ok(_) => Err(error),
                Err(_) => Err(format!(
                    "{error}; Linux terminal writer thread panicked during startup cleanup"
                )),
            };
        }
    };
    let mut application = LinuxWindow::new(
        colors,
        palette_overrides,
        config.font.family,
        config.font.size,
        initial_size,
        exit_after_first_frame,
        grid,
        graphics,
        pty,
        reader,
        writer,
        redraw_pending,
        window_title_pending,
        notifications,
        window_focused,
    );
    application.clipboard = clipboard;
    application.theme_watcher = theme_watcher;
    if crate::update::startup_check_enabled(config.update.check_on_startup) {
        // Linux has no status bar; the notice goes into the window title.
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
    let writer_join_result = application.join_writer();
    let finish_result = application.finish();

    combine_launch_results(
        run_result,
        reader_join_result,
        writer_join_result,
        finish_result,
    )
}

struct LinuxWindow {
    app_id: String,
    clipboard: Option<LinuxClipboard>,
    paste_request: u64,
    pending_pastes: HashMap<u64, (bool, u64)>,
    selection: SelectionState,
    selection_context: SelectionContext,
    selection_drag: Option<DeviceId>,
    // Drop the surface and its display connection before releasing the window.
    surface: Option<SoftwareSurface>,
    context: Option<Context<Arc<Window>>>,
    window: Option<Arc<Window>>,
    glyph_atlas: Option<GlyphAtlas>,
    atlas_scale_factor: Option<f64>,
    cell_dimensions: Option<(u16, u16)>,
    applied_pty_size: Option<TerminalSize>,
    presented_scenes: std::collections::VecDeque<PresentedScene>,
    background: u32,
    colors: TerminalColors,
    palette_overrides: PaletteOverrides,
    theme_watcher: Option<ThemeWatcher>,
    font_family: String,
    font_size: f32,
    initial_size: LogicalSize<u32>,
    exit_after_first_frame: bool,
    first_frame_presented: bool,
    applied_window_title: String,
    grid: Arc<Mutex<Grid>>,
    graphics: Arc<Mutex<SoftwareGraphics>>,
    pty: Arc<Pty>,
    reader: Option<TerminalReader>,
    writer: Option<TerminalWriter>,
    redraw_pending: Arc<AtomicBool>,
    window_title_pending: Arc<AtomicBool>,
    notifications: Arc<NotificationController>,
    window_focused: Arc<AtomicBool>,
    update_status: crate::update::SharedUpdateStatus,
    reader_status: Option<ReaderStatus>,
    modifiers: ModifiersState,
    last_window_focus: Option<bool>,
    occluded: bool,
    pointer_route: PointerRouteState<DeviceId>,
    ime_active: bool,
    ime_preedit: Option<ImePreedit>,
    last_ime_cursor_area: Option<ImeCursorArea>,
    error: Option<String>,
}

impl LinuxWindow {
    fn new(
        colors: TerminalColors,
        palette_overrides: PaletteOverrides,
        font_family: String,
        font_size: f32,
        initial_size: LogicalSize<u32>,
        exit_after_first_frame: bool,
        grid: Arc<Mutex<Grid>>,
        graphics: Arc<Mutex<SoftwareGraphics>>,
        pty: Arc<Pty>,
        reader: TerminalReader,
        writer: TerminalWriter,
        redraw_pending: Arc<AtomicBool>,
        window_title_pending: Arc<AtomicBool>,
        notifications: Arc<NotificationController>,
        window_focused: Arc<AtomicBool>,
    ) -> Self {
        let background = colors.default_background();
        Self {
            app_id: crate::app_icon::APP_ID.to_string(),
            clipboard: None,
            paste_request: 0,
            pending_pastes: HashMap::new(),
            selection: SelectionState::default(),
            selection_context: SelectionContext::default(),
            selection_drag: None,
            surface: None,
            context: None,
            window: None,
            glyph_atlas: None,
            atlas_scale_factor: None,
            cell_dimensions: None,
            applied_pty_size: None,
            presented_scenes: std::collections::VecDeque::new(),
            background: rgb_to_xrgb(background.0, background.1, background.2),
            colors,
            palette_overrides,
            theme_watcher: None,
            font_family,
            font_size,
            initial_size,
            exit_after_first_frame,
            first_frame_presented: false,
            applied_window_title: WINDOW_TITLE.to_string(),
            grid,
            graphics,
            pty,
            reader: Some(reader),
            writer: Some(writer),
            redraw_pending,
            window_title_pending,
            notifications,
            window_focused,
            update_status: Arc::new(Mutex::new(crate::update::UpdateStatus::Idle)),
            reader_status: None,
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

    fn title_with_update_notice(&self, title: &str) -> String {
        crate::update::title_with_notice(title, &crate::update::read_status(&self.update_status))
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let initial_title =
            snapshot_window_title(self.grid.as_ref()).map_err(|error| error.to_string())?;
        let initial_title = self.title_with_update_notice(&initial_title);
        let attributes = Window::default_attributes()
            .with_title(normalized_window_title(&initial_title).into_owned())
            .with_window_icon(application_icon())
            .with_transparent(false)
            .with_inner_size(self.initial_size);
        let attributes = WindowAttributesExtWayland::with_name(
            attributes,
            self.app_id.clone(),
            "kokuban",
        );
        let attributes = WindowAttributesExtX11::with_name(
            attributes,
            self.app_id.clone(),
            "kokuban",
        );
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
        set_grid_cell_dimensions(self.grid.as_ref(), cell_dimensions)
            .map_err(|error| error.to_string())?;
        let terminal_dimensions =
            terminal_dimensions_from_grid(self.grid.as_ref()).map_err(|error| error.to_string())?;
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
        self.cell_dimensions = Some(cell_dimensions);
        let applied_inner_size = immediate_surface_size_to_reconcile(
            window.request_inner_size(requested_inner_size),
            window.inner_size(),
        );
        self.resize_terminal_for_surface(applied_inner_size)?;
        window.request_redraw();
        Ok(())
    }

    fn create_glyph_atlas(&self, scale_factor: f64) -> Result<GlyphAtlas, String> {
        create_glyph_atlas(&self.font_family, self.font_size, scale_factor)
    }

    fn rebuild_glyph_atlas(&mut self, scale_factor: f64) -> Result<bool, GlyphAtlasRebuildError> {
        let grid = self.grid.clone();
        let mut replacement_dimensions = None;
        let changed = replace_glyph_atlas_for_scale(
            &mut self.glyph_atlas,
            &mut self.atlas_scale_factor,
            &self.font_family,
            self.font_size,
            scale_factor,
            |_, cell_dimensions| {
                set_grid_cell_dimensions(grid.as_ref(), cell_dimensions)?;
                replacement_dimensions = Some(cell_dimensions);
                Ok(())
            },
        )?;
        if changed {
            self.presented_scenes.clear();
            self.cell_dimensions = replacement_dimensions;
        }
        Ok(changed)
    }

    fn resize_terminal_for_surface(&mut self, size: PhysicalSize<u32>) -> Result<bool, String> {
        let cell_dimensions = self.cell_dimensions.ok_or_else(|| {
            "could not resize the Linux terminal before glyph metrics were ready".to_string()
        })?;
        let Some(target) = terminal_size_for_surface(size, cell_dimensions) else {
            return Ok(false);
        };

        resize_terminal_with(
            self.grid.as_ref(),
            &mut self.applied_pty_size,
            target,
            |size| {
                self.pty.resize_with_pixels(
                    size.cells.columns,
                    size.cells.rows,
                    size.pixels.0,
                    size.pixels.1,
                )
            },
        )
        .map_err(|error| error.to_string())
    }

    fn current_grid_physical_size(&self) -> Result<PhysicalSize<u32>, String> {
        let cell_dimensions = self.cell_dimensions.ok_or_else(|| {
            "could not size the Linux window before glyph metrics were ready".to_string()
        })?;
        let terminal_dimensions =
            terminal_dimensions_from_grid(self.grid.as_ref()).map_err(|error| error.to_string())?;
        physical_size_for_terminal(terminal_dimensions, cell_dimensions).ok_or_else(|| {
            format!("could not calculate the Linux window size for terminal {terminal_dimensions}")
        })
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
        let (snapshot, images) = {
            // Match reader lock order; text and image placements share one snapshot.
            let grid = self.grid.lock().map_err(|_| GridAccessError::Poisoned.to_string())?;
            let graphics = self.graphics.lock().map_err(|_| "image cache lock is poisoned".to_string())?;
            sync_selection(&mut self.selection, &mut self.selection_context, &grid);
            let mut snapshot = snapshot_locked_grid(&grid);
            apply_selection_to_snapshot(&mut snapshot, &self.selection, &grid, &self.colors);
            (snapshot, graphics.snapshot(&grid, cell_dimensions))
        };
        let preedit_layout = self
            .ime_preedit
            .as_ref()
            .map(|preedit| layout_ime_preedit_for_snapshot(preedit, &snapshot));
        let ime_cursor = preedit_layout
            .as_ref()
            .and_then(|layout| layout.caret_cell)
            .unwrap_or(snapshot.input_cursor);
        let next_ime_cursor_area = ime_cursor_area(
            ime_cursor,
            (snapshot.columns, snapshot.rows),
            cell_dimensions,
            (width.get(), height.get()),
        );
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
        let frame_size = (width.get(), height.get());
        let has_overlays = !images.is_empty() || preedit_layout.is_some();
        let damage = frame_damage(
            scene_for_buffer_age(&self.presented_scenes, buffer.age()),
            &snapshot,
            frame_size,
            cell_dimensions,
            has_overlays,
            glyph_atlas,
        );
        if !damage.is_empty() {
            let start = damage.start as usize * width.get() as usize;
            let end = damage.end as usize * width.get() as usize;
            buffer[start..end].fill(self.background);
            draw_grid_snapshot_in_band(
                &mut buffer,
                frame_size,
                glyph_atlas,
                &self.colors,
                cell_dimensions,
                &snapshot,
                preedit_layout.is_none(),
                &images,
                damage,
            );
        }
        if let Some(layout) = preedit_layout.as_ref() {
            draw_ime_preedit(
                &mut buffer,
                (width.get(), height.get()),
                glyph_atlas,
                &self.colors,
                cell_dimensions,
                &snapshot,
                layout,
            );
        }
        let terminal_content_visible =
            !self.exit_after_first_frame || buffer.iter().any(|&pixel| pixel != self.background);
        window.pre_present_notify();
        buffer
            .present()
            .map_err(|error| format!("could not present a Linux frame: {error}"))?;
        self.presented_scenes.push_front(PresentedScene {
            snapshot,
            frame_size,
            cell_dimensions,
            has_overlays,
        });
        self.presented_scenes.truncate(MAX_PRESENTED_SCENES);
        sync_ime_cursor_area_with(
            self.ime_active,
            &mut self.last_ime_cursor_area,
            next_ime_cursor_area,
            |area| window.set_ime_cursor_area(area.position, area.size),
        );
        Ok(Some(terminal_content_visible))
    }

    fn clear_selection(&mut self) {
        if self.selection.is_active() {
            self.selection.clear();
            if let Some(window) = &self.window {
                window.request_redraw();
            }
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
        let grid = self.grid.lock().map_err(|_| GridAccessError::Poisoned)?;
        let Some(point) = selection_point_at_pointer(&grid, position, cell_size) else {
            return Ok(None);
        };
        let Ok(retained_row) = usize::try_from(point.row) else {
            return Ok(None);
        };
        Ok(grid
            .hyperlink_at_retained(retained_row, point.col)
            .map(str::to_owned))
    }

    fn select_at_pointer(
        &mut self,
        position: PhysicalPosition<f64>,
        start: bool,
    ) -> Result<bool, GridAccessError> {
        let Some(cell_size) = self.cell_dimensions else {
            return Ok(false);
        };
        let grid = self.grid.lock().map_err(|_| GridAccessError::Poisoned)?;
        sync_selection(&mut self.selection, &mut self.selection_context, &grid);
        if !start && !self.selection.is_active() {
            return Ok(false);
        }
        let Some(point) = selection_point_at_pointer(&grid, position, cell_size) else {
            return Ok(false);
        };
        if start {
            self.selection.start(point);
        } else {
            self.selection.update(point);
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        Ok(true)
    }

    fn handle_clipboard_action(&mut self, action: ClipboardAction) -> Result<(), GridAccessError> {
        let grid = self.grid.lock().map_err(|_| GridAccessError::Poisoned)?;
        sync_selection(&mut self.selection, &mut self.selection_context, &grid);
        match action {
            ClipboardAction::SelectAll => {
                let first_row = if grid.using_alt_screen {
                    grid.scrollback_len()
                } else {
                    0
                };
                self.selection.start(GridPoint {
                    row: first_row as i64,
                    col: 0,
                });
                self.selection.update(GridPoint {
                    row: (grid.retained_rows() - 1) as i64,
                    col: grid.cols() - 1,
                });
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            ClipboardAction::Copy => {
                if self.selection.is_active() {
                    match self
                        .selection
                        .get_text_with_limit(&grid, MAX_CLIPBOARD_TEXT_BYTES)
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
                // Keep the target mode/revision with this request. Clipboard IO
                // runs asynchronously and may finish after a screen transition.
                // Bound replies waiting in the UI event queue, as well as the
                // clipboard worker's own bounded command queue.
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
                                (grid.bracketed_paste, grid.screen_revision()),
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
                let Some((bracketed, revision)) = self.pending_pastes.remove(&request_id) else {
                    return Ok(());
                };
                let text = match result {
                    Ok(text) => text,
                    Err(error) => {
                        log::warn!("could not read clipboard: {error}");
                        return Ok(());
                    }
                };
                let Some(writer) = self.writer.as_ref() else {
                    return Ok(());
                };
                let bytes = match encode_paste(
                    &text,
                    bracketed,
                    writer.max_nonfatal_input_bytes().min(MAX_PASTE_BYTES),
                ) {
                    Ok(bytes) if !bytes.is_empty() => bytes,
                    Ok(_) => return Ok(()),
                    Err(error) => {
                        log::warn!("could not encode clipboard paste: {error}");
                        return Ok(());
                    }
                };
                let mut grid = self.grid.lock().map_err(|_| GridAccessError::Poisoned)?;
                if grid.bracketed_paste != bracketed || grid.screen_revision() != revision {
                    log::warn!("clipboard paste cancelled because the terminal screen or paste mode changed");
                    return Ok(());
                }
                match writer.enqueue_nonfatal(bytes) {
                    Ok(()) => {
                        grid.scroll_offset = 0;
                        self.selection.clear();
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
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
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
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
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
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

    fn request_terminal_shutdown(&mut self) {
        self.theme_watcher.take();
        self.ime_preedit = None;
        if let Some(reader) = self.reader.as_ref() {
            reader.request_shutdown();
        }
        if let Some(writer) = self.writer.as_mut() {
            writer.request_shutdown();
        }
    }

    fn join_reader(&mut self) -> Result<(), String> {
        let Some(reader) = self.reader.take() else {
            return Ok(());
        };

        match reader.shutdown_and_join() {
            Ok(exit) => reader_status_result(classify_reader_exit(&exit)),
            Err(_) => Err("Linux terminal reader thread panicked".to_string()),
        }
    }

    fn join_writer(&mut self) -> Result<(), String> {
        let Some(writer) = self.writer.take() else {
            return Ok(());
        };

        match writer.shutdown_and_join() {
            Ok(exit) => writer_status_result(classify_writer_exit(&exit)),
            Err(_) => Err("Linux terminal writer thread panicked".to_string()),
        }
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

    fn finish_reader_exit_after_frame(&mut self, event_loop: &ActiveEventLoop) {
        let Some(status) = self.reader_status.take() else {
            return;
        };

        if let ReaderStatus::Failed(error) = status {
            if self.error.is_none() {
                self.error = Some(error);
            }
        }
        self.request_terminal_shutdown();
        event_loop.exit();
    }
}

impl Drop for LinuxWindow {
    fn drop(&mut self) {
        // An exceptional unwind can bypass `launch`'s ordered joins. Signalling
        // both workers before field drop lets TerminalWriter's joining Drop break
        // output-lock contention with the independently cancellable reader.
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
            // Preserve the PTY reader's grid -> graphics lock order.
            let grid = self.grid.lock().map_err(|_| GridAccessError::Poisoned.to_string())?;
            let mut graphics = self.graphics.lock().map_err(|_| "image cache lock is poisoned".to_string())?;
            Ok::<_, String>(graphics.advance_animations(&grid, cell_size, Instant::now()))
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

        match event {
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if !occluded {
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::CloseRequested => {
                self.request_terminal_shutdown();
                event_loop.exit();
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
                if !self.pointer_route.cursor_moved(device_id, position)
                    || !terminal_accepts_input(event_loop.exiting(), self.reader_status.as_ref())
                {
                    return;
                }
                let active_button = self.pointer_route.active_motion_button();
                if self.selection_drag == Some(device_id) {
                    if let Err(error) = self.select_at_pointer(position, false) {
                        self.fail(event_loop, error.to_string());
                    }
                    return;
                }
                let grid = self.grid.as_ref();
                let writer = self.writer.as_ref();
                let result = dispatch_mouse_motion_with(
                    grid,
                    position,
                    self.cell_dimensions,
                    active_button,
                    self.modifiers,
                    self.pointer_route.motion_state.last_cell,
                    |bytes| {
                        writer
                            .ok_or(TerminalWriteQueueError::Disconnected)?
                            .enqueue_nonfatal(bytes)
                    },
                );
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
                if !terminal_accepts_input(event_loop.exiting(), self.reader_status.as_ref()) {
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
                // Ctrl+click opens an OSC 8 hyperlink under the pointer instead
                // of reporting the press or starting a selection.
                if button == MouseButton::Left
                    && state.is_pressed()
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                {
                    if let Some(position) = self.pointer_route.position_for(device_id) {
                        match self.hyperlink_at_pointer(position) {
                            Ok(Some(url)) => {
                                if let Err(error) = crate::notifications::open_hyperlink(&url) {
                                    log::warn!("Failed to open hyperlink: {error}");
                                }
                                self.pointer_route.record_button_dispatch(device_id, state, button_code, false, cell_dimensions);
                                return;
                            }
                            Ok(None) => {}
                            Err(error) => { self.fail(event_loop, error.to_string()); return; }
                        }
                    }
                }
                let local_press = if button == MouseButton::Left && state.is_pressed() {
                    match self.grid.lock() {
                        Ok(grid) => grid.mouse_tracking == MouseTracking::None || self.modifiers.shift_key(),
                        Err(_) => {
                            // Drop the poisoned guard before calling the error path.
                            false
                        }
                    }
                } else { false };
                let local_release = button == MouseButton::Left && state == ElementState::Released
                    && self.selection_drag == Some(device_id);
                if local_press || local_release {
                    if let Some(position) = self.pointer_route.position_for(device_id) {
                        match self.select_at_pointer(position, local_press) {
                            Ok(true) if local_press => self.selection_drag = Some(device_id),
                            Ok(_) => {},
                            Err(error) => { self.fail(event_loop, error.to_string()); return; }
                        }
                    }
                    if local_release { self.selection_drag = None; }
                    self.pointer_route.record_button_dispatch(device_id, state, button_code, false, cell_dimensions);
                    return;
                }
                let result = dispatch_mouse_button_and_motion_with(
                    self.grid.as_ref(),
                    self.pointer_route.position_for(device_id),
                    cell_dimensions,
                    state,
                    button,
                    self.modifiers,
                    &mut self.pointer_route.motion_state,
                    |bytes| {
                        self.writer
                            .as_ref()
                            .ok_or(TerminalWriteQueueError::Disconnected)?
                            .enqueue(bytes)
                    },
                );
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
                if !terminal_accepts_input(event_loop.exiting(), self.reader_status.as_ref()) {
                    return;
                }
                let Some(pointer_position) = self.pointer_route.select_wheel_device(device_id)
                else {
                    return;
                };
                let result = dispatch_mouse_wheel_with(
                    self.grid.as_ref(),
                    pointer_position,
                    self.cell_dimensions,
                    delta,
                    phase,
                    self.modifiers,
                    &mut self.pointer_route.wheel_state,
                    |bytes| {
                        self.writer
                            .as_ref()
                            .ok_or(TerminalWriteQueueError::Disconnected)?
                            .enqueue(bytes)
                    },
                );
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
                if focus_changed
                    && terminal_accepts_input(event_loop.exiting(), self.reader_status.as_ref())
                {
                    let result = dispatch_focus_event_with(self.grid.as_ref(), focused, |bytes| {
                        self.writer
                            .as_ref()
                            .ok_or(TerminalWriteQueueError::Disconnected)?
                            .enqueue(bytes)
                    });
                    self.handle_terminal_input_result(event_loop, result);
                }
            }
            WindowEvent::Ime(Ime::Enabled) => {
                self.ime_active = true;
                self.update_ime_preedit(None);
                self.last_ime_cursor_area = None;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::Ime(Ime::Disabled) => {
                self.ime_active = false;
                self.update_ime_preedit(None);
                self.last_ime_cursor_area = None;
            }
            WindowEvent::Ime(Ime::Preedit(text, cursor)) => {
                if !self.ime_active
                    || !terminal_accepts_input(event_loop.exiting(), self.reader_status.as_ref())
                {
                    return;
                }
                let next = match ime_preedit_payload(text, cursor) {
                    ImePreeditPayload::Clear => None,
                    ImePreeditPayload::Text(preedit) => {
                        let viewport_changed = match reveal_ime_input_viewport(self.grid.as_ref()) {
                            Ok(changed) => changed,
                            Err(error) => {
                                self.fail(event_loop, error.to_string());
                                return;
                            }
                        };
                        if viewport_changed {
                            if let Some(window) = self.window.as_ref() {
                                window.request_redraw();
                            }
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
                if !terminal_accepts_input(event_loop.exiting(), self.reader_status.as_ref()) {
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
                let result = dispatch_encoded_terminal_input_with(
                    self.grid.as_ref(),
                    Some(bytes),
                    |bytes| {
                        self.writer
                            .as_ref()
                            .ok_or(TerminalWriteQueueError::Disconnected)?
                            .enqueue(bytes)
                    },
                );
                if matches!(result, Ok(KeyboardInputOutcome::Forwarded { .. })) { self.clear_selection(); }
                self.handle_terminal_input_result(event_loop, result);
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if !terminal_accepts_input(event_loop.exiting(), self.reader_status.as_ref()) {
                    return;
                }
                if !is_synthetic && event.state.is_pressed() {
                    if let Some(action) = clipboard_action(event.logical_key.as_ref(), self.modifiers) {
                        if !event.repeat {
                            if let Err(error) = self.handle_clipboard_action(action) {
                                self.fail(event_loop, error.to_string());
                            }
                        }
                        return;
                    }
                }
                let scrollback_action =
                    scrollback_action_from_winit(&event, is_synthetic, self.modifiers);
                let key_press = key_press_from_winit(&event, is_synthetic, self.modifiers);
                if scrollback_action.is_none() && key_press.is_none() {
                    return;
                }
                let result = dispatch_keyboard_input_with(
                    self.grid.as_ref(),
                    scrollback_action,
                    |application_cursor_keys, kitty_disambiguate| {
                        key_press.and_then(|key_press| {
                            encode_key_press(key_press, application_cursor_keys, kitty_disambiguate)
                        })
                    },
                    |bytes| {
                        self.writer
                            .as_ref()
                            .ok_or(TerminalWriteQueueError::Disconnected)?
                            .enqueue(bytes)
                    },
                );
                if matches!(result, Ok(KeyboardInputOutcome::Forwarded { .. })) { self.clear_selection(); }
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
                if let Err(error) = self.resize_terminal_for_surface(size) {
                    self.fail(event_loop, error);
                    return;
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged {
                scale_factor,
                mut inner_size_writer,
            } => {
                self.pointer_route
                    .scale_factor_changed(self.cell_dimensions);
                self.last_ime_cursor_area = None;
                match self.rebuild_glyph_atlas(scale_factor) {
                    Ok(changed) => {
                        if changed {
                            log::info!("Linux scale factor changed to {scale_factor}");
                        }
                        let requested_inner_size = match self.current_grid_physical_size() {
                            Ok(size) => size,
                            Err(error) => {
                                self.fail(event_loop, error);
                                return;
                            }
                        };
                        match inner_size_writer.request_inner_size(requested_inner_size) {
                            Ok(()) => {
                                if let Err(error) =
                                    self.resize_terminal_for_surface(requested_inner_size)
                                {
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
                                    if let Err(error) = self.resize_terminal_for_surface(size) {
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
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
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
                self.finish_reader_exit_after_frame(event_loop);
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: LinuxEvent) {
        match event {
            LinuxEvent::ThemeChanged => {
                let theme = self.theme_watcher.as_ref().and_then(ThemeWatcher::take_update);
                if let Some(theme) = theme {
                    let colors = self.palette_overrides.resolve(Some(theme));
                    if colors != self.colors {
                        let foreground = colors.resolve_foreground(Color::Default, false);
                        let background = colors.default_background();
                        let result = self.grid.lock().map(|mut grid| {
                            // OSC queries and the next frame observe the same palette.
                            grid.default_fg_hex = terminal_color_query_value(foreground);
                            grid.default_bg_hex = terminal_color_query_value(background);
                            self.colors = colors;
                            self.background = rgb_to_xrgb(background.0, background.1, background.2);
                        }).map_err(|_| GridAccessError::Poisoned);
                        if let Err(error) = result {
                            self.fail(event_loop, error.to_string());
                            return;
                        }
                        // Palette changes affect unchanged cells and buffer-age history.
                        self.presented_scenes.clear();
                        if let Some(window) = self.window.as_ref() {
                            window.request_redraw();
                        }
                    }
                }
            }
            LinuxEvent::Clipboard(event) => {
                if terminal_accepts_input(event_loop.exiting(), self.reader_status.as_ref()) {
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
            }
            LinuxEvent::GridUpdated => {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            LinuxEvent::WindowTitleChanged => {
                begin_window_title_update(self.window_title_pending.as_ref());
                let Some(window) = self.window.as_ref() else {
                    return;
                };
                let title = match snapshot_window_title(self.grid.as_ref()) {
                    Ok(title) => self.title_with_update_notice(&title),
                    Err(error) => {
                        self.fail(event_loop, error.to_string());
                        return;
                    }
                };
                sync_window_title_with(&mut self.applied_window_title, &title, |title| {
                    window.set_title(title)
                });
            }
            LinuxEvent::ReaderExited(status) => {
                self.update_ime_preedit(None);
                if self.reader_status.is_none() {
                    self.reader_status = Some(status);
                    if let Some(writer) = self.writer.as_mut() {
                        writer.request_shutdown();
                    }
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            LinuxEvent::WriterExited(WriterStatus::Normal) => {}
            LinuxEvent::WriterExited(WriterStatus::Failed(error)) => {
                self.fail(event_loop, error);
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

fn changed_window_title(
    grid: &Mutex<Grid>,
    observed_title: &mut String,
) -> Result<bool, GridAccessError> {
    let grid = grid.lock().map_err(|_| GridAccessError::Poisoned)?;
    let next_title = normalized_window_title(grid.title());
    if observed_title == next_title.as_ref() {
        return Ok(false);
    }

    observed_title.clear();
    observed_title.push_str(next_title.as_ref());
    Ok(true)
}

fn signal_grid_update(
    event_proxy: &winit::event_loop::EventLoopProxy<LinuxEvent>,
    redraw_pending: &AtomicBool,
) {
    if arm_grid_redraw(redraw_pending) && event_proxy.send_event(LinuxEvent::GridUpdated).is_err() {
        redraw_pending.store(false, Ordering::Release);
    }
}

fn signal_window_title_update(
    event_proxy: &winit::event_loop::EventLoopProxy<LinuxEvent>,
    update_pending: &AtomicBool,
) {
    if arm_window_title_update(update_pending)
        && event_proxy
            .send_event(LinuxEvent::WindowTitleChanged)
            .is_err()
    {
        update_pending.store(false, Ordering::Release);
    }
}

fn arm_window_title_update(update_pending: &AtomicBool) -> bool {
    update_pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

fn begin_window_title_update(update_pending: &AtomicBool) {
    update_pending.store(false, Ordering::Release);
}

fn arm_grid_redraw(redraw_pending: &AtomicBool) -> bool {
    redraw_pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

fn begin_grid_redraw(redraw_pending: &AtomicBool) {
    redraw_pending.store(false, Ordering::Release);
}

fn rgb_to_xrgb(red: u8, green: u8, blue: u8) -> u32 {
    (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)
}

#[cfg(test)]
fn snapshot_grid(grid: &Mutex<Grid>) -> Result<GridSnapshot, GridAccessError> {
    let grid = grid.lock().map_err(|_| GridAccessError::Poisoned)?;
    Ok(snapshot_locked_grid(&grid))
}

fn snapshot_locked_grid(grid: &Grid) -> GridSnapshot {
    let columns = grid.cols();
    let rows = grid.rows();
    let mut cells = Vec::with_capacity(columns.saturating_mul(rows));

    for row in 0..rows {
        for column in 0..columns {
            cells.push(grid.visible_cell(row, column).clone());
        }
    }

    let cursor = if grid.cursor_visible && grid.scroll_offset == 0 && grid.cursor_row < rows {
        grid.screen_cursor_col().map(|column| CursorSnapshot {
            row: grid.cursor_row,
            column,
            shape: grid.cursor_style.shape,
        })
    } else {
        None
    };

    GridSnapshot {
        columns,
        rows,
        cells,
        cursor,
        input_cursor: (grid.cursor_row, grid.cursor_col),
        wrap_pending: grid.is_wrap_pending(),
        auto_wrap: grid.auto_wrap,
    }
}

fn snapshot_window_title(grid: &Mutex<Grid>) -> Result<String, GridAccessError> {
    grid.lock()
        .map(|grid| normalized_window_title(grid.title()).into_owned())
        .map_err(|_| GridAccessError::Poisoned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedCellColors {
    foreground: u32,
    background: u32,
}

fn cell_content_is_visible(flags: CellFlags) -> bool {
    !flags.contains(CellFlags::HIDDEN)
}

fn resolve_cell_colors(colors: &TerminalColors, cell: &Cell) -> ResolvedCellColors {
    let resolved = colors.resolve_cell_colors(cell.fg, cell.bg, cell.flags);

    ResolvedCellColors {
        foreground: rgb_to_xrgb(
            resolved.foreground.0,
            resolved.foreground.1,
            resolved.foreground.2,
        ),
        background: rgb_to_xrgb(
            resolved.background.0,
            resolved.background.1,
            resolved.background.2,
        ),
    }
}

fn resolve_cell_underline_color(
    colors: &TerminalColors,
    cell: &Cell,
    resolved_foreground: u32,
) -> u32 {
    match cell.underline_color {
        Color::Default => resolved_foreground,
        color => {
            let color = colors.resolve_foreground(color, false);
            rgb_to_xrgb(color.0, color.1, color.2)
        }
    }
}

fn underline_anchor_y(
    cell_origin_y: i32,
    cell_height: u32,
    ascent: f32,
    style: UnderlineStyle,
) -> Option<i32> {
    if style == UnderlineStyle::None || cell_height == 0 {
        return None;
    }

    let cell_height = i32::try_from(cell_height).ok()?;
    let last_row = cell_origin_y.checked_add(cell_height.checked_sub(1)?)?;
    let desired = rounded_f64_i32(f64::from(cell_origin_y) + f64::from(ascent))?
        .checked_add(2)?;
    let (leading_inset, trailing_inset) = match style {
        UnderlineStyle::Double => (0, 2),
        UnderlineStyle::Curly => (1, 1),
        UnderlineStyle::None
        | UnderlineStyle::Single
        | UnderlineStyle::Dotted
        | UnderlineStyle::Dashed => (0, 0),
    };
    let preferred_min = cell_origin_y.checked_add(leading_inset)?;
    let preferred_max = last_row.checked_sub(trailing_inset)?;

    if preferred_min <= preferred_max {
        Some(desired.clamp(preferred_min, preferred_max))
    } else {
        Some(desired.clamp(cell_origin_y, last_row))
    }
}

#[cfg(test)]
fn draw_cell_underline(
    frame: &mut [u32],
    frame_size: (u32, u32),
    origin: (i32, i32),
    width: u32,
    cell_height: u32,
    ascent: f32,
    style: UnderlineStyle,
    color: u32,
) {
    draw_cell_underline_in_band(frame, frame_size, origin, width, cell_height,
        ascent, style, color, 0);
}

fn draw_cell_underline_in_band(
    frame: &mut [u32],
    frame_size: (u32, u32),
    origin: (i32, i32),
    width: u32,
    cell_height: u32,
    ascent: f32,
    style: UnderlineStyle,
    color: u32,
    vertical_offset: i32,
) {
    let Some(anchor_y) = underline_anchor_y(origin.1, cell_height, ascent, style) else {
        return;
    };
    let Some(last_y) = i32::try_from(cell_height)
        .ok()
        .and_then(|height| height.checked_sub(1))
        .and_then(|height| origin.1.checked_add(height))
    else {
        return;
    };

    let draw_segment = |frame: &mut [u32], x_offset: u32, y: i32, segment_width: u32| {
        let Some(x_offset) = i32::try_from(x_offset).ok() else {
            return;
        };
        let Some(x) = origin.0.checked_add(x_offset) else {
            return;
        };
        let Some(y) = y.checked_sub(vertical_offset) else { return; };
        fill_rect(
            frame,
            frame_size,
            (x, y),
            (segment_width, 1),
            color,
            u8::MAX,
        );
    };

    match style {
        UnderlineStyle::None => {}
        UnderlineStyle::Single => draw_segment(frame, 0, anchor_y, width),
        UnderlineStyle::Double => {
            draw_segment(frame, 0, anchor_y, width);
            if let Some(second_y) = anchor_y.checked_add(2).filter(|y| *y <= last_y) {
                draw_segment(frame, 0, second_y, width);
            }
        }
        UnderlineStyle::Curly => {
            const OFFSETS: [i32; 6] = [0, 1, 1, 0, -1, -1];
            for x_offset in 0..width {
                let offset = OFFSETS[x_offset as usize % OFFSETS.len()];
                if let Some(y) = anchor_y.checked_add(offset) {
                    draw_segment(frame, x_offset, y.clamp(origin.1, last_y), 1);
                }
            }
        }
        UnderlineStyle::Dotted => {
            for x_offset in (0..width).step_by(3) {
                draw_segment(frame, x_offset, anchor_y, 1);
            }
        }
        UnderlineStyle::Dashed => {
            for x_offset in (0..width).step_by(6) {
                draw_segment(frame, x_offset, anchor_y, (width - x_offset).min(4));
            }
        }
    }
}

#[cfg(test)]
fn draw_grid_snapshot(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: TerminalColors,
    cell_dimensions: (u16, u16),
    snapshot: &GridSnapshot,
    draw_terminal_cursor: bool,
) {
    draw_grid_snapshot_with_images(frame, frame_size, atlas, colors, cell_dimensions,
        snapshot, draw_terminal_cursor, &[]);
}

fn draw_images(
    frame: &mut [u32],
    frame_size: (u32, u32),
    images: &[ImageSnapshot],
    include: impl Fn(i32) -> bool,
) {
    for image in images.iter().filter(|image| include(image.z_index)) {
        draw_image_rgba(frame, frame_size, &image.pixels, image.size, image.rectangle);
    }
}

#[cfg(test)]
fn draw_grid_snapshot_with_images(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: TerminalColors,
    cell_dimensions: (u16, u16),
    snapshot: &GridSnapshot,
    draw_terminal_cursor: bool,
    images: &[ImageSnapshot],
) {
    draw_grid_snapshot_in_band(frame, frame_size, atlas, &colors, cell_dimensions,
        snapshot, draw_terminal_cursor, images, 0..frame_size.1);
}

fn draw_grid_snapshot_in_band(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: &TerminalColors,
    cell_dimensions: (u16, u16),
    snapshot: &GridSnapshot,
    draw_terminal_cursor: bool,
    images: &[ImageSnapshot],
    band: std::ops::Range<u32>,
) {
    if band.is_empty() || band.end > frame_size.1 {
        return;
    }
    let partial = band != (0..frame_size.1);
    debug_assert!(images.is_empty() || band == (0..frame_size.1));
    let start = u64::from(band.start) * u64::from(frame_size.0);
    let end = u64::from(band.end) * u64::from(frame_size.0);
    let (Ok(start), Ok(end), Ok(offset)) =
        (usize::try_from(start), usize::try_from(end), i32::try_from(band.start))
    else {
        return;
    };
    let Some(frame) = frame.get_mut(start..end) else { return; };
    let frame_size = (frame_size.0, band.end - band.start);
    let band_origin = |row, column| {
        let (x, y) = cell_origin(row, column, cell_dimensions)?;
        Some((x, y.checked_sub(offset)?))
    };
    let cell_size = (u32::from(cell_dimensions.0), u32::from(cell_dimensions.1));
    let background_rows = if partial && cell_size.1 != 0 {
        (band.start / cell_size.1) as usize..band.end.div_ceil(cell_size.1) as usize
    } else {
        0..snapshot.rows
    };
    let background_rows = background_rows.start.min(snapshot.rows)
        ..background_rows.end.min(snapshot.rows);

    // Kitty's lowest layer is behind non-default cell backgrounds. Ordinary
    // negative z values are above backgrounds but below glyphs.
    draw_images(frame, frame_size, images, |z| z < i32::MIN / 2);
    for row in background_rows.clone() {
        for column in 0..snapshot.columns {
            let Some(cell) = snapshot.cell(row, column) else { continue; };
            if cell.flags.contains(CellFlags::WIDE_CONT) { continue; }
            let Some(origin) = band_origin(row, column) else { continue; };
            // The caller initializes the whole frame or damaged band first.
            if cell.bg == Color::Default
                && !cell.flags.contains(CellFlags::REVERSE) {
                continue;
            }
            let width = if cell.flags.contains(CellFlags::WIDE) {
                cell_size.0.saturating_mul(2)
            } else { cell_size.0 };
            fill_rect(frame, frame_size, origin, (width, cell_size.1),
                resolve_cell_colors(colors, cell).background, u8::MAX);
        }
    }
    draw_images(frame, frame_size, images, |z| (i32::MIN / 2..0).contains(&z));

    for row in 0..snapshot.rows {
        // Ink can overhang any number of rows, so keep looking up its actual
        // bounds. Full repaints do not need these extra intersection checks.
        let baseline = if partial {
            cell_origin(row, 0, cell_dimensions).and_then(|(_, y)| {
                rounded_f64_i32(f64::from(y) + f64::from(atlas.ascent))
            })
        } else {
            None
        };
        let underline_row = !partial || background_rows.contains(&row);
        for column in 0..snapshot.columns {
            let Some(cell) = snapshot.cell(row, column) else {
                continue;
            };
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                continue;
            }
            let Some(origin) = cell_origin(row, column, cell_dimensions) else {
                continue;
            };
            if !cell_content_is_visible(cell.flags) {
                continue;
            }

            let has_content = cell.grapheme.is_some() || (cell.c != ' ' && cell.c != '\0');
            let builtin = if has_content {
                builtin_cell_rects(cell, cell_size, atlas.scale_factor())
            } else {
                None
            };
            let glyph = if has_content && builtin.is_none() {
                let glyph = atlas.get_or_insert_cell(cell);
                let intersects = !partial || baseline
                    .and_then(|y| y.checked_add(glyph.bearing_y))
                    .is_some_and(|top| {
                        i64::from(top) < i64::from(band.end)
                            && i64::from(top) + i64::from(glyph.pixel_h) > i64::from(band.start)
                    });
                intersects.then_some(glyph)
            } else {
                None
            };
            // Underlines stay inside the cell box even when the glyph's ink
            // misses the band; they must not be skipped along with that glyph.
            let underline = underline_row && cell.underline_style != UnderlineStyle::None;
            // Built-in shapes stay inside the cell box, like underlines.
            let builtin = builtin.filter(|_| underline_row);
            if glyph.is_none() && builtin.is_none() && !underline {
                continue;
            }
            let resolved = resolve_cell_colors(colors, cell);
            if let Some(rects) = builtin {
                if let Some(origin) = band_origin(row, column) {
                    draw_builtin_rects(frame, frame_size, &rects, origin, resolved.foreground, None);
                }
            }
            if let Some(glyph) = glyph {
                let source = GlyphSource {
                    pixels: &atlas.pixels,
                    rgba_pixels: atlas.is_color(glyph).then_some(&atlas.rgba_pixels),
                    size: (atlas.width, atlas.height),
                    ascent: atlas.ascent,
                };
                draw_cell_glyph_in_band(
                    frame,
                    frame_size,
                    source,
                    glyph,
                    origin,
                    resolved.foreground,
                    offset,
                );
            }

            if underline {
                let background_width = if cell.flags.contains(CellFlags::WIDE) {
                    cell_size.0.saturating_mul(2)
                } else {
                    cell_size.0
                };
                let underline_color =
                    resolve_cell_underline_color(colors, cell, resolved.foreground);
                draw_cell_underline_in_band(
                    frame,
                    frame_size,
                    origin,
                    background_width,
                    cell_size.1,
                    atlas.ascent,
                    cell.underline_style,
                    underline_color,
                    offset,
                );
            }
        }
    }

    draw_images(frame, frame_size, images, |z| z >= 0);
    if !draw_terminal_cursor {
        return;
    }
    let Some(cursor) = snapshot.cursor else {
        return;
    };
    let Some(origin) = band_origin(cursor.row, cursor.column) else {
        return;
    };
    let Some((cursor_origin, cursor_size)) = cursor_rectangle(cursor.shape, origin, cell_size)
    else {
        return;
    };
    let cursor_cell = snapshot.rendered_cell(cursor.row, cursor.column);
    let cursor_background = cursor_cell
        .map(|cell| resolve_cell_colors(colors, cell).background)
        .unwrap_or_else(|| {
            let background = colors.default_background();
            rgb_to_xrgb(background.0, background.1, background.2)
        });
    fill_rect(
        frame,
        frame_size,
        cursor_origin,
        cursor_size,
        colors.cursor().map(|color| rgb_to_xrgb(color.0, color.1, color.2))
            .unwrap_or_else(|| contrasting_cursor_color(cursor_background)),
        if colors.cursor().is_some() { u8::MAX } else { CURSOR_ALPHA },
    );
    let Some(cursor_color) = colors.cursor().filter(|_| cursor.shape == CursorShape::Block) else {
        return;
    };
    let Some(cell) = cursor_cell.filter(|cell| {
        cell_content_is_visible(cell.flags)
            && !cell.flags.contains(CellFlags::WIDE_CONT)
            && (cell.grapheme.is_some() || (cell.c != ' ' && cell.c != '\0'))
    }) else {
        return;
    };
    // The cursor can sit on the second cell of a wide grapheme. Its ink still
    // originates at the leader, while only the cursor's own rectangle changes.
    let column = if snapshot.cell(cursor.row, cursor.column)
        .is_some_and(|cell| cell.flags.contains(CellFlags::WIDE_CONT)) {
        cursor.column.saturating_sub(1)
    } else {
        cursor.column
    };
    let Some(cell_origin) = cell_origin(cursor.row, column, cell_dimensions) else { return; };
    let cursor_color = rgb_to_xrgb(cursor_color.0, cursor_color.1, cursor_color.2);
    let default_background = colors.default_background();
    let mut foreground = cursor_background;
    if foreground == cursor_color {
        foreground = rgb_to_xrgb(default_background.0, default_background.1, default_background.2);
    }
    if foreground == cursor_color {
        foreground = contrasting_cursor_color(cursor_color);
    }
    if let Some(rects) = builtin_cell_rects(cell, cell_size, atlas.scale_factor()) {
        let Some(origin_y) = cell_origin.1.checked_sub(offset) else { return; };
        draw_builtin_rects(frame, frame_size, &rects, (cell_origin.0, origin_y), foreground,
            Some((cursor_origin, cursor_size)));
        return;
    }
    let glyph = atlas.get_or_insert_cell(cell);
    let Some(destination_x) = cell_origin.0.checked_add(glyph.bearing_x) else { return; };
    let Some(baseline) = rounded_f64_i32(f64::from(cell_origin.1) + f64::from(atlas.ascent)) else {
        return;
    };
    let Some(destination_y) = baseline.checked_add(glyph.bearing_y)
        .and_then(|y| y.checked_sub(offset)) else { return; };
    let source = GlyphSource {
        pixels: &atlas.pixels,
        rgba_pixels: atlas.is_color(glyph).then_some(&atlas.rgba_pixels),
        size: (atlas.width, atlas.height),
        ascent: atlas.ascent,
    };
    draw_glyph_in_cursor(frame, frame_size, source, glyph, (destination_x, destination_y),
        foreground, (cursor_origin, cursor_size));
}

/// Pixel-exact shapes for block and box drawing characters, which bypass the
/// font so neighbouring cells join without blended seams.
fn builtin_cell_rects(cell: &Cell, cell_size: (u32, u32), scale: f32) -> Option<Vec<CellRect>> {
    if cell.grapheme.is_some() {
        return None;
    }
    let width = if cell.flags.contains(CellFlags::WIDE) {
        cell_size.0.saturating_mul(2)
    } else {
        cell_size.0
    };
    box_drawing::builtin_rects(cell.c, width, cell_size.1, scale)
}

/// Fill built-in shape rectangles at a cell origin, optionally clipped to a
/// cursor rectangle.
fn draw_builtin_rects(
    frame: &mut [u32],
    frame_size: (u32, u32),
    rects: &[CellRect],
    origin: (i32, i32),
    color: u32,
    clip: Option<((i32, i32), (u32, u32))>,
) {
    for rect in rects {
        let mut left = i64::from(origin.0) + i64::from(rect.x);
        let mut top = i64::from(origin.1) + i64::from(rect.y);
        let mut right = left + i64::from(rect.width);
        let mut bottom = top + i64::from(rect.height);
        if let Some((clip_origin, clip_size)) = clip {
            left = left.max(i64::from(clip_origin.0));
            top = top.max(i64::from(clip_origin.1));
            right = right.min(i64::from(clip_origin.0) + i64::from(clip_size.0));
            bottom = bottom.min(i64::from(clip_origin.1) + i64::from(clip_size.1));
        }
        if left >= right || top >= bottom {
            continue;
        }
        let (Ok(x), Ok(y), Ok(width), Ok(height)) = (
            i32::try_from(left), i32::try_from(top),
            u32::try_from(right - left), u32::try_from(bottom - top),
        ) else { continue; };
        fill_rect(frame, frame_size, (x, y), (width, height), color, rect.alpha);
    }
}

/// Crop the cached glyph itself, retaining the destination stride and source
/// bearings. Repainting the whole glyph could recolor a wide neighbor or draw
/// outside a damaged band. No scratch bitmap or frame allocation is needed.
fn draw_glyph_in_cursor(
    frame: &mut [u32],
    frame_size: (u32, u32),
    source: GlyphSource<'_>,
    mut glyph: crate::glyph_atlas::GlyphEntry,
    destination: (i32, i32),
    foreground: u32,
    cursor: ((i32, i32), (u32, u32)),
) {
    let (origin, size) = cursor;
    let left = i64::from(destination.0).max(i64::from(origin.0));
    let top = i64::from(destination.1).max(i64::from(origin.1));
    let right = (i64::from(destination.0) + i64::from(glyph.pixel_w))
        .min(i64::from(origin.0) + i64::from(size.0));
    let bottom = (i64::from(destination.1) + i64::from(glyph.pixel_h))
        .min(i64::from(origin.1) + i64::from(size.1));
    if left >= right || top >= bottom { return; }
    let (Ok(source_x), Ok(source_y), Ok(width), Ok(height)) = (
        u32::try_from(left - i64::from(destination.0)),
        u32::try_from(top - i64::from(destination.1)),
        u32::try_from(right - left), u32::try_from(bottom - top),
    ) else { return; };
    let (Some(atlas_x), Some(atlas_y)) = (
        glyph.atlas_x.checked_add(source_x), glyph.atlas_y.checked_add(source_y),
    ) else { return; };
    glyph.atlas_x = atlas_x;
    glyph.atlas_y = atlas_y;
    glyph.pixel_w = width;
    glyph.pixel_h = height;
    // left/top are maxima of i32 origins, so their conversions are exact.
    let destination = (left as i32, top as i32);
    if let Some(pixels) = source.rgba_pixels {
        draw_glyph_rgba(frame, frame_size, pixels, source.size, glyph, destination);
    } else {
        draw_glyph_a8(frame, frame_size, source.pixels, source.size, glyph, destination, foreground);
    }
}

fn draw_ime_preedit(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: &TerminalColors,
    cell_dimensions: (u16, u16),
    snapshot: &GridSnapshot,
    layout: &ImePreeditLayout,
) {
    let cell_size = (u32::from(cell_dimensions.0), u32::from(cell_dimensions.1));

    for &(row, column) in &layout.cleared_cells {
        let Some(origin) = cell_origin(row, column, cell_dimensions) else {
            continue;
        };
        let background = snapshot
            .rendered_cell(row, column)
            .map(|cell| resolve_cell_colors(colors, cell).background)
            .unwrap_or_else(|| {
                let background = colors.default_background();
                rgb_to_xrgb(background.0, background.1, background.2)
            });
        fill_rect(frame, frame_size, origin, cell_size, background, u8::MAX);
    }

    for glyph in &layout.glyphs {
        let Some(origin) = cell_origin(glyph.row, glyph.column, cell_dimensions) else {
            continue;
        };
        let Some(width) = cell_size.0.checked_mul(u32::from(glyph.width.max(1))) else {
            continue;
        };
        let (_, background) = ime_preedit_glyph_colors(colors, snapshot, glyph);
        fill_rect(
            frame,
            frame_size,
            origin,
            (width, cell_size.1),
            background,
            u8::MAX,
        );
    }

    for glyph in &layout.glyphs {
        if glyph.character == ' ' || glyph.character == '\0' {
            continue;
        }
        let Some(origin) = cell_origin(glyph.row, glyph.column, cell_dimensions) else {
            continue;
        };
        let (foreground, _) = ime_preedit_glyph_colors(colors, snapshot, glyph);
        let atlas_glyph = atlas.get_or_insert(GlyphKey {
            c: glyph.character,
            bold: false,
            italic: false,
        });
        let source = GlyphSource {
            pixels: &atlas.pixels,
            rgba_pixels: atlas.is_color(atlas_glyph).then_some(&atlas.rgba_pixels),
            size: (atlas.width, atlas.height),
            ascent: atlas.ascent,
        };
        draw_cell_glyph(frame, frame_size, source, atlas_glyph, origin, foreground);
    }

    let underline_height = CURSOR_THICKNESS.min(cell_size.1);
    for glyph in &layout.glyphs {
        let Some(origin) = cell_origin(glyph.row, glyph.column, cell_dimensions) else {
            continue;
        };
        let Some(width) = cell_size.0.checked_mul(u32::from(glyph.width.max(1))) else {
            continue;
        };
        let Some(y_offset) = cell_size.1.checked_sub(underline_height) else {
            continue;
        };
        let Some(y) = i32::try_from(y_offset)
            .ok()
            .and_then(|offset| origin.1.checked_add(offset))
        else {
            continue;
        };
        let (foreground, _) = ime_preedit_glyph_colors(colors, snapshot, glyph);
        fill_rect(
            frame,
            frame_size,
            (origin.0, y),
            (width, underline_height),
            foreground,
            u8::MAX,
        );
    }

    let Some((row, column)) = layout.caret_cell else {
        return;
    };
    let Some(origin) = cell_origin(row, column, cell_dimensions) else {
        return;
    };
    let background = ime_preedit_background_at(colors, snapshot, layout, row, column);
    fill_rect(
        frame,
        frame_size,
        origin,
        (CURSOR_THICKNESS.min(cell_size.0), cell_size.1),
        colors.cursor().map(|color| rgb_to_xrgb(color.0, color.1, color.2))
            .unwrap_or_else(|| contrasting_cursor_color(background)),
        u8::MAX,
    );
}

fn ime_preedit_glyph_colors(
    colors: &TerminalColors,
    snapshot: &GridSnapshot,
    glyph: &ImePreeditGlyph,
) -> (u32, u32) {
    let underlying = snapshot
        .rendered_cell(glyph.row, glyph.column)
        .map(|cell| resolve_cell_colors(colors, cell))
        .unwrap_or_else(|| {
            let foreground = colors.resolve_foreground(Color::Default, false);
            let background = colors.default_background();
            ResolvedCellColors {
                foreground: rgb_to_xrgb(foreground.0, foreground.1, foreground.2),
                background: rgb_to_xrgb(background.0, background.1, background.2),
            }
        });
    if glyph.selected {
        (underlying.background, underlying.foreground)
    } else {
        (underlying.foreground, underlying.background)
    }
}

fn ime_preedit_background_at(
    colors: &TerminalColors,
    snapshot: &GridSnapshot,
    layout: &ImePreeditLayout,
    row: usize,
    column: usize,
) -> u32 {
    layout
        .glyphs
        .iter()
        .rev()
        .find(|glyph| {
            glyph.row == row
                && column >= glyph.column
                && column < glyph.column.saturating_add(usize::from(glyph.width.max(1)))
        })
        .map(|glyph| ime_preedit_glyph_colors(colors, snapshot, glyph).1)
        .or_else(|| {
            snapshot
                .rendered_cell(row, column)
                .map(|cell| resolve_cell_colors(colors, cell).background)
        })
        .unwrap_or_else(|| {
            let background = colors.default_background();
            rgb_to_xrgb(background.0, background.1, background.2)
        })
}

fn cell_origin(row: usize, column: usize, cell_dimensions: (u16, u16)) -> Option<(i32, i32)> {
    let x = i64::try_from(column)
        .ok()?
        .checked_mul(i64::from(cell_dimensions.0))?;
    let y = i64::try_from(row)
        .ok()?
        .checked_mul(i64::from(cell_dimensions.1))?;
    Some((i32::try_from(x).ok()?, i32::try_from(y).ok()?))
}

fn cursor_rectangle(
    shape: CursorShape,
    cell_origin: (i32, i32),
    cell_size: (u32, u32),
) -> Option<((i32, i32), (u32, u32))> {
    match shape {
        CursorShape::Block => Some((cell_origin, cell_size)),
        CursorShape::Bar => Some((
            cell_origin,
            (CURSOR_THICKNESS.min(cell_size.0), cell_size.1),
        )),
        CursorShape::Underline => {
            let height = CURSOR_THICKNESS.min(cell_size.1);
            let y_offset = i32::try_from(cell_size.1.checked_sub(height)?).ok()?;
            let y = cell_origin.1.checked_add(y_offset)?;
            Some(((cell_origin.0, y), (cell_size.0, height)))
        }
    }
}

fn contrasting_cursor_color(background: u32) -> u32 {
    let red = (background >> 16) & 0xff;
    let green = (background >> 8) & 0xff;
    let blue = background & 0xff;
    let luminance = red * 299 + green * 587 + blue * 114;

    if luminance >= 128_000 {
        0x0000_0000
    } else {
        0x00ff_ffff
    }
}

fn draw_cell_glyph(
    frame: &mut [u32],
    frame_size: (u32, u32),
    source: GlyphSource<'_>,
    glyph: crate::glyph_atlas::GlyphEntry,
    cell_origin: (i32, i32),
    foreground: u32,
) {
    draw_cell_glyph_in_band(frame, frame_size, source, glyph, cell_origin, foreground, 0);
}

fn draw_cell_glyph_in_band(
    frame: &mut [u32],
    frame_size: (u32, u32),
    source: GlyphSource<'_>,
    glyph: crate::glyph_atlas::GlyphEntry,
    cell_origin: (i32, i32),
    foreground: u32,
    vertical_offset: i32,
) {
    let Some(destination_x) = cell_origin.0.checked_add(glyph.bearing_x) else {
        return;
    };
    let Some(baseline) = rounded_f64_i32(f64::from(cell_origin.1) + f64::from(source.ascent))
    else {
        return;
    };
    let Some(destination_y) = baseline.checked_add(glyph.bearing_y)
        .and_then(|y| y.checked_sub(vertical_offset)) else {
        return;
    };

    if let Some(rgba_pixels) = source.rgba_pixels {
        draw_glyph_rgba(frame, frame_size, rgba_pixels, source.size, glyph, (destination_x, destination_y));
        return;
    }
    draw_glyph_a8(
        frame,
        frame_size,
        source.pixels,
        source.size,
        glyph,
        (destination_x, destination_y),
        foreground,
    );
}

fn rounded_positive_i32(value: f32) -> Option<i32> {
    let rounded = rounded_i32(value)?;
    (rounded > 0).then_some(rounded)
}

fn rounded_i32(value: f32) -> Option<i32> {
    rounded_f64_i32(f64::from(value))
}

fn rounded_f64_i32(value: f64) -> Option<i32> {
    let rounded = value.round();
    if !rounded.is_finite() || rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        return None;
    }
    Some(rounded as i32)
}

#[cfg(test)]
mod themed_cursor_tests {
    use super::*;

    const CURSOR: (u8, u8, u8) = (45, 210, 140);

    fn atlas() -> GlyphAtlas {
        GlyphAtlas::new("kokuban-test-font-that-does-not-exist", 14.0, 1.0).unwrap()
    }

    fn colors() -> TerminalColors {
        TerminalColors::new((220, 130, 85), (7, 25, 55)).with_cursor(CURSOR)
    }

    fn render(snapshot: &GridSnapshot, atlas: &mut GlyphAtlas, cursor: bool) -> Vec<u32> {
        let dimensions = atlas_cell_dimensions(atlas).unwrap();
        let size = (snapshot.columns as u32 * u32::from(dimensions.0),
                    snapshot.rows as u32 * u32::from(dimensions.1));
        let mut frame = vec![rgb_to_xrgb(7, 25, 55); (size.0 * size.1) as usize];
        draw_grid_snapshot(&mut frame, size, atlas, colors(), dimensions, snapshot, cursor);
        frame
    }

    #[test]
    fn opaque_block_cursor_preserves_ascii_combining_and_wide_grapheme_ink() {
        let mut atlas = atlas();
        let (width, height) = atlas_cell_dimensions(&atlas).unwrap();
        let (width, height) = (usize::from(width), usize::from(height));
        let cursor_rgb = rgb_to_xrgb(CURSOR.0, CURSOR.1, CURSOR.2);
        for text in ["M", "e\u{301}", "日", "👩🏽\u{200d}💻"] {
            let mut grid = Grid::new(4, 2, 0);
            grid.cursor_row = 1;
            for character in text.chars() { grid.put_char(character); }
            let mut snapshot = snapshot_locked_grid(&grid);
            let columns = if snapshot.cell(1, 0).unwrap().flags.contains(CellFlags::WIDE) { 2 } else { 1 };
            let baseline = render(&snapshot, &mut atlas, false);
            let mut grapheme_ink_pixels = 0;
            for column in 0..columns {
                snapshot.cursor.as_mut().unwrap().column = column;
                atlas.dirty = false;
                let actual = render(&snapshot, &mut atlas, true);
                assert!(!atlas.dirty, "cursor should reuse the cached {text} grapheme");

                // Independent oracle: ordinary rendering with reversed cursor
                // colors supplies the expected ink, without the cursor helper.
                let mut reference = snapshot_locked_grid(&grid);
                for cell in &mut reference.cells {
                    cell.fg = Color::Rgb(7, 25, 55);
                    cell.bg = Color::Rgb(CURSOR.0, CURSOR.1, CURSOR.2);
                }
                let ink = render(&reference, &mut atlas, false);
                let mut expected = baseline.clone();
                let mut ink_pixels = 0;
                for y in height..height * 2 {
                    for x in column * width..(column + 1) * width {
                        let index = y * width * 4 + x;
                        expected[index] = ink[index];
                        ink_pixels += usize::from(ink[index] != cursor_rgb);
                    }
                }
                grapheme_ink_pixels += ink_pixels;
                assert_eq!(actual, expected, "cursor erased ink or repainted outside {text} cell {column}");

                // A partial repaint clips against both the cursor and the
                // damaged row band, preserving the untouched buffer pixels.
                let frame_size = ((width * 4) as u32, (height * 2) as u32);
                let band = (height + height / 3) as u32..(height * 2 - 2) as u32;
                let start = band.start as usize * width * 4;
                let end = band.end as usize * width * 4;
                let mut partial = baseline.clone();
                partial[start..end].fill(rgb_to_xrgb(7, 25, 55));
                draw_grid_snapshot_in_band(&mut partial, frame_size, &mut atlas, &colors(),
                    (width as u16, height as u16), &snapshot, true, &[], band);
                assert_eq!(&partial[start..end], &actual[start..end], "damaged cursor band: {text}");
                assert_eq!(&partial[..start], &baseline[..start]);
                assert_eq!(&partial[end..], &baseline[end..]);
            }
            // A missing CJK font may draw a narrow replacement glyph inside a
            // two-cell cluster. Both halves still require exact pixel equality.
            assert!(grapheme_ink_pixels > 0, "fixture must contain ink for {text}");
        }
    }

    #[test]
    fn opaque_block_cursor_preserves_ink_in_both_halves_of_a_synthetic_wide_glyph() {
        let mut atlas = atlas();
        let (cell_width, cell_height) = atlas_cell_dimensions(&atlas).unwrap();
        let (width, height) = (usize::from(cell_width), usize::from(cell_height));
        let glyph = crate::glyph_atlas::GlyphEntry {
            atlas_x: atlas.width.checked_sub(u32::from(cell_width) * 2).unwrap(),
            atlas_y: atlas.height.checked_sub(u32::from(cell_height)).unwrap(),
            pixel_w: u32::from(cell_width) * 2,
            pixel_h: u32::from(cell_height),
            bearing_x: 0,
            bearing_y: -rounded_f64_i32(f64::from(atlas.ascent)).unwrap(),
            color: false,
        };
        assert!(!atlas.is_color(glyph));
        // Distinct stripes in each half expose a wrong source origin when the
        // cursor is on WIDE_CONT, without requiring an installed CJK font.
        let has_ink = |x: usize, y: usize| {
            let stripe = if x < width { width / 3 } else { width * 2 / 3 };
            x % width == stripe || y == height / 2
        };
        for y in 0..height {
            for x in 0..width * 2 {
                let index = (glyph.atlas_y as usize + y) * atlas.width as usize
                    + glyph.atlas_x as usize + x;
                atlas.pixels[index] = if has_ink(x, y) { 255 } else { 0 };
            }
        }
        atlas.glyphs.insert(GlyphKey { c: '日', bold: false, italic: false }, glyph);
        let mut grid = Grid::new(4, 2, 0);
        grid.cursor_row = 1;
        grid.put_char('日');
        let mut snapshot = snapshot_locked_grid(&grid);
        assert!(snapshot.cell(1, 1).unwrap().flags.contains(CellFlags::WIDE_CONT));
        let baseline = render(&snapshot, &mut atlas, false);
        for column in 0..2 {
            snapshot.cursor.as_mut().unwrap().column = column;
            let mut expected = baseline.clone();
            let mut ink_pixels = 0;
            for y in 0..height {
                for x in column * width..(column + 1) * width {
                    let ink = has_ink(x, y);
                    ink_pixels += usize::from(ink);
                    expected[(height + y) * width * 4 + x] = if ink {
                        rgb_to_xrgb(7, 25, 55)
                    } else {
                        rgb_to_xrgb(CURSOR.0, CURSOR.1, CURSOR.2)
                    };
                }
            }
            assert!(ink_pixels > 0, "synthetic glyph must cover wide cell {column}");
            atlas.dirty = false;
            let actual = render(&snapshot, &mut atlas, true);
            assert!(!atlas.dirty, "synthetic glyph must stay cached");
            assert_eq!(actual, expected, "wide cell {column}: lost ink or modified neighboring pixels");
            let size = ((width * 4) as u32, (height * 2) as u32);
            let band = (height + height / 3) as u32..(height * 2 - 2) as u32;
            let start = band.start as usize * width * 4;
            let end = band.end as usize * width * 4;
            let mut partial = baseline.clone();
            partial[start..end].fill(rgb_to_xrgb(7, 25, 55));
            draw_grid_snapshot_in_band(&mut partial, size, &mut atlas, &colors(),
                (cell_width, cell_height), &snapshot, true, &[], band);
            assert_eq!(&partial[start..end], &expected[start..end], "wide cell {column}: damaged band");
            assert_eq!(&partial[..start], &baseline[..start]);
            assert_eq!(&partial[end..], &baseline[end..]);
        }
    }

    #[test]
    fn opaque_cursor_uses_readable_text_when_cell_background_matches_cursor() {
        let mut atlas = atlas();
        let mut grid = Grid::new(1, 1, 0);
        grid.bg = Color::Rgb(CURSOR.0, CURSOR.1, CURSOR.2);
        grid.put_char('M');
        grid.cursor_col = 0;
        let snapshot = snapshot_locked_grid(&grid);
        let actual = render(&snapshot, &mut atlas, true);
        let mut reference = snapshot_locked_grid(&grid);
        reference.cells[0].fg = Color::Rgb(7, 25, 55);
        let expected = render(&reference, &mut atlas, false);
        assert_eq!(actual, expected);
        assert!(actual.iter().any(|&pixel| pixel != rgb_to_xrgb(CURSOR.0, CURSOR.1, CURSOR.2)));
    }

    #[test]
    fn opaque_cursor_does_not_reveal_concealed_wide_or_combining_content() {
        let mut atlas = atlas();
        let (width, height) = atlas_cell_dimensions(&atlas).unwrap();
        let (width, height) = (usize::from(width), usize::from(height));
        let cursor_rgb = rgb_to_xrgb(CURSOR.0, CURSOR.1, CURSOR.2);
        for text in ["M", "e\u{301}", "日"] {
            let mut grid = Grid::new(3, 1, 0);
            grid.flags = CellFlags::HIDDEN;
            for character in text.chars() { grid.put_char(character); }
            grid.cursor_col = usize::from(grid.buffer.cell(0, 0).flags.contains(CellFlags::WIDE));
            let snapshot = snapshot_locked_grid(&grid);
            atlas.dirty = false;
            let actual = render(&snapshot, &mut atlas, true);
            assert!(!atlas.dirty, "concealed {text} must never enter the glyph atlas");
            let column = snapshot.cursor.unwrap().column;
            for y in 0..height {
                let start = y * width * 3 + column * width;
                assert!(actual[start..start + width].iter().all(|&pixel| pixel == cursor_rgb));
            }
        }
    }

    #[test]
    fn cropped_cursor_glyph_preserves_neighbor_pixels_and_rgba_source_coordinates() {
        let pixels = [255; 12];
        let mut rgba = [0; 48];
        for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&[(index * 10) as u8, 20, 30, 255]);
        }
        let glyph = crate::glyph_atlas::GlyphEntry {
            atlas_x: 0, atlas_y: 0, pixel_w: 4, pixel_h: 3, bearing_x: 0, bearing_y: 0,
            color: false,
        };
        for colored in [false, true] {
            let source = GlyphSource {
                pixels: &pixels, rgba_pixels: colored.then_some(&rgba), size: (4, 3), ascent: 0.0,
            };
            let mut frame = [0xdead_beef; 15];
            draw_glyph_in_cursor(&mut frame, (4, 3), source, glyph, (-1, 0), 0x112233,
                ((1, 1), (2, 1)));
            let mut expected = [0xdead_beef; 15];
            expected[5] = if colored { rgb_to_xrgb(60, 20, 30) } else { 0x112233 };
            expected[6] = if colored { rgb_to_xrgb(70, 20, 30) } else { 0x112233 };
            assert_eq!(frame, expected, "colored={colored}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sizing::TerminalResizeError;
    use super::test_support::{test_atlas, test_colors, DEFAULT_BACKGROUND};
    use super::{
        arm_grid_redraw,
        arm_window_title_update,
        begin_grid_redraw,
        begin_window_title_update,
        cell_content_is_visible,
        changed_window_title,
        draw_cell_glyph,
        draw_cell_underline,
        draw_grid_snapshot,
        resize_terminal_with,
        resolve_cell_colors,
        resolve_cell_underline_color,
        rgb_to_xrgb,
        rounded_i32,
        snapshot_grid,
        snapshot_window_title,
        terminal_color_query_value,
        terminal_size_for_surface,
        underline_anchor_y,
        GridAccessError,
        ResolvedCellColors,
        WINDOW_TITLE,
        atlas_cell_dimensions,
    };
    use crate::glyph_atlas::{GlyphAtlas, GlyphEntry};
    use crate::grid::cell::{Cell, CellFlags, Color, UnderlineStyle};
    use crate::grid::{CursorShape, Grid};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use winit::dpi::PhysicalSize;

    fn render_grid(grid: Grid, atlas: &mut GlyphAtlas) -> (Vec<u32>, (u16, u16)) {
        let columns = grid.cols();
        let rows = grid.rows();
        let cell_dimensions =
            atlas_cell_dimensions(atlas).expect("test atlas should have valid cell metrics");
        let frame_size = (
            u32::try_from(columns).expect("test grid width should fit u32")
                * u32::from(cell_dimensions.0),
            u32::try_from(rows).expect("test grid height should fit u32")
                * u32::from(cell_dimensions.1),
        );
        let frame_len = usize::try_from(u64::from(frame_size.0) * u64::from(frame_size.1))
            .expect("test frame should fit memory");
        let background = rgb_to_xrgb(
            DEFAULT_BACKGROUND.0,
            DEFAULT_BACKGROUND.1,
            DEFAULT_BACKGROUND.2,
        );
        let mut frame = vec![background; frame_len];
        let grid = Mutex::new(grid);
        let snapshot = snapshot_grid(&grid).expect("test grid should not be poisoned");

        draw_grid_snapshot(
            &mut frame,
            frame_size,
            atlas,
            test_colors(),
            cell_dimensions,
            &snapshot,
            true,
        );

        (frame, cell_dimensions)
    }

    #[test]
    fn converts_rgb_to_softbuffer_xrgb() {
        assert_eq!(rgb_to_xrgb(0x1a, 0x2b, 0x3c), 0x001a_2b3c);
        assert_eq!(rgb_to_xrgb(0xff, 0xff, 0xff) >> 24, 0);
    }

    #[test]
    fn terminal_query_colors_are_canonical_six_digit_hex() {
        assert_eq!(terminal_color_query_value((0, 10, 255)), "000aff");
    }

    #[test]
    fn window_title_snapshot_reads_the_latest_grid_title() {
        let mut grid = Grid::new(2, 1, 0);
        grid.set_title("Codex — kokuban");

        let title =
            snapshot_window_title(&Mutex::new(grid)).expect("title snapshot should succeed");

        assert_eq!(title, "Codex — kokuban");
    }

    #[test]
    fn window_title_change_detection_is_independent_from_frame_redraws() {
        let grid = Mutex::new(Grid::new(2, 1, 0));
        let mut observed_title = WINDOW_TITLE.to_string();

        assert!(!changed_window_title(&grid, &mut observed_title).expect("Grid should be readable"));
        grid.lock()
            .expect("Grid should lock")
            .set_title("btop\0 — 日本");
        assert!(changed_window_title(&grid, &mut observed_title).expect("Grid should be readable"));
        assert_eq!(observed_title, "btop — 日本");
        assert!(!changed_window_title(&grid, &mut observed_title).expect("Grid should be readable"));
        grid.lock().expect("Grid should lock").set_title("\0\n");
        assert!(changed_window_title(&grid, &mut observed_title).expect("Grid should be readable"));
        assert_eq!(observed_title, WINDOW_TITLE);
    }

    #[test]
    fn window_title_events_coalesce_while_preserving_the_latest_value() {
        let grid = Mutex::new(Grid::new(2, 1, 0));
        let pending = AtomicBool::new(false);
        let mut observed_title = WINDOW_TITLE.to_string();

        grid.lock().expect("Grid should lock").set_title("first");
        assert!(changed_window_title(&grid, &mut observed_title).expect("Grid should be readable"));
        assert!(arm_window_title_update(&pending));

        grid.lock().expect("Grid should lock").set_title("latest");
        assert!(changed_window_title(&grid, &mut observed_title).expect("Grid should be readable"));
        assert!(!arm_window_title_update(&pending));

        begin_window_title_update(&pending);
        assert_eq!(
            snapshot_window_title(&grid).expect("latest title should be readable"),
            "latest"
        );
        grid.lock().expect("Grid should lock").set_title("after");
        assert!(changed_window_title(&grid, &mut observed_title).expect("Grid should be readable"));
        assert!(arm_window_title_update(&pending));
    }

    #[test]
    fn rounds_only_finite_coordinates_that_fit_i32() {
        assert_eq!(rounded_i32(12.4), Some(12));
        assert_eq!(rounded_i32(12.5), Some(13));
        assert_eq!(rounded_i32(f32::NAN), None);
        assert_eq!(rounded_i32(f32::INFINITY), None);
        assert_eq!(rounded_i32(f32::MAX), None);
        assert_eq!(rounded_i32(i32::MAX as f32), None);
    }

    #[test]
    fn positions_a8_glyphs_from_cell_origin_ascent_and_bearings() {
        let background = rgb_to_xrgb(0x10, 0x20, 0x30);
        let foreground = rgb_to_xrgb(0xe0, 0x40, 0x20);
        let mut frame = vec![background; 5 * 5];
        let glyph = GlyphEntry {
            atlas_x: 0,
            atlas_y: 0,
            pixel_w: 1,
            pixel_h: 1,
            bearing_x: -1,
            bearing_y: 2,
            color: false,
        };

        draw_cell_glyph(
            &mut frame,
            (5, 5),
            super::GlyphSource {
                pixels: &[255],
                rgba_pixels: None,
                size: (1, 1),
                ascent: 1.4,
            },
            glyph,
            (2, 1),
            foreground,
        );

        assert_eq!(frame[4 * 5 + 1], foreground);
        assert_eq!(
            frame.iter().filter(|&&pixel| pixel != background).count(),
            1
        );
    }

    #[test]
    fn empty_grid_has_no_banner_and_first_cell_renders_at_the_frame_origin() {
        let background = rgb_to_xrgb(
            DEFAULT_BACKGROUND.0,
            DEFAULT_BACKGROUND.1,
            DEFAULT_BACKGROUND.2,
        );
        let mut atlas = test_atlas();
        let mut empty_grid = Grid::new(2, 1, 0);
        empty_grid.cursor_visible = false;

        let (empty_frame, _) = render_grid(empty_grid, &mut atlas);

        assert!(empty_frame.iter().all(|&pixel| pixel == background));

        let character = 'é';
        let key = crate::glyph_atlas::GlyphKey {
            c: character,
            bold: true,
            italic: true,
        };
        assert!(!atlas.glyphs.contains_key(&key));
        let mut grid = Grid::new(2, 1, 0);
        grid.cursor_visible = false;
        *grid.buffer.cell_mut(0, 0) = Cell {
            c: character,
            bg: Color::Rgb(1, 2, 3),
            flags: CellFlags::BOLD | CellFlags::FAINT | CellFlags::ITALIC,
            ..Cell::default()
        };

        let (frame, cell_dimensions) = render_grid(grid, &mut atlas);

        let frame_width = usize::from(cell_dimensions.0) * 2;
        let first_cell_background = rgb_to_xrgb(1, 2, 3);
        assert!(atlas.glyphs.contains_key(&key));
        assert!((0..usize::from(cell_dimensions.1)).any(|row| {
            let start = row * frame_width;
            frame[start..start + usize::from(cell_dimensions.0)].contains(&first_cell_background)
        }));
    }

    #[test]
    fn snapshot_renderer_sends_the_full_grapheme_to_the_atlas() {
        for text in ["e\u{301}", "👩🏽\u{200d}💻", "🇧🇷", "1\u{fe0f}\u{20e3}"] {
            let mut atlas = test_atlas();
            let mut grid = Grid::new(4, 1, 0);
            grid.cursor_visible = false;
            for character in text.chars() {
                grid.put_char(character);
            }
            let (frame, _) = render_grid(grid, &mut atlas);
            atlas.dirty = false;
            let glyph = atlas.get_or_insert_text(text, false, false);
            assert!(!atlas.dirty, "renderer only cached part of {text}");
            assert!(glyph.pixel_w > 0 && glyph.pixel_h > 0);
            let background = rgb_to_xrgb(DEFAULT_BACKGROUND.0, DEFAULT_BACKGROUND.1, DEFAULT_BACKGROUND.2);
            assert!(frame.iter().any(|&pixel| pixel != background), "blank rendered grapheme: {text}");
        }
    }

    #[test]
    fn resolves_bold_indexed_foreground_before_reverse_and_fills_cell_backgrounds() {
        let colors = test_colors();
        let normal = Cell {
            fg: Color::Indexed(1),
            bg: Color::Rgb(4, 5, 6),
            flags: CellFlags::BOLD,
            ..Cell::default()
        };
        let reversed = Cell {
            flags: CellFlags::BOLD | CellFlags::REVERSE,
            ..normal.clone()
        };

        assert_eq!(
            resolve_cell_colors(&colors, &normal),
            ResolvedCellColors {
                foreground: rgb_to_xrgb(241, 76, 76),
                background: rgb_to_xrgb(4, 5, 6),
            }
        );
        assert_eq!(
            resolve_cell_colors(&colors, &reversed),
            ResolvedCellColors {
                foreground: rgb_to_xrgb(4, 5, 6),
                background: rgb_to_xrgb(241, 76, 76),
            }
        );

        let mut grid = Grid::new(2, 1, 0);
        grid.cursor_visible = false;
        *grid.buffer.cell_mut(0, 0) = Cell {
            bg: Color::Indexed(4),
            ..Cell::default()
        };
        *grid.buffer.cell_mut(0, 1) = reversed;
        let mut atlas = test_atlas();

        let (frame, cell_dimensions) = render_grid(grid, &mut atlas);

        let cell_area = usize::from(cell_dimensions.0) * usize::from(cell_dimensions.1);
        assert_eq!(frame.len(), cell_area * 2);
        for row in 0..usize::from(cell_dimensions.1) {
            let row_start = row * usize::from(cell_dimensions.0) * 2;
            assert!(frame[row_start..row_start + usize::from(cell_dimensions.0)]
                .iter()
                .all(|&pixel| pixel == rgb_to_xrgb(36, 114, 200)));
            assert!(frame[row_start + usize::from(cell_dimensions.0)
                ..row_start + usize::from(cell_dimensions.0) * 2]
                .iter()
                .all(|&pixel| pixel == rgb_to_xrgb(241, 76, 76)));
        }
    }

    #[test]
    fn converts_faint_cell_colors_to_xrgb_without_mutating_style_flags() {
        let cell = Cell {
            fg: Color::Rgb(0, 0, 0),
            bg: Color::Rgb(255, 255, 255),
            flags: CellFlags::BOLD | CellFlags::FAINT,
            ..Cell::default()
        };

        assert_eq!(
            resolve_cell_colors(&test_colors(), &cell),
            ResolvedCellColors {
                foreground: 0x007f_7f7f,
                background: 0x00ff_ffff,
            }
        );
        assert!(cell.flags.contains(CellFlags::BOLD));
        assert!(cell.flags.contains(CellFlags::FAINT));
    }

    #[test]
    fn grid_snapshot_preserves_faint_cell_flags() {
        let mut grid = Grid::new(1, 1, 0);
        grid.flags = CellFlags::FAINT;
        grid.put_char('A');

        let snapshot = snapshot_grid(&Mutex::new(grid)).expect("snapshot should succeed");

        assert!(snapshot
            .cell(0, 0)
            .expect("printed cell should be present")
            .flags
            .contains(CellFlags::FAINT));
    }

    #[test]
    fn linux_underline_rasterizes_every_style_inside_its_cell() {
        let frame_size = (20, 12);
        let background = rgb_to_xrgb(1, 2, 3);
        let underline = rgb_to_xrgb(200, 100, 50);
        let origin = (2, 1);
        let width = 12;
        let height = 10;
        let ascent = 6.0;

        for (style, expected_pixels) in [
            (UnderlineStyle::Single, 12),
            (UnderlineStyle::Double, 24),
            (UnderlineStyle::Curly, 12),
            (UnderlineStyle::Dotted, 4),
            (UnderlineStyle::Dashed, 8),
        ] {
            let mut frame = vec![background; (frame_size.0 * frame_size.1) as usize];

            draw_cell_underline(
                &mut frame,
                frame_size,
                origin,
                width,
                height,
                ascent,
                style,
                underline,
            );

            let changed = frame
                .iter()
                .enumerate()
                .filter(|&(_, pixel)| *pixel != background)
                .collect::<Vec<_>>();
            assert_eq!(changed.len(), expected_pixels, "{style:?}");
            assert!(changed.iter().all(|&(index, &pixel)| {
                let x = index % frame_size.0 as usize;
                let y = index / frame_size.0 as usize;
                pixel == underline && (2..14).contains(&x) && (1..11).contains(&y)
            }));
        }

        let mut frame = vec![background; (frame_size.0 * frame_size.1) as usize];
        draw_cell_underline(
            &mut frame,
            frame_size,
            origin,
            width,
            height,
            ascent,
            UnderlineStyle::None,
            underline,
        );
        assert!(frame.iter().all(|&pixel| pixel == background));
    }

    #[test]
    fn linux_underlines_blank_and_wide_cells_with_explicit_colors() {
        let mut grid = Grid::new(3, 1, 0);
        grid.cursor_visible = false;
        *grid.buffer.cell_mut(0, 0) = Cell {
            flags: CellFlags::UNDERLINE,
            underline_style: UnderlineStyle::Single,
            underline_color: Color::Rgb(220, 30, 40),
            ..Cell::default()
        };
        *grid.buffer.cell_mut(0, 1) = Cell {
            flags: CellFlags::UNDERLINE | CellFlags::WIDE,
            underline_style: UnderlineStyle::Double,
            underline_color: Color::Rgb(20, 210, 80),
            ..Cell::default()
        };
        *grid.buffer.cell_mut(0, 2) = Cell {
            c: '\0',
            flags: CellFlags::WIDE_CONT,
            ..Cell::default()
        };
        let mut atlas = test_atlas();
        let ascent = atlas.ascent;

        let (frame, cell_dimensions) = render_grid(grid, &mut atlas);

        let cell_width = usize::from(cell_dimensions.0);
        let frame_width = cell_width * 3;
        let single_y = usize::try_from(
            underline_anchor_y(
                0,
                u32::from(cell_dimensions.1),
                ascent,
                UnderlineStyle::Single,
            )
            .expect("single underline should fit its cell"),
        )
        .unwrap();
        let double_y = usize::try_from(
            underline_anchor_y(
                0,
                u32::from(cell_dimensions.1),
                ascent,
                UnderlineStyle::Double,
            )
            .expect("double underline should fit its cell"),
        )
        .unwrap();
        let single = rgb_to_xrgb(220, 30, 40);
        let double = rgb_to_xrgb(20, 210, 80);

        assert!(frame[single_y * frame_width..single_y * frame_width + cell_width]
            .iter()
            .all(|&pixel| pixel == single));
        for row in [double_y, double_y + 2] {
            let start = row * frame_width + cell_width;
            assert!(frame[start..start + cell_width * 2]
                .iter()
                .all(|&pixel| pixel == double));
        }
    }

    #[test]
    fn linux_default_underline_tracks_resolved_faint_reverse_foreground() {
        let default_underline = Cell {
            fg: Color::Rgb(0, 0, 0),
            bg: Color::Rgb(255, 255, 255),
            flags: CellFlags::BOLD | CellFlags::FAINT | CellFlags::REVERSE,
            underline_style: UnderlineStyle::Single,
            ..Cell::default()
        };
        let resolved = resolve_cell_colors(&test_colors(), &default_underline);
        assert_eq!(
            resolve_cell_underline_color(
                &test_colors(),
                &default_underline,
                resolved.foreground,
            ),
            resolved.foreground
        );

        let explicit_underline = Cell {
            underline_color: Color::Indexed(1),
            ..default_underline
        };
        assert_eq!(
            resolve_cell_underline_color(
                &test_colors(),
                &explicit_underline,
                resolved.foreground,
            ),
            rgb_to_xrgb(205, 49, 49),
            "explicit underline color is resolved independently from bold/faint/reverse"
        );
    }

    #[test]
    fn linux_underline_anchor_clamps_extreme_metrics_to_the_cell() {
        for style in [
            UnderlineStyle::Single,
            UnderlineStyle::Double,
            UnderlineStyle::Curly,
            UnderlineStyle::Dotted,
            UnderlineStyle::Dashed,
        ] {
            for ascent in [-100.0, 100.0] {
                let y = underline_anchor_y(7, 5, ascent, style)
                    .expect("finite metrics should produce an underline anchor");
                assert!((7..=11).contains(&y), "{style:?} at ascent {ascent}");
            }
        }
        assert_eq!(
            underline_anchor_y(0, 10, 5.0, UnderlineStyle::None),
            None
        );
        assert_eq!(
            underline_anchor_y(0, 0, 5.0, UnderlineStyle::Single),
            None
        );
        assert_eq!(
            underline_anchor_y(0, 10, f32::NAN, UnderlineStyle::Single),
            None
        );

        let background = rgb_to_xrgb(1, 2, 3);
        let underline = rgb_to_xrgb(4, 5, 6);
        for style in [UnderlineStyle::Double, UnderlineStyle::Curly] {
            let mut frame = vec![background; 12];
            draw_cell_underline(
                &mut frame,
                (4, 3),
                (0, 1),
                4,
                1,
                100.0,
                style,
                underline,
            );
            assert!(frame[..4].iter().all(|&pixel| pixel == background));
            assert!(frame[4..8].iter().all(|&pixel| pixel == underline));
            assert!(frame[8..].iter().all(|&pixel| pixel == background));
        }
    }

    #[test]
    fn concealed_linux_cells_never_render_content_or_decorations() {
        for visible_flags in [
            CellFlags::empty(),
            CellFlags::BOLD | CellFlags::FAINT | CellFlags::REVERSE,
            CellFlags::ITALIC | CellFlags::UNDERLINE | CellFlags::WIDE,
        ] {
            assert!(cell_content_is_visible(visible_flags));
            assert!(!cell_content_is_visible(visible_flags | CellFlags::HIDDEN));
        }
    }

    #[test]
    fn hidden_wide_snapshot_keeps_resolved_background_without_glyph_or_underline() {
        let character = '日';
        let key = crate::glyph_atlas::GlyphKey {
            c: character,
            bold: true,
            italic: true,
        };
        let mut grid = Grid::new(2, 1, 0);
        grid.cursor_visible = false;
        grid.fg = Color::Rgb(180, 120, 60);
        grid.bg = Color::Rgb(10, 20, 30);
        grid.flags = CellFlags::BOLD
            | CellFlags::FAINT
            | CellFlags::ITALIC
            | CellFlags::UNDERLINE
            | CellFlags::REVERSE
            | CellFlags::HIDDEN;
        grid.underline_style = UnderlineStyle::Double;
        grid.put_char(character);

        let grid = Mutex::new(grid);
        let snapshot = snapshot_grid(&grid).expect("hidden grid should snapshot");
        let leader = snapshot.cell(0, 0).expect("wide leader should exist");
        assert!(leader.flags.contains(CellFlags::HIDDEN | CellFlags::WIDE));
        assert_eq!(leader.underline_style, UnderlineStyle::Double);
        assert_eq!(
            snapshot.cell(0, 1).expect("wide continuation should exist").flags,
            CellFlags::WIDE_CONT
        );
        assert!(snapshot
            .rendered_cell(0, 1)
            .expect("continuation should resolve through its leader")
            .flags
            .contains(CellFlags::HIDDEN));
        let expected_background = resolve_cell_colors(&test_colors(), leader).background;

        let mut atlas = test_atlas();
        assert!(!atlas.glyphs.contains_key(&key));
        let (frame, _) = render_grid(grid.into_inner().unwrap(), &mut atlas);

        assert!(frame.iter().all(|&pixel| pixel == expected_background));
        assert!(!atlas.glyphs.contains_key(&key));
    }

    #[test]
    fn terminal_cursor_remains_visible_over_hidden_linux_content() {
        let character = 'é';
        let key = crate::glyph_atlas::GlyphKey {
            c: character,
            bold: false,
            italic: false,
        };
        let mut grid = Grid::new(1, 1, 0);
        *grid.buffer.cell_mut(0, 0) = Cell {
            c: character,
            bg: Color::Rgb(10, 20, 30),
            flags: CellFlags::HIDDEN,
            ..Cell::default()
        };
        let background = rgb_to_xrgb(10, 20, 30);
        let mut atlas = test_atlas();
        assert!(!atlas.glyphs.contains_key(&key));

        let (frame, _) = render_grid(grid, &mut atlas);

        assert!(frame.iter().any(|&pixel| pixel != background));
        assert!(!atlas.glyphs.contains_key(&key));
    }

    #[test]
    fn wide_continuations_neither_overwrite_the_leader_background_nor_add_a_glyph() {
        let continuation_character = 'Ω';
        let continuation_key = crate::glyph_atlas::GlyphKey {
            c: continuation_character,
            bold: false,
            italic: false,
        };
        let leader_background = rgb_to_xrgb(9, 8, 7);
        let mut grid = Grid::new(2, 1, 0);
        grid.cursor_visible = false;
        *grid.buffer.cell_mut(0, 0) = Cell {
            bg: Color::Rgb(9, 8, 7),
            flags: CellFlags::WIDE,
            ..Cell::default()
        };
        *grid.buffer.cell_mut(0, 1) = Cell {
            c: continuation_character,
            bg: Color::Rgb(200, 100, 50),
            flags: CellFlags::WIDE_CONT,
            ..Cell::default()
        };
        let mut atlas = test_atlas();
        assert!(!atlas.glyphs.contains_key(&continuation_key));

        let (frame, _) = render_grid(grid, &mut atlas);

        assert!(frame.iter().all(|&pixel| pixel == leader_background));
        assert!(!atlas.glyphs.contains_key(&continuation_key));
    }

    #[test]
    fn pending_cursor_uses_the_wide_leader_background_for_contrast() {
        let mut atlas = test_atlas();

        for shape in [CursorShape::Block, CursorShape::Bar, CursorShape::Underline] {
            let mut grid = Grid::new(2, 1, 0);
            grid.cursor_style.shape = shape;
            *grid.buffer.cell_mut(0, 0) = Cell {
                c: ' ',
                fg: Color::Rgb(255, 255, 255),
                bg: Color::Rgb(0, 0, 0),
                flags: CellFlags::WIDE | CellFlags::REVERSE,
                ..Cell::default()
            };
            *grid.buffer.cell_mut(0, 1) = Cell {
                c: '\0',
                fg: Color::Rgb(255, 255, 255),
                bg: Color::Rgb(0, 0, 0),
                flags: CellFlags::WIDE_CONT,
                ..Cell::default()
            };
            grid.cursor_col = grid.cols();

            let (frame, cell_dimensions) = render_grid(grid, &mut atlas);

            let width = usize::from(cell_dimensions.0);
            let height = usize::from(cell_dimensions.1);
            let frame_width = width * 2;
            let changed_pixels = (0..height)
                .flat_map(|row| {
                    let start = row * frame_width + width;
                    frame[start..start + width].iter()
                })
                .filter(|&&pixel| pixel != rgb_to_xrgb(255, 255, 255))
                .count();
            let thickness = usize::try_from(super::CURSOR_THICKNESS)
                .expect("cursor thickness should fit usize");
            let expected = match shape {
                CursorShape::Block => width * height,
                CursorShape::Bar => thickness.min(width) * height,
                CursorShape::Underline => width * thickness.min(height),
            };
            assert_eq!(changed_pixels, expected, "{shape:?}");
        }
    }

    #[test]
    fn empty_grid_cursor_is_visible_and_follows_each_cursor_shape() {
        let background = rgb_to_xrgb(
            DEFAULT_BACKGROUND.0,
            DEFAULT_BACKGROUND.1,
            DEFAULT_BACKGROUND.2,
        );
        let mut atlas = test_atlas();

        for shape in [CursorShape::Block, CursorShape::Bar, CursorShape::Underline] {
            let mut grid = Grid::new(1, 1, 0);
            grid.cursor_style.shape = shape;
            let (frame, cell_dimensions) = render_grid(grid, &mut atlas);
            let width = usize::from(cell_dimensions.0);
            let height = usize::from(cell_dimensions.1);
            let thickness = usize::try_from(super::CURSOR_THICKNESS)
                .expect("cursor thickness should fit usize")
                .min(if shape == CursorShape::Bar {
                    width
                } else {
                    height
                });
            let expected_changed = match shape {
                CursorShape::Block => width * height,
                CursorShape::Bar => thickness * height,
                CursorShape::Underline => width * thickness,
            };

            assert_eq!(
                frame.iter().filter(|&&pixel| pixel != background).count(),
                expected_changed
            );
        }

        let mut hidden_grid = Grid::new(1, 1, 0);
        hidden_grid.cursor_visible = false;
        let (hidden_frame, _) = render_grid(hidden_grid, &mut atlas);
        assert!(hidden_frame.iter().all(|&pixel| pixel == background));

        let mut pending_wrap_grid = Grid::new(1, 1, 0);
        pending_wrap_grid.cursor_col = pending_wrap_grid.cols();
        let (pending_wrap_frame, _) = render_grid(pending_wrap_grid, &mut atlas);
        assert!(pending_wrap_frame.iter().any(|&pixel| pixel != background));

        let mut invalid_cursor_grid = Grid::new(1, 1, 0);
        invalid_cursor_grid.cursor_col = invalid_cursor_grid.cols() + 1;
        let (invalid_cursor_frame, _) = render_grid(invalid_cursor_grid, &mut atlas);
        assert!(invalid_cursor_frame
            .iter()
            .all(|&pixel| pixel == background));
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
        assert_eq!(
            snapshot_window_title(grid.as_ref())
                .expect_err("poisoned grid title should not be recovered"),
            GridAccessError::Poisoned
        );
        let mut observed_title = WINDOW_TITLE.to_string();
        assert_eq!(
            changed_window_title(grid.as_ref(), &mut observed_title)
                .expect_err("poisoned grid title changes should not be recovered"),
            GridAccessError::Poisoned
        );
        let pty_calls = AtomicUsize::new(0);
        let error = resize_terminal_with(
            grid.as_ref(),
            &mut None,
            terminal_size_for_surface(PhysicalSize::new(20, 40), (10, 20)).unwrap(),
            |_| {
                pty_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect_err("poisoned grid must reject terminal resize");
        assert!(matches!(
            error,
            TerminalResizeError::Grid {
                source: GridAccessError::Poisoned,
                ..
            }
        ));
        assert_eq!(pty_calls.load(Ordering::Relaxed), 0);
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
}

#[cfg(test)]
mod graphics_tests {
    use super::{atlas_cell_dimensions, draw_grid_snapshot_with_images, snapshot_grid};
    use crate::config::ImagesConfig;
    use crate::glyph_atlas::GlyphAtlas;
    use crate::grid::cell::Color;
    use crate::grid::Grid;
    use crate::parser::ansi::GraphicsSupport;
    use crate::software_graphics::SoftwareGraphics;
    use crate::terminal_colors::TerminalColors;
    use crate::terminal_decoder::TerminalDecoder;
    use std::sync::Mutex;

    fn render_images(mut grid: Grid, commands: &[u8]) -> (Vec<u32>, (u16, u16)) {
        let mut atlas = GlyphAtlas::new("kokuban-test-font-that-does-not-exist", 14.0, 1.0)
            .expect("system monospace fallback should be available");
        let cell_size = atlas_cell_dimensions(&atlas).unwrap();
        grid.cell_pixel_width = cell_size.0;
        grid.cell_pixel_height = cell_size.1;
        let mut graphics = SoftwareGraphics::new(&ImagesConfig::default());
        let mut decoder = TerminalDecoder::new(GraphicsSupport {
            kitty: true,
            sixel: true,
        });
        let mut remaining = commands;
        while !remaining.is_empty() {
            let step = decoder.feed_until_event(remaining, &mut grid);
            assert!(step.consumed > 0);
            remaining = &remaining[step.consumed..];
            for event in step.events {
                graphics.process(event, &mut grid);
            }
        }
        let images = graphics.snapshot(&grid, cell_size);
        assert!(
            !images.is_empty(),
            "test must exercise actual image compositing"
        );
        let frame_size = (
            grid.cols() as u32 * u32::from(cell_size.0),
            grid.rows() as u32 * u32::from(cell_size.1),
        );
        let snapshot = snapshot_grid(&Mutex::new(grid)).unwrap();
        let mut frame = vec![0; frame_size.0 as usize * frame_size.1 as usize];
        draw_grid_snapshot_with_images(
            &mut frame,
            frame_size,
            &mut atlas,
            TerminalColors::new((255, 255, 255), (0, 0, 0)),
            cell_size,
            &snapshot,
            false,
            &images,
        );
        (frame, cell_size)
    }

    #[test]
    fn lowest_layer_is_visible_through_default_background_but_below_explicit_background() {
        for (z_index, expected_second_cell) in
            [(-1_073_741_825, 0x0000ff), (-1_073_741_824, 0xff0000)]
        {
            let mut grid = Grid::new(2, 1, 0);
            grid.buffer.cell_mut(0, 1).bg = Color::Rgb(0, 0, 255);
            let command =
                format!("\x1b_Ga=T,f=32,s=1,v=1,i=1,c=2,r=1,C=1,z={z_index};/wAA/w==\x1b\\");
            let (frame, cell_size) = render_images(grid, command.as_bytes());

            for row in frame.chunks_exact(usize::from(cell_size.0) * 2) {
                assert!(row[..usize::from(cell_size.0)]
                    .iter()
                    .all(|pixel| *pixel == 0xff0000));
                assert!(row[usize::from(cell_size.0)..]
                    .iter()
                    .all(|pixel| *pixel == expected_second_cell));
            }
        }
    }

    #[test]
    fn negative_layer_is_below_text_and_zero_layer_covers_text() {
        for z_index in [-1, 0] {
            let mut grid = Grid::new(2, 1, 0);
            grid.buffer.cell_mut(0, 0).c = 'M';
            grid.buffer.cell_mut(0, 0).fg = Color::Rgb(255, 255, 255);
            let command =
                format!("\x1b_Ga=T,f=32,s=1,v=1,i=1,c=2,r=1,C=1,z={z_index};/wAA/w==\x1b\\");
            let (frame, cell_size) = render_images(grid, command.as_bytes());

            let glyph_visible = frame.chunks_exact(usize::from(cell_size.0) * 2).any(|row| {
                row[..usize::from(cell_size.0)]
                    .iter()
                    .any(|pixel| *pixel != 0xff0000)
            });
            assert_eq!(glyph_visible, z_index < 0);
            assert!(frame
                .chunks_exact(usize::from(cell_size.0) * 2)
                .all(|row| row[usize::from(cell_size.0)..]
                    .iter()
                    .all(|pixel| *pixel == 0xff0000)));
        }
    }

    #[test]
    fn overlapping_images_are_sorted_by_z_index_before_compositing() {
        let commands = concat!(
            "\x1b_Ga=T,f=32,s=1,v=1,i=1,c=2,r=1,C=1,z=2;/wAA/w==\x1b\\",
            "\x1b_Ga=T,f=32,s=1,v=1,i=2,c=2,r=1,C=1,z=-1;AP8A/w==\x1b\\",
            "\x1b_Ga=T,f=32,s=1,v=1,i=3,c=2,r=1,C=1,z=1;AAD//w==\x1b\\",
        );
        let (frame, _) = render_images(Grid::new(2, 1, 0), commands.as_bytes());

        assert!(frame.iter().all(|pixel| *pixel == 0xff0000));
    }

    #[test]
    fn equal_z_index_uses_client_image_id_independently_of_transmission_order() {
        for (first, second) in [(20, 10), (10, 20)] {
            let pixel = |id| if id == 20 { "/wAA/w==" } else { "AP8A/w==" };
            let commands = format!(
                "\x1b_Ga=T,f=32,s=1,v=1,i={first},c=2,r=1,C=1,z=0;{}\x1b\\\x1b_Ga=T,f=32,s=1,v=1,i={second},c=2,r=1,C=1,z=0;{}\x1b\\",
                pixel(first), pixel(second),
            );
            let (frame, _) = render_images(Grid::new(2, 1, 0), commands.as_bytes());

            assert!(
                frame.iter().all(|pixel| *pixel == 0xff0000),
                "client image 20 must cover image 10 in either transmission order"
            );
        }
    }
}

#[cfg(test)]
mod builtin_shape_tests {
    use super::*;
    use crate::renderer::box_drawing::light_thickness;
    use crate::selection::{point_from_viewport, SelectionState};

    const FOREGROUND: (u8, u8, u8) = (220, 130, 85);
    const BACKGROUND: (u8, u8, u8) = (7, 25, 55);
    const SELECTION_FOREGROUND: (u8, u8, u8) = (250, 240, 10);
    const SELECTION_BACKGROUND: (u8, u8, u8) = (30, 90, 200);
    const CURSOR: (u8, u8, u8) = (45, 210, 140);

    fn xrgb(color: (u8, u8, u8)) -> u32 {
        rgb_to_xrgb(color.0, color.1, color.2)
    }

    fn atlas(scale: f32) -> GlyphAtlas {
        GlyphAtlas::new("kokuban-test-font-that-does-not-exist", 14.0, scale).unwrap()
    }

    fn colors() -> TerminalColors {
        TerminalColors::new(FOREGROUND, BACKGROUND)
            .with_selection(Some(SELECTION_FOREGROUND), Some(SELECTION_BACKGROUND))
    }

    fn grid(lines: &[&str]) -> Grid {
        let columns = lines.iter().map(|line| line.chars().count()).max().unwrap();
        let mut grid = Grid::new(columns, lines.len(), 0);
        for (row, line) in lines.iter().enumerate() {
            grid.carriage_return();
            grid.cursor_row = row;
            for c in line.chars() {
                grid.put_char(c);
            }
        }
        grid.carriage_return();
        grid.cursor_row = 0;
        grid
    }

    struct Frame {
        pixels: Vec<u32>,
        width: u32,
        cell: (u32, u32),
    }

    impl Frame {
        fn at(&self, x: u32, y: u32) -> u32 {
            self.pixels[(y * self.width + x) as usize]
        }

        /// Pixels of one cell, row by row.
        fn cell_pixels(&self, row: u32, column: u32) -> Vec<u32> {
            let (w, h) = self.cell;
            (0..h).flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| self.at(column * w + x, row * h + y))
                .collect()
        }
    }

    fn render(
        snapshot: &GridSnapshot,
        atlas: &mut GlyphAtlas,
        colors: &TerminalColors,
        cursor: bool,
    ) -> Frame {
        let dimensions = atlas_cell_dimensions(atlas).unwrap();
        let cell = (u32::from(dimensions.0), u32::from(dimensions.1));
        let size = (snapshot.columns as u32 * cell.0, snapshot.rows as u32 * cell.1);
        let mut pixels = vec![xrgb(BACKGROUND); (size.0 * size.1) as usize];
        draw_grid_snapshot(&mut pixels, size, atlas, *colors, dimensions, snapshot, cursor);
        Frame { pixels, width: size.0, cell }
    }

    /// Expected cell pixels for a block filling rows `ink_rows`.
    fn rows_cell(cell: (u32, u32), ink_rows: std::ops::Range<u32>, ink: u32, paper: u32) -> Vec<u32> {
        (0..cell.1).flat_map(|y| {
            let color = if ink_rows.contains(&y) { ink } else { paper };
            std::iter::repeat_n(color, cell.0 as usize)
        }).collect()
    }

    #[test]
    fn blocks_fill_whole_cells_and_lines_join_across_cells_at_scale_1_and_2() {
        for scale in [1.0, 2.0] {
            let mut atlas = atlas(scale);
            let snapshot = snapshot_locked_grid(&grid(&["████", "────", "│▀▄│", "│▄▀│"]));
            let frame = render(&snapshot, &mut atlas, &colors(), false);
            let (w, h) = frame.cell;
            let (fg, bg) = (xrgb(FOREGROUND), xrgb(BACKGROUND));

            // Solid blocks leave no seams between cells.
            for y in 0..h {
                for x in 0..frame.width {
                    assert_eq!(frame.at(x, y), fg, "seam in █ row at ({x}, {y}), scale {scale}");
                }
            }
            // The horizontal line is one unbroken band of light thickness.
            let thickness = light_thickness(scale);
            let top = (h - thickness) / 2;
            for y in 0..h {
                let expected = if (top..top + thickness).contains(&y) { fg } else { bg };
                for x in 0..frame.width {
                    assert_eq!(frame.at(x, h + y), expected, "─ at ({x}, {y}), scale {scale}");
                }
            }
            // Vertical lines continue between rows 2 and 3 in both edge columns.
            let left = (w - thickness) / 2;
            for column in [0, 3] {
                for y in 2 * h..4 * h {
                    for x in 0..w {
                        let expected = if (left..left + thickness).contains(&x) { fg } else { bg };
                        assert_eq!(frame.at(column * w + x, y), expected,
                            "│ at column {column} ({x}, {y}), scale {scale}");
                    }
                }
            }
            // Half blocks: the upper half is floor(h / 2) rows.
            assert_eq!(frame.cell_pixels(2, 1), rows_cell(frame.cell, 0..h / 2, fg, bg));
            assert_eq!(frame.cell_pixels(2, 2), rows_cell(frame.cell, h / 2..h, fg, bg));
            // Stacked ▀ over ▄ and ▄ over ▀ meet without a gap.
            assert_eq!(frame.cell_pixels(3, 1), rows_cell(frame.cell, h / 2..h, fg, bg));
            assert_eq!(frame.cell_pixels(3, 2), rows_cell(frame.cell, 0..h / 2, fg, bg));
        }
    }

    #[test]
    fn selection_and_reverse_video_recolor_blocks_like_text() {
        let mut atlas = atlas(1.0);
        let colors = colors();
        let grid = grid(&["▀█▀▄"]);
        let mut snapshot = snapshot_locked_grid(&grid);
        let mut selection = SelectionState::default();
        selection.start(point_from_viewport(&grid, 0, 0));
        selection.update(point_from_viewport(&grid, 0, 1));
        apply_selection_to_snapshot(&mut snapshot, &selection, &grid, &colors);
        snapshot.cells[3].flags.insert(CellFlags::REVERSE);
        let frame = render(&snapshot, &mut atlas, &colors, false);
        let h = frame.cell.1;

        let (selected_ink, selected_paper) = (xrgb(SELECTION_FOREGROUND), xrgb(SELECTION_BACKGROUND));
        assert_eq!(frame.cell_pixels(0, 0), rows_cell(frame.cell, 0..h / 2, selected_ink, selected_paper));
        assert_eq!(frame.cell_pixels(0, 1), rows_cell(frame.cell, 0..h, selected_ink, selected_paper));
        // Unselected cells keep the ordinary colours; reverse video swaps them.
        let (fg, bg) = (xrgb(FOREGROUND), xrgb(BACKGROUND));
        assert_eq!(frame.cell_pixels(0, 2), rows_cell(frame.cell, 0..h / 2, fg, bg));
        assert_eq!(frame.cell_pixels(0, 3), rows_cell(frame.cell, h / 2..h, bg, fg));
    }

    #[test]
    fn opaque_block_cursor_inverts_builtin_shapes() {
        let colors = colors().with_cursor(CURSOR);
        for scale in [1.0, 2.0] {
            let mut atlas = atlas(scale);
            for text in ["▀", "░", "╭", "┼", "█", "╳"] {
                let grid = grid(&[&format!("{text}─"), "  "]);
                let snapshot = snapshot_locked_grid(&grid);
                let baseline = render(&snapshot, &mut atlas, &colors, false);
                let actual = render(&snapshot, &mut atlas, &colors, true);

                // Oracle: the cursor cell drawn normally with the cursor
                // colour as background and the cell background as ink.
                let mut reference = snapshot_locked_grid(&grid);
                reference.cells[0].fg = Color::Rgb(BACKGROUND.0, BACKGROUND.1, BACKGROUND.2);
                reference.cells[0].bg = Color::Rgb(CURSOR.0, CURSOR.1, CURSOR.2);
                let reference = render(&reference, &mut atlas, &colors, false);
                assert_eq!(actual.cell_pixels(0, 0), reference.cell_pixels(0, 0),
                    "cursor over {text} at scale {scale}");
                assert_ne!(actual.cell_pixels(0, 0), baseline.cell_pixels(0, 0), "{text}");
                // Nothing outside the cursor cell changes.
                for (row, column) in [(0, 1), (1, 0), (1, 1)] {
                    assert_eq!(actual.cell_pixels(row, column), baseline.cell_pixels(row, column));
                }
            }
        }
    }

    #[test]
    fn translucent_cursor_is_drawn_over_builtin_shapes() {
        let mut atlas = atlas(1.0);
        let colors = colors();
        for shape in [CursorShape::Block, CursorShape::Bar, CursorShape::Underline] {
            let grid = grid(&["▄"]);
            let mut snapshot = snapshot_locked_grid(&grid);
            snapshot.cursor.as_mut().unwrap().shape = shape;
            let baseline = render(&snapshot, &mut atlas, &colors, false);
            let actual = render(&snapshot, &mut atlas, &colors, true);

            let mut expected = baseline.pixels.clone();
            let (origin, size) = cursor_rectangle(shape, (0, 0), baseline.cell).unwrap();
            fill_rect(&mut expected, (baseline.width, baseline.cell.1), origin, size,
                contrasting_cursor_color(xrgb(BACKGROUND)), CURSOR_ALPHA);
            assert_eq!(actual.pixels, expected, "{shape:?} cursor over ▄");
        }
    }

    #[test]
    fn partial_band_repaint_matches_a_full_repaint() {
        let mut atlas = atlas(2.0);
        let colors = colors().with_cursor(CURSOR);
        let snapshot = snapshot_locked_grid(&grid(&["▀╭─╮█", "▒│▚│▄", "▄╰┴╯▀"]));
        let full = render(&snapshot, &mut atlas, &colors, true);
        let h = full.cell.1;
        let width = full.width as usize;
        let frame_size = (full.width, 3 * h);
        let band = h / 2..2 * h + h / 3;
        let (start, end) = (band.start as usize * width, band.end as usize * width);
        let mut partial = full.pixels.clone();
        partial[start..end].fill(xrgb(BACKGROUND));
        draw_grid_snapshot_in_band(&mut partial, frame_size, &mut atlas, &colors,
            (full.cell.0 as u16, h as u16), &snapshot, true, &[], band);
        assert_eq!(partial, full.pixels);
    }
}

