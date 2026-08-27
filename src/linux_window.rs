use crate::config::{ColorConfig, Config};
use crate::glyph_atlas::{GlyphAtlas, GlyphKey};
use crate::grid::cell::{Cell, CellFlags, Color, UnderlineStyle};
use crate::grid::{CursorShape, Grid, MouseTracking};
use crate::input::linux::{
    encode_key_press, ime_commit_payload, key_press_from_winit, scrollback_action_from_winit,
    ImeCommitPayload, ScrollbackAction,
};
use crate::input::mouse::{
    encode_alternate_scroll_steps, encode_mouse_event, mouse_button_with_modifier_flags,
    mouse_wheel_route, MouseWheelRoute, MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP,
    MAX_WHEEL_STEPS_PER_EVENT,
};
use crate::pty::Pty;
use crate::software_raster::{draw_glyph_a8, fill_rect};
use crate::terminal_colors::TerminalColors;
use crate::terminal_reader::{ReaderExit, TerminalReader};
use crate::terminal_writer::{TerminalWriteQueueError, TerminalWriter, WriterExit};
use crate::window_title::{normalized_window_title, sync_window_title_with, WINDOW_TITLE};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use unicode_width::UnicodeWidthChar;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{
    DeviceId, ElementState, Ime, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{ImePurpose, Window, WindowId};

const INITIAL_CELL_WIDTH: u32 = 10;
const INITIAL_CELL_HEIGHT: u32 = 20;
// Keep the pre-atlas placeholder modest; Winit interprets this size in logical pixels.
const MAX_INITIAL_LOGICAL_WIDTH: u32 = 1920;
const MAX_INITIAL_LOGICAL_HEIGHT: u32 = 1080;
// A full RGBA software frame at this bound is about 135 MiB.
const MAX_REQUESTED_PHYSICAL_WIDTH: u32 = 8192;
const MAX_REQUESTED_PHYSICAL_HEIGHT: u32 = 4320;
const SCALE_CHANGE_EPSILON: f64 = 0.001;
const EXIT_AFTER_FIRST_FRAME_ENV: &str = "KOKUBAN_EXIT_AFTER_FIRST_FRAME";
const CURSOR_THICKNESS: u32 = 2;
const CURSOR_ALPHA: u8 = 180;
const FOCUS_IN_REPORT: &[u8] = b"\x1b[I";
const FOCUS_OUT_REPORT: &[u8] = b"\x1b[O";
const MAX_IME_PREEDIT_BYTES: usize = 4 * 1024;
const MAX_IME_PREEDIT_RENDER_CELLS: usize = 1024;
const IME_PREEDIT_REPLACEMENT: char = '\u{fffd}';
const MOUSE_MOTION_FLAG: u8 = 32;
const MOUSE_NO_BUTTON: u8 = 3;
// Limit the Grid/snapshot budget to 262,144 visible cells while still covering wide 8K layouts.
const MAX_TERMINAL_COLUMNS: u16 = 1024;
const MAX_TERMINAL_ROWS: u16 = 256;

type SoftwareSurface = Surface<Arc<Window>, Arc<Window>>;

#[derive(Debug)]
enum LinuxEvent {
    GridUpdated,
    WindowTitleChanged,
    ReaderExited(ReaderStatus),
    WriterExited(WriterStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReaderStatus {
    Normal,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WriterStatus {
    Normal,
    Failed(String),
}

#[derive(Clone, Copy)]
struct GlyphSource<'a> {
    pixels: &'a [u8],
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
struct TerminalDimensions {
    columns: u16,
    rows: u16,
}

#[derive(Debug, Default)]
struct MouseWheelState {
    line_remainder: f64,
    last_route: Option<MouseWheelRoute>,
}

impl MouseWheelState {
    fn reset(&mut self) {
        self.line_remainder = 0.0;
        self.last_route = None;
    }
}

#[derive(Debug, Default)]
struct MouseMotionState {
    // Xterm reports character-cell transitions, not raw subpixel movement.
    last_cell: Option<(usize, usize)>,
}

impl MouseMotionState {
    fn reset(&mut self) {
        self.last_cell = None;
    }

    fn anchor(&mut self, cell: (usize, usize)) {
        self.last_cell = Some(cell);
    }

    fn record_dispatch(&mut self, outcome: MouseMotionDispatchOutcome) {
        if let MouseMotionDispatchOutcome::Observed(cell)
        | MouseMotionDispatchOutcome::Enqueued(cell) = outcome
        {
            self.anchor(cell);
        }
    }
}

#[derive(Debug)]
struct PointerRouteState<D: Copy + Eq> {
    active_device: Option<D>,
    position: Option<PhysicalPosition<f64>>,
    wheel_state: MouseWheelState,
    motion_state: MouseMotionState,
    // Physical state is separate from successfully forwarded presses so 1002 can
    // become active while a button that was pressed under another mode is held.
    held_buttons: u8,
    held_button_order: [u8; 3],
    held_button_count: u8,
    forwarded_buttons: u8,
    left_while_captured: bool,
    captured_cell_dimensions: Option<(u16, u16)>,
    position_uses_captured_metrics: bool,
}

impl<D: Copy + Eq> Default for PointerRouteState<D> {
    fn default() -> Self {
        Self {
            active_device: None,
            position: None,
            wheel_state: MouseWheelState::default(),
            motion_state: MouseMotionState::default(),
            held_buttons: 0,
            held_button_order: [0; 3],
            held_button_count: 0,
            forwarded_buttons: 0,
            left_while_captured: false,
            captured_cell_dimensions: None,
            position_uses_captured_metrics: false,
        }
    }
}

impl<D: Copy + Eq> PointerRouteState<D> {
    fn reset(&mut self) {
        self.active_device = None;
        self.position = None;
        self.wheel_state.reset();
        self.motion_state.reset();
        self.held_buttons = 0;
        self.held_button_order = [0; 3];
        self.held_button_count = 0;
        self.forwarded_buttons = 0;
        self.left_while_captured = false;
        self.captured_cell_dimensions = None;
        self.position_uses_captured_metrics = false;
    }

    fn select_device(&mut self, device_id: D) -> bool {
        if self.active_device == Some(device_id) {
            return true;
        }
        if self.has_pointer_capture() {
            return false;
        }
        self.reset();
        self.active_device = Some(device_id);
        true
    }

    fn cursor_entered(&mut self, device_id: D) {
        if self.select_device(device_id) {
            self.left_while_captured = false;
        }
    }

    fn cursor_moved(&mut self, device_id: D, position: PhysicalPosition<f64>) -> bool {
        if !self.select_device(device_id) {
            return false;
        }
        self.position = Some(position);
        self.position_uses_captured_metrics = false;
        true
    }

    fn cursor_left(&mut self, device_id: D) {
        if self.active_device == Some(device_id) {
            self.motion_state.reset();
            if !self.has_pointer_capture() {
                self.reset();
            } else {
                self.left_while_captured = true;
            }
        }
    }

    fn position_for(&self, device_id: D) -> Option<PhysicalPosition<f64>> {
        if self.active_device == Some(device_id) {
            self.position
        } else {
            None
        }
    }

    fn select_wheel_device(&mut self, device_id: D) -> Option<Option<PhysicalPosition<f64>>> {
        if !self.select_device(device_id) {
            return None;
        }
        Some(if self.position_uses_captured_metrics {
            None
        } else {
            self.position
        })
    }

    fn button_cell_dimensions_for(
        &self,
        device_id: D,
        state: ElementState,
        button: Option<u8>,
        current: Option<(u16, u16)>,
    ) -> Option<(u16, u16)> {
        if self.active_device != Some(device_id) || !self.position_uses_captured_metrics {
            return current;
        }
        let captured_release = state == ElementState::Released
            && button
                .filter(|button| *button <= 2)
                .is_some_and(|button| self.forwarded_buttons & (1 << button) != 0);
        if captured_release {
            self.captured_cell_dimensions.or(current)
        } else {
            None
        }
    }

    fn scale_factor_changed(&mut self, current_cell_dimensions: Option<(u16, u16)>) {
        self.wheel_state.reset();
        self.motion_state.reset();
        if !self.has_pointer_capture() {
            self.reset();
        } else if self.forwarded_buttons != 0 {
            if !self.position_uses_captured_metrics {
                self.captured_cell_dimensions =
                    current_cell_dimensions.or(self.captured_cell_dimensions);
            }
            self.position_uses_captured_metrics = true;
        } else {
            self.position = None;
            self.captured_cell_dimensions = None;
            self.position_uses_captured_metrics = false;
        }
    }

    fn record_physical_button_event(
        &mut self,
        device_id: D,
        state: ElementState,
        button: Option<u8>,
    ) -> bool {
        let Some(button) = button.filter(|button| *button <= 2) else {
            return self.active_device == Some(device_id);
        };
        match state {
            ElementState::Pressed => {
                if !self.select_device(device_id) {
                    return false;
                }
                self.remove_held_button(button);
                let index = usize::from(self.held_button_count);
                self.held_button_order[index] = button;
                self.held_button_count += 1;
                self.held_buttons |= 1 << button;
            }
            ElementState::Released => {
                if self.active_device != Some(device_id) {
                    return false;
                }
                self.remove_held_button(button);
            }
        }
        true
    }

    fn active_motion_button(&self) -> Option<u8> {
        self.held_button_count
            .checked_sub(1)
            .map(|index| self.held_button_order[usize::from(index)])
    }

    fn has_pointer_capture(&self) -> bool {
        self.held_buttons != 0 || self.forwarded_buttons != 0
    }

    fn remove_held_button(&mut self, button: u8) {
        let count = usize::from(self.held_button_count);
        let Some(index) = self.held_button_order[..count]
            .iter()
            .position(|held| *held == button)
        else {
            self.held_buttons &= !(1 << button);
            return;
        };
        self.held_button_order.copy_within(index + 1..count, index);
        self.held_button_count -= 1;
        self.held_button_order[usize::from(self.held_button_count)] = 0;
        self.held_buttons &= !(1 << button);
    }

    fn record_button_dispatch(
        &mut self,
        device_id: D,
        state: ElementState,
        button: Option<u8>,
        forwarded: bool,
        cell_dimensions: Option<(u16, u16)>,
    ) {
        let Some(button) = button.filter(|button| *button <= 2) else {
            return;
        };
        let mask = 1 << button;
        match state {
            ElementState::Pressed if forwarded && self.active_device == Some(device_id) => {
                self.forwarded_buttons |= mask;
                self.captured_cell_dimensions = cell_dimensions;
            }
            ElementState::Released if self.active_device == Some(device_id) => {
                self.forwarded_buttons &= !mask;
                if !self.has_pointer_capture() {
                    if self.left_while_captured {
                        self.reset();
                    } else {
                        if self.position_uses_captured_metrics {
                            self.position = None;
                            self.motion_state.reset();
                        }
                        self.captured_cell_dimensions = None;
                        self.position_uses_captured_metrics = false;
                    }
                }
            }
            _ => {}
        }
    }
}

impl std::fmt::Display for TerminalDimensions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}x{}", self.columns, self.rows)
    }
}

#[derive(Debug, Error)]
enum TerminalResizeError {
    #[error("could not resize Linux terminal to {target}: {source}")]
    Grid {
        target: TerminalDimensions,
        #[source]
        source: GridAccessError,
    },
    #[error("could not resize Linux PTY to {target}: {source}")]
    Pty {
        target: TerminalDimensions,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Error)]
enum KeyboardInputError {
    #[error("could not access Linux terminal input state: {0}")]
    Grid(#[from] GridAccessError),
    #[error("could not queue Linux terminal input: {0}")]
    Queue(#[from] TerminalWriteQueueError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyboardInputOutcome {
    Ignored { viewport_changed: bool },
    Forwarded { viewport_changed: bool },
    Scrollback { viewport_changed: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseMotionDispatchOutcome {
    Ignored,
    Observed((usize, usize)),
    Deduplicated,
    DroppedFull,
    Enqueued((usize, usize)),
}

impl KeyboardInputOutcome {
    fn viewport_changed(self) -> bool {
        match self {
            Self::Ignored { viewport_changed }
            | Self::Forwarded { viewport_changed }
            | Self::Scrollback { viewport_changed } => viewport_changed,
        }
    }
}

#[derive(Debug, Error)]
enum GlyphAtlasRebuildError {
    #[error("{0}")]
    Atlas(String),
    #[error(transparent)]
    Grid(#[from] GridAccessError),
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
enum SurfaceSizeError {
    #[error(
        "Linux surface {width}x{height} exceeds the renderer budget of \
         {MAX_REQUESTED_PHYSICAL_WIDTH}x{MAX_REQUESTED_PHYSICAL_HEIGHT} pixels"
    )]
    ExceedsRenderBudget { width: u32, height: u32 },
}

#[derive(Debug, Clone, Copy)]
struct CursorSnapshot {
    row: usize,
    column: usize,
    shape: CursorShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImeCursorArea {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImePreeditCursor {
    selection_start: usize,
    selection_end: usize,
    caret: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImePreedit {
    text: String,
    cursor: Option<ImePreeditCursor>,
}

#[derive(Debug, PartialEq, Eq)]
enum ImePreeditPayload {
    Clear,
    Text(ImePreedit),
    TooManyBytes { byte_count: usize, limit: usize },
    TooManyRenderCells { cell_count: usize, limit: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImePreeditGlyph {
    character: char,
    byte_start: usize,
    byte_end: usize,
    row: usize,
    column: usize,
    width: u8,
    selected: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ImePreeditLayout {
    glyphs: Vec<ImePreeditGlyph>,
    cleared_cells: Vec<(usize, usize)>,
    caret_cell: Option<(usize, usize)>,
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

pub(crate) fn launch(config: Config) -> Result<(), String> {
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
    let background = ColorConfig::parse_hex(&config.colors.background);
    let foreground = ColorConfig::parse_hex(&config.colors.foreground);
    let initial_size = initial_window_dimensions(columns, rows);
    let exit_after_first_frame = std::env::var(EXIT_AFTER_FIRST_FRAME_ENV).as_deref() == Ok("1");
    let mut grid = Grid::new(
        usize::from(columns),
        usize::from(rows),
        config.window.scrollback_lines,
    );
    grid.default_fg_hex = terminal_color_query_value(foreground);
    grid.default_bg_hex = terminal_color_query_value(background);
    let grid = Arc::new(Mutex::new(grid));
    let pty = Arc::new(
        Pty::spawn(columns, rows, false, false)
            .map_err(|error| format!("could not start the Linux shell: {error}"))?,
    );
    let redraw_pending = Arc::new(AtomicBool::new(false));
    let window_title_pending = Arc::new(AtomicBool::new(false));
    let event_proxy = event_loop.create_proxy();
    let writer_proxy = event_proxy.clone();
    let mut writer = TerminalWriter::spawn(pty.clone(), move |exit| {
        let _ = writer_proxy.send_event(LinuxEvent::WriterExited(classify_writer_exit(exit)));
    })
    .map_err(|error| format!("could not start the Linux terminal writer: {error}"))?;
    let update_proxy = event_proxy.clone();
    let update_pending = redraw_pending.clone();
    let update_title_grid = grid.clone();
    let update_title_pending = window_title_pending.clone();
    let mut observed_window_title = WINDOW_TITLE.to_string();
    let reader = match TerminalReader::spawn_text(
        pty.clone(),
        grid.clone(),
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
        background,
        foreground,
        config.font.family,
        config.font.size,
        initial_size,
        exit_after_first_frame,
        grid,
        pty,
        reader,
        writer,
        redraw_pending,
        window_title_pending,
    );

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
    // Drop the surface and its display connection before releasing the window.
    surface: Option<SoftwareSurface>,
    context: Option<Context<Arc<Window>>>,
    window: Option<Arc<Window>>,
    glyph_atlas: Option<GlyphAtlas>,
    atlas_scale_factor: Option<f64>,
    cell_dimensions: Option<(u16, u16)>,
    background: u32,
    colors: TerminalColors,
    font_family: String,
    font_size: f32,
    initial_size: LogicalSize<u32>,
    exit_after_first_frame: bool,
    first_frame_presented: bool,
    applied_window_title: String,
    grid: Arc<Mutex<Grid>>,
    pty: Arc<Pty>,
    reader: Option<TerminalReader>,
    writer: Option<TerminalWriter>,
    redraw_pending: Arc<AtomicBool>,
    window_title_pending: Arc<AtomicBool>,
    reader_status: Option<ReaderStatus>,
    modifiers: ModifiersState,
    last_window_focus: Option<bool>,
    pointer_route: PointerRouteState<DeviceId>,
    ime_active: bool,
    ime_preedit: Option<ImePreedit>,
    last_ime_cursor_area: Option<ImeCursorArea>,
    error: Option<String>,
}

impl LinuxWindow {
    fn new(
        background: (u8, u8, u8),
        foreground: (u8, u8, u8),
        font_family: String,
        font_size: f32,
        initial_size: LogicalSize<u32>,
        exit_after_first_frame: bool,
        grid: Arc<Mutex<Grid>>,
        pty: Arc<Pty>,
        reader: TerminalReader,
        writer: TerminalWriter,
        redraw_pending: Arc<AtomicBool>,
        window_title_pending: Arc<AtomicBool>,
    ) -> Self {
        Self {
            surface: None,
            context: None,
            window: None,
            glyph_atlas: None,
            atlas_scale_factor: None,
            cell_dimensions: None,
            background: rgb_to_xrgb(background.0, background.1, background.2),
            colors: TerminalColors::new(foreground, background),
            font_family,
            font_size,
            initial_size,
            exit_after_first_frame,
            first_frame_presented: false,
            applied_window_title: WINDOW_TITLE.to_string(),
            grid,
            pty,
            reader: Some(reader),
            writer: Some(writer),
            redraw_pending,
            window_title_pending,
            reader_status: None,
            modifiers: ModifiersState::empty(),
            last_window_focus: None,
            pointer_route: PointerRouteState::default(),
            ime_active: false,
            ime_preedit: None,
            last_ime_cursor_area: None,
            error: None,
        }
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let attributes = Window::default_attributes()
            .with_title(WINDOW_TITLE)
            .with_transparent(false)
            .with_inner_size(self.initial_size);
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| format!("could not create an opaque window: {error}"))?,
        );
        let initial_title =
            snapshot_window_title(self.grid.as_ref()).map_err(|error| error.to_string())?;
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

        self.surface = Some(surface);
        self.context = Some(context);
        self.window = Some(window.clone());
        self.glyph_atlas = Some(glyph_atlas);
        self.atlas_scale_factor = Some(scale_factor);
        self.cell_dimensions = Some(cell_dimensions);
        if let Some(applied_inner_size) =
            immediate_surface_size_to_reconcile(window.request_inner_size(requested_inner_size))
        {
            self.resize_terminal_for_surface(applied_inner_size)?;
        }
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
            self.cell_dimensions = replacement_dimensions;
        }
        Ok(changed)
    }

    fn resize_terminal_for_surface(&self, size: PhysicalSize<u32>) -> Result<bool, String> {
        let cell_dimensions = self.cell_dimensions.ok_or_else(|| {
            "could not resize the Linux terminal before glyph metrics were ready".to_string()
        })?;
        let Some(target) = terminal_dimensions_for_surface(size, cell_dimensions) else {
            return Ok(false);
        };

        resize_terminal_with(self.grid.as_ref(), target, |columns, rows| {
            self.pty.resize(columns, rows)
        })
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
        let snapshot = snapshot_grid(self.grid.as_ref()).map_err(|error| error.to_string())?;
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
        buffer.fill(self.background);
        draw_grid_snapshot(
            &mut buffer,
            (width.get(), height.get()),
            glyph_atlas,
            self.colors,
            cell_dimensions,
            &snapshot,
            preedit_layout.is_none(),
        );
        if let Some(layout) = preedit_layout.as_ref() {
            draw_ime_preedit(
                &mut buffer,
                (width.get(), height.get()),
                glyph_atlas,
                self.colors,
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
        sync_ime_cursor_area_with(
            self.ime_active,
            &mut self.last_ime_cursor_area,
            next_ime_cursor_area,
            |area| window.set_ime_cursor_area(area.position, area.size),
        );
        Ok(Some(terminal_content_visible))
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
                let scrollback_action =
                    scrollback_action_from_winit(&event, is_synthetic, self.modifiers);
                let key_press = key_press_from_winit(&event, is_synthetic, self.modifiers);
                if scrollback_action.is_none() && key_press.is_none() {
                    return;
                }
                let result = dispatch_keyboard_input_with(
                    self.grid.as_ref(),
                    scrollback_action,
                    |application_cursor_keys| {
                        key_press.and_then(|key_press| {
                            encode_key_press(key_press, application_cursor_keys)
                        })
                    },
                    |bytes| {
                        self.writer
                            .as_ref()
                            .ok_or(TerminalWriteQueueError::Disconnected)?
                            .enqueue(bytes)
                    },
                );
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
                                     {scale_factor}: {error}; waiting for the next resize event"
                                );
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
                    Ok(title) => title,
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

fn classify_reader_exit(exit: &ReaderExit) -> ReaderStatus {
    match exit {
        ReaderExit::Shutdown | ReaderExit::Eof => ReaderStatus::Normal,
        ReaderExit::WaitFailed(error) => ReaderStatus::Failed(format!(
            "terminal reader could not wait for PTY output: {error}"
        )),
        ReaderExit::ReadFailed(error) => ReaderStatus::Failed(format!(
            "terminal reader could not read PTY output: {error}"
        )),
        ReaderExit::ResponseWriteFailed(error) => ReaderStatus::Failed(format!(
            "terminal reader could not write a protocol response: {error}"
        )),
        ReaderExit::GridPoisoned => {
            ReaderStatus::Failed("terminal reader lost access to the terminal grid".to_string())
        }
        ReaderExit::DecoderStalled => {
            ReaderStatus::Failed("terminal decoder stopped making progress".to_string())
        }
        ReaderExit::UnexpectedProtocolEvent => ReaderStatus::Failed(
            "terminal reader received a graphics event in text-only mode".to_string(),
        ),
    }
}

fn reader_status_result(status: ReaderStatus) -> Result<(), String> {
    match status {
        ReaderStatus::Normal => Ok(()),
        ReaderStatus::Failed(error) => Err(error),
    }
}

fn classify_writer_exit(exit: &WriterExit) -> WriterStatus {
    match exit {
        WriterExit::Shutdown => WriterStatus::Normal,
        WriterExit::ChannelClosed => {
            WriterStatus::Failed("terminal writer lost its input queue".to_string())
        }
        WriterExit::WriteFailed(error) => WriterStatus::Failed(format!(
            "terminal writer could not write keyboard input to the PTY: {error}"
        )),
    }
}

fn writer_status_result(status: WriterStatus) -> Result<(), String> {
    match status {
        WriterStatus::Normal => Ok(()),
        WriterStatus::Failed(error) => Err(error),
    }
}

fn combine_launch_results(
    run_result: Result<(), String>,
    reader_join_result: Result<(), String>,
    writer_join_result: Result<(), String>,
    finish_result: Result<(), String>,
) -> Result<(), String> {
    run_result
        .and(reader_join_result)
        .and(writer_join_result)
        .and(finish_result)
}

fn terminal_accepts_input(exiting: bool, reader_status: Option<&ReaderStatus>) -> bool {
    !exiting && reader_status.is_none()
}

fn create_glyph_atlas(
    font_family: &str,
    font_size: f32,
    scale_factor: f64,
) -> Result<GlyphAtlas, String> {
    GlyphAtlas::new(font_family, font_size, scale_factor as f32).map_err(|error| {
        format!("could not initialize the Linux glyph atlas at scale {scale_factor}: {error}")
    })
}

fn replace_glyph_atlas_for_scale<F>(
    glyph_atlas: &mut Option<GlyphAtlas>,
    atlas_scale_factor: &mut Option<f64>,
    font_family: &str,
    font_size: f32,
    scale_factor: f64,
    before_replace: F,
) -> Result<bool, GlyphAtlasRebuildError>
where
    F: FnOnce(&GlyphAtlas, (u16, u16)) -> Result<(), GridAccessError>,
{
    if atlas_scale_factor
        .is_some_and(|current| (current - scale_factor).abs() <= SCALE_CHANGE_EPSILON)
    {
        return Ok(false);
    }

    let replacement = create_glyph_atlas(font_family, font_size, scale_factor)
        .map_err(GlyphAtlasRebuildError::Atlas)?;
    let cell_dimensions =
        atlas_cell_dimensions(&replacement).map_err(GlyphAtlasRebuildError::Atlas)?;
    before_replace(&replacement, cell_dimensions)?;
    *glyph_atlas = Some(replacement);
    *atlas_scale_factor = Some(scale_factor);
    Ok(true)
}

fn initial_window_dimensions(columns: u16, rows: u16) -> LogicalSize<u32> {
    LogicalSize::new(
        (u32::from(columns).max(1) * INITIAL_CELL_WIDTH).min(MAX_INITIAL_LOGICAL_WIDTH),
        (u32::from(rows).max(1) * INITIAL_CELL_HEIGHT).min(MAX_INITIAL_LOGICAL_HEIGHT),
    )
}

fn configured_terminal_dimensions(columns: u16, rows: u16) -> TerminalDimensions {
    TerminalDimensions {
        columns: columns.clamp(1, MAX_TERMINAL_COLUMNS),
        rows: rows.clamp(1, MAX_TERMINAL_ROWS),
    }
}

fn immediate_surface_size_to_reconcile(
    immediate: Option<PhysicalSize<u32>>,
) -> Option<PhysicalSize<u32>> {
    immediate
}

fn is_current_surface_size(event_size: PhysicalSize<u32>, current_size: PhysicalSize<u32>) -> bool {
    event_size == current_size
}

fn drawable_dimensions(
    size: PhysicalSize<u32>,
) -> Result<Option<(NonZeroU32, NonZeroU32)>, SurfaceSizeError> {
    let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
    else {
        return Ok(None);
    };
    if size.width > MAX_REQUESTED_PHYSICAL_WIDTH || size.height > MAX_REQUESTED_PHYSICAL_HEIGHT {
        return Err(SurfaceSizeError::ExceedsRenderBudget {
            width: size.width,
            height: size.height,
        });
    }
    Ok(Some((width, height)))
}

fn rgb_to_xrgb(red: u8, green: u8, blue: u8) -> u32 {
    (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)
}

fn atlas_cell_dimensions(atlas: &GlyphAtlas) -> Result<(u16, u16), String> {
    let cell_width = rounded_positive_i32(atlas.cell_width)
        .and_then(|width| u16::try_from(width).ok())
        .ok_or_else(|| {
            format!(
                "Linux glyph atlas produced an invalid cell width: {}",
                atlas.cell_width
            )
        })?;
    let cell_height = rounded_positive_i32(atlas.cell_height)
        .and_then(|height| u16::try_from(height).ok())
        .ok_or_else(|| {
            format!(
                "Linux glyph atlas produced an invalid cell height: {}",
                atlas.cell_height
            )
        })?;
    Ok((cell_width, cell_height))
}

fn set_grid_cell_dimensions(
    grid: &Mutex<Grid>,
    cell_dimensions: (u16, u16),
) -> Result<(), GridAccessError> {
    let mut grid = grid.lock().map_err(|_| GridAccessError::Poisoned)?;
    grid.cell_pixel_width = cell_dimensions.0;
    grid.cell_pixel_height = cell_dimensions.1;
    Ok(())
}

fn terminal_dimensions_from_grid(
    grid: &Mutex<Grid>,
) -> Result<TerminalDimensions, GridAccessError> {
    let grid = grid.lock().map_err(|_| GridAccessError::Poisoned)?;
    terminal_dimensions_from_locked_grid(&grid)
}

fn terminal_dimensions_from_locked_grid(
    grid: &Grid,
) -> Result<TerminalDimensions, GridAccessError> {
    let columns = grid.cols();
    let rows = grid.rows();
    Ok(TerminalDimensions {
        columns: u16::try_from(columns)
            .map_err(|_| GridAccessError::DimensionsOutOfRange { columns, rows })?,
        rows: u16::try_from(rows)
            .map_err(|_| GridAccessError::DimensionsOutOfRange { columns, rows })?,
    })
}

fn terminal_dimensions_for_surface(
    surface_size: PhysicalSize<u32>,
    cell_dimensions: (u16, u16),
) -> Option<TerminalDimensions> {
    if surface_size.width == 0
        || surface_size.height == 0
        || cell_dimensions.0 == 0
        || cell_dimensions.1 == 0
    {
        return None;
    }

    let columns = (surface_size.width / u32::from(cell_dimensions.0))
        .clamp(1, u32::from(MAX_TERMINAL_COLUMNS));
    let rows =
        (surface_size.height / u32::from(cell_dimensions.1)).clamp(1, u32::from(MAX_TERMINAL_ROWS));
    Some(TerminalDimensions {
        columns: u16::try_from(columns).ok()?,
        rows: u16::try_from(rows).ok()?,
    })
}

fn physical_size_for_terminal(
    terminal_dimensions: TerminalDimensions,
    cell_dimensions: (u16, u16),
) -> Option<PhysicalSize<u32>> {
    if terminal_dimensions.columns == 0
        || terminal_dimensions.rows == 0
        || cell_dimensions.0 == 0
        || cell_dimensions.1 == 0
    {
        return None;
    }

    let width = u32::from(terminal_dimensions.columns)
        .checked_mul(u32::from(cell_dimensions.0))?
        .min(MAX_REQUESTED_PHYSICAL_WIDTH);
    let height = u32::from(terminal_dimensions.rows)
        .checked_mul(u32::from(cell_dimensions.1))?
        .min(MAX_REQUESTED_PHYSICAL_HEIGHT);
    Some(PhysicalSize::new(width, height))
}

fn apply_terminal_resize<P, G>(
    current: TerminalDimensions,
    target: TerminalDimensions,
    resize_pty: P,
    resize_grid: G,
) -> std::io::Result<bool>
where
    P: FnOnce(u16, u16) -> std::io::Result<()>,
    G: FnOnce(u16, u16),
{
    if current == target {
        return Ok(false);
    }

    resize_pty(target.columns, target.rows)?;
    resize_grid(target.columns, target.rows);
    Ok(true)
}

fn resize_terminal_with<F>(
    grid: &Mutex<Grid>,
    target: TerminalDimensions,
    resize_pty: F,
) -> Result<bool, TerminalResizeError>
where
    F: FnOnce(u16, u16) -> std::io::Result<()>,
{
    let mut grid = grid.lock().map_err(|_| TerminalResizeError::Grid {
        target,
        source: GridAccessError::Poisoned,
    })?;
    let current = terminal_dimensions_from_locked_grid(&grid)
        .map_err(|source| TerminalResizeError::Grid { target, source })?;

    apply_terminal_resize(current, target, resize_pty, |columns, rows| {
        grid.resize(usize::from(columns), usize::from(rows));
    })
    .map_err(|source| TerminalResizeError::Pty { target, source })
}

fn dispatch_keyboard_input_with<E, W>(
    grid: &Mutex<Grid>,
    scrollback_action: Option<ScrollbackAction>,
    encode: E,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    E: FnOnce(bool) -> Option<Vec<u8>>,
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let application_cursor_keys = {
        let mut grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;

        if let Some(action) = scrollback_action {
            let viewport_changed = apply_scrollback_action(&mut grid, action);
            return Ok(KeyboardInputOutcome::Scrollback { viewport_changed });
        }

        grid.application_cursor_keys
    };
    dispatch_encoded_terminal_input_with(grid, encode(application_cursor_keys), write)
}

fn accumulate_wheel_steps(
    delta: MouseScrollDelta,
    cell_height: Option<u16>,
    remainder: &mut f64,
) -> i32 {
    let line_delta = match delta {
        MouseScrollDelta::LineDelta(_, vertical) => f64::from(vertical),
        MouseScrollDelta::PixelDelta(position) => {
            let Some(cell_height) = cell_height.filter(|height| *height != 0) else {
                *remainder = 0.0;
                return 0;
            };
            position.y / f64::from(cell_height)
        }
    };
    if !line_delta.is_finite() {
        *remainder = 0.0;
        return 0;
    }

    let total = *remainder + line_delta;
    if !total.is_finite() {
        *remainder = 0.0;
        return 0;
    }
    let whole_steps = total.trunc();
    let limit = f64::from(MAX_WHEEL_STEPS_PER_EVENT);
    if whole_steps.abs() > limit {
        *remainder = 0.0;
        return if whole_steps.is_sign_positive() {
            i32::try_from(MAX_WHEEL_STEPS_PER_EVENT).expect("wheel step limit should fit i32")
        } else {
            -i32::try_from(MAX_WHEEL_STEPS_PER_EVENT).expect("wheel step limit should fit i32")
        };
    }

    *remainder = total - whole_steps;
    whole_steps as i32
}

fn terminal_cell_at_pointer(
    position: PhysicalPosition<f64>,
    cell_dimensions: (u16, u16),
    terminal_dimensions: TerminalDimensions,
) -> Option<(usize, usize)> {
    if !position.x.is_finite()
        || !position.y.is_finite()
        || position.x < 0.0
        || position.y < 0.0
        || cell_dimensions.0 == 0
        || cell_dimensions.1 == 0
        || terminal_dimensions.columns == 0
        || terminal_dimensions.rows == 0
    {
        return None;
    }

    let last_column = terminal_dimensions.columns - 1;
    let last_row = terminal_dimensions.rows - 1;
    let column = (position.x / f64::from(cell_dimensions.0))
        .floor()
        .min(f64::from(last_column)) as usize;
    let row = (position.y / f64::from(cell_dimensions.1))
        .floor()
        .min(f64::from(last_row)) as usize;
    Some((column + 1, row + 1))
}

fn terminal_cell_at_mouse_button(
    position: PhysicalPosition<f64>,
    cell_dimensions: (u16, u16),
    terminal_dimensions: TerminalDimensions,
    state: ElementState,
) -> Option<(usize, usize)> {
    let position =
        if state == ElementState::Released && position.x.is_finite() && position.y.is_finite() {
            PhysicalPosition::new(position.x.max(0.0), position.y.max(0.0))
        } else {
            position
        };
    terminal_cell_at_pointer(position, cell_dimensions, terminal_dimensions)
}

fn terminal_cell_at_mouse_motion(
    position: PhysicalPosition<f64>,
    cell_dimensions: (u16, u16),
    terminal_dimensions: TerminalDimensions,
    button_held: bool,
) -> Option<(usize, usize)> {
    let position = if button_held && position.x.is_finite() && position.y.is_finite() {
        PhysicalPosition::new(position.x.max(0.0), position.y.max(0.0))
    } else {
        position
    };
    terminal_cell_at_pointer(position, cell_dimensions, terminal_dimensions)
}

fn apply_wheel_scrollback(grid: &mut Grid, steps: i32) -> bool {
    let previous_offset = grid.scroll_offset;
    let lines = usize::try_from(steps.unsigned_abs().min(MAX_WHEEL_STEPS_PER_EVENT))
        .expect("bounded wheel steps should fit usize");
    if steps.is_positive() {
        grid.scroll_viewport_up(lines);
    } else {
        grid.scroll_viewport_down(lines);
    }
    grid.scroll_offset != previous_offset
}

fn mouse_button_with_modifiers(button: u8, modifiers: ModifiersState) -> u8 {
    mouse_button_with_modifier_flags(
        button,
        modifiers.shift_key(),
        modifiers.alt_key(),
        modifiers.control_key(),
    )
}

fn mouse_button_code_from_winit(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => None,
    }
}

#[cfg(test)]
fn dispatch_mouse_button_with<W>(
    grid: &Mutex<Grid>,
    pointer_position: Option<PhysicalPosition<f64>>,
    cell_dimensions: Option<(u16, u16)>,
    state: ElementState,
    button: MouseButton,
    modifiers: ModifiersState,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    dispatch_mouse_button_inner_with(
        grid,
        pointer_position,
        cell_dimensions,
        state,
        button,
        modifiers,
        None,
        write,
    )
}

#[allow(clippy::too_many_arguments)]
fn dispatch_mouse_button_and_motion_with<W>(
    grid: &Mutex<Grid>,
    pointer_position: Option<PhysicalPosition<f64>>,
    cell_dimensions: Option<(u16, u16)>,
    state: ElementState,
    button: MouseButton,
    modifiers: ModifiersState,
    motion_state: &mut MouseMotionState,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    dispatch_mouse_button_inner_with(
        grid,
        pointer_position,
        cell_dimensions,
        state,
        button,
        modifiers,
        Some(motion_state),
        write,
    )
}

#[allow(clippy::too_many_arguments)]
fn dispatch_mouse_button_inner_with<W>(
    grid: &Mutex<Grid>,
    pointer_position: Option<PhysicalPosition<f64>>,
    cell_dimensions: Option<(u16, u16)>,
    state: ElementState,
    button: MouseButton,
    modifiers: ModifiersState,
    mut motion_state: Option<&mut MouseMotionState>,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let Some(button) = mouse_button_code_from_winit(button) else {
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    };
    let Some((pointer_position, cell_dimensions)) = pointer_position.zip(cell_dimensions) else {
        if let Some(motion_state) = motion_state.as_deref_mut() {
            motion_state.reset();
        }
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    };

    let (mouse_tracking, mouse_encoding, terminal_dimensions) = {
        let grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;
        if grid.mouse_tracking == MouseTracking::None && motion_state.is_none() {
            return Ok(KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            });
        }
        (
            grid.mouse_tracking,
            grid.mouse_encoding,
            terminal_dimensions_from_locked_grid(&grid)?,
        )
    };

    let Some((column, row)) = terminal_cell_at_mouse_button(
        pointer_position,
        cell_dimensions,
        terminal_dimensions,
        state,
    ) else {
        if let Some(motion_state) = motion_state.as_deref_mut() {
            motion_state.reset();
        }
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    };

    if mouse_tracking == MouseTracking::None {
        if let Some(motion_state) = motion_state {
            motion_state.anchor((column, row));
        }
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    }

    let button = mouse_button_with_modifiers(button, modifiers);
    let report = encode_mouse_event(button, column, row, state.is_pressed(), mouse_encoding);
    write(report)?;
    if let Some(motion_state) = motion_state {
        motion_state.anchor((column, row));
    }
    Ok(KeyboardInputOutcome::Forwarded {
        viewport_changed: false,
    })
}

fn dispatch_mouse_motion_with<W>(
    grid: &Mutex<Grid>,
    pointer_position: PhysicalPosition<f64>,
    cell_dimensions: Option<(u16, u16)>,
    active_button: Option<u8>,
    modifiers: ModifiersState,
    last_cell: Option<(usize, usize)>,
    write: W,
) -> Result<MouseMotionDispatchOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let Some(cell_dimensions) = cell_dimensions else {
        return Ok(MouseMotionDispatchOutcome::Ignored);
    };
    let (mouse_tracking, mouse_encoding, terminal_dimensions) = {
        let grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;
        (
            grid.mouse_tracking,
            grid.mouse_encoding,
            terminal_dimensions_from_locked_grid(&grid)?,
        )
    };
    let Some(cell) = terminal_cell_at_mouse_motion(
        pointer_position,
        cell_dimensions,
        terminal_dimensions,
        active_button.is_some(),
    ) else {
        return Ok(MouseMotionDispatchOutcome::Ignored);
    };
    if last_cell == Some(cell) {
        return Ok(MouseMotionDispatchOutcome::Deduplicated);
    }

    let button = match mouse_tracking {
        MouseTracking::None | MouseTracking::Normal => {
            return Ok(MouseMotionDispatchOutcome::Observed(cell));
        }
        MouseTracking::ButtonEvent => {
            let Some(button) = active_button else {
                return Ok(MouseMotionDispatchOutcome::Observed(cell));
            };
            button
        }
        MouseTracking::AnyEvent => active_button.unwrap_or(MOUSE_NO_BUTTON),
    };
    let button = mouse_button_with_modifiers(button | MOUSE_MOTION_FLAG, modifiers);
    let report = encode_mouse_event(button, cell.0, cell.1, true, mouse_encoding);
    match write(report) {
        Ok(()) => Ok(MouseMotionDispatchOutcome::Enqueued(cell)),
        // Motion is coalescible. Keeping the previous cell makes the next event
        // retry while keyboard, button, wheel, IME, and focus input stay lossless.
        Err(TerminalWriteQueueError::Full) => Ok(MouseMotionDispatchOutcome::DroppedFull),
        Err(error @ TerminalWriteQueueError::Disconnected) => Err(error.into()),
    }
}

fn dispatch_mouse_wheel_with<W>(
    grid: &Mutex<Grid>,
    pointer_position: Option<PhysicalPosition<f64>>,
    cell_dimensions: Option<(u16, u16)>,
    delta: MouseScrollDelta,
    phase: TouchPhase,
    modifiers: ModifiersState,
    state: &mut MouseWheelState,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let (steps, route, terminal_dimensions) = {
        let mut grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;
        let route = mouse_wheel_route(&grid);
        if state.last_route.replace(route) != Some(route) || phase == TouchPhase::Started {
            state.line_remainder = 0.0;
        }
        let steps = accumulate_wheel_steps(
            delta,
            cell_dimensions.map(|(_, height)| height),
            &mut state.line_remainder,
        );
        if matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            state.line_remainder = 0.0;
        }
        if steps == 0 {
            return Ok(KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            });
        }

        let terminal_dimensions = match route {
            MouseWheelRoute::Scrollback => {
                let viewport_changed = apply_wheel_scrollback(&mut grid, steps);
                return Ok(KeyboardInputOutcome::Scrollback { viewport_changed });
            }
            MouseWheelRoute::AlternateScroll { .. } => None,
            MouseWheelRoute::Terminal(_) => Some(terminal_dimensions_from_locked_grid(&grid)?),
        };

        (steps, route, terminal_dimensions)
    };

    if let MouseWheelRoute::AlternateScroll {
        application_cursor_keys,
    } = route
    {
        write(encode_alternate_scroll_steps(
            steps,
            application_cursor_keys,
        ))?;
        return Ok(KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        });
    }

    let MouseWheelRoute::Terminal(mouse_encoding) = route else {
        unreachable!("scrollback wheel input returns while the Grid lock is held");
    };
    let terminal_dimensions =
        terminal_dimensions.expect("terminal mouse reporting should snapshot Grid dimensions");

    let Some((column, row)) =
        pointer_position
            .zip(cell_dimensions)
            .and_then(|(position, dimensions)| {
                terminal_cell_at_pointer(position, dimensions, terminal_dimensions)
            })
    else {
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    };

    let button = if steps.is_positive() {
        MOUSE_WHEEL_UP
    } else {
        MOUSE_WHEEL_DOWN
    };
    let button = mouse_button_with_modifiers(button, modifiers);
    let report = encode_mouse_event(button, column, row, true, mouse_encoding);
    let report_count = usize::try_from(steps.unsigned_abs().min(MAX_WHEEL_STEPS_PER_EVENT))
        .expect("bounded wheel report count should fit usize");
    let mut reports = Vec::with_capacity(report.len().saturating_mul(report_count));
    for _ in 0..report_count {
        reports.extend_from_slice(&report);
    }
    write(reports)?;
    Ok(KeyboardInputOutcome::Forwarded {
        viewport_changed: false,
    })
}

fn focus_report_bytes(focused: bool) -> &'static [u8] {
    if focused {
        FOCUS_IN_REPORT
    } else {
        FOCUS_OUT_REPORT
    }
}

fn record_window_focus_change(last_window_focus: &mut Option<bool>, focused: bool) -> bool {
    last_window_focus.replace(focused) != Some(focused)
}

fn dispatch_focus_event_with<W>(
    grid: &Mutex<Grid>,
    focused: bool,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let focus_events = grid
        .lock()
        .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?
        .focus_events;
    if !focus_events {
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    }

    write(focus_report_bytes(focused).to_vec())?;
    Ok(KeyboardInputOutcome::Forwarded {
        viewport_changed: false,
    })
}

fn ime_payload_from_event(event: Ime) -> Option<ImeCommitPayload> {
    match event {
        Ime::Commit(text) => Some(ime_commit_payload(text)),
        // Preedit is provisional UI state. Sending it to the PTY would duplicate
        // every revision before Winit emits the final commit.
        Ime::Enabled | Ime::Preedit(_, _) | Ime::Disabled => None,
    }
}

fn ime_preedit_payload(text: String, cursor: Option<(usize, usize)>) -> ImePreeditPayload {
    ime_preedit_payload_with_limits(
        text,
        cursor,
        MAX_IME_PREEDIT_BYTES,
        MAX_IME_PREEDIT_RENDER_CELLS,
    )
}

fn ime_preedit_payload_with_limits(
    text: String,
    cursor: Option<(usize, usize)>,
    byte_limit: usize,
    render_cell_limit: usize,
) -> ImePreeditPayload {
    if text.is_empty() {
        return ImePreeditPayload::Clear;
    }
    let byte_count = text.len();
    if byte_count > byte_limit {
        return ImePreeditPayload::TooManyBytes {
            byte_count,
            limit: byte_limit,
        };
    }

    let mut cell_count = 0_usize;
    for character in text.chars() {
        let render_cells = character.width().unwrap_or(1).max(1);
        cell_count = cell_count.saturating_add(render_cells);
        if cell_count > render_cell_limit {
            return ImePreeditPayload::TooManyRenderCells {
                cell_count,
                limit: render_cell_limit,
            };
        }
    }

    let cursor = cursor.and_then(|(anchor, caret)| {
        (anchor <= text.len()
            && caret <= text.len()
            && text.is_char_boundary(anchor)
            && text.is_char_boundary(caret))
        .then_some(ImePreeditCursor {
            selection_start: anchor.min(caret),
            selection_end: anchor.max(caret),
            caret,
        })
    });
    ImePreeditPayload::Text(ImePreedit { text, cursor })
}

fn replace_ime_preedit(current: &mut Option<ImePreedit>, next: Option<ImePreedit>) -> bool {
    if *current == next {
        return false;
    }
    *current = next;
    true
}

fn reveal_ime_input_viewport(grid: &Mutex<Grid>) -> Result<bool, GridAccessError> {
    let mut grid = grid.lock().map_err(|_| GridAccessError::Poisoned)?;
    let changed = grid.scroll_offset != 0;
    grid.scroll_to_bottom();
    Ok(changed)
}

fn dispatch_encoded_terminal_input_with<W>(
    grid: &Mutex<Grid>,
    bytes: Option<Vec<u8>>,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let Some(bytes) = bytes else {
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    };
    let viewport_changed = {
        let mut grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;
        let viewport_changed = grid.scroll_offset != 0;
        grid.scroll_to_bottom();
        viewport_changed
    };
    write(bytes)?;
    Ok(KeyboardInputOutcome::Forwarded { viewport_changed })
}

fn apply_scrollback_action(grid: &mut Grid, action: ScrollbackAction) -> bool {
    let previous_offset = grid.scroll_offset;
    let page_lines = grid.rows().saturating_sub(1).max(1);

    match action {
        ScrollbackAction::PageUp => grid.scroll_viewport_up(page_lines),
        ScrollbackAction::PageDown => grid.scroll_viewport_down(page_lines),
        ScrollbackAction::Top => grid.scroll_viewport_up(grid.scrollback_len()),
        ScrollbackAction::Bottom => grid.scroll_to_bottom(),
    }

    grid.scroll_offset != previous_offset
}

fn modifiers_after_focus_change(current: ModifiersState, focused: bool) -> ModifiersState {
    if focused {
        current
    } else {
        ModifiersState::empty()
    }
}

fn snapshot_grid(grid: &Mutex<Grid>) -> Result<GridSnapshot, GridAccessError> {
    let grid = grid.lock().map_err(|_| GridAccessError::Poisoned)?;
    let columns = grid.cols();
    let rows = grid.rows();
    let mut cells = Vec::with_capacity(columns.saturating_mul(rows));

    for row in 0..rows {
        for column in 0..columns {
            cells.push(*grid.visible_cell(row, column));
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

    Ok(GridSnapshot {
        columns,
        rows,
        cells,
        cursor,
        input_cursor: (grid.cursor_row, grid.cursor_col),
        wrap_pending: grid.is_wrap_pending(),
        auto_wrap: grid.auto_wrap,
    })
}

fn snapshot_window_title(grid: &Mutex<Grid>) -> Result<String, GridAccessError> {
    grid.lock()
        .map(|grid| normalized_window_title(grid.title()).into_owned())
        .map_err(|_| GridAccessError::Poisoned)
}

fn ime_cursor_area(
    input_cursor: (usize, usize),
    grid_dimensions: (usize, usize),
    cell_dimensions: (u16, u16),
    surface_dimensions: (u32, u32),
) -> Option<ImeCursorArea> {
    let (columns, rows) = grid_dimensions;
    let last_row = rows.checked_sub(1)?;
    let last_column = columns.checked_sub(1)?;
    if input_cursor.0 > last_row || input_cursor.1 > columns {
        return None;
    }
    let row = input_cursor.0;
    let column = input_cursor.1.min(last_column);
    let cell_size = (u32::from(cell_dimensions.0), u32::from(cell_dimensions.1));
    let (surface_width, surface_height) = surface_dimensions;
    if cell_size.0 == 0 || cell_size.1 == 0 || surface_width == 0 || surface_height == 0 {
        return None;
    }

    let origin = cell_origin(row, column, cell_dimensions)?;
    let desired_x = u32::try_from(origin.0).ok()?;
    let desired_y = u32::try_from(origin.1).ok()?;
    let area_width = cell_size.0.min(surface_width);
    let area_height = cell_size.1.min(surface_height);
    let x = desired_x.min(surface_width.checked_sub(area_width)?);
    let y = desired_y.min(surface_height.checked_sub(area_height)?);

    Some(ImeCursorArea {
        position: PhysicalPosition::new(i32::try_from(x).ok()?, i32::try_from(y).ok()?),
        size: PhysicalSize::new(area_width, area_height),
    })
}

fn sync_ime_cursor_area_with<F>(
    ime_active: bool,
    last_area: &mut Option<ImeCursorArea>,
    next_area: Option<ImeCursorArea>,
    set_area: F,
) where
    F: FnOnce(ImeCursorArea),
{
    if !ime_active {
        return;
    }
    let Some(next_area) = next_area else {
        return;
    };
    if *last_area == Some(next_area) {
        return;
    }

    set_area(next_area);
    *last_area = Some(next_area);
}

#[cfg(test)]
fn layout_ime_preedit(
    preedit: &ImePreedit,
    input_cursor: (usize, usize),
    grid_dimensions: (usize, usize),
) -> ImePreeditLayout {
    layout_ime_preedit_with_mode(preedit, input_cursor, grid_dimensions, true, None)
}

fn layout_ime_preedit_for_snapshot(
    preedit: &ImePreedit,
    snapshot: &GridSnapshot,
) -> ImePreeditLayout {
    layout_ime_preedit_with_mode(
        preedit,
        snapshot.input_cursor,
        (snapshot.columns, snapshot.rows),
        snapshot.auto_wrap,
        Some(snapshot),
    )
}

fn layout_ime_preedit_with_mode(
    preedit: &ImePreedit,
    input_cursor: (usize, usize),
    grid_dimensions: (usize, usize),
    auto_wrap: bool,
    snapshot: Option<&GridSnapshot>,
) -> ImePreeditLayout {
    let (columns, rows) = grid_dimensions;
    if columns == 0 || rows == 0 || input_cursor.0 >= rows || input_cursor.1 > columns {
        return ImePreeditLayout::default();
    }

    let mut layout = ImePreeditLayout::default();
    let mut row = input_cursor.0;
    let mut column = input_cursor.1;
    let mut wrap_pending = snapshot
        .map(|snapshot| snapshot.wrap_pending)
        .unwrap_or(column == columns);
    let mut previous_cell = None;
    let mut last_visible_cell = clamp_ime_layout_position(input_cursor, grid_dimensions);

    for (byte_start, character) in preedit.text.char_indices() {
        let byte_end = byte_start + character.len_utf8();
        let (mut display_character, mut width) = match character.width() {
            Some(width) => (character, u8::try_from(width.min(2)).unwrap_or(2)),
            None => (IME_PREEDIT_REPLACEMENT, 1),
        };
        if usize::from(width) > columns {
            display_character = IME_PREEDIT_REPLACEMENT;
            width = 1;
        }
        let selected = preedit.cursor.is_some_and(|cursor| {
            cursor.selection_start < byte_end && byte_start < cursor.selection_end
        });
        let width_usize = usize::from(width);
        let mut renderable = true;

        let attaches_to_previous = width == 0 && previous_cell.is_some();
        if !attaches_to_previous && (wrap_pending || column >= columns) {
            wrap_pending = false;
            if auto_wrap {
                row = row.saturating_add(1);
                column = 0;
            } else {
                column = column.min(columns - 1);
            }
        }
        if width != 0 {
            if width_usize > columns.saturating_sub(column) {
                if auto_wrap {
                    row = row.saturating_add(1);
                    column = 0;
                } else {
                    renderable = false;
                }
            }
        }
        if preedit
            .cursor
            .is_some_and(|cursor| cursor.caret == byte_start)
        {
            layout.caret_cell =
                ime_layout_caret_position((row, column), grid_dimensions, last_visible_cell);
        }

        if width == 0 {
            let position =
                previous_cell.or_else(|| clamp_ime_layout_position((row, column), grid_dimensions));
            if let Some((glyph_row, glyph_column)) =
                position.filter(|(row, column)| *row < rows && *column < columns)
            {
                let has_cluster_anchor = previous_cell == Some((glyph_row, glyph_column))
                    && layout
                        .glyphs
                        .iter()
                        .any(|glyph| glyph.row == glyph_row && glyph.column == glyph_column);
                if !auto_wrap && !has_cluster_anchor {
                    prepare_ime_no_wrap_write(&mut layout, snapshot, glyph_row, glyph_column, 1);
                }
                layout.glyphs.push(ImePreeditGlyph {
                    character: display_character,
                    byte_start,
                    byte_end,
                    row: glyph_row,
                    column: glyph_column,
                    width,
                    selected,
                });
                last_visible_cell = Some(
                    last_visible_cell
                        .map(|cell| cell.max((glyph_row, glyph_column)))
                        .unwrap_or((glyph_row, glyph_column)),
                );
            }
            continue;
        }

        if !renderable {
            if !auto_wrap {
                previous_cell = None;
            }
            continue;
        }

        if row < rows {
            if !auto_wrap {
                prepare_ime_no_wrap_write(&mut layout, snapshot, row, column, width_usize);
            }
            layout.glyphs.push(ImePreeditGlyph {
                character: display_character,
                byte_start,
                byte_end,
                row,
                column,
                width,
                selected,
            });
            let last_column = column
                .saturating_add(width_usize.saturating_sub(1))
                .min(columns - 1);
            last_visible_cell = Some(
                last_visible_cell
                    .map(|cell| cell.max((row, last_column)))
                    .unwrap_or((row, last_column)),
            );
        }
        previous_cell = Some((row, column));
        column = column.saturating_add(width_usize);
        wrap_pending = column >= columns;
    }
    normalize_ime_preedit_cluster_selection(&mut layout.glyphs);

    if preedit
        .cursor
        .is_some_and(|cursor| cursor.caret == preedit.text.len())
    {
        layout.caret_cell =
            ime_layout_caret_position((row, column), grid_dimensions, last_visible_cell);
    }
    layout
}

fn prepare_ime_no_wrap_write(
    layout: &mut ImePreeditLayout,
    snapshot: Option<&GridSnapshot>,
    row: usize,
    column: usize,
    width: usize,
) {
    let end = column.saturating_add(width);
    layout.cleared_cells.retain(|&(cell_row, cell_column)| {
        cell_row != row || cell_column < column || cell_column >= end
    });

    let mut removed_anchors = Vec::new();
    let mut kept = Vec::with_capacity(layout.glyphs.len());
    for glyph in std::mem::take(&mut layout.glyphs) {
        let glyph_width = usize::from(glyph.width.max(1));
        let glyph_end = glyph.column.saturating_add(glyph_width);
        let overlaps = glyph.row == row && glyph.column < end && column < glyph_end;
        if !overlaps {
            kept.push(glyph);
            continue;
        }

        if glyph.width > 0 {
            removed_anchors.push((glyph.row, glyph.column));
            for cleared_column in glyph.column..glyph_end {
                if cleared_column < column || cleared_column >= end {
                    layout.cleared_cells.push((row, cleared_column));
                }
            }
        }
    }
    kept.retain(|glyph| glyph.width != 0 || !removed_anchors.contains(&(glyph.row, glyph.column)));
    layout.glyphs = kept;

    if let Some(snapshot) = snapshot {
        for target_column in column..end {
            let Some(cell) = snapshot.cell(row, target_column) else {
                continue;
            };
            let partner = if cell.flags.contains(CellFlags::WIDE_CONT) {
                target_column.checked_sub(1)
            } else if cell.flags.contains(CellFlags::WIDE) {
                target_column.checked_add(1)
            } else {
                None
            };
            if let Some(partner) = partner.filter(|partner| *partner < column || *partner >= end) {
                layout.cleared_cells.push((row, partner));
            }
        }
    }

    layout.cleared_cells.sort_unstable();
    layout.cleared_cells.dedup();
}

fn ime_layout_caret_position(
    position: (usize, usize),
    grid_dimensions: (usize, usize),
    last_visible_cell: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    let (columns, rows) = grid_dimensions;
    if position.0 >= rows {
        return last_visible_cell;
    }
    Some((position.0, position.1.min(columns.checked_sub(1)?)))
}

fn normalize_ime_preedit_cluster_selection(glyphs: &mut [ImePreeditGlyph]) {
    let mut selected_anchors = glyphs
        .iter()
        .filter(|glyph| glyph.selected)
        .map(|glyph| (glyph.row, glyph.column))
        .collect::<Vec<_>>();
    selected_anchors.sort_unstable();
    selected_anchors.dedup();

    for glyph in glyphs {
        if selected_anchors
            .binary_search(&(glyph.row, glyph.column))
            .is_ok()
        {
            glyph.selected = true;
        }
    }
}

fn clamp_ime_layout_position(
    position: (usize, usize),
    grid_dimensions: (usize, usize),
) -> Option<(usize, usize)> {
    let (columns, rows) = grid_dimensions;
    Some((
        position.0.min(rows.checked_sub(1)?),
        position.1.min(columns.checked_sub(1)?),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedCellColors {
    foreground: u32,
    background: u32,
}

fn cell_content_is_visible(flags: CellFlags) -> bool {
    !flags.contains(CellFlags::HIDDEN)
}

fn resolve_cell_colors(colors: TerminalColors, cell: &Cell) -> ResolvedCellColors {
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
    colors: TerminalColors,
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

fn draw_grid_snapshot(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: TerminalColors,
    cell_dimensions: (u16, u16),
    snapshot: &GridSnapshot,
    draw_terminal_cursor: bool,
) {
    let cell_size = (u32::from(cell_dimensions.0), u32::from(cell_dimensions.1));

    for row in 0..snapshot.rows {
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
            let resolved = resolve_cell_colors(colors, cell);
            let background_width = if cell.flags.contains(CellFlags::WIDE) {
                cell_size.0.saturating_mul(2)
            } else {
                cell_size.0
            };

            fill_rect(
                frame,
                frame_size,
                origin,
                (background_width, cell_size.1),
                resolved.background,
                u8::MAX,
            );

            if !cell_content_is_visible(cell.flags) {
                continue;
            }

            if cell.c != ' ' && cell.c != '\0' {
                let glyph = atlas.get_or_insert(GlyphKey {
                    c: cell.c,
                    bold: cell.flags.contains(CellFlags::BOLD),
                    italic: cell.flags.contains(CellFlags::ITALIC),
                });
                let source = GlyphSource {
                    pixels: &atlas.pixels,
                    size: (atlas.width, atlas.height),
                    ascent: atlas.ascent,
                };
                draw_cell_glyph(
                    frame,
                    frame_size,
                    source,
                    glyph,
                    origin,
                    resolved.foreground,
                );
            }

            if cell.underline_style != UnderlineStyle::None {
                let underline_color =
                    resolve_cell_underline_color(colors, cell, resolved.foreground);
                draw_cell_underline(
                    frame,
                    frame_size,
                    origin,
                    background_width,
                    cell_size.1,
                    atlas.ascent,
                    cell.underline_style,
                    underline_color,
                );
            }
        }
    }

    if !draw_terminal_cursor {
        return;
    }
    let Some(cursor) = snapshot.cursor else {
        return;
    };
    let Some(origin) = cell_origin(cursor.row, cursor.column, cell_dimensions) else {
        return;
    };
    let Some((cursor_origin, cursor_size)) = cursor_rectangle(cursor.shape, origin, cell_size)
    else {
        return;
    };
    let cursor_background = snapshot
        .rendered_cell(cursor.row, cursor.column)
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
        contrasting_cursor_color(cursor_background),
        CURSOR_ALPHA,
    );
}

fn draw_ime_preedit(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: TerminalColors,
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
        contrasting_cursor_color(background),
        u8::MAX,
    );
}

fn ime_preedit_glyph_colors(
    colors: TerminalColors,
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
    colors: TerminalColors,
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
    let Some(destination_x) = cell_origin.0.checked_add(glyph.bearing_x) else {
        return;
    };
    let Some(baseline) = rounded_f64_i32(f64::from(cell_origin.1) + f64::from(source.ascent))
    else {
        return;
    };
    let Some(destination_y) = baseline.checked_add(glyph.bearing_y) else {
        return;
    };

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
mod tests {
    use super::{
        accumulate_wheel_steps, apply_scrollback_action, apply_terminal_resize, arm_grid_redraw,
        arm_window_title_update, atlas_cell_dimensions, begin_grid_redraw,
        begin_window_title_update, cell_content_is_visible, changed_window_title,
        classify_reader_exit, classify_writer_exit, combine_launch_results,
        configured_terminal_dimensions,
        contrasting_cursor_color, dispatch_encoded_terminal_input_with, dispatch_focus_event_with,
        dispatch_keyboard_input_with, dispatch_mouse_button_and_motion_with,
        dispatch_mouse_button_with, dispatch_mouse_motion_with, dispatch_mouse_wheel_with,
        draw_cell_glyph, draw_cell_underline, draw_grid_snapshot, draw_ime_preedit,
        drawable_dimensions,
        focus_report_bytes, ime_cursor_area, ime_payload_from_event, ime_preedit_background_at,
        ime_preedit_glyph_colors, ime_preedit_payload_with_limits,
        immediate_surface_size_to_reconcile, initial_window_dimensions, is_current_surface_size,
        layout_ime_preedit, layout_ime_preedit_for_snapshot, layout_ime_preedit_with_mode,
        modifiers_after_focus_change, mouse_button_code_from_winit, mouse_button_with_modifiers,
        physical_size_for_terminal, record_window_focus_change, replace_glyph_atlas_for_scale,
        replace_ime_preedit, resize_terminal_with, resolve_cell_colors,
        resolve_cell_underline_color, reveal_ime_input_viewport, rgb_to_xrgb, rounded_i32,
        set_grid_cell_dimensions, snapshot_grid, snapshot_window_title,
        sync_ime_cursor_area_with, terminal_accepts_input, terminal_cell_at_pointer,
        terminal_color_query_value, terminal_dimensions_for_surface, underline_anchor_y,
        GridAccessError,
        ImeCursorArea, ImePreedit, ImePreeditCursor, ImePreeditGlyph, ImePreeditLayout,
        ImePreeditPayload, KeyboardInputError, KeyboardInputOutcome, MouseMotionDispatchOutcome,
        MouseMotionState, MouseWheelState, PointerRouteState, ReaderStatus, ResolvedCellColors,
        SurfaceSizeError, TerminalDimensions, TerminalResizeError, WriterStatus, CURSOR_THICKNESS,
        IME_PREEDIT_REPLACEMENT, MAX_IME_PREEDIT_BYTES, MAX_IME_PREEDIT_RENDER_CELLS,
        MAX_INITIAL_LOGICAL_HEIGHT, MAX_INITIAL_LOGICAL_WIDTH, MAX_REQUESTED_PHYSICAL_HEIGHT,
        MAX_REQUESTED_PHYSICAL_WIDTH, MAX_TERMINAL_COLUMNS, MAX_TERMINAL_ROWS, WINDOW_TITLE,
    };
    use crate::glyph_atlas::{GlyphAtlas, GlyphEntry};
    use crate::grid::cell::{Cell, CellFlags, Color, UnderlineStyle};
    use crate::grid::{CursorShape, Grid, MouseEncoding, MouseTracking};
    use crate::input::linux::{ime_commit_payload, ImeCommitPayload, ScrollbackAction};
    use crate::input::mouse::{
        MouseWheelRoute, MOUSE_WHEEL_UP, MAX_WHEEL_STEPS_PER_EVENT,
    };
    use crate::terminal_colors::TerminalColors;
    use crate::terminal_reader::ReaderExit;
    use crate::terminal_writer::{TerminalWriteQueueError, WriterExit};
    use std::cell::RefCell;
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, TryLockError};
    use winit::dpi::{PhysicalPosition, PhysicalSize};
    use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, TouchPhase};
    use winit::keyboard::ModifiersState;

    const DEFAULT_FOREGROUND: (u8, u8, u8) = (192, 192, 192);
    const DEFAULT_BACKGROUND: (u8, u8, u8) = (26, 26, 46);

    fn test_colors() -> TerminalColors {
        TerminalColors::new(DEFAULT_FOREGROUND, DEFAULT_BACKGROUND)
    }

    fn test_atlas() -> GlyphAtlas {
        GlyphAtlas::new("kokuban-test-font-that-does-not-exist", 14.0, 1.0)
            .expect("system monospace fallback should be available")
    }

    fn accepted_preedit(text: &str, cursor: Option<(usize, usize)>) -> ImePreedit {
        match ime_preedit_payload_with_limits(text.to_string(), cursor, usize::MAX, usize::MAX) {
            ImePreeditPayload::Text(preedit) => preedit,
            payload => panic!("expected accepted IME preedit, got {payload:?}"),
        }
    }

    fn grid_with_scrollback(rows: usize, history_lines: usize) -> Grid {
        let mut grid = Grid::new(8, rows, history_lines + rows);
        grid.cursor_row = rows - 1;
        for _ in 0..history_lines {
            grid.newline();
        }
        assert_eq!(grid.scrollback_len(), history_lines);
        grid
    }

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

    fn render_preedit(
        grid: Grid,
        preedit: &ImePreedit,
        atlas: &mut GlyphAtlas,
    ) -> (Vec<u32>, (u16, u16), ImePreeditLayout) {
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
            .expect("test preedit frame should fit memory");
        let background = rgb_to_xrgb(
            DEFAULT_BACKGROUND.0,
            DEFAULT_BACKGROUND.1,
            DEFAULT_BACKGROUND.2,
        );
        let mut frame = vec![background; frame_len];
        let grid = Mutex::new(grid);
        let snapshot = snapshot_grid(&grid).expect("test grid should not be poisoned");
        let layout = layout_ime_preedit_for_snapshot(preedit, &snapshot);

        draw_grid_snapshot(
            &mut frame,
            frame_size,
            atlas,
            test_colors(),
            cell_dimensions,
            &snapshot,
            false,
        );
        draw_ime_preedit(
            &mut frame,
            frame_size,
            atlas,
            test_colors(),
            cell_dimensions,
            &snapshot,
            &layout,
        );

        (frame, cell_dimensions, layout)
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
    fn initial_dimensions_follow_the_configured_grid() {
        let size = initial_window_dimensions(80, 24);
        assert_eq!(size.width, 800);
        assert_eq!(size.height, 480);
    }

    #[test]
    fn initial_dimensions_never_start_at_zero() {
        let size = initial_window_dimensions(0, 0);
        assert_eq!(size.width, 10);
        assert_eq!(size.height, 20);
    }

    #[test]
    fn initial_placeholder_dimensions_respect_the_logical_size_budget() {
        let size = initial_window_dimensions(u16::MAX, u16::MAX);
        assert_eq!(size.width, MAX_INITIAL_LOGICAL_WIDTH);
        assert_eq!(size.height, MAX_INITIAL_LOGICAL_HEIGHT);
    }

    #[test]
    fn configured_dimensions_are_clamped_before_allocating_grid_or_pty() {
        assert_eq!(
            configured_terminal_dimensions(0, 0),
            TerminalDimensions {
                columns: 1,
                rows: 1,
            }
        );
        assert_eq!(
            configured_terminal_dimensions(u16::MAX, u16::MAX),
            TerminalDimensions {
                columns: MAX_TERMINAL_COLUMNS,
                rows: MAX_TERMINAL_ROWS,
            }
        );
        assert_eq!(
            configured_terminal_dimensions(80, 24),
            TerminalDimensions {
                columns: 80,
                rows: 24,
            }
        );
    }

    #[test]
    fn immediate_surface_sizes_reconcile_and_stale_events_are_filtered() {
        let requested = PhysicalSize::new(800, 480);
        let constrained = PhysicalSize::new(790, 470);

        assert_eq!(immediate_surface_size_to_reconcile(None), None);
        assert_eq!(
            immediate_surface_size_to_reconcile(Some(requested)),
            Some(requested)
        );
        assert_eq!(
            immediate_surface_size_to_reconcile(Some(constrained)),
            Some(constrained)
        );
        assert!(is_current_surface_size(constrained, constrained));
        assert!(!is_current_surface_size(requested, constrained));
    }

    #[test]
    fn unchanged_capped_surface_reconciles_after_cell_metrics_grow() {
        let configured = TerminalDimensions {
            columns: 512,
            rows: 135,
        };
        let current_surface = physical_size_for_terminal(configured, (16, 32))
            .expect("the original cell metrics should produce a valid surface");
        let grown_cell_dimensions = (32, 64);
        let requested = physical_size_for_terminal(configured, grown_cell_dimensions)
            .expect("the grown cell metrics should produce a capped request");
        assert_eq!(
            current_surface,
            PhysicalSize::new(MAX_REQUESTED_PHYSICAL_WIDTH, MAX_REQUESTED_PHYSICAL_HEIGHT)
        );
        assert_eq!(requested, current_surface);
        let target = terminal_dimensions_for_surface(requested, grown_cell_dimensions)
            .expect("the capped physical size should map to a terminal grid");
        let grid = Mutex::new(Grid::new(
            usize::from(configured.columns),
            usize::from(configured.rows),
            0,
        ));
        let pty_size = RefCell::new(None);

        assert!(resize_terminal_with(&grid, target, |columns, rows| {
            pty_size.replace(Some((columns, rows)));
            Ok(())
        })
        .expect("the unchanged capped size should reconcile successfully"));
        assert_eq!(pty_size.into_inner(), Some((256, 67)));
        let grid = grid
            .lock()
            .expect("reconciled grid should remain available");
        assert_eq!((grid.cols(), grid.rows()), (256, 67));
    }

    #[test]
    fn surface_dimensions_use_floor_minimum_and_memory_budget_limits() {
        let cells = |width, height| {
            terminal_dimensions_for_surface(PhysicalSize::new(width, height), (10, 20))
        };

        assert_eq!(cells(0, 480), None);
        assert_eq!(cells(800, 0), None);
        assert_eq!(
            terminal_dimensions_for_surface(PhysicalSize::new(800, 480), (0, 20)),
            None
        );
        assert_eq!(
            cells(9, 19),
            Some(TerminalDimensions {
                columns: 1,
                rows: 1,
            })
        );
        assert_eq!(
            cells(29, 59),
            Some(TerminalDimensions {
                columns: 2,
                rows: 2,
            })
        );
        assert_eq!(
            cells(u32::MAX, u32::MAX),
            Some(TerminalDimensions {
                columns: MAX_TERMINAL_COLUMNS,
                rows: MAX_TERMINAL_ROWS,
            })
        );
    }

    #[test]
    fn physical_grid_size_is_checked_and_dpi_metrics_preserve_rows_and_columns() {
        let maximum = TerminalDimensions {
            columns: u16::MAX,
            rows: u16::MAX,
        };
        let maximum_size = physical_size_for_terminal(maximum, (u16::MAX, u16::MAX))
            .expect("u16 terminal and cell products should fit u32");
        assert_eq!(
            maximum_size,
            PhysicalSize::new(MAX_REQUESTED_PHYSICAL_WIDTH, MAX_REQUESTED_PHYSICAL_HEIGHT,)
        );
        assert_eq!(
            physical_size_for_terminal(
                TerminalDimensions {
                    columns: 0,
                    rows: 24,
                },
                (10, 20),
            ),
            None
        );

        let grid = TerminalDimensions {
            columns: 80,
            rows: 24,
        };
        for cell_dimensions in [(8, 16), (13, 29), (24, 48)] {
            let physical_size = physical_size_for_terminal(grid, cell_dimensions)
                .expect("normal DPI dimensions should fit");
            assert_eq!(
                terminal_dimensions_for_surface(physical_size, cell_dimensions),
                Some(grid)
            );
        }

        let capped = physical_size_for_terminal(
            TerminalDimensions {
                columns: MAX_TERMINAL_COLUMNS,
                rows: MAX_TERMINAL_ROWS,
            },
            (20, 30),
        )
        .expect("large terminal requests should be capped, not overflow");
        assert_eq!(
            capped,
            PhysicalSize::new(MAX_REQUESTED_PHYSICAL_WIDTH, MAX_REQUESTED_PHYSICAL_HEIGHT,)
        );
        assert_ne!(
            terminal_dimensions_for_surface(capped, (20, 30)),
            Some(TerminalDimensions {
                columns: MAX_TERMINAL_COLUMNS,
                rows: MAX_TERMINAL_ROWS,
            }),
            "a capped window request must be reconciled by the following resize event"
        );
    }

    #[test]
    fn resize_transaction_calls_pty_before_grid_commit() {
        let order = RefCell::new(Vec::new());
        let current = TerminalDimensions {
            columns: 80,
            rows: 24,
        };
        let target = TerminalDimensions {
            columns: 100,
            rows: 30,
        };

        assert!(apply_terminal_resize(
            current,
            target,
            |columns, rows| {
                assert_eq!((columns, rows), (100, 30));
                order.borrow_mut().push("pty");
                Ok(())
            },
            |columns, rows| {
                assert_eq!((columns, rows), (100, 30));
                order.borrow_mut().push("grid");
            },
        )
        .expect("fake PTY resize should succeed"));
        assert_eq!(order.into_inner(), ["pty", "grid"]);
    }

    #[test]
    fn unchanged_terminal_dimensions_do_not_call_the_pty() {
        let grid = Mutex::new(Grid::new(80, 24, 0));
        let pty_calls = AtomicUsize::new(0);
        {
            let mut grid = grid.lock().expect("test grid should be available");
            grid.cursor_row = 7;
            grid.cursor_col = 80;
            grid.scroll_top = 3;
            grid.scroll_bottom = 20;
            grid.dirty[0] = false;
            grid.buffer.cell_mut(0, 0).c = 'x';
        }

        assert!(!resize_terminal_with(
            &grid,
            TerminalDimensions {
                columns: 80,
                rows: 24,
            },
            |_, _| {
                pty_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect("unchanged dimensions should be a no-op"));
        assert_eq!(pty_calls.load(Ordering::Relaxed), 0);
        let grid = grid.lock().expect("no-op should keep grid available");
        assert_eq!((grid.cols(), grid.rows()), (80, 24));
        assert_eq!((grid.cursor_row, grid.cursor_col), (7, 80));
        assert_eq!((grid.scroll_top, grid.scroll_bottom), (3, 20));
        assert!(!grid.dirty[0]);
        assert_eq!(grid.buffer.cell(0, 0).c, 'x');
    }

    #[test]
    fn failed_pty_resize_preserves_grid_dimensions() {
        let grid = Mutex::new(Grid::new(80, 24, 0));
        {
            let mut grid = grid.lock().expect("test grid should be available");
            grid.cursor_row = 7;
            grid.cursor_col = 80;
            grid.scroll_top = 3;
            grid.scroll_bottom = 20;
            grid.dirty[0] = false;
            grid.buffer.cell_mut(0, 0).c = 'x';
        }
        let error = resize_terminal_with(
            &grid,
            TerminalDimensions {
                columns: 100,
                rows: 30,
            },
            |_, _| Err(io::Error::from_raw_os_error(5)),
        )
        .expect_err("fake PTY failure should abort the transaction");

        assert!(matches!(error, TerminalResizeError::Pty { .. }));
        assert!(error.to_string().contains("100x30"));
        let TerminalResizeError::Pty { source, .. } = &error else {
            panic!("expected a PTY resize error");
        };
        assert_eq!(source.raw_os_error(), Some(5));
        let grid = grid
            .lock()
            .expect("failed PTY resize should not poison grid");
        assert_eq!((grid.cols(), grid.rows()), (80, 24));
        assert_eq!((grid.cursor_row, grid.cursor_col), (7, 80));
        assert_eq!((grid.scroll_top, grid.scroll_bottom), (3, 20));
        assert!(!grid.dirty[0]);
        assert_eq!(grid.buffer.cell(0, 0).c, 'x');
    }

    #[test]
    fn successful_terminal_resize_updates_pty_and_grid() {
        let grid = Mutex::new(Grid::new(80, 24, 0));
        let pty_size = RefCell::new(None);

        assert!(resize_terminal_with(
            &grid,
            TerminalDimensions {
                columns: 100,
                rows: 30,
            },
            |columns, rows| {
                assert!(matches!(grid.try_lock(), Err(TryLockError::WouldBlock)));
                pty_size.replace(Some((columns, rows)));
                Ok(())
            },
        )
        .expect("fake PTY resize should commit"));

        assert_eq!(pty_size.into_inner(), Some((100, 30)));
        let grid = grid
            .lock()
            .expect("successful resize should keep grid available");
        assert_eq!((grid.cols(), grid.rows()), (100, 30));
    }

    #[test]
    fn keyboard_forwarding_releases_the_grid_before_encoding_and_enqueueing() {
        let mut history = grid_with_scrollback(4, 10);
        history.application_cursor_keys = true;
        history.scroll_viewport_up(history.scrollback_len());
        let grid = Mutex::new(history);
        let written = RefCell::new(Vec::new());

        let outcome = dispatch_keyboard_input_with(
            &grid,
            None,
            |application_cursor_keys| {
                assert!(application_cursor_keys);
                assert!(
                    grid.try_lock().is_ok(),
                    "encoder must run without the Grid lock"
                );
                Some(b"\x1bOA".to_vec())
            },
            |bytes| {
                assert!(
                    grid.try_lock().is_ok(),
                    "queue enqueue must run without the Grid lock"
                );
                written.borrow_mut().extend_from_slice(&bytes);
                Ok(())
            },
        )
        .expect("fake keyboard write should succeed");

        assert_eq!(
            outcome,
            KeyboardInputOutcome::Forwarded {
                viewport_changed: true,
            }
        );
        assert_eq!(written.into_inner(), b"\x1bOA");
        assert_eq!(
            grid.lock()
                .expect("keyboard dispatch should keep the grid available")
                .scroll_offset,
            0
        );
    }

    #[test]
    fn only_final_ime_commits_become_terminal_payloads() {
        let grid = Mutex::new(Grid::new(8, 2, 0));
        let written = RefCell::new(Vec::new());
        {
            let forward_if_committed = |event| {
                let Some(ImeCommitPayload::Bytes(bytes)) = ime_payload_from_event(event) else {
                    return;
                };
                dispatch_encoded_terminal_input_with(&grid, Some(bytes), |bytes| {
                    written.borrow_mut().extend_from_slice(&bytes);
                    Ok(())
                })
                .expect("fresh Grid should accept committed IME text");
            };

            forward_if_committed(Ime::Enabled);
            forward_if_committed(Ime::Preedit("に".to_string(), Some((3, 3))));
            forward_if_committed(Ime::Preedit(String::new(), None));
            forward_if_committed(Ime::Disabled);
            assert!(
                written.borrow().is_empty(),
                "provisional IME events must never reach the terminal writer"
            );

            forward_if_committed(Ime::Commit("日".to_string()));
        }
        assert_eq!(written.into_inner(), "日".as_bytes());
    }

    #[test]
    fn ime_preedit_preserves_utf8_ranges_and_hides_malformed_cursors() {
        let text = "aé日🙂";
        for boundary in [0, 1, 3, 6, 10] {
            let preedit = accepted_preedit(text, Some((boundary, boundary)));
            assert_eq!(
                preedit.cursor,
                Some(ImePreeditCursor {
                    selection_start: boundary,
                    selection_end: boundary,
                    caret: boundary,
                })
            );
        }

        let reverse = accepted_preedit(text, Some((10, 1)));
        assert_eq!(
            reverse.cursor,
            Some(ImePreeditCursor {
                selection_start: 1,
                selection_end: 10,
                caret: 1,
            }),
            "the second directional endpoint remains the active caret"
        );

        for malformed in [Some((2, 3)), Some((1, 7)), Some((0, 11))] {
            let preedit = accepted_preedit(text, malformed);
            assert_eq!(preedit.text, text);
            assert_eq!(preedit.cursor, None);
        }
        assert_eq!(accepted_preedit(text, None).cursor, None);
    }

    #[test]
    fn ime_preedit_limits_are_atomic_and_never_truncate_utf8() {
        assert_eq!(
            ime_preedit_payload_with_limits(String::new(), Some((0, 0)), 0, 0),
            ImePreeditPayload::Clear
        );

        let exact_bytes = "é".repeat(MAX_IME_PREEDIT_BYTES / "é".len());
        assert_eq!(exact_bytes.len(), MAX_IME_PREEDIT_BYTES);
        assert!(matches!(
            ime_preedit_payload_with_limits(
                exact_bytes.clone(),
                None,
                MAX_IME_PREEDIT_BYTES,
                usize::MAX,
            ),
            ImePreeditPayload::Text(ImePreedit { text, .. }) if text == exact_bytes
        ));
        assert_eq!(
            ime_preedit_payload_with_limits(
                format!("{exact_bytes}x"),
                None,
                MAX_IME_PREEDIT_BYTES,
                usize::MAX,
            ),
            ImePreeditPayload::TooManyBytes {
                byte_count: MAX_IME_PREEDIT_BYTES + 1,
                limit: MAX_IME_PREEDIT_BYTES,
            }
        );

        let exact_cells = "x".repeat(MAX_IME_PREEDIT_RENDER_CELLS);
        assert!(matches!(
            ime_preedit_payload_with_limits(
                exact_cells,
                None,
                usize::MAX,
                MAX_IME_PREEDIT_RENDER_CELLS,
            ),
            ImePreeditPayload::Text(_)
        ));
        assert_eq!(
            ime_preedit_payload_with_limits(
                "x".repeat(MAX_IME_PREEDIT_RENDER_CELLS + 1),
                None,
                usize::MAX,
                MAX_IME_PREEDIT_RENDER_CELLS,
            ),
            ImePreeditPayload::TooManyRenderCells {
                cell_count: MAX_IME_PREEDIT_RENDER_CELLS + 1,
                limit: MAX_IME_PREEDIT_RENDER_CELLS,
            }
        );

        let mut visible = Some(accepted_preedit("old", None));
        assert!(replace_ime_preedit(&mut visible, None));
        assert_eq!(
            visible, None,
            "a rejected update must clear the old overlay"
        );
        assert!(!replace_ime_preedit(&mut visible, None));
    }

    #[test]
    fn ime_preedit_layout_handles_wide_combining_selection_and_controls() {
        let preedit = accepted_preedit("A日🙂\u{301}B", Some((10, 4)));
        let layout = layout_ime_preedit(&preedit, (0, 0), (6, 2));

        assert_eq!(
            layout.glyphs,
            vec![
                ImePreeditGlyph {
                    character: 'A',
                    byte_start: 0,
                    byte_end: 1,
                    row: 0,
                    column: 0,
                    width: 1,
                    selected: false,
                },
                ImePreeditGlyph {
                    character: '日',
                    byte_start: 1,
                    byte_end: 4,
                    row: 0,
                    column: 1,
                    width: 2,
                    selected: false,
                },
                ImePreeditGlyph {
                    character: '🙂',
                    byte_start: 4,
                    byte_end: 8,
                    row: 0,
                    column: 3,
                    width: 2,
                    selected: true,
                },
                ImePreeditGlyph {
                    character: '\u{301}',
                    byte_start: 8,
                    byte_end: 10,
                    row: 0,
                    column: 3,
                    width: 0,
                    selected: true,
                },
                ImePreeditGlyph {
                    character: 'B',
                    byte_start: 10,
                    byte_end: 11,
                    row: 0,
                    column: 5,
                    width: 1,
                    selected: false,
                },
            ]
        );
        assert_eq!(layout.caret_cell, Some((0, 3)));

        let controls = accepted_preedit("\0\t\n\r\u{1b}\u{7f}", None);
        let control_layout = layout_ime_preedit(&controls, (0, 0), (8, 1));
        assert_eq!(control_layout.glyphs.len(), 6);
        for (column, glyph) in control_layout.glyphs.iter().enumerate() {
            assert_eq!(glyph.character, IME_PREEDIT_REPLACEMENT);
            assert_eq!(glyph.column, column);
            assert_eq!(glyph.width, 1);
            assert_eq!((glyph.byte_start, glyph.byte_end), (column, column + 1));
        }
    }

    #[test]
    fn ime_preedit_cluster_selection_keeps_base_and_combining_colors_coherent() {
        for cursor in [Some((0, 1)), Some((1, 3))] {
            let layout = layout_ime_preedit(&accepted_preedit("e\u{301}", cursor), (0, 0), (4, 1));
            assert_eq!(layout.glyphs.len(), 2);
            assert!(layout.glyphs.iter().all(|glyph| glyph.selected));
            assert!(layout
                .glyphs
                .iter()
                .all(|glyph| (glyph.row, glyph.column) == (0, 0)));
        }

        let unselected =
            layout_ime_preedit(&accepted_preedit("e\u{301}", Some((3, 3))), (0, 0), (4, 1));
        assert!(unselected.glyphs.iter().all(|glyph| !glyph.selected));

        let wide = layout_ime_preedit(&accepted_preedit("日\u{301}", Some((3, 5))), (0, 0), (4, 1));
        assert_eq!(wide.glyphs[0].width, 2);
        assert_eq!(wide.glyphs[1].width, 0);
        assert!(wide.glyphs.iter().all(|glyph| glyph.selected));
        assert!(wide
            .glyphs
            .iter()
            .all(|glyph| (glyph.row, glyph.column) == (0, 0)));
    }

    #[test]
    fn ime_preedit_layout_wraps_without_half_wide_glyphs_and_clips_at_bottom() {
        let ascii = layout_ime_preedit(&accepted_preedit("abc", Some((3, 3))), (0, 3), (4, 3));
        assert_eq!(
            ascii
                .glyphs
                .iter()
                .map(|glyph| (glyph.character, glyph.row, glyph.column))
                .collect::<Vec<_>>(),
            [('a', 0, 3), ('b', 1, 0), ('c', 1, 1)]
        );
        assert_eq!(ascii.caret_cell, Some((1, 2)));

        let wide = layout_ime_preedit(&accepted_preedit("日", Some((3, 3))), (0, 3), (4, 3));
        assert_eq!((wide.glyphs[0].row, wide.glyphs[0].column), (1, 0));
        assert_eq!(wide.glyphs[0].width, 2);
        assert_eq!(wide.caret_cell, Some((1, 2)));

        let pending = layout_ime_preedit(&accepted_preedit("a", Some((1, 1))), (0, 4), (4, 3));
        assert_eq!((pending.glyphs[0].row, pending.glyphs[0].column), (1, 0));

        let pending_start =
            layout_ime_preedit(&accepted_preedit("a", Some((0, 0))), (0, 4), (4, 3));
        assert_eq!(pending_start.caret_cell, Some((1, 0)));
        assert_eq!(
            (pending_start.glyphs[0].row, pending_start.glyphs[0].column),
            (1, 0),
            "a caret before a wrapped glyph must follow it onto the next line"
        );

        let wide_start = layout_ime_preedit(&accepted_preedit("日", Some((0, 0))), (0, 3), (4, 3));
        assert_eq!(wide_start.caret_cell, Some((1, 0)));
        assert_eq!(
            (wide_start.glyphs[0].row, wide_start.glyphs[0].column),
            (1, 0)
        );

        let clipped = layout_ime_preedit(&accepted_preedit("ab", Some((2, 2))), (1, 3), (4, 2));
        assert_eq!(clipped.glyphs.len(), 1);
        assert_eq!((clipped.glyphs[0].row, clipped.glyphs[0].column), (1, 3));
        assert_eq!(clipped.caret_cell, Some((1, 3)));

        let clipped_wide =
            layout_ime_preedit(&accepted_preedit("a日", Some((4, 4))), (1, 2), (4, 2));
        assert_eq!(clipped_wide.glyphs.len(), 1);
        assert_eq!(
            (clipped_wide.glyphs[0].row, clipped_wide.glyphs[0].column),
            (1, 2)
        );
        assert_eq!(
            clipped_wide.caret_cell,
            Some((1, 2)),
            "an offscreen wide glyph must anchor the caret to the last visible preedit cell"
        );

        let clipped_wide_at_edge =
            layout_ime_preedit(&accepted_preedit("a日", Some((4, 4))), (1, 3), (4, 2));
        assert_eq!(clipped_wide_at_edge.glyphs.len(), 1);
        assert_eq!(
            (
                clipped_wide_at_edge.glyphs[0].row,
                clipped_wide_at_edge.glyphs[0].column
            ),
            (1, 3)
        );
        assert_eq!(clipped_wide_at_edge.caret_cell, Some((1, 3)));

        let one_column = layout_ime_preedit(&accepted_preedit("日", None), (0, 0), (1, 1));
        assert_eq!(one_column.glyphs[0].character, IME_PREEDIT_REPLACEMENT);
        assert_eq!(one_column.glyphs[0].width, 1);

        let leading_combining =
            layout_ime_preedit(&accepted_preedit("\u{301}", None), (0, 2), (4, 1));
        assert_eq!(
            (
                leading_combining.glyphs[0].row,
                leading_combining.glyphs[0].column
            ),
            (0, 2)
        );
        assert_eq!(leading_combining.glyphs[0].width, 0);
    }

    #[test]
    fn ime_preedit_obeys_disabled_auto_wrap_at_the_right_margin() {
        let no_wrap = |text: &str, input_cursor| {
            layout_ime_preedit_with_mode(
                &accepted_preedit(text, Some((text.len(), text.len()))),
                input_cursor,
                (4, 2),
                false,
                None,
            )
        };

        let pending = no_wrap("ab", (0, 4));
        assert_eq!(
            pending
                .glyphs
                .iter()
                .map(|glyph| (glyph.character, glyph.row, glyph.column))
                .collect::<Vec<_>>(),
            [('b', 0, 3)]
        );
        assert_eq!(pending.caret_cell, Some((0, 3)));

        let narrow = no_wrap("abc", (0, 2));
        assert_eq!(
            narrow
                .glyphs
                .iter()
                .map(|glyph| (glyph.character, glyph.row, glyph.column))
                .collect::<Vec<_>>(),
            [('a', 0, 2), ('c', 0, 3)]
        );
        assert_eq!(narrow.caret_cell, Some((0, 3)));

        let ignored_wide = no_wrap("a日", (0, 2));
        assert_eq!(ignored_wide.glyphs.len(), 1);
        assert_eq!(ignored_wide.glyphs[0].character, 'a');
        assert_eq!(ignored_wide.caret_cell, Some((0, 3)));

        let overwritten_wide = no_wrap("日x", (0, 2));
        assert_eq!(overwritten_wide.glyphs.len(), 1);
        assert_eq!(overwritten_wide.glyphs[0].character, 'x');
        assert_eq!(
            (
                overwritten_wide.glyphs[0].row,
                overwritten_wide.glyphs[0].column
            ),
            (0, 3)
        );
        assert_eq!(overwritten_wide.cleared_cells, [(0, 2)]);
        assert_eq!(overwritten_wide.caret_cell, Some((0, 3)));

        let ignored_wide_before_combining = no_wrap("日本\u{301}", (0, 2));
        assert_eq!(ignored_wide_before_combining.glyphs.len(), 1);
        assert_eq!(ignored_wide_before_combining.glyphs[0].character, '\u{301}');
        assert_eq!(
            (
                ignored_wide_before_combining.glyphs[0].row,
                ignored_wide_before_combining.glyphs[0].column
            ),
            (0, 3)
        );
        assert_eq!(ignored_wide_before_combining.cleared_cells, [(0, 2)]);

        let ignored_wide_breaks_the_previous_cluster = no_wrap("ab日\u{301}", (0, 2));
        assert_eq!(
            ignored_wide_breaks_the_previous_cluster
                .glyphs
                .iter()
                .map(|glyph| (glyph.character, glyph.row, glyph.column))
                .collect::<Vec<_>>(),
            [('a', 0, 2), ('\u{301}', 0, 3)]
        );
    }

    #[test]
    fn ime_preedit_uses_pending_wrap_with_the_physical_cursor_after_resize() {
        let resized_snapshot = |auto_wrap| {
            let mut grid = Grid::new(3, 2, 0);
            for character in ['a', 'b', 'c'] {
                grid.put_char(character);
            }
            grid.resize(5, 2);
            grid.set_auto_wrap(auto_wrap);
            snapshot_grid(&Mutex::new(grid)).expect("resized Grid snapshot should succeed")
        };

        let wrapping = resized_snapshot(true);
        assert_eq!(wrapping.input_cursor, (0, 2));
        assert!(wrapping.wrap_pending);
        let wrapping_layout =
            layout_ime_preedit_for_snapshot(&accepted_preedit("X", None), &wrapping);
        assert_eq!(
            wrapping_layout
                .glyphs
                .iter()
                .map(|glyph| (glyph.character, glyph.row, glyph.column))
                .collect::<Vec<_>>(),
            [('X', 1, 0)]
        );

        let overwriting = resized_snapshot(false);
        assert_eq!(overwriting.input_cursor, (0, 2));
        assert!(overwriting.wrap_pending);
        let overwriting_layout =
            layout_ime_preedit_for_snapshot(&accepted_preedit("X", None), &overwriting);
        assert_eq!(
            overwriting_layout
                .glyphs
                .iter()
                .map(|glyph| (glyph.character, glyph.row, glyph.column))
                .collect::<Vec<_>>(),
            [('X', 0, 2)]
        );
    }

    #[test]
    fn ime_preedit_clears_an_underlying_wide_leader_without_auto_wrap() {
        let mut grid = Grid::new(4, 1, 0);
        grid.set_auto_wrap(false);
        grid.set_cursor_pos(0, 2);
        grid.put_char('日');
        let snapshot = snapshot_grid(&Mutex::new(grid)).expect("snapshot should succeed");

        let layout =
            layout_ime_preedit_for_snapshot(&accepted_preedit("x", Some((1, 1))), &snapshot);

        assert!(!snapshot.auto_wrap);
        assert_eq!(layout.glyphs.len(), 1);
        assert_eq!(layout.glyphs[0].character, 'x');
        assert_eq!((layout.glyphs[0].row, layout.glyphs[0].column), (0, 3));
        assert_eq!(layout.cleared_cells, [(0, 2)]);
        assert_eq!(layout.caret_cell, Some((0, 3)));

        let combining =
            layout_ime_preedit_for_snapshot(&accepted_preedit("\u{301}", Some((2, 2))), &snapshot);
        assert_eq!(combining.glyphs.len(), 1);
        assert_eq!(combining.glyphs[0].character, '\u{301}');
        assert_eq!(
            (combining.glyphs[0].row, combining.glyphs[0].column),
            (0, 3)
        );
        assert_eq!(combining.cleared_cells, [(0, 2)]);
    }

    #[test]
    fn ime_preedit_colors_project_a_wide_continuation_to_its_leader() {
        let mut grid = Grid::new(4, 1, 0);
        grid.cursor_visible = false;
        grid.set_auto_wrap(false);
        grid.set_cursor_pos(0, 2);
        grid.fg = Color::Rgb(17, 34, 51);
        grid.bg = Color::Rgb(68, 85, 102);
        grid.flags = CellFlags::REVERSE;
        grid.put_char('日');
        let snapshot = snapshot_grid(&Mutex::new(grid)).expect("snapshot should succeed");
        let layout = layout_ime_preedit_for_snapshot(&accepted_preedit("x", None), &snapshot);
        let glyph = layout
            .glyphs
            .first()
            .expect("preedit glyph should be visible");
        assert_eq!((glyph.row, glyph.column), (0, 3));

        let colors = test_colors();
        let leader_colors = resolve_cell_colors(
            colors,
            snapshot
                .cell(0, 2)
                .expect("wide leader should be available in the snapshot"),
        );
        assert_eq!(
            ime_preedit_glyph_colors(colors, &snapshot, glyph),
            (leader_colors.foreground, leader_colors.background)
        );
        assert_eq!(
            ime_preedit_background_at(colors, &snapshot, &ImePreeditLayout::default(), 0, 3,),
            leader_colors.background
        );
    }

    #[test]
    fn ime_preedit_reveals_live_input_and_replaces_provisional_state() {
        let mut history = grid_with_scrollback(3, 5);
        history.scroll_viewport_up(history.scrollback_len());
        let grid = Mutex::new(history);
        let mut state = None;

        assert!(reveal_ime_input_viewport(&grid).expect("fresh Grid should reveal input"));
        assert_eq!(
            grid.lock()
                .expect("preedit reveal should release the Grid lock")
                .scroll_offset,
            0
        );
        assert!(!reveal_ime_input_viewport(&grid).expect("visible input should be a no-op"));

        let first = accepted_preedit("に", Some((3, 3)));
        let second = accepted_preedit("日本", Some((6, 6)));
        assert!(replace_ime_preedit(&mut state, Some(first)));
        let unchanged = state.clone();
        assert!(!replace_ime_preedit(&mut state, unchanged));
        assert!(replace_ime_preedit(&mut state, Some(second.clone())));
        assert_eq!(state, Some(second));
        assert!(replace_ime_preedit(&mut state, None));
        assert_eq!(state, None);
    }

    #[test]
    fn ime_preedit_render_uses_reverse_selection_underline_and_topmost_caret() {
        let grid = Mutex::new(Grid::new(3, 1, 0));
        let snapshot = snapshot_grid(&grid).expect("fresh Grid snapshot should succeed");
        let preedit = accepted_preedit("  ", Some((0, 1)));
        let layout = layout_ime_preedit(&preedit, snapshot.input_cursor, (3, 1));
        assert_eq!(layout.caret_cell, Some((0, 1)));

        let mut atlas = test_atlas();
        let cell_dimensions =
            atlas_cell_dimensions(&atlas).expect("test atlas should have valid metrics");
        let cell_width = u32::from(cell_dimensions.0);
        let cell_height = u32::from(cell_dimensions.1);
        assert!(cell_width > CURSOR_THICKNESS);
        assert!(cell_height > CURSOR_THICKNESS);
        let frame_size = (cell_width * 3, cell_height);
        let mut frame = vec![
            rgb_to_xrgb(
                DEFAULT_BACKGROUND.0,
                DEFAULT_BACKGROUND.1,
                DEFAULT_BACKGROUND.2,
            );
            usize::try_from(frame_size.0 * frame_size.1)
                .expect("small preedit frame should fit usize")
        ];
        draw_grid_snapshot(
            &mut frame,
            frame_size,
            &mut atlas,
            test_colors(),
            cell_dimensions,
            &snapshot,
            false,
        );
        draw_ime_preedit(
            &mut frame,
            frame_size,
            &mut atlas,
            test_colors(),
            cell_dimensions,
            &snapshot,
            &layout,
        );

        let pixel = |x: u32, y: u32| {
            let index = usize::try_from(y * frame_size.0 + x)
                .expect("small preedit pixel should fit usize");
            frame[index]
        };
        let foreground = rgb_to_xrgb(
            DEFAULT_FOREGROUND.0,
            DEFAULT_FOREGROUND.1,
            DEFAULT_FOREGROUND.2,
        );
        let background = rgb_to_xrgb(
            DEFAULT_BACKGROUND.0,
            DEFAULT_BACKGROUND.1,
            DEFAULT_BACKGROUND.2,
        );
        let middle = cell_width / 2;

        assert_eq!(
            pixel(middle, 0),
            foreground,
            "selection uses reverse background"
        );
        assert_eq!(
            pixel(cell_width + middle, 0),
            background,
            "unselected preedit preserves the terminal background"
        );
        assert_eq!(
            pixel(middle, cell_height - 1),
            background,
            "selected preedit remains underlined with its reversed foreground"
        );
        assert_eq!(
            pixel(cell_width + middle, cell_height - 1),
            foreground,
            "unselected preedit has a visible underline"
        );
        assert_eq!(
            pixel(cell_width, 0),
            contrasting_cursor_color(background),
            "the caret bar is drawn after the overlay"
        );
        assert_eq!(
            ime_cursor_area(
                layout
                    .caret_cell
                    .expect("valid preedit should expose a caret"),
                (snapshot.columns, snapshot.rows),
                cell_dimensions,
                frame_size,
            ),
            Some(ImeCursorArea {
                position: PhysicalPosition::new(
                    i32::try_from(cell_width).expect("cell width should fit i32"),
                    0,
                ),
                size: PhysicalSize::new(cell_width, cell_height),
            })
        );
    }

    #[test]
    fn ime_preedit_render_preserves_contrast_on_reversed_terminal_cells() {
        let mut grid = Grid::new(1, 1, 0);
        grid.cursor_visible = false;
        grid.buffer.cell_mut(0, 0).flags = CellFlags::REVERSE;
        let mut atlas = test_atlas();

        let (frame, cell_dimensions, layout) =
            render_preedit(grid, &accepted_preedit(" ", None), &mut atlas);

        assert_eq!(layout.caret_cell, None);
        let cell_width = usize::from(cell_dimensions.0);
        let cell_height = usize::from(cell_dimensions.1);
        let pixel = |x: usize, y: usize| frame[y * cell_width + x];
        let foreground = rgb_to_xrgb(
            DEFAULT_FOREGROUND.0,
            DEFAULT_FOREGROUND.1,
            DEFAULT_FOREGROUND.2,
        );
        let background = rgb_to_xrgb(
            DEFAULT_BACKGROUND.0,
            DEFAULT_BACKGROUND.1,
            DEFAULT_BACKGROUND.2,
        );

        assert_eq!(pixel(cell_width / 2, 0), foreground);
        assert_eq!(pixel(cell_width / 2, cell_height - 1), background);
        assert_ne!(
            pixel(cell_width / 2, 0),
            pixel(cell_width / 2, cell_height - 1),
            "the underline must contrast with a reversed cell background"
        );
    }

    #[test]
    fn ime_preedit_render_decorates_zero_width_glyphs_without_advancing_them() {
        let combining_grid = || {
            let mut grid = Grid::new(1, 1, 0);
            grid.cursor_visible = false;
            *grid.buffer.cell_mut(0, 0) = Cell {
                fg: Color::Rgb(17, 34, 51),
                bg: Color::Rgb(68, 85, 102),
                ..Cell::default()
            };
            grid
        };
        let mut atlas = test_atlas();

        let (selected_frame, _, selected_layout) = render_preedit(
            combining_grid(),
            &accepted_preedit("\u{301}", Some((0, 2))),
            &mut atlas,
        );
        assert_eq!(selected_layout.glyphs[0].width, 0);
        assert_eq!(selected_layout.glyphs[0].column, 0);
        let selected_background = rgb_to_xrgb(17, 34, 51);
        assert!(
            selected_frame.contains(&selected_background),
            "a selected combining mark needs a one-cell reverse background"
        );

        let (underlined_frame, cell_dimensions, underlined_layout) = render_preedit(
            combining_grid(),
            &accepted_preedit("\u{301}", None),
            &mut atlas,
        );
        assert_eq!(underlined_layout.glyphs[0].width, 0);
        assert_eq!(underlined_layout.glyphs[0].column, 0);
        let cell_width = usize::from(cell_dimensions.0);
        let cell_height = usize::from(cell_dimensions.1);
        let underline_start = (cell_height - 1) * cell_width;
        assert!(
            underlined_frame[underline_start..]
                .iter()
                .all(|&pixel| pixel == rgb_to_xrgb(17, 34, 51)),
            "a zero-width glyph needs a visible one-cell underline"
        );
    }

    #[test]
    fn ime_preedit_render_hides_the_terminal_cursor_before_a_wrapped_wide_glyph() {
        let mut grid = Grid::new(4, 2, 0);
        grid.cursor_row = 0;
        grid.cursor_col = 3;
        grid.cursor_style.shape = CursorShape::Block;
        let mut atlas = test_atlas();

        let (frame, cell_dimensions, layout) =
            render_preedit(grid, &accepted_preedit("日", Some((0, 0))), &mut atlas);

        assert_eq!(layout.caret_cell, Some((1, 0)));
        assert_eq!((layout.glyphs[0].row, layout.glyphs[0].column), (1, 0));
        let cell_width = usize::from(cell_dimensions.0);
        let cell_height = usize::from(cell_dimensions.1);
        let frame_width = cell_width * 4;
        let pixel = |x: usize, y: usize| frame[y * frame_width + x];
        let background = rgb_to_xrgb(
            DEFAULT_BACKGROUND.0,
            DEFAULT_BACKGROUND.1,
            DEFAULT_BACKGROUND.2,
        );

        assert_eq!(
            pixel(3 * cell_width + cell_width / 2, cell_height / 2),
            background,
            "composition must suppress the stale terminal cursor on the previous line"
        );
        assert_ne!(
            pixel(0, cell_height),
            background,
            "the preedit caret should appear beside the wrapped glyph"
        );
    }

    #[test]
    fn ime_cursor_area_tracks_the_physical_terminal_cell_and_dpi_metrics() {
        assert_eq!(
            ime_cursor_area((0, 0), (80, 24), (10, 20), (800, 480)),
            Some(ImeCursorArea {
                position: PhysicalPosition::new(0, 0),
                size: PhysicalSize::new(10, 20),
            })
        );
        assert_eq!(
            ime_cursor_area((2, 3), (80, 24), (10, 20), (800, 480)),
            Some(ImeCursorArea {
                position: PhysicalPosition::new(30, 40),
                size: PhysicalSize::new(10, 20),
            })
        );
        assert_eq!(
            ime_cursor_area((2, 3), (80, 24), (20, 40), (1600, 960)),
            Some(ImeCursorArea {
                position: PhysicalPosition::new(60, 80),
                size: PhysicalSize::new(20, 40),
            })
        );
    }

    #[test]
    fn ime_cursor_area_handles_pending_wrap_clipping_and_invalid_geometry() {
        assert_eq!(
            ime_cursor_area((2, 80), (80, 24), (10, 20), (800, 480)),
            Some(ImeCursorArea {
                position: PhysicalPosition::new(790, 40),
                size: PhysicalSize::new(10, 20),
            }),
            "pending wrap should anchor IME at the last terminal cell"
        );
        assert_eq!(
            ime_cursor_area((0, 0), (1, 1), (10, 20), (5, 7)),
            Some(ImeCursorArea {
                position: PhysicalPosition::new(0, 0),
                size: PhysicalSize::new(5, 7),
            }),
            "an undersized surface should still receive an in-bounds area"
        );
        assert_eq!(
            ime_cursor_area((2, 3), (80, 24), (10, 20), (25, 25)),
            Some(ImeCursorArea {
                position: PhysicalPosition::new(15, 5),
                size: PhysicalSize::new(10, 20),
            }),
            "a cursor beyond a stale surface must clamp the full area in bounds"
        );

        for invalid in [
            ime_cursor_area((0, 0), (0, 24), (10, 20), (800, 480)),
            ime_cursor_area((0, 0), (80, 0), (10, 20), (800, 480)),
            ime_cursor_area((24, 0), (80, 24), (10, 20), (800, 480)),
            ime_cursor_area((0, 81), (80, 24), (10, 20), (800, 480)),
            ime_cursor_area((0, 0), (80, 24), (0, 20), (800, 480)),
            ime_cursor_area((0, 0), (80, 24), (10, 0), (800, 480)),
            ime_cursor_area((0, 0), (80, 24), (10, 20), (0, 480)),
            ime_cursor_area((0, 0), (80, 24), (10, 20), (800, 0)),
        ] {
            assert_eq!(invalid, None);
        }

        let last_fitting_column =
            usize::try_from(i32::MAX).expect("positive i32 should fit usize") / 10;
        assert!(ime_cursor_area(
            (0, last_fitting_column),
            (last_fitting_column + 2, 1),
            (10, 20),
            (u32::MAX, 20),
        )
        .is_some());
        assert_eq!(
            ime_cursor_area(
                (0, last_fitting_column + 1),
                (last_fitting_column + 2, 1),
                (10, 20),
                (u32::MAX, 20),
            ),
            None,
            "IME coordinates beyond i32 must not wrap"
        );
    }

    #[test]
    fn ime_cursor_uses_live_input_position_while_visual_cursor_is_suppressed() {
        let mut history = grid_with_scrollback(3, 5);
        history.cursor_row = 1;
        history.cursor_col = 2;
        history.cursor_visible = false;
        history.scroll_viewport_up(history.scrollback_len());
        let grid = Mutex::new(history);
        let snapshot = snapshot_grid(&grid).expect("history snapshot should succeed");

        assert!(snapshot.cursor.is_none());
        assert_eq!(snapshot.input_cursor, (1, 2));
        let area = ime_cursor_area(
            snapshot.input_cursor,
            (snapshot.columns, snapshot.rows),
            (10, 20),
            (80, 60),
        );
        assert_eq!(
            area,
            Some(ImeCursorArea {
                position: PhysicalPosition::new(20, 20),
                size: PhysicalSize::new(10, 20),
            })
        );

        let mut last_area = None;
        let observed = RefCell::new(None);
        sync_ime_cursor_area_with(true, &mut last_area, area, |area| {
            assert!(
                grid.try_lock().is_ok(),
                "IME window requests must run without the Grid lock"
            );
            observed.replace(Some(area));
        });
        assert_eq!(observed.into_inner(), area);
    }

    #[test]
    fn ime_cursor_area_sync_requires_an_active_session_and_deduplicates_updates() {
        let first = ImeCursorArea {
            position: PhysicalPosition::new(30, 40),
            size: PhysicalSize::new(10, 20),
        };
        let second = ImeCursorArea {
            position: PhysicalPosition::new(40, 40),
            size: PhysicalSize::new(10, 20),
        };
        let calls = RefCell::new(Vec::new());
        let mut last_area = None;

        sync_ime_cursor_area_with(false, &mut last_area, Some(first), |area| {
            calls.borrow_mut().push(area);
        });
        sync_ime_cursor_area_with(true, &mut last_area, Some(first), |area| {
            calls.borrow_mut().push(area);
        });
        sync_ime_cursor_area_with(true, &mut last_area, Some(first), |area| {
            calls.borrow_mut().push(area);
        });
        assert_eq!(calls.borrow().as_slice(), [first]);
        sync_ime_cursor_area_with(true, &mut last_area, Some(second), |area| {
            calls.borrow_mut().push(area);
        });
        sync_ime_cursor_area_with(true, &mut last_area, Some(second), |area| {
            calls.borrow_mut().push(area);
        });
        assert_eq!(calls.borrow().as_slice(), [first, second]);
        assert_eq!(last_area, Some(second));

        // Ime::Enabled invalidates the cache so a recreated input context is synchronized.
        last_area = None;
        sync_ime_cursor_area_with(true, &mut last_area, Some(first), |area| {
            calls.borrow_mut().push(area);
        });
        sync_ime_cursor_area_with(false, &mut last_area, Some(second), |area| {
            calls.borrow_mut().push(area);
        });

        assert_eq!(calls.into_inner(), [first, second, first]);
        assert_eq!(last_area, Some(first));
    }

    #[test]
    fn ime_commit_is_queued_once_as_raw_utf8_outside_the_grid_lock() {
        let mut history = grid_with_scrollback(4, 10);
        history.bracketed_paste = true;
        history.scroll_viewport_up(history.scrollback_len());
        let grid = Mutex::new(history);
        let text = "é日本🙂\0".to_string();
        let expected = text.as_bytes().to_vec();
        let ImeCommitPayload::Bytes(bytes) = ime_commit_payload(text) else {
            panic!("normal IME text should fit the commit budget");
        };
        let writes = RefCell::new(Vec::new());

        let outcome = dispatch_encoded_terminal_input_with(&grid, Some(bytes), |bytes| {
            assert!(
                grid.try_lock().is_ok(),
                "IME enqueue must run without the Grid lock"
            );
            writes.borrow_mut().push(bytes);
            Ok(())
        })
        .expect("fake IME write should succeed");

        assert_eq!(
            outcome,
            KeyboardInputOutcome::Forwarded {
                viewport_changed: true,
            }
        );
        assert_eq!(
            writes.into_inner(),
            vec![expected],
            "IME text must not receive modifier or bracketed-paste framing"
        );
        assert_eq!(
            grid.lock()
                .expect("IME dispatch should keep the grid available")
                .scroll_offset,
            0
        );
    }

    #[test]
    fn scrollback_shortcuts_navigate_locally_without_queueing_input() {
        let grid = Mutex::new(grid_with_scrollback(4, 10));
        let encode_calls = AtomicUsize::new(0);
        let enqueue_calls = AtomicUsize::new(0);
        let dispatch = |action| {
            dispatch_keyboard_input_with(
                &grid,
                Some(action),
                |_| {
                    encode_calls.fetch_add(1, Ordering::Relaxed);
                    Some(b"trap".to_vec())
                },
                |_| {
                    enqueue_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .expect("local scrollback action should succeed")
        };

        assert_eq!(
            dispatch(ScrollbackAction::PageUp),
            KeyboardInputOutcome::Scrollback {
                viewport_changed: true,
            }
        );
        assert_eq!(
            grid.lock()
                .expect("scrollback dispatch should keep the grid available")
                .scroll_offset,
            3
        );
        assert_eq!(
            dispatch(ScrollbackAction::PageUp),
            KeyboardInputOutcome::Scrollback {
                viewport_changed: true,
            }
        );
        assert_eq!(
            grid.lock()
                .expect("scrollback dispatch should keep the grid available")
                .scroll_offset,
            6
        );
        assert_eq!(
            dispatch(ScrollbackAction::PageDown),
            KeyboardInputOutcome::Scrollback {
                viewport_changed: true,
            }
        );
        assert_eq!(
            grid.lock()
                .expect("scrollback dispatch should keep the grid available")
                .scroll_offset,
            3
        );
        assert_eq!(
            dispatch(ScrollbackAction::Top),
            KeyboardInputOutcome::Scrollback {
                viewport_changed: true,
            }
        );
        assert_eq!(
            grid.lock()
                .expect("scrollback dispatch should keep the grid available")
                .scroll_offset,
            10
        );
        assert_eq!(
            dispatch(ScrollbackAction::Top),
            KeyboardInputOutcome::Scrollback {
                viewport_changed: false,
            },
            "a shortcut at its limit must still be consumed"
        );
        assert_eq!(
            dispatch(ScrollbackAction::Bottom),
            KeyboardInputOutcome::Scrollback {
                viewport_changed: true,
            }
        );
        assert_eq!(
            grid.lock()
                .expect("scrollback dispatch should keep the grid available")
                .scroll_offset,
            0
        );
        assert_eq!(encode_calls.load(Ordering::Relaxed), 0);
        assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn one_row_scrollback_pages_by_one_line_without_underflow() {
        let mut grid = grid_with_scrollback(1, 3);

        assert!(apply_scrollback_action(&mut grid, ScrollbackAction::PageUp));
        assert_eq!(grid.scroll_offset, 1);
        assert!(apply_scrollback_action(
            &mut grid,
            ScrollbackAction::PageDown
        ));
        assert_eq!(grid.scroll_offset, 0);
    }

    #[test]
    fn wheel_delta_accumulates_fractional_lines_and_bounds_each_event() {
        let mut remainder = 0.0;

        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::LineDelta(7.0, 0.5),
                Some(20),
                &mut remainder,
            ),
            0,
            "horizontal motion must not affect vertical wheel steps"
        );
        assert_eq!(remainder, 0.5);
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::LineDelta(0.0, 0.5),
                Some(20),
                &mut remainder,
            ),
            1
        );
        assert_eq!(remainder, 0.0);

        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -5.0)),
                Some(20),
                &mut remainder,
            ),
            0
        );
        assert_eq!(remainder, -0.25);
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -15.0)),
                Some(20),
                &mut remainder,
            ),
            -1
        );
        assert_eq!(remainder, 0.0);

        remainder = 0.75;
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 10.0)),
                None,
                &mut remainder,
            ),
            0
        );
        assert_eq!(remainder, 0.0);

        remainder = 0.5;
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::LineDelta(0.0, f32::NAN),
                Some(20),
                &mut remainder,
            ),
            0
        );
        assert_eq!(remainder, 0.0);
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, f64::INFINITY)),
                Some(20),
                &mut remainder,
            ),
            0
        );
        assert_eq!(remainder, 0.0);

        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::LineDelta(0.0, f32::MAX),
                Some(20),
                &mut remainder,
            ),
            i32::try_from(MAX_WHEEL_STEPS_PER_EVENT).unwrap()
        );
        assert_eq!(remainder, 0.0);
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::LineDelta(0.0, -f32::MAX),
                Some(20),
                &mut remainder,
            ),
            -i32::try_from(MAX_WHEEL_STEPS_PER_EVENT).unwrap()
        );
        assert_eq!(remainder, 0.0);
    }

    #[test]
    fn mouse_wheel_modifiers_match_xterm_button_bits() {
        for (modifiers, expected) in [
            (ModifiersState::empty(), MOUSE_WHEEL_UP),
            (ModifiersState::SUPER, MOUSE_WHEEL_UP),
            (ModifiersState::SHIFT, MOUSE_WHEEL_UP | 4),
            (ModifiersState::ALT, MOUSE_WHEEL_UP | 8),
            (ModifiersState::CONTROL, MOUSE_WHEEL_UP | 16),
            (
                ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL,
                MOUSE_WHEEL_UP | 4 | 8 | 16,
            ),
        ] {
            assert_eq!(
                mouse_button_with_modifiers(MOUSE_WHEEL_UP, modifiers),
                expected
            );
        }
    }

    #[test]
    fn maps_only_primary_winit_mouse_buttons_to_xterm_codes() {
        for (button, expected) in [
            (MouseButton::Left, Some(0)),
            (MouseButton::Middle, Some(1)),
            (MouseButton::Right, Some(2)),
            (MouseButton::Back, None),
            (MouseButton::Forward, None),
            (MouseButton::Other(0), None),
            (MouseButton::Other(u16::MAX), None),
        ] {
            assert_eq!(mouse_button_code_from_winit(button), expected);
        }
    }

    #[test]
    fn pointer_route_isolates_mouse_button_positions_by_device() {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = MouseTracking::Normal;
        history.mouse_encoding = MouseEncoding::Sgr;
        history.scroll_viewport_up(2);
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let mut pointer_route = PointerRouteState::<u8>::default();

        pointer_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
        assert_eq!(
            dispatch_mouse_button_with(
                &grid,
                pointer_route.position_for(2),
                Some((10, 20)),
                ElementState::Pressed,
                MouseButton::Left,
                ModifiersState::empty(),
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("a click from another pointer device should be ignored"),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert!(writes.borrow().is_empty());

        pointer_route.cursor_moved(2, PhysicalPosition::new(25.0, 45.0));
        assert_eq!(pointer_route.position_for(1), None);
        assert_eq!(
            dispatch_mouse_button_with(
                &grid,
                pointer_route.position_for(2),
                Some((10, 20)),
                ElementState::Pressed,
                MouseButton::Left,
                ModifiersState::empty(),
                |bytes| {
                    assert!(
                        grid.try_lock().is_ok(),
                        "routed mouse button enqueue must run without the Grid lock"
                    );
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("the active pointer device should use its own position"),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(writes.into_inner(), [b"\x1b[<0;3;3M".to_vec()]);
        assert_eq!(
            grid.lock()
                .expect("routed mouse button grid should remain available")
                .scroll_offset,
            2
        );
    }

    #[test]
    fn pointer_route_keeps_a_forwarded_button_captured_until_its_release() {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = MouseTracking::Normal;
        history.mouse_encoding = MouseEncoding::Sgr;
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let mut pointer_route = PointerRouteState::<u8>::default();
        let first_position = PhysicalPosition::new(15.0, 25.0);
        let second_position = PhysicalPosition::new(75.0, 85.0);

        pointer_route.cursor_moved(1, first_position);
        let press = dispatch_mouse_button_with(
            &grid,
            pointer_route.position_for(1),
            Some((10, 20)),
            ElementState::Pressed,
            MouseButton::Left,
            ModifiersState::empty(),
            |bytes| {
                assert!(grid.try_lock().is_ok());
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("the first device press should be forwarded");
        pointer_route.record_button_dispatch(
            1,
            ElementState::Pressed,
            Some(0),
            matches!(
                press,
                KeyboardInputOutcome::Forwarded {
                    viewport_changed: false
                }
            ),
            Some((10, 20)),
        );
        assert_eq!(pointer_route.forwarded_buttons, 1);

        pointer_route.wheel_state.line_remainder = 0.5;
        pointer_route.cursor_entered(2);
        pointer_route.cursor_moved(2, second_position);
        assert_eq!(pointer_route.active_device, Some(1));
        assert_eq!(pointer_route.position_for(1), Some(first_position));
        assert_eq!(pointer_route.position_for(2), None);
        assert_eq!(pointer_route.select_wheel_device(2), None);
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.5);

        let second_press = dispatch_mouse_button_with(
            &grid,
            pointer_route.position_for(2),
            Some((10, 20)),
            ElementState::Pressed,
            MouseButton::Left,
            ModifiersState::empty(),
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("a press from another device should fail closed during capture");
        assert_eq!(
            second_press,
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        pointer_route.record_button_dispatch(
            2,
            ElementState::Pressed,
            Some(0),
            false,
            Some((10, 20)),
        );
        assert_eq!(pointer_route.forwarded_buttons, 1);

        pointer_route.cursor_left(1);
        assert!(pointer_route.left_while_captured);
        assert_eq!(pointer_route.position_for(1), Some(first_position));
        pointer_route.scale_factor_changed(Some((10, 20)));
        assert_eq!(pointer_route.active_device, Some(1));
        assert_eq!(pointer_route.forwarded_buttons, 1);
        assert_eq!(pointer_route.position_for(1), Some(first_position));
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.0);
        assert!(pointer_route.position_uses_captured_metrics);
        let stale_wheel_position = pointer_route
            .select_wheel_device(1)
            .expect("the captured wheel device should keep its route");
        assert_eq!(stale_wheel_position, None);
        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                stale_wheel_position,
                Some((20, 40)),
                MouseScrollDelta::LineDelta(0.0, 1.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut pointer_route.wheel_state,
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("tracked wheel input should fail closed with stale DPI geometry"),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert_eq!(writes.borrow().len(), 1);

        grid.lock()
            .expect("DPI wheel grid should disable mouse tracking")
            .mouse_tracking = MouseTracking::None;
        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                stale_wheel_position,
                Some((20, 40)),
                MouseScrollDelta::LineDelta(0.0, 1.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut pointer_route.wheel_state,
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("local scrollback should not require fresh DPI pointer geometry"),
            KeyboardInputOutcome::Scrollback {
                viewport_changed: true,
            }
        );
        {
            let mut grid = grid
                .lock()
                .expect("DPI wheel grid should restore mouse tracking");
            grid.mouse_tracking = MouseTracking::Normal;
            grid.mouse_encoding = MouseEncoding::Sgr;
        }
        let release_cell_dimensions = pointer_route.button_cell_dimensions_for(
            1,
            ElementState::Released,
            Some(0),
            Some((20, 40)),
        );
        assert_eq!(release_cell_dimensions, Some((10, 20)));
        let release = dispatch_mouse_button_with(
            &grid,
            pointer_route.position_for(1),
            release_cell_dimensions,
            ElementState::Released,
            MouseButton::Left,
            ModifiersState::empty(),
            |bytes| {
                assert!(grid.try_lock().is_ok());
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("the captured device release should use its retained position");
        pointer_route.record_button_dispatch(
            1,
            ElementState::Released,
            Some(0),
            matches!(
                release,
                KeyboardInputOutcome::Forwarded {
                    viewport_changed: false
                }
            ),
            release_cell_dimensions,
        );

        assert_eq!(
            writes.into_inner(),
            [b"\x1b[<0;2;2M".to_vec(), b"\x1b[<0;2;2m".to_vec()]
        );
        assert_eq!(pointer_route.active_device, None);
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.forwarded_buttons, 0);
        assert!(!pointer_route.left_while_captured);
        assert_eq!(pointer_route.captured_cell_dimensions, None);
        assert!(!pointer_route.position_uses_captured_metrics);

        pointer_route.cursor_moved(2, second_position);
        assert_eq!(pointer_route.active_device, Some(2));
        assert_eq!(pointer_route.position_for(2), Some(second_position));
    }

    #[test]
    fn pointer_route_releases_chords_even_when_the_release_is_not_forwarded() {
        let mut pointer_route = PointerRouteState::<u8>::default();
        pointer_route.cursor_moved(1, PhysicalPosition::new(15.0, 25.0));
        pointer_route.record_button_dispatch(
            1,
            ElementState::Pressed,
            Some(0),
            true,
            Some((10, 20)),
        );
        pointer_route.record_button_dispatch(
            1,
            ElementState::Pressed,
            Some(2),
            true,
            Some((10, 20)),
        );
        assert_eq!(pointer_route.forwarded_buttons, 0b101);

        pointer_route.scale_factor_changed(Some((10, 20)));
        assert_eq!(pointer_route.select_wheel_device(1), Some(None));
        let moved_position = PhysicalPosition::new(45.0, 65.0);
        pointer_route.cursor_moved(1, moved_position);
        assert_eq!(
            pointer_route.select_wheel_device(1),
            Some(Some(moved_position))
        );
        assert!(!pointer_route.position_uses_captured_metrics);

        pointer_route.cursor_left(1);
        pointer_route.record_button_dispatch(
            2,
            ElementState::Released,
            Some(2),
            false,
            Some((10, 20)),
        );
        assert_eq!(
            pointer_route.forwarded_buttons, 0b101,
            "another device must not release a captured button"
        );

        pointer_route.record_button_dispatch(
            1,
            ElementState::Released,
            Some(0),
            false,
            Some((10, 20)),
        );
        assert_eq!(pointer_route.active_device, Some(1));
        assert_eq!(pointer_route.forwarded_buttons, 0b100);
        assert!(pointer_route.left_while_captured);

        pointer_route.record_button_dispatch(
            1,
            ElementState::Released,
            Some(2),
            false,
            Some((10, 20)),
        );
        assert_eq!(pointer_route.active_device, None);
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.forwarded_buttons, 0);
        assert!(!pointer_route.left_while_captured);
    }

    #[test]
    fn pointer_route_drops_stale_dpi_position_after_the_last_release() {
        let mut pointer_route = PointerRouteState::<u8>::default();
        let old_position = PhysicalPosition::new(15.0, 25.0);
        pointer_route.cursor_moved(1, old_position);
        pointer_route.record_button_dispatch(
            1,
            ElementState::Pressed,
            Some(0),
            true,
            Some((10, 20)),
        );
        pointer_route.scale_factor_changed(Some((10, 20)));
        assert_eq!(
            pointer_route.button_cell_dimensions_for(
                1,
                ElementState::Released,
                Some(0),
                Some((20, 40)),
            ),
            Some((10, 20))
        );

        pointer_route.record_button_dispatch(
            1,
            ElementState::Released,
            Some(0),
            false,
            Some((10, 20)),
        );
        assert_eq!(pointer_route.active_device, Some(1));
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.forwarded_buttons, 0);
        assert_eq!(pointer_route.captured_cell_dimensions, None);
        assert!(!pointer_route.position_uses_captured_metrics);

        let new_position = PhysicalPosition::new(35.0, 45.0);
        pointer_route.cursor_moved(1, new_position);
        assert_eq!(pointer_route.position_for(1), Some(new_position));
    }

    #[test]
    fn pointer_route_resets_on_device_changes_matching_leaves_and_scale_changes() {
        let mut pointer_route = PointerRouteState::<u8>::default();
        let second_position = PhysicalPosition::new(25.0, 45.0);

        pointer_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
        pointer_route.wheel_state.line_remainder = 0.5;
        pointer_route.wheel_state.last_route = Some(MouseWheelRoute::Scrollback);
        pointer_route.cursor_entered(2);
        assert_eq!(pointer_route.active_device, Some(2));
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.0);
        assert_eq!(pointer_route.wheel_state.last_route, None);

        pointer_route.cursor_moved(2, second_position);
        pointer_route.wheel_state.line_remainder = 0.75;
        pointer_route.wheel_state.last_route = Some(MouseWheelRoute::Scrollback);
        pointer_route.cursor_entered(2);
        pointer_route.cursor_left(1);
        assert_eq!(pointer_route.active_device, Some(2));
        assert_eq!(pointer_route.position, Some(second_position));
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.75);
        assert_eq!(
            pointer_route.wheel_state.last_route,
            Some(MouseWheelRoute::Scrollback)
        );

        pointer_route.cursor_left(2);
        assert_eq!(pointer_route.active_device, None);
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.0);
        assert_eq!(pointer_route.wheel_state.last_route, None);

        pointer_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
        pointer_route.wheel_state.line_remainder = 0.5;
        pointer_route.wheel_state.last_route = Some(MouseWheelRoute::Scrollback);
        pointer_route.scale_factor_changed(Some((10, 20)));
        assert_eq!(pointer_route.active_device, None);
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.0);
        assert_eq!(pointer_route.wheel_state.last_route, None);
        assert_eq!(pointer_route.forwarded_buttons, 0);
        assert_eq!(pointer_route.captured_cell_dimensions, None);
        assert!(!pointer_route.position_uses_captured_metrics);
    }

    #[test]
    fn pointer_route_preserves_physical_drag_identity_across_leave_and_dpi() {
        let mut route = PointerRouteState::<u8>::default();
        route.cursor_moved(1, PhysicalPosition::new(15.0, 25.0));
        route.motion_state.anchor((2, 2));
        assert!(route.record_physical_button_event(1, ElementState::Pressed, Some(0)));
        assert_eq!(route.active_motion_button(), Some(0));

        route.cursor_left(2);
        assert_eq!(route.active_device, Some(1));
        assert_eq!(route.motion_state.last_cell, Some((2, 2)));
        route.cursor_left(1);
        assert!(route.left_while_captured);
        assert_eq!(route.motion_state.last_cell, None);
        assert!(!route.cursor_moved(2, PhysicalPosition::new(55.0, 65.0)));
        assert!(!route.record_physical_button_event(2, ElementState::Pressed, Some(2)));
        assert!(!route.record_physical_button_event(2, ElementState::Released, Some(0)));
        assert_eq!(route.active_motion_button(), Some(0));

        route.scale_factor_changed(Some((10, 20)));
        assert_eq!(route.active_device, Some(1));
        assert_eq!(route.active_motion_button(), Some(0));
        assert_eq!(route.position, None);
        assert!(!route.position_uses_captured_metrics);
        assert_eq!(route.motion_state.last_cell, None);

        let new_position = PhysicalPosition::new(35.0, 45.0);
        route.cursor_entered(1);
        assert!(route.cursor_moved(1, new_position));
        assert_eq!(route.position_for(1), Some(new_position));
        assert!(!route.left_while_captured);
        assert!(route.record_physical_button_event(1, ElementState::Released, Some(0)));
        route.record_button_dispatch(1, ElementState::Released, Some(0), false, Some((20, 40)));
        assert_eq!(route.active_motion_button(), None);
        assert!(route.cursor_moved(2, PhysicalPosition::new(55.0, 65.0)));
        assert_eq!(route.active_device, Some(2));
    }

    #[test]
    fn pointer_route_keeps_fractional_wheel_steps_per_active_device() {
        let grid = Mutex::new(grid_with_scrollback(4, 10));
        let mut pointer_route = PointerRouteState::<u8>::default();
        let writes = AtomicUsize::new(0);

        pointer_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
        let first_position = pointer_route
            .select_wheel_device(1)
            .expect("the active wheel device should be accepted");
        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                first_position,
                Some((10, 20)),
                MouseScrollDelta::LineDelta(0.0, 0.5),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut pointer_route.wheel_state,
                |_| {
                    writes.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .expect("the first fractional wheel event should be accumulated"),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.5);

        let second_position = pointer_route
            .select_wheel_device(2)
            .expect("a wheel device switch without a capture should be accepted");
        assert_eq!(second_position, None);
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.0);
        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                second_position,
                Some((10, 20)),
                MouseScrollDelta::LineDelta(0.0, 0.5),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut pointer_route.wheel_state,
                |_| {
                    writes.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .expect("a different device should start its own wheel remainder"),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.5);
        assert_eq!(
            grid.lock()
                .expect("fractional wheel grid should remain available")
                .scroll_offset,
            0
        );
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn pointer_route_requires_matching_position_only_for_tracked_wheel_input() {
        let mut tracked_history = grid_with_scrollback(4, 10);
        tracked_history.mouse_tracking = MouseTracking::Normal;
        tracked_history.mouse_encoding = MouseEncoding::Sgr;
        tracked_history.scroll_viewport_up(2);
        let tracked_grid = Mutex::new(tracked_history);
        let tracked_writes = RefCell::new(Vec::new());
        let mut tracked_route = PointerRouteState::<u8>::default();

        tracked_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
        let mismatched_position = tracked_route
            .select_wheel_device(2)
            .expect("a tracked wheel device switch should be accepted");
        assert_eq!(mismatched_position, None);
        assert_eq!(
            dispatch_mouse_wheel_with(
                &tracked_grid,
                mismatched_position,
                Some((10, 20)),
                MouseScrollDelta::LineDelta(0.0, 1.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut tracked_route.wheel_state,
                |bytes| {
                    tracked_writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("tracked wheel input without a matching position should fail closed"),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert!(tracked_writes.borrow().is_empty());
        assert_eq!(
            tracked_grid
                .lock()
                .expect("tracked wheel grid should remain available")
                .scroll_offset,
            2,
            "tracked mismatches must neither enqueue input nor move scrollback"
        );

        tracked_route.cursor_moved(2, PhysicalPosition::new(25.0, 45.0));
        let matching_position = tracked_route
            .select_wheel_device(2)
            .expect("the matching tracked wheel device should be accepted");
        assert_eq!(
            dispatch_mouse_wheel_with(
                &tracked_grid,
                matching_position,
                Some((10, 20)),
                MouseScrollDelta::LineDelta(0.0, 1.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut tracked_route.wheel_state,
                |bytes| {
                    assert!(
                        tracked_grid.try_lock().is_ok(),
                        "routed wheel enqueue must run without the Grid lock"
                    );
                    tracked_writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("matching tracked wheel input should preserve its encoded bytes"),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(tracked_writes.into_inner(), [b"\x1b[<64;3;3M".to_vec()]);

        let local_grid = Mutex::new(grid_with_scrollback(4, 10));
        let local_writes = AtomicUsize::new(0);
        let mut local_route = PointerRouteState::<u8>::default();
        local_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
        let mismatched_position = local_route
            .select_wheel_device(2)
            .expect("local wheel input should select its device without a position");
        assert_eq!(mismatched_position, None);
        assert_eq!(
            dispatch_mouse_wheel_with(
                &local_grid,
                mismatched_position,
                Some((10, 20)),
                MouseScrollDelta::LineDelta(0.0, 1.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut local_route.wheel_state,
                |_| {
                    local_writes.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .expect("local scrollback should not require a pointer position"),
            KeyboardInputOutcome::Scrollback {
                viewport_changed: true,
            }
        );
        assert_eq!(
            local_grid
                .lock()
                .expect("local wheel grid should remain available")
                .scroll_offset,
            1
        );
        assert_eq!(local_writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn every_supported_mouse_tracking_mode_forwards_button_press_and_release() {
        for tracking in [
            MouseTracking::Normal,
            MouseTracking::ButtonEvent,
            MouseTracking::AnyEvent,
        ] {
            let mut history = grid_with_scrollback(4, 10);
            history.mouse_tracking = tracking;
            history.mouse_encoding = MouseEncoding::Sgr;
            let grid = Mutex::new(history);
            let writes = RefCell::new(Vec::new());
            let dispatch = |state| {
                dispatch_mouse_button_with(
                    &grid,
                    Some(PhysicalPosition::new(25.0, 45.0)),
                    Some((10, 20)),
                    state,
                    MouseButton::Left,
                    ModifiersState::empty(),
                    |bytes| {
                        writes.borrow_mut().push(bytes);
                        Ok(())
                    },
                )
                .expect("supported mouse tracking should forward button input")
            };

            assert_eq!(
                dispatch(ElementState::Pressed),
                KeyboardInputOutcome::Forwarded {
                    viewport_changed: false,
                }
            );
            assert_eq!(
                dispatch(ElementState::Released),
                KeyboardInputOutcome::Forwarded {
                    viewport_changed: false,
                }
            );
            assert_eq!(
                writes.into_inner(),
                [b"\x1b[<0;3;3M".to_vec(), b"\x1b[<0;3;3m".to_vec()]
            );
        }
    }

    #[test]
    fn mouse_button_release_clamps_negative_grab_coordinates_to_the_terminal_edge() {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = MouseTracking::Normal;
        history.mouse_encoding = MouseEncoding::Sgr;
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let dispatch = |state, position| {
            dispatch_mouse_button_with(
                &grid,
                Some(position),
                Some((10, 20)),
                state,
                MouseButton::Left,
                ModifiersState::empty(),
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("implicit-grab mouse button input should be handled")
        };

        assert_eq!(
            dispatch(ElementState::Pressed, PhysicalPosition::new(15.0, 25.0)),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(
            dispatch(ElementState::Released, PhysicalPosition::new(-15.0, -25.0),),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(
            writes.into_inner(),
            [b"\x1b[<0;2;2M".to_vec(), b"\x1b[<0;1;1m".to_vec()]
        );
    }

    #[test]
    fn mouse_buttons_encode_modifiers_without_holding_the_grid_lock_or_scrolling() {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = MouseTracking::Normal;
        history.mouse_encoding = MouseEncoding::Sgr;
        history.scroll_viewport_up(2);
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let modified = ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL;
        let dispatch = |state, button| {
            dispatch_mouse_button_with(
                &grid,
                Some(PhysicalPosition::new(25.0, 45.0)),
                Some((10, 20)),
                state,
                button,
                modified,
                |bytes| {
                    assert!(
                        grid.try_lock().is_ok(),
                        "mouse button enqueue must run without the Grid lock"
                    );
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("tracked mouse button should be queued")
        };

        assert_eq!(
            dispatch(ElementState::Pressed, MouseButton::Right),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(
            dispatch(ElementState::Released, MouseButton::Right),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );

        grid.lock()
            .expect("mouse button grid should select legacy encoding")
            .mouse_encoding = MouseEncoding::Default;
        assert_eq!(
            dispatch(ElementState::Pressed, MouseButton::Middle),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(
            dispatch(ElementState::Released, MouseButton::Middle),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );

        assert_eq!(
            writes.into_inner(),
            [
                b"\x1b[<30;3;3M".to_vec(),
                b"\x1b[<30;3;3m".to_vec(),
                vec![0x1b, b'[', b'M', 61, 35, 35],
                vec![0x1b, b'[', b'M', 63, 35, 35],
            ]
        );
        assert_eq!(
            grid.lock()
                .expect("mouse button grid should remain available")
                .scroll_offset,
            2,
            "tracked mouse buttons must preserve scrollback"
        );
    }

    #[test]
    fn mouse_buttons_ignore_disabled_unsupported_or_missing_geometry_and_preserve_queue_errors() {
        let grid = Mutex::new(grid_with_scrollback(4, 10));
        let enqueue_calls = AtomicUsize::new(0);

        assert_eq!(
            dispatch_mouse_button_with(
                &grid,
                Some(PhysicalPosition::new(1.0, 1.0)),
                Some((10, 20)),
                ElementState::Pressed,
                MouseButton::Left,
                ModifiersState::empty(),
                |_| {
                    enqueue_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .expect("disabled mouse tracking should ignore button input"),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );

        {
            let mut grid = grid
                .lock()
                .expect("mouse button grid should enable tracking");
            grid.mouse_tracking = MouseTracking::AnyEvent;
            grid.mouse_encoding = MouseEncoding::Sgr;
        }
        for (position, dimensions, button) in [
            (None, Some((10, 20)), MouseButton::Left),
            (
                Some(PhysicalPosition::new(1.0, 1.0)),
                None,
                MouseButton::Left,
            ),
            (
                Some(PhysicalPosition::new(-1.0, 1.0)),
                Some((10, 20)),
                MouseButton::Left,
            ),
            (
                Some(PhysicalPosition::new(1.0, 1.0)),
                Some((0, 20)),
                MouseButton::Left,
            ),
            (
                Some(PhysicalPosition::new(1.0, 1.0)),
                Some((10, 20)),
                MouseButton::Back,
            ),
            (
                Some(PhysicalPosition::new(1.0, 1.0)),
                Some((10, 20)),
                MouseButton::Forward,
            ),
            (
                Some(PhysicalPosition::new(1.0, 1.0)),
                Some((10, 20)),
                MouseButton::Other(9),
            ),
        ] {
            assert_eq!(
                dispatch_mouse_button_with(
                    &grid,
                    position,
                    dimensions,
                    ElementState::Pressed,
                    button,
                    ModifiersState::empty(),
                    |_| {
                        enqueue_calls.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    },
                )
                .expect("invalid mouse button input should be ignored"),
                KeyboardInputOutcome::Ignored {
                    viewport_changed: false,
                }
            );
        }
        assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

        for queue_error in [
            TerminalWriteQueueError::Full,
            TerminalWriteQueueError::Disconnected,
        ] {
            let error = dispatch_mouse_button_with(
                &grid,
                Some(PhysicalPosition::new(1.0, 1.0)),
                Some((10, 20)),
                ElementState::Released,
                MouseButton::Left,
                ModifiersState::empty(),
                |_| {
                    assert!(
                        grid.try_lock().is_ok(),
                        "mouse button queue errors must occur outside the Grid lock"
                    );
                    Err(queue_error)
                },
            )
            .expect_err("mouse button queue failures should be preserved");
            assert!(matches!(
                error,
                KeyboardInputError::Queue(actual) if actual == queue_error
            ));
        }
    }

    #[test]
    fn mouse_motion_respects_xterm_tracking_modes() {
        let cases = [
            (
                MouseTracking::None,
                None,
                MouseMotionDispatchOutcome::Observed((3, 3)),
                None,
            ),
            (
                MouseTracking::Normal,
                Some(0),
                MouseMotionDispatchOutcome::Observed((3, 3)),
                None,
            ),
            (
                MouseTracking::ButtonEvent,
                None,
                MouseMotionDispatchOutcome::Observed((3, 3)),
                None,
            ),
            (
                MouseTracking::ButtonEvent,
                Some(0),
                MouseMotionDispatchOutcome::Enqueued((3, 3)),
                Some(b"\x1b[<32;3;3M".to_vec()),
            ),
            (
                MouseTracking::AnyEvent,
                None,
                MouseMotionDispatchOutcome::Enqueued((3, 3)),
                Some(b"\x1b[<35;3;3M".to_vec()),
            ),
            (
                MouseTracking::AnyEvent,
                Some(2),
                MouseMotionDispatchOutcome::Enqueued((3, 3)),
                Some(b"\x1b[<34;3;3M".to_vec()),
            ),
        ];

        for (tracking, active_button, expected_outcome, expected_write) in cases {
            let mut history = grid_with_scrollback(4, 10);
            history.mouse_tracking = tracking;
            history.mouse_encoding = MouseEncoding::Sgr;
            history.scroll_viewport_up(2);
            let grid = Mutex::new(history);
            let writes = RefCell::new(Vec::new());
            let outcome = dispatch_mouse_motion_with(
                &grid,
                PhysicalPosition::new(25.0, 45.0),
                Some((10, 20)),
                active_button,
                ModifiersState::empty(),
                None,
                |bytes| {
                    assert!(
                        grid.try_lock().is_ok(),
                        "mouse motion enqueue must run without the Grid lock"
                    );
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("supported mouse tracking mode should be handled");

            assert_eq!(outcome, expected_outcome, "tracking mode {tracking:?}");
            assert_eq!(
                writes.into_inner(),
                expected_write.into_iter().collect::<Vec<_>>()
            );
            assert_eq!(
                grid.lock()
                    .expect("mouse motion grid should remain available")
                    .scroll_offset,
                2,
                "mouse motion must preserve scrollback"
            );
        }
    }

    #[test]
    fn mouse_motion_encodes_buttons_modifiers_and_both_encodings() {
        let cases = [
            (
                MouseEncoding::Sgr,
                Some(0),
                ModifiersState::empty(),
                b"\x1b[<32;3;3M".to_vec(),
            ),
            (
                MouseEncoding::Sgr,
                Some(1),
                ModifiersState::empty(),
                b"\x1b[<33;3;3M".to_vec(),
            ),
            (
                MouseEncoding::Sgr,
                Some(2),
                ModifiersState::empty(),
                b"\x1b[<34;3;3M".to_vec(),
            ),
            (
                MouseEncoding::Sgr,
                None,
                ModifiersState::empty(),
                b"\x1b[<35;3;3M".to_vec(),
            ),
            (
                MouseEncoding::Sgr,
                Some(0),
                ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL,
                b"\x1b[<60;3;3M".to_vec(),
            ),
            (
                MouseEncoding::Sgr,
                Some(0),
                ModifiersState::SUPER,
                b"\x1b[<32;3;3M".to_vec(),
            ),
            (
                MouseEncoding::Default,
                Some(0),
                ModifiersState::empty(),
                vec![0x1b, b'[', b'M', 64, 35, 35],
            ),
            (
                MouseEncoding::Default,
                Some(1),
                ModifiersState::empty(),
                vec![0x1b, b'[', b'M', 65, 35, 35],
            ),
            (
                MouseEncoding::Default,
                Some(2),
                ModifiersState::empty(),
                vec![0x1b, b'[', b'M', 66, 35, 35],
            ),
            (
                MouseEncoding::Default,
                None,
                ModifiersState::empty(),
                vec![0x1b, b'[', b'M', 67, 35, 35],
            ),
            (
                MouseEncoding::Default,
                Some(0),
                ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL,
                vec![0x1b, b'[', b'M', 92, 35, 35],
            ),
        ];

        for (encoding, active_button, modifiers, expected) in cases {
            let mut history = grid_with_scrollback(4, 10);
            history.mouse_tracking = MouseTracking::AnyEvent;
            history.mouse_encoding = encoding;
            let grid = Mutex::new(history);
            let writes = RefCell::new(Vec::new());

            assert_eq!(
                dispatch_mouse_motion_with(
                    &grid,
                    PhysicalPosition::new(25.0, 45.0),
                    Some((10, 20)),
                    active_button,
                    modifiers,
                    None,
                    |bytes| {
                        writes.borrow_mut().push(bytes);
                        Ok(())
                    },
                )
                .expect("mouse motion should encode"),
                MouseMotionDispatchOutcome::Enqueued((3, 3))
            );
            assert_eq!(writes.into_inner(), [expected]);
        }
    }

    #[test]
    fn physical_mouse_buttons_survive_tracking_transitions_and_follow_chord_order() {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_encoding = MouseEncoding::Sgr;
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let mut route = PointerRouteState::<u8>::default();
        assert!(route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0)));
        assert!(route.record_physical_button_event(1, ElementState::Pressed, Some(0)));
        assert_eq!(route.active_motion_button(), Some(0));
        assert!(!route.cursor_moved(2, PhysicalPosition::new(15.0, 25.0)));

        grid.lock()
            .expect("motion grid should enable button-event tracking")
            .mouse_tracking = MouseTracking::ButtonEvent;
        for (position, expected_button) in [
            (PhysicalPosition::new(15.0, 25.0), 32),
            (PhysicalPosition::new(25.0, 25.0), 34),
            (PhysicalPosition::new(35.0, 25.0), 33),
            (PhysicalPosition::new(45.0, 25.0), 34),
            (PhysicalPosition::new(55.0, 25.0), 32),
        ] {
            if expected_button == 34 && route.active_motion_button() == Some(0) {
                assert!(route.record_physical_button_event(1, ElementState::Pressed, Some(2),));
            } else if expected_button == 33 {
                assert!(route.record_physical_button_event(1, ElementState::Pressed, Some(1),));
            } else if expected_button == 34 && route.active_motion_button() == Some(1) {
                assert!(route.record_physical_button_event(1, ElementState::Released, Some(1),));
            } else if expected_button == 32 && route.active_motion_button() == Some(2) {
                assert!(route.record_physical_button_event(1, ElementState::Released, Some(2),));
            }
            assert_eq!(
                route.active_motion_button().map(|button| button | 32),
                Some(expected_button)
            );
            let outcome = dispatch_mouse_motion_with(
                &grid,
                position,
                Some((10, 20)),
                route.active_motion_button(),
                ModifiersState::empty(),
                route.motion_state.last_cell,
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("ordered chord motion should be queued");
            let MouseMotionDispatchOutcome::Enqueued(cell) = outcome else {
                panic!("expected an enqueued chord motion, got {outcome:?}");
            };
            route.motion_state.anchor(cell);
        }
        assert!(route.record_physical_button_event(1, ElementState::Released, Some(0)));
        assert_eq!(route.active_motion_button(), None);
        assert_eq!(
            dispatch_mouse_motion_with(
                &grid,
                PhysicalPosition::new(65.0, 25.0),
                Some((10, 20)),
                route.active_motion_button(),
                ModifiersState::empty(),
                route.motion_state.last_cell,
                |_| panic!("1002 without a held button must not enqueue"),
            )
            .expect("button-event hover should be observed"),
            MouseMotionDispatchOutcome::Observed((7, 2))
        );
        assert_eq!(
            writes.into_inner(),
            [
                b"\x1b[<32;2;2M".to_vec(),
                b"\x1b[<34;3;2M".to_vec(),
                b"\x1b[<33;4;2M".to_vec(),
                b"\x1b[<34;5;2M".to_vec(),
                b"\x1b[<32;6;2M".to_vec(),
            ]
        );
    }

    #[test]
    fn mouse_motion_deduplicates_only_committed_cells_and_uses_current_context_next() {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = MouseTracking::AnyEvent;
        history.mouse_encoding = MouseEncoding::Sgr;
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let first_cell = PhysicalPosition::new(25.0, 45.0);
        let second_cell = PhysicalPosition::new(35.0, 45.0);
        let mut motion_state = MouseMotionState::default();

        let dropped = dispatch_mouse_motion_with(
            &grid,
            first_cell,
            Some((10, 20)),
            None,
            ModifiersState::empty(),
            motion_state.last_cell,
            |_| Err(TerminalWriteQueueError::Full),
        )
        .expect("a full queue should drop only mouse motion");
        assert_eq!(dropped, MouseMotionDispatchOutcome::DroppedFull);
        motion_state.record_dispatch(dropped);
        assert_eq!(
            motion_state.last_cell, None,
            "the production reducer must keep a dropped motion retryable"
        );

        let outcome = dispatch_mouse_motion_with(
            &grid,
            first_cell,
            Some((10, 20)),
            None,
            ModifiersState::empty(),
            motion_state.last_cell,
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("the same cell should retry after backpressure");
        assert_eq!(outcome, MouseMotionDispatchOutcome::Enqueued((3, 3)));
        motion_state.record_dispatch(outcome);
        assert_eq!(motion_state.last_cell, Some((3, 3)));

        grid.lock()
            .expect("motion grid should switch encoding")
            .mouse_encoding = MouseEncoding::Default;
        assert_eq!(
            dispatch_mouse_motion_with(
                &grid,
                first_cell,
                Some((10, 20)),
                None,
                ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL,
                motion_state.last_cell,
                |_| panic!("context changes in the same cell must stay deduplicated"),
            )
            .expect("same-cell context change should be handled"),
            MouseMotionDispatchOutcome::Deduplicated
        );

        let modified = ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL;
        let outcome = dispatch_mouse_motion_with(
            &grid,
            second_cell,
            Some((10, 20)),
            None,
            modified,
            motion_state.last_cell,
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("the next cell should use the current encoding and modifiers");
        assert_eq!(outcome, MouseMotionDispatchOutcome::Enqueued((4, 3)));
        motion_state.record_dispatch(outcome);

        assert_eq!(
            dispatch_mouse_motion_with(
                &grid,
                first_cell,
                Some((10, 20)),
                None,
                modified,
                motion_state.last_cell,
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("returning to an earlier cell should report again"),
            MouseMotionDispatchOutcome::Enqueued((3, 3))
        );
        assert_eq!(
            writes.into_inner(),
            [
                b"\x1b[<35;3;3M".to_vec(),
                vec![0x1b, b'[', b'M', 95, 36, 35],
                vec![0x1b, b'[', b'M', 95, 35, 35],
            ]
        );
    }

    #[test]
    fn mouse_button_reports_anchor_motion_without_hiding_button_queue_failures() {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = MouseTracking::AnyEvent;
        history.mouse_encoding = MouseEncoding::Sgr;
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let mut motion_state = MouseMotionState::default();

        assert_eq!(
            dispatch_mouse_button_and_motion_with(
                &grid,
                Some(PhysicalPosition::new(25.0, 45.0)),
                Some((10, 20)),
                ElementState::Pressed,
                MouseButton::Left,
                ModifiersState::empty(),
                &mut motion_state,
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("button press should anchor motion"),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(motion_state.last_cell, Some((3, 3)));
        assert_eq!(
            dispatch_mouse_motion_with(
                &grid,
                PhysicalPosition::new(29.0, 49.0),
                Some((10, 20)),
                Some(0),
                ModifiersState::empty(),
                motion_state.last_cell,
                |_| panic!("same-cell motion after a button press must be deduplicated"),
            )
            .expect("same-cell motion should be handled"),
            MouseMotionDispatchOutcome::Deduplicated
        );

        motion_state.reset();
        let error = dispatch_mouse_button_and_motion_with(
            &grid,
            Some(PhysicalPosition::new(25.0, 45.0)),
            Some((10, 20)),
            ElementState::Pressed,
            MouseButton::Left,
            ModifiersState::empty(),
            &mut motion_state,
            |_| Err(TerminalWriteQueueError::Full),
        )
        .expect_err("button queue backpressure must remain fatal");
        assert!(matches!(
            error,
            KeyboardInputError::Queue(TerminalWriteQueueError::Full)
        ));
        assert_eq!(
            motion_state.last_cell, None,
            "a failed button report must not anchor motion"
        );
        assert_eq!(writes.into_inner(), [b"\x1b[<0;3;3M".to_vec()]);
    }

    #[test]
    fn mouse_motion_clamps_only_finite_drags_and_propagates_disconnects() {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = MouseTracking::AnyEvent;
        history.mouse_encoding = MouseEncoding::Sgr;
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());

        assert_eq!(
            dispatch_mouse_motion_with(
                &grid,
                PhysicalPosition::new(-25.0, -45.0),
                Some((10, 20)),
                Some(0),
                ModifiersState::empty(),
                None,
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("finite implicit-grab motion should clamp"),
            MouseMotionDispatchOutcome::Enqueued((1, 1))
        );
        for (position, button) in [
            (PhysicalPosition::new(-1.0, 1.0), None),
            (PhysicalPosition::new(f64::NAN, 1.0), Some(0)),
            (PhysicalPosition::new(1.0, f64::INFINITY), Some(0)),
        ] {
            assert_eq!(
                dispatch_mouse_motion_with(
                    &grid,
                    position,
                    Some((10, 20)),
                    button,
                    ModifiersState::empty(),
                    None,
                    |_| panic!("invalid motion geometry must not enqueue"),
                )
                .expect("invalid motion geometry should be ignored"),
                MouseMotionDispatchOutcome::Ignored
            );
        }
        assert_eq!(
            dispatch_mouse_motion_with(
                &grid,
                PhysicalPosition::new(f64::MAX, f64::MAX),
                Some((10, 20)),
                None,
                ModifiersState::empty(),
                None,
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("positive motion beyond the surface should clamp"),
            MouseMotionDispatchOutcome::Enqueued((8, 4))
        );

        let error = dispatch_mouse_motion_with(
            &grid,
            PhysicalPosition::new(15.0, 25.0),
            Some((10, 20)),
            None,
            ModifiersState::empty(),
            None,
            |_| Err(TerminalWriteQueueError::Disconnected),
        )
        .expect_err("a disconnected motion queue must remain fatal");
        assert!(matches!(
            error,
            KeyboardInputError::Queue(TerminalWriteQueueError::Disconnected)
        ));
        assert_eq!(
            writes.into_inner(),
            [b"\x1b[<32;1;1M".to_vec(), b"\x1b[<35;8;4M".to_vec(),]
        );
    }

    #[test]
    fn wheel_remainder_resets_between_gestures_routes_and_pointer_contexts() {
        let grid = Mutex::new(grid_with_scrollback(4, 10));
        let writes = RefCell::new(Vec::new());
        let mut state = MouseWheelState::default();
        let dispatch = |delta, phase, state: &mut MouseWheelState| {
            dispatch_mouse_wheel_with(
                &grid,
                Some(PhysicalPosition::new(1.0, 1.0)),
                Some((10, 20)),
                MouseScrollDelta::LineDelta(0.0, delta),
                phase,
                ModifiersState::empty(),
                state,
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("wheel gesture dispatch should succeed")
        };

        assert_eq!(
            dispatch(0.5, TouchPhase::Started, &mut state),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert_eq!(state.line_remainder, 0.5);
        assert_eq!(
            dispatch(0.0, TouchPhase::Ended, &mut state),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert_eq!(state.line_remainder, 0.0);

        assert_eq!(
            dispatch(0.5, TouchPhase::Moved, &mut state),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        {
            let mut grid = grid
                .lock()
                .expect("wheel grid should switch to SGR terminal reporting");
            grid.mouse_tracking = MouseTracking::Normal;
            grid.mouse_encoding = MouseEncoding::Sgr;
        }
        assert_eq!(
            dispatch(0.5, TouchPhase::Moved, &mut state),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            },
            "a local-scroll remainder must not become terminal input"
        );
        assert_eq!(state.line_remainder, 0.5);
        assert_eq!(
            dispatch(0.5, TouchPhase::Moved, &mut state),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );

        assert_eq!(
            dispatch(0.25, TouchPhase::Started, &mut state),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert_eq!(state.line_remainder, 0.25);
        assert_eq!(
            dispatch(0.25, TouchPhase::Cancelled, &mut state),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert_eq!(state.line_remainder, 0.0);
        assert_eq!(writes.into_inner(), [b"\x1b[<64;1;1M".to_vec()]);

        state.line_remainder = 0.75;
        state.reset();
        assert_eq!(state.line_remainder, 0.0);
        assert_eq!(state.last_route, None);
    }

    #[test]
    fn every_supported_mouse_tracking_mode_forwards_wheel_reports() {
        for tracking in [
            MouseTracking::Normal,
            MouseTracking::ButtonEvent,
            MouseTracking::AnyEvent,
        ] {
            let mut history = grid_with_scrollback(4, 10);
            history.alternate_scroll = true;
            history.enter_alt_screen();
            history.mouse_tracking = tracking;
            history.mouse_encoding = MouseEncoding::Sgr;
            let grid = Mutex::new(history);
            let writes = RefCell::new(Vec::new());
            let mut state = MouseWheelState::default();

            assert_eq!(
                dispatch_mouse_wheel_with(
                    &grid,
                    Some(PhysicalPosition::new(1.0, 1.0)),
                    Some((10, 20)),
                    MouseScrollDelta::LineDelta(0.0, 1.0),
                    TouchPhase::Moved,
                    ModifiersState::empty(),
                    &mut state,
                    |bytes| {
                        writes.borrow_mut().push(bytes);
                        Ok(())
                    },
                )
                .expect("supported mouse tracking should forward wheel input"),
                KeyboardInputOutcome::Forwarded {
                    viewport_changed: false,
                }
            );
            assert_eq!(writes.into_inner(), [b"\x1b[<64;1;1M".to_vec()]);
        }
    }

    #[test]
    fn alternate_scroll_forwards_unmodified_cursor_keys_without_pointer_geometry() {
        let mut history = grid_with_scrollback(4, 10);
        history.alternate_scroll = true;
        history.enter_alt_screen();
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let mut state = MouseWheelState::default();
        let modified = ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL;

        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                None,
                None,
                MouseScrollDelta::LineDelta(0.0, 2.0),
                TouchPhase::Moved,
                modified,
                &mut state,
                |bytes| {
                    assert!(
                        grid.try_lock().is_ok(),
                        "alternate scroll must enqueue outside the Grid lock"
                    );
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("alternate scroll should forward normal cursor keys"),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );

        grid.lock()
            .expect("alternate scroll grid should enable application cursor keys")
            .application_cursor_keys = true;
        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                None,
                None,
                MouseScrollDelta::LineDelta(0.0, -2.0),
                TouchPhase::Moved,
                modified,
                &mut state,
                |bytes| {
                    assert!(grid.try_lock().is_ok());
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("alternate scroll should honor application cursor key mode"),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );

        assert_eq!(
            writes.into_inner(),
            [b"\x1b[A\x1b[A".to_vec(), b"\x1bOB\x1bOB".to_vec()]
        );

        for queue_error in [
            TerminalWriteQueueError::Full,
            TerminalWriteQueueError::Disconnected,
        ] {
            let error = dispatch_mouse_wheel_with(
                &grid,
                None,
                None,
                MouseScrollDelta::LineDelta(0.0, 1.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut state,
                |_| {
                    assert!(grid.try_lock().is_ok());
                    Err(queue_error)
                },
            )
            .expect_err("alternate scroll queue failures should be preserved");
            assert!(matches!(
                error,
                KeyboardInputError::Queue(actual) if actual == queue_error
            ));
        }
    }

    #[test]
    fn pointer_positions_map_to_one_based_clamped_terminal_cells() {
        let terminal = TerminalDimensions {
            columns: 80,
            rows: 24,
        };

        assert_eq!(
            terminal_cell_at_pointer(PhysicalPosition::new(0.0, 0.0), (10, 20), terminal),
            Some((1, 1))
        );
        assert_eq!(
            terminal_cell_at_pointer(PhysicalPosition::new(9.999, 19.999), (10, 20), terminal,),
            Some((1, 1))
        );
        assert_eq!(
            terminal_cell_at_pointer(PhysicalPosition::new(10.0, 20.0), (10, 20), terminal),
            Some((2, 2))
        );
        assert_eq!(
            terminal_cell_at_pointer(
                PhysicalPosition::new(f64::MAX, f64::MAX),
                (10, 20),
                terminal,
            ),
            Some((80, 24)),
            "positions beyond a stale surface must clamp to the terminal edge"
        );

        for invalid in [
            terminal_cell_at_pointer(PhysicalPosition::new(-0.1, 0.0), (10, 20), terminal),
            terminal_cell_at_pointer(PhysicalPosition::new(0.0, -0.1), (10, 20), terminal),
            terminal_cell_at_pointer(PhysicalPosition::new(f64::NAN, 0.0), (10, 20), terminal),
            terminal_cell_at_pointer(
                PhysicalPosition::new(0.0, f64::INFINITY),
                (10, 20),
                terminal,
            ),
            terminal_cell_at_pointer(PhysicalPosition::new(0.0, 0.0), (0, 20), terminal),
            terminal_cell_at_pointer(PhysicalPosition::new(0.0, 0.0), (10, 0), terminal),
            terminal_cell_at_pointer(
                PhysicalPosition::new(0.0, 0.0),
                (10, 20),
                TerminalDimensions {
                    columns: 0,
                    rows: 24,
                },
            ),
            terminal_cell_at_pointer(
                PhysicalPosition::new(0.0, 0.0),
                (10, 20),
                TerminalDimensions {
                    columns: 80,
                    rows: 0,
                },
            ),
        ] {
            assert_eq!(invalid, None);
        }
    }

    #[test]
    fn wheel_scrolls_locally_or_reports_tracking_without_holding_the_grid_lock() {
        let grid = Mutex::new(grid_with_scrollback(4, 10));
        let enqueue_calls = AtomicUsize::new(0);
        let mut wheel_state = MouseWheelState::default();

        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                None,
                None,
                MouseScrollDelta::LineDelta(0.0, 3.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut wheel_state,
                |_| {
                    enqueue_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .expect("local wheel scrolling should succeed"),
            KeyboardInputOutcome::Scrollback {
                viewport_changed: true,
            }
        );
        assert_eq!(
            grid.lock()
                .expect("wheel grid should remain available")
                .scroll_offset,
            3
        );
        assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                None,
                None,
                MouseScrollDelta::LineDelta(0.0, -2.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut wheel_state,
                |_| {
                    enqueue_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .expect("local wheel scrolling down should succeed"),
            KeyboardInputOutcome::Scrollback {
                viewport_changed: true,
            }
        );
        assert_eq!(
            grid.lock()
                .expect("wheel grid should remain available")
                .scroll_offset,
            1
        );

        {
            let mut grid = grid.lock().expect("wheel grid should enable tracking");
            grid.mouse_tracking = MouseTracking::Normal;
            grid.mouse_encoding = MouseEncoding::Sgr;
        }
        let writes = RefCell::new(Vec::new());
        let modified = ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL;
        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                Some(PhysicalPosition::new(25.0, 45.0)),
                Some((10, 20)),
                MouseScrollDelta::LineDelta(0.0, 2.0),
                TouchPhase::Moved,
                modified,
                &mut wheel_state,
                |bytes| {
                    assert!(
                        grid.try_lock().is_ok(),
                        "mouse enqueue must run without the Grid lock"
                    );
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("tracked SGR wheel should succeed"),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(
            writes.borrow().as_slice(),
            [b"\x1b[<92;3;3M\x1b[<92;3;3M".to_vec()]
        );
        assert_eq!(
            grid.lock()
                .expect("tracked wheel grid should remain available")
                .scroll_offset,
            1,
            "tracked wheel input must preserve scrollback"
        );

        grid.lock()
            .expect("wheel grid should select legacy encoding")
            .mouse_encoding = MouseEncoding::Default;
        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                Some(PhysicalPosition::new(25.0, 45.0)),
                Some((10, 20)),
                MouseScrollDelta::LineDelta(0.0, -1.0),
                TouchPhase::Moved,
                modified,
                &mut wheel_state,
                |bytes| {
                    assert!(grid.try_lock().is_ok());
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("tracked legacy wheel should succeed"),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(
            writes.into_inner(),
            [
                b"\x1b[<92;3;3M\x1b[<92;3;3M".to_vec(),
                vec![0x1b, b'[', b'M', 125, 35, 35],
            ]
        );
        assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn tracked_wheel_ignores_missing_geometry_and_preserves_queue_errors() {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = MouseTracking::AnyEvent;
        let grid = Mutex::new(history);
        let enqueue_calls = AtomicUsize::new(0);
        let mut wheel_state = MouseWheelState::default();

        for (position, cell_dimensions) in [
            (None, Some((10, 20))),
            (Some(PhysicalPosition::new(1.0, 1.0)), None),
            (Some(PhysicalPosition::new(-1.0, 1.0)), Some((10, 20))),
        ] {
            assert_eq!(
                dispatch_mouse_wheel_with(
                    &grid,
                    position,
                    cell_dimensions,
                    MouseScrollDelta::LineDelta(0.0, 1.0),
                    TouchPhase::Moved,
                    ModifiersState::empty(),
                    &mut wheel_state,
                    |_| {
                        enqueue_calls.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    },
                )
                .expect("missing mouse geometry should be ignored"),
                KeyboardInputOutcome::Ignored {
                    viewport_changed: false,
                }
            );
        }
        assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

        for queue_error in [
            TerminalWriteQueueError::Full,
            TerminalWriteQueueError::Disconnected,
        ] {
            let error = dispatch_mouse_wheel_with(
                &grid,
                Some(PhysicalPosition::new(1.0, 1.0)),
                Some((10, 20)),
                MouseScrollDelta::LineDelta(0.0, 1.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut wheel_state,
                |_| {
                    assert!(
                        grid.try_lock().is_ok(),
                        "mouse queue errors must be returned outside the Grid lock"
                    );
                    Err(queue_error)
                },
            )
            .expect_err("mouse queue failures should be preserved");
            assert!(matches!(
                error,
                KeyboardInputError::Queue(actual) if actual == queue_error
            ));
        }
    }

    #[test]
    fn focus_loss_clears_stale_keyboard_modifiers() {
        let active = ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SHIFT;

        assert_eq!(modifiers_after_focus_change(active, true), active);
        assert_eq!(
            modifiers_after_focus_change(active, false),
            ModifiersState::empty()
        );
    }

    #[test]
    fn repeated_window_focus_states_are_not_transitions() {
        let mut last_window_focus = None;

        assert!(record_window_focus_change(&mut last_window_focus, false));
        assert_eq!(last_window_focus, Some(false));
        assert!(!record_window_focus_change(&mut last_window_focus, false));
        assert!(record_window_focus_change(&mut last_window_focus, true));
        assert_eq!(last_window_focus, Some(true));
        assert!(!record_window_focus_change(&mut last_window_focus, true));
        assert!(record_window_focus_change(&mut last_window_focus, false));
    }

    #[test]
    fn focus_reporting_is_gated_and_writes_exact_bytes_without_scrolling() {
        assert_eq!(focus_report_bytes(true), b"\x1b[I");
        assert_eq!(focus_report_bytes(false), b"\x1b[O");

        let mut history = grid_with_scrollback(3, 5);
        history.scroll_viewport_up(history.scrollback_len());
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let dispatch = |focused| {
            dispatch_focus_event_with(&grid, focused, |bytes| {
                assert!(
                    grid.try_lock().is_ok(),
                    "focus enqueue must run without the Grid lock"
                );
                writes.borrow_mut().push(bytes);
                Ok(())
            })
            .expect("fake focus report should succeed")
        };

        assert_eq!(
            dispatch(true),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert!(writes.borrow().is_empty());

        grid.lock()
            .expect("fresh Grid should enable focus reporting")
            .focus_events = true;
        assert_eq!(
            dispatch(true),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(
            dispatch(false),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(
            writes.into_inner(),
            [b"\x1b[I".to_vec(), b"\x1b[O".to_vec()]
        );
        assert_eq!(
            grid.lock()
                .expect("focus reporting should keep the Grid available")
                .scroll_offset,
            5,
            "focus reports must preserve the user's scrollback position"
        );

        for queue_error in [
            TerminalWriteQueueError::Full,
            TerminalWriteQueueError::Disconnected,
        ] {
            let error = dispatch_focus_event_with(&grid, true, |_| {
                assert!(
                    grid.try_lock().is_ok(),
                    "focus queue errors must be returned outside the Grid lock"
                );
                Err(queue_error)
            })
            .expect_err("focus queue failures should be preserved");
            assert!(matches!(
                error,
                KeyboardInputError::Queue(actual) if actual == queue_error
            ));
        }
    }

    #[test]
    fn keyboard_forwarding_skips_empty_output_and_preserves_queue_errors() {
        let mut history = grid_with_scrollback(4, 6);
        history.scroll_viewport_up(history.scrollback_len());
        let grid = Mutex::new(history);
        let writes = AtomicUsize::new(0);

        assert_eq!(
            dispatch_keyboard_input_with(
                &grid,
                None,
                |_| None,
                |_| {
                    writes.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .expect("empty keyboard output should be a no-op"),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
        assert_eq!(
            grid.lock()
                .expect("keyboard dispatch should keep the grid available")
                .scroll_offset,
            6,
            "ignored keys must preserve the scrollback position"
        );
        assert_eq!(writes.load(Ordering::Relaxed), 0);

        let error = dispatch_keyboard_input_with(
            &grid,
            None,
            |_| Some(b"x".to_vec()),
            |_| Err(TerminalWriteQueueError::Full),
        )
        .expect_err("fake queue failure should be preserved");
        assert!(matches!(
            error,
            KeyboardInputError::Queue(TerminalWriteQueueError::Full)
        ));
    }

    #[test]
    fn poisoned_grid_prevents_keyboard_ime_focus_and_mouse_queue_writes() {
        let grid = Arc::new(Mutex::new(Grid::new(80, 24, 0)));
        let poison_target = grid.clone();
        assert!(std::thread::spawn(move || {
            let _guard = poison_target
                .lock()
                .expect("fresh test grid should lock before poisoning");
            panic!("poison the keyboard test grid");
        })
        .join()
        .is_err());
        let encode_calls = AtomicUsize::new(0);
        let enqueue_calls = AtomicUsize::new(0);

        let error = dispatch_keyboard_input_with(
            grid.as_ref(),
            Some(ScrollbackAction::PageUp),
            |_| {
                encode_calls.fetch_add(1, Ordering::Relaxed);
                Some(b"x".to_vec())
            },
            |_| {
                enqueue_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect_err("poisoned Grid must reject keyboard input");
        assert!(matches!(
            error,
            KeyboardInputError::Grid(GridAccessError::Poisoned)
        ));
        assert_eq!(encode_calls.load(Ordering::Relaxed), 0);

        let ime_error = dispatch_encoded_terminal_input_with(
            grid.as_ref(),
            Some("é".as_bytes().to_vec()),
            |_| {
                enqueue_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect_err("poisoned Grid must reject IME input");
        assert!(matches!(
            ime_error,
            KeyboardInputError::Grid(GridAccessError::Poisoned)
        ));
        assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

        let focus_error = dispatch_focus_event_with(grid.as_ref(), true, |_| {
            enqueue_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .expect_err("poisoned Grid must reject focus reporting");
        assert!(matches!(
            focus_error,
            KeyboardInputError::Grid(GridAccessError::Poisoned)
        ));
        assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

        let mut wheel_state = MouseWheelState::default();
        let mouse_error = dispatch_mouse_wheel_with(
            grid.as_ref(),
            Some(PhysicalPosition::new(1.0, 1.0)),
            Some((10, 20)),
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut wheel_state,
            |_| {
                enqueue_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect_err("poisoned Grid must reject mouse input");
        assert!(matches!(
            mouse_error,
            KeyboardInputError::Grid(GridAccessError::Poisoned)
        ));
        assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

        let mouse_button_error = dispatch_mouse_button_with(
            grid.as_ref(),
            Some(PhysicalPosition::new(1.0, 1.0)),
            Some((10, 20)),
            ElementState::Pressed,
            MouseButton::Left,
            ModifiersState::empty(),
            |_| {
                enqueue_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect_err("poisoned Grid must reject mouse button input");
        assert!(matches!(
            mouse_button_error,
            KeyboardInputError::Grid(GridAccessError::Poisoned)
        ));
        assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

        let mouse_motion_error = dispatch_mouse_motion_with(
            grid.as_ref(),
            PhysicalPosition::new(1.0, 1.0),
            Some((10, 20)),
            None,
            ModifiersState::empty(),
            None,
            |_| {
                enqueue_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect_err("poisoned Grid must reject mouse motion");
        assert!(matches!(
            mouse_motion_error,
            KeyboardInputError::Grid(GridAccessError::Poisoned)
        ));
        assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn zero_sized_windows_do_not_create_a_surface_extent() {
        assert_eq!(drawable_dimensions(PhysicalSize::new(0, 480)), Ok(None));
        assert_eq!(drawable_dimensions(PhysicalSize::new(800, 0)), Ok(None));
        assert_eq!(drawable_dimensions(PhysicalSize::new(0, 0)), Ok(None));

        let (width, height) = drawable_dimensions(PhysicalSize::new(800, 480))
            .expect("normal dimensions should fit the renderer budget")
            .expect("non-zero dimensions should be drawable");
        assert_eq!(width.get(), 800);
        assert_eq!(height.get(), 480);
    }

    #[test]
    fn surface_extent_guard_rejects_frames_above_the_physical_budget() {
        assert!(drawable_dimensions(PhysicalSize::new(
            MAX_REQUESTED_PHYSICAL_WIDTH,
            MAX_REQUESTED_PHYSICAL_HEIGHT,
        ))
        .expect("the configured maximum must be drawable")
        .is_some());
        assert_eq!(
            drawable_dimensions(PhysicalSize::new(
                MAX_REQUESTED_PHYSICAL_WIDTH + 1,
                MAX_REQUESTED_PHYSICAL_HEIGHT,
            )),
            Err(SurfaceSizeError::ExceedsRenderBudget {
                width: MAX_REQUESTED_PHYSICAL_WIDTH + 1,
                height: MAX_REQUESTED_PHYSICAL_HEIGHT,
            })
        );
        assert_eq!(
            drawable_dimensions(PhysicalSize::new(
                MAX_REQUESTED_PHYSICAL_WIDTH,
                MAX_REQUESTED_PHYSICAL_HEIGHT + 1,
            )),
            Err(SurfaceSizeError::ExceedsRenderBudget {
                width: MAX_REQUESTED_PHYSICAL_WIDTH,
                height: MAX_REQUESTED_PHYSICAL_HEIGHT + 1,
            })
        );
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
        };

        draw_cell_glyph(
            &mut frame,
            (5, 5),
            super::GlyphSource {
                pixels: &[255],
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
            ..normal
        };

        assert_eq!(
            resolve_cell_colors(colors, &normal),
            ResolvedCellColors {
                foreground: rgb_to_xrgb(241, 76, 76),
                background: rgb_to_xrgb(4, 5, 6),
            }
        );
        assert_eq!(
            resolve_cell_colors(colors, &reversed),
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
            resolve_cell_colors(test_colors(), &cell),
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
        let resolved = resolve_cell_colors(test_colors(), &default_underline);
        assert_eq!(
            resolve_cell_underline_color(
                test_colors(),
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
                test_colors(),
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
        let expected_background = resolve_cell_colors(test_colors(), leader).background;

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
            TerminalDimensions {
                columns: 2,
                rows: 2,
            },
            |_, _| {
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
    fn atlas_metrics_are_positive_rounded_and_synchronized_with_the_grid() {
        let atlas = test_atlas();
        let dimensions =
            atlas_cell_dimensions(&atlas).expect("test atlas should have valid dimensions");
        let grid = Mutex::new(Grid::new(1, 1, 0));

        set_grid_cell_dimensions(&grid, dimensions)
            .expect("fresh test grid dimensions should synchronize");

        assert_eq!(dimensions.0, atlas.cell_width.round() as u16);
        assert_eq!(dimensions.1, atlas.cell_height.round() as u16);
        assert!(dimensions.0 > 0);
        assert!(dimensions.1 > 0);
        let grid = grid.lock().expect("test grid should remain available");
        assert_eq!(grid.cell_pixel_width, dimensions.0);
        assert_eq!(grid.cell_pixel_height, dimensions.1);
    }

    #[test]
    fn failed_scale_or_metric_rebuild_preserves_the_previous_atlas() {
        let font_family = "kokuban-test-font-that-does-not-exist";
        let font_size = 14.0;
        let atlas = GlyphAtlas::new(font_family, font_size, 1.0)
            .expect("system monospace fallback should be available");
        let original_cell_width = atlas.cell_width;
        let original_glyph_count = atlas.glyphs.len();
        let mut glyph_atlas = Some(atlas);
        let mut atlas_scale_factor = Some(1.0);

        assert!(replace_glyph_atlas_for_scale(
            &mut glyph_atlas,
            &mut atlas_scale_factor,
            font_family,
            font_size,
            f64::NAN,
            |_, _| Ok(()),
        )
        .is_err());

        let preserved = glyph_atlas
            .as_ref()
            .expect("failed replacement must keep the old atlas");
        assert_eq!(atlas_scale_factor, Some(1.0));
        assert_eq!(preserved.cell_width, original_cell_width);
        assert_eq!(preserved.glyphs.len(), original_glyph_count);
        assert!(replace_glyph_atlas_for_scale(
            &mut glyph_atlas,
            &mut atlas_scale_factor,
            font_family,
            font_size,
            2.0,
            |_, _| Err(GridAccessError::Poisoned),
        )
        .is_err());
        let preserved = glyph_atlas
            .as_ref()
            .expect("failed metric synchronization must keep the old atlas");
        assert_eq!(atlas_scale_factor, Some(1.0));
        assert_eq!(preserved.cell_width, original_cell_width);
        assert_eq!(preserved.glyphs.len(), original_glyph_count);
        assert!(!replace_glyph_atlas_for_scale(
            &mut glyph_atlas,
            &mut atlas_scale_factor,
            font_family,
            font_size,
            1.0,
            |_, _| Ok(()),
        )
        .expect("duplicate scale should be a no-op"));
    }

    #[test]
    fn shutdown_and_eof_are_normal_reader_exits() {
        assert_eq!(
            classify_reader_exit(&ReaderExit::Shutdown),
            ReaderStatus::Normal
        );
        assert_eq!(classify_reader_exit(&ReaderExit::Eof), ReaderStatus::Normal);
    }

    #[test]
    fn reader_failures_include_actionable_context() {
        let failures = [
            (
                ReaderExit::WaitFailed(io::Error::from_raw_os_error(5)),
                "wait for PTY output",
            ),
            (
                ReaderExit::ReadFailed(io::Error::from_raw_os_error(5)),
                "read PTY output",
            ),
            (
                ReaderExit::ResponseWriteFailed(io::Error::from_raw_os_error(32)),
                "write a protocol response",
            ),
            (ReaderExit::GridPoisoned, "terminal grid"),
            (ReaderExit::DecoderStalled, "stopped making progress"),
            (
                ReaderExit::UnexpectedProtocolEvent,
                "graphics event in text-only mode",
            ),
        ];

        for (exit, expected_context) in failures {
            let ReaderStatus::Failed(message) = classify_reader_exit(&exit) else {
                panic!("reader failure was classified as a normal exit");
            };
            assert!(
                message.contains(expected_context),
                "missing `{expected_context}` in `{message}`"
            );
        }
    }

    #[test]
    fn writer_shutdown_is_normal_and_failures_include_actionable_context() {
        assert_eq!(
            classify_writer_exit(&WriterExit::Shutdown),
            WriterStatus::Normal
        );

        let channel_status = classify_writer_exit(&WriterExit::ChannelClosed);
        let WriterStatus::Failed(channel_message) = channel_status else {
            panic!("closed writer queue should be a failure");
        };
        assert!(channel_message.contains("input queue"));

        let write_status =
            classify_writer_exit(&WriterExit::WriteFailed(io::Error::from_raw_os_error(5)));
        let WriterStatus::Failed(write_message) = write_status else {
            panic!("PTY write error should be a failure");
        };
        assert!(write_message.contains("write keyboard input to the PTY"));
        assert!(write_message.contains("5"));
    }

    #[test]
    fn launch_result_preserves_worker_failures_before_late_finish_errors() {
        assert_eq!(
            combine_launch_results(
                Ok(()),
                Ok(()),
                Err("native writer failure".to_string()),
                Err("input queue disconnected".to_string()),
            ),
            Err("native writer failure".to_string())
        );
        assert_eq!(
            combine_launch_results(
                Ok(()),
                Err("reader failure".to_string()),
                Err("writer failure".to_string()),
                Err("finish failure".to_string()),
            ),
            Err("reader failure".to_string())
        );
        assert_eq!(
            combine_launch_results(
                Err("event loop failure".to_string()),
                Err("reader failure".to_string()),
                Err("writer failure".to_string()),
                Err("finish failure".to_string()),
            ),
            Err("event loop failure".to_string())
        );
        assert_eq!(
            combine_launch_results(Ok(()), Ok(()), Ok(()), Err("finish failure".to_string()),),
            Err("finish failure".to_string())
        );
    }

    #[test]
    fn terminal_stops_accepting_input_while_exiting_or_after_the_reader_exits() {
        assert!(terminal_accepts_input(false, None));
        assert!(!terminal_accepts_input(true, None));
        assert!(!terminal_accepts_input(false, Some(&ReaderStatus::Normal)));
        assert!(!terminal_accepts_input(
            false,
            Some(&ReaderStatus::Failed("read failed".to_string()))
        ));
        assert!(!terminal_accepts_input(true, Some(&ReaderStatus::Normal)));
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
