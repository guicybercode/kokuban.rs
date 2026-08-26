use crate::config::{ColorConfig, Config};
use crate::glyph_atlas::{GlyphAtlas, GlyphKey};
use crate::grid::cell::{Cell, CellFlags};
use crate::grid::{CursorShape, Grid};
use crate::input::linux::{encode_key_press, key_press_from_winit};
use crate::pty::Pty;
use crate::software_raster::{draw_glyph_a8, fill_rect};
use crate::terminal_colors::TerminalColors;
use crate::terminal_reader::{ReaderExit, TerminalReader};
use crate::terminal_writer::{TerminalWriteQueueError, TerminalWriter, WriterExit};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

const WINDOW_TITLE: &str = "黒板kokuban";
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
// Limit the Grid/snapshot budget to 262,144 visible cells while still covering wide 8K layouts.
const MAX_TERMINAL_COLUMNS: u16 = 1024;
const MAX_TERMINAL_ROWS: u16 = 256;

type SoftwareSurface = Surface<Arc<Window>, Arc<Window>>;

#[derive(Debug)]
enum LinuxEvent {
    GridUpdated,
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
    #[error("could not read the Linux terminal keyboard mode: {0}")]
    Grid(#[from] GridAccessError),
    #[error("could not queue Linux keyboard input: {0}")]
    Queue(#[from] TerminalWriteQueueError),
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

#[derive(Debug)]
struct GridSnapshot {
    columns: usize,
    rows: usize,
    cells: Vec<Cell>,
    cursor: Option<CursorSnapshot>,
}

impl GridSnapshot {
    fn cell(&self, row: usize, column: usize) -> Option<&Cell> {
        let index = row.checked_mul(self.columns)?.checked_add(column)?;
        self.cells.get(index)
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
    let event_proxy = event_loop.create_proxy();
    let writer_proxy = event_proxy.clone();
    let mut writer = TerminalWriter::spawn(pty.clone(), move |exit| {
        let _ = writer_proxy.send_event(LinuxEvent::WriterExited(classify_writer_exit(exit)));
    })
    .map_err(|error| format!("could not start the Linux terminal writer: {error}"))?;
    let update_proxy = event_proxy.clone();
    let update_pending = redraw_pending.clone();
    let reader = match TerminalReader::spawn_text(
        pty.clone(),
        grid.clone(),
        move || signal_grid_update(&update_proxy, update_pending.as_ref()),
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
    grid: Arc<Mutex<Grid>>,
    pty: Arc<Pty>,
    reader: Option<TerminalReader>,
    writer: Option<TerminalWriter>,
    redraw_pending: Arc<AtomicBool>,
    reader_status: Option<ReaderStatus>,
    modifiers: ModifiersState,
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
            grid,
            pty,
            reader: Some(reader),
            writer: Some(writer),
            redraw_pending,
            reader_status: None,
            modifiers: ModifiersState::empty(),
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
        let surface = self
            .surface
            .as_mut()
            .ok_or_else(|| "redraw requested before the software renderer was ready".to_string())?;
        let cell_dimensions = self
            .cell_dimensions
            .ok_or_else(|| "redraw requested before Linux glyph metrics were ready".to_string())?;
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
        let snapshot = snapshot_grid(self.grid.as_ref()).map_err(|error| error.to_string())?;
        draw_grid_snapshot(
            &mut buffer,
            (width.get(), height.get()),
            glyph_atlas,
            self.colors,
            cell_dimensions,
            &snapshot,
        );
        let terminal_content_visible =
            !self.exit_after_first_frame || buffer.iter().any(|&pixel| pixel != self.background);
        window.pre_present_notify();
        buffer
            .present()
            .map_err(|error| format!("could not present a Linux frame: {error}"))?;
        Ok(Some(terminal_content_visible))
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: String) {
        if self.error.is_none() {
            self.error = Some(error);
        }
        self.request_terminal_shutdown();
        event_loop.exit();
    }

    fn request_terminal_shutdown(&mut self) {
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
            WindowEvent::Focused(focused) => {
                self.modifiers = modifiers_after_focus_change(self.modifiers, focused);
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if !terminal_accepts_keyboard_input(self.reader_status.as_ref()) {
                    return;
                }
                let Some(key_press) = key_press_from_winit(&event, is_synthetic, self.modifiers)
                else {
                    return;
                };
                if let Err(error) = forward_keyboard_input_with(
                    self.grid.as_ref(),
                    |application_cursor_keys| encode_key_press(key_press, application_cursor_keys),
                    |bytes| {
                        self.writer
                            .as_ref()
                            .ok_or(TerminalWriteQueueError::Disconnected)?
                            .enqueue(bytes)
                    },
                ) {
                    self.fail(event_loop, error.to_string());
                }
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
            LinuxEvent::ReaderExited(status) => {
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

fn terminal_accepts_keyboard_input(reader_status: Option<&ReaderStatus>) -> bool {
    reader_status.is_none()
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

fn forward_keyboard_input_with<E, W>(
    grid: &Mutex<Grid>,
    encode: E,
    write: W,
) -> Result<bool, KeyboardInputError>
where
    E: FnOnce(bool) -> Option<Vec<u8>>,
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let application_cursor_keys = {
        let grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;
        grid.application_cursor_keys
    };
    let Some(bytes) = encode(application_cursor_keys) else {
        return Ok(false);
    };
    write(bytes)?;
    Ok(true)
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

    let cursor = (grid.cursor_visible
        && grid.scroll_offset == 0
        && grid.cursor_row < rows
        && grid.cursor_col < columns)
        .then_some(CursorSnapshot {
            row: grid.cursor_row,
            column: grid.cursor_col,
            shape: grid.cursor_style.shape,
        });

    Ok(GridSnapshot {
        columns,
        rows,
        cells,
        cursor,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedCellColors {
    foreground: u32,
    background: u32,
}

fn resolve_cell_colors(colors: TerminalColors, cell: &Cell) -> ResolvedCellColors {
    let bold = cell.flags.contains(CellFlags::BOLD);
    let semantic_foreground = colors.resolve_foreground(cell.fg, bold);
    let semantic_background = colors.resolve_background(cell.bg);
    let (foreground, background) = if cell.flags.contains(CellFlags::REVERSE) {
        (semantic_background, semantic_foreground)
    } else {
        (semantic_foreground, semantic_background)
    };

    ResolvedCellColors {
        foreground: rgb_to_xrgb(foreground.0, foreground.1, foreground.2),
        background: rgb_to_xrgb(background.0, background.1, background.2),
    }
}

fn draw_grid_snapshot(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: TerminalColors,
    cell_dimensions: (u16, u16),
    snapshot: &GridSnapshot,
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

            if cell.c == ' ' || cell.c == '\0' {
                continue;
            }

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
        .cell(cursor.row, cursor.column)
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
        apply_terminal_resize, arm_grid_redraw, atlas_cell_dimensions, begin_grid_redraw,
        classify_reader_exit, classify_writer_exit, combine_launch_results,
        configured_terminal_dimensions, draw_cell_glyph, draw_grid_snapshot, drawable_dimensions,
        forward_keyboard_input_with, immediate_surface_size_to_reconcile,
        initial_window_dimensions, is_current_surface_size, modifiers_after_focus_change,
        physical_size_for_terminal, replace_glyph_atlas_for_scale, resize_terminal_with,
        resolve_cell_colors, rgb_to_xrgb, rounded_i32, set_grid_cell_dimensions, snapshot_grid,
        terminal_accepts_keyboard_input, terminal_color_query_value,
        terminal_dimensions_for_surface, GridAccessError, KeyboardInputError, ReaderStatus,
        ResolvedCellColors, SurfaceSizeError, TerminalDimensions, TerminalResizeError,
        WriterStatus, MAX_INITIAL_LOGICAL_HEIGHT, MAX_INITIAL_LOGICAL_WIDTH,
        MAX_REQUESTED_PHYSICAL_HEIGHT, MAX_REQUESTED_PHYSICAL_WIDTH, MAX_TERMINAL_COLUMNS,
        MAX_TERMINAL_ROWS,
    };
    use crate::glyph_atlas::{GlyphAtlas, GlyphEntry};
    use crate::grid::cell::{Cell, CellFlags, Color};
    use crate::grid::{CursorShape, Grid};
    use crate::terminal_colors::TerminalColors;
    use crate::terminal_reader::ReaderExit;
    use crate::terminal_writer::{TerminalWriteQueueError, WriterExit};
    use std::cell::RefCell;
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, TryLockError};
    use winit::dpi::PhysicalSize;
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
        let grid = Mutex::new(Grid::new(80, 24, 0));
        grid.lock()
            .expect("test grid should be available")
            .application_cursor_keys = true;
        let written = RefCell::new(Vec::new());

        assert!(forward_keyboard_input_with(
            &grid,
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
        .expect("fake keyboard write should succeed"));
        assert_eq!(written.into_inner(), b"\x1bOA");
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
    fn keyboard_forwarding_skips_empty_output_and_preserves_queue_errors() {
        let grid = Mutex::new(Grid::new(80, 24, 0));
        let writes = AtomicUsize::new(0);

        assert!(!forward_keyboard_input_with(
            &grid,
            |_| None,
            |_| {
                writes.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect("empty keyboard output should be a no-op"));
        assert_eq!(writes.load(Ordering::Relaxed), 0);

        let error = forward_keyboard_input_with(
            &grid,
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
    fn poisoned_grid_prevents_keyboard_encoding_and_queue_writes() {
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

        let error = forward_keyboard_input_with(
            grid.as_ref(),
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
            flags: CellFlags::BOLD | CellFlags::ITALIC,
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
        assert!(pending_wrap_frame.iter().all(|&pixel| pixel == background));
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
    fn terminal_stops_accepting_keyboard_input_after_the_reader_exits() {
        assert!(terminal_accepts_keyboard_input(None));
        assert!(!terminal_accepts_keyboard_input(Some(
            &ReaderStatus::Normal
        )));
        assert!(!terminal_accepts_keyboard_input(Some(
            &ReaderStatus::Failed("read failed".to_string())
        )));
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
