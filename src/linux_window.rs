use crate::config::Config;
use crate::glyph_atlas::GlyphAtlas;
use crate::grid::cell::Color;
use crate::grid::TerminalEvent;
use crate::grid::{Grid, MouseTracking};
use crate::input::linux::{
    encode_key_press, key_press_from_winit, scrollback_action_from_winit, ImeCommitPayload,
};
use crate::input::paste::{encode_paste, MAX_PASTE_BYTES};
use crate::linux_clipboard::{ClipboardEvent, LinuxClipboard, MAX_CLIPBOARD_TEXT_BYTES};
use crate::notifications::{NotificationController, NotificationRequest};
use crate::omarchy_theme::{watch_current_theme, ThemeWatcher};
use crate::palette::PaletteOverrides;
use crate::parser::ansi::GraphicsSupport;
use crate::pty::Pty;
use crate::selection::{GridPoint, SelectionState};
use crate::software_graphics::SoftwareGraphics;
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
use winit::event::{DeviceId, ElementState, Ime, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::platform::x11::WindowAttributesExtX11;
use winit::window::{Icon, ImePurpose, UserAttentionType, Window, WindowId};

mod clipboard;
mod damage;
mod dispatch;
mod draw;
mod ime;
mod pointer;
mod sizing;
mod status;
#[cfg(test)]
mod test_support;

use clipboard::{
    apply_selection_to_snapshot, clipboard_action, selection_point_at_pointer, sync_selection,
    ClipboardAction, SelectionContext,
};
use damage::{frame_damage, scene_for_buffer_age, PresentedScene, MAX_PRESENTED_SCENES};
use dispatch::{
    dispatch_encoded_terminal_input_with, dispatch_focus_event_with, dispatch_keyboard_input_with,
    dispatch_mouse_button_and_motion_with, dispatch_mouse_motion_with, dispatch_mouse_wheel_with,
    modifiers_after_focus_change, record_window_focus_change, KeyboardInputError,
    KeyboardInputOutcome, MouseMotionDispatchOutcome,
};
use draw::{draw_grid_snapshot_in_band, draw_ime_preedit, rgb_to_xrgb, snapshot_locked_grid};
use ime::{
    ime_cursor_area, ime_payload_from_event, ime_preedit_payload, layout_ime_preedit_for_snapshot,
    replace_ime_preedit, reveal_ime_input_viewport, sync_ime_cursor_area_with, ImeCursorArea,
    ImePreedit, ImePreeditPayload,
};
use pointer::{mouse_button_code_from_winit, PointerRouteState};
use sizing::{
    atlas_cell_dimensions, configured_terminal_dimensions, create_glyph_atlas, drawable_dimensions,
    immediate_surface_size_to_reconcile, initial_window_dimensions, is_current_surface_size,
    physical_size_for_terminal, replace_glyph_atlas_for_scale, resize_terminal_with,
    set_grid_cell_dimensions, terminal_dimensions_from_grid, terminal_size_for_surface,
    GlyphAtlasRebuildError, TerminalSize, MAX_TERMINAL_COLUMNS, MAX_TERMINAL_ROWS,
};
use status::{
    classify_reader_exit, classify_writer_exit, combine_launch_results, reader_status_result,
    terminal_accepts_input, writer_status_result, ReaderStatus, WriterStatus,
};

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
    ReaderExited(ReaderStatus),
    WriterExited(WriterStatus),
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
enum GridAccessError {
    #[error("could not access the Linux terminal grid because its lock is poisoned")]
    Poisoned,
    #[error("terminal grid dimensions {columns}x{rows} exceed the Linux PTY limit")]
    DimensionsOutOfRange { columns: usize, rows: usize },
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
        Pty::spawn(
            columns,
            rows,
            graphics_support.kitty,
            graphics_support.sixel,
        )
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
    })
    .map_err(|error| log::warn!("Linux clipboard worker unavailable: {error}"))
    .ok();
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
        move |event, grid| match event {
            TerminalEvent::Notification { title, body } => {
                reader_notifications.handle_notification(NotificationRequest::new(title, body));
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
                let mut graphics = reader_graphics
                    .lock()
                    .map_err(|_| ReaderExit::GridPoisoned)?;
                Ok(graphics.process(event, grid))
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
    // Window construction takes every subsystem handle once; a builder is not worth it.
    #[allow(clippy::too_many_arguments)]
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
            let grid = self
                .grid
                .lock()
                .map_err(|_| GridAccessError::Poisoned.to_string())?;
            let graphics = self
                .graphics
                .lock()
                .map_err(|_| "image cache lock is poisoned".to_string())?;
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
            let grid = self
                .grid
                .lock()
                .map_err(|_| GridAccessError::Poisoned.to_string())?;
            let mut graphics = self
                .graphics
                .lock()
                .map_err(|_| "image cache lock is poisoned".to_string())?;
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
                    match self.grid.lock() {
                        Ok(grid) => {
                            grid.mouse_tracking == MouseTracking::None || self.modifiers.shift_key()
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
                    && self.selection_drag == Some(device_id);
                if local_press || local_release {
                    if let Some(position) = self.pointer_route.position_for(device_id) {
                        match self.select_at_pointer(position, local_press) {
                            Ok(true) if local_press => self.selection_drag = Some(device_id),
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
                if matches!(result, Ok(KeyboardInputOutcome::Forwarded { .. })) {
                    self.clear_selection();
                }
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
                if matches!(result, Ok(KeyboardInputOutcome::Forwarded { .. })) {
                    self.clear_selection();
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
                let theme = self
                    .theme_watcher
                    .as_ref()
                    .and_then(ThemeWatcher::take_update);
                if let Some(theme) = theme {
                    let colors = self.palette_overrides.resolve(Some(theme));
                    if colors != self.colors {
                        let foreground = colors.resolve_foreground(Color::Default, false);
                        let background = colors.default_background();
                        let result = self
                            .grid
                            .lock()
                            .map(|mut grid| {
                                // OSC queries and the next frame observe the same palette.
                                grid.default_fg_hex = terminal_color_query_value(foreground);
                                grid.default_bg_hex = terminal_color_query_value(background);
                                self.colors = colors;
                                self.background =
                                    rgb_to_xrgb(background.0, background.1, background.2);
                            })
                            .map_err(|_| GridAccessError::Poisoned);
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

fn snapshot_window_title(grid: &Mutex<Grid>) -> Result<String, GridAccessError> {
    grid.lock()
        .map(|grid| normalized_window_title(grid.title()).into_owned())
        .map_err(|_| GridAccessError::Poisoned)
}

#[cfg(test)]
mod tests {
    use super::draw::snapshot_grid;
    use super::sizing::TerminalResizeError;
    use super::{
        arm_grid_redraw, arm_window_title_update, begin_grid_redraw, begin_window_title_update,
        changed_window_title, resize_terminal_with, snapshot_window_title,
        terminal_color_query_value, terminal_size_for_surface, GridAccessError, WINDOW_TITLE,
    };
    use crate::grid::Grid;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use winit::dpi::PhysicalSize;

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
