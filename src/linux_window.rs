use crate::config::{ColorConfig, Config};
use crate::glyph_atlas::{GlyphAtlas, GlyphKey};
use crate::grid::cell::{Cell, CellFlags};
use crate::grid::{CursorShape, Grid};
use crate::pty::Pty;
use crate::software_raster::{draw_glyph_a8, fill_rect};
use crate::terminal_colors::TerminalColors;
use crate::terminal_reader::{ReaderExit, TerminalReader};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

const WINDOW_TITLE: &str = "黒板kokuban";
const INITIAL_CELL_WIDTH: u32 = 10;
const INITIAL_CELL_HEIGHT: u32 = 20;
const SCALE_CHANGE_EPSILON: f64 = 0.001;
const EXIT_AFTER_FIRST_FRAME_ENV: &str = "KOKUBAN_EXIT_AFTER_FIRST_FRAME";
const CURSOR_THICKNESS: u32 = 2;
const CURSOR_ALPHA: u8 = 180;

type SoftwareSurface = Surface<Arc<Window>, Arc<Window>>;

#[derive(Debug)]
enum LinuxEvent {
    GridUpdated,
    ReaderExited(ReaderStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReaderStatus {
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
    #[error("Linux renderer could not access the terminal grid because its lock is poisoned")]
    Poisoned,
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

    let columns = config.window.columns.max(1);
    let rows = config.window.rows.max(1);
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
    let update_proxy = event_proxy.clone();
    let update_pending = redraw_pending.clone();
    let reader = TerminalReader::spawn_text(
        pty.clone(),
        grid.clone(),
        move || signal_grid_update(&update_proxy, update_pending.as_ref()),
        move |exit| {
            let _ = event_proxy.send_event(LinuxEvent::ReaderExited(classify_reader_exit(exit)));
        },
    )
    .map_err(|error| format!("could not start the Linux terminal reader: {error}"))?;
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
        redraw_pending,
    );

    let run_result = event_loop
        .run_app(&mut application)
        .map_err(|error| format!("Linux event loop failed: {error}"));
    application.request_reader_shutdown();
    let join_result = application.join_reader();
    let finish_result = application.finish();

    run_result.and(finish_result).and(join_result)
}

struct LinuxWindow {
    // Drop the surface and its display connection before releasing the window.
    surface: Option<SoftwareSurface>,
    context: Option<Context<Arc<Window>>>,
    window: Option<Arc<Window>>,
    glyph_atlas: Option<GlyphAtlas>,
    atlas_scale_factor: Option<f64>,
    background: u32,
    colors: TerminalColors,
    font_family: String,
    font_size: f32,
    initial_size: LogicalSize<u32>,
    exit_after_first_frame: bool,
    first_frame_presented: bool,
    grid: Arc<Mutex<Grid>>,
    #[allow(dead_code)] // Kept alive for the reader and upcoming input/resize handling.
    pty: Arc<Pty>,
    reader: Option<TerminalReader>,
    redraw_pending: Arc<AtomicBool>,
    reader_status: Option<ReaderStatus>,
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
        redraw_pending: Arc<AtomicBool>,
    ) -> Self {
        Self {
            surface: None,
            context: None,
            window: None,
            glyph_atlas: None,
            atlas_scale_factor: None,
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
            redraw_pending,
            reader_status: None,
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

        self.surface = Some(surface);
        self.context = Some(context);
        self.window = Some(window.clone());
        self.glyph_atlas = Some(glyph_atlas);
        self.atlas_scale_factor = Some(scale_factor);
        window.request_redraw();
        Ok(())
    }

    fn create_glyph_atlas(&self, scale_factor: f64) -> Result<GlyphAtlas, String> {
        create_glyph_atlas(&self.font_family, self.font_size, scale_factor)
    }

    fn rebuild_glyph_atlas(&mut self, scale_factor: f64) -> Result<bool, String> {
        let grid = self.grid.clone();
        replace_glyph_atlas_for_scale(
            &mut self.glyph_atlas,
            &mut self.atlas_scale_factor,
            &self.font_family,
            self.font_size,
            scale_factor,
            move |replacement| {
                let cell_dimensions = atlas_cell_dimensions(replacement)?;
                set_grid_cell_dimensions(grid.as_ref(), cell_dimensions)
                    .map_err(|error| error.to_string())
            },
        )
    }

    fn present_frame(&mut self) -> Result<Option<bool>, String> {
        let window =
            self.window.as_ref().cloned().ok_or_else(|| {
                "redraw requested before the Linux window was created".to_string()
            })?;
        let size = window.inner_size();
        let Some((width, height)) = drawable_dimensions(size) else {
            return Ok(None);
        };
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
        let snapshot = snapshot_grid(self.grid.as_ref()).map_err(|error| error.to_string())?;
        let cell_dimensions = atlas_cell_dimensions(glyph_atlas)?;
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
        event_loop.exit();
    }

    fn request_reader_shutdown(&self) {
        if let Some(reader) = self.reader.as_ref() {
            reader.request_shutdown();
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
        self.request_reader_shutdown();
        event_loop.exit();
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
                self.request_reader_shutdown();
                event_loop.exit();
            }
            WindowEvent::Resized(_) => {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                match self.rebuild_glyph_atlas(scale_factor) {
                    Ok(true) => log::info!("Linux scale factor changed to {scale_factor}"),
                    Ok(false) => {}
                    Err(error) => log::error!(
                        "{error}; keeping scale {}",
                        self.atlas_scale_factor.unwrap_or(1.0)
                    ),
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
                                self.request_reader_shutdown();
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

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: LinuxEvent) {
        match event {
            LinuxEvent::GridUpdated => {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            LinuxEvent::ReaderExited(status) => {
                if self.reader_status.is_none() {
                    self.reader_status = Some(status);
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.request_reader_shutdown();
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
) -> Result<bool, String>
where
    F: FnOnce(&GlyphAtlas) -> Result<(), String>,
{
    if atlas_scale_factor
        .is_some_and(|current| (current - scale_factor).abs() <= SCALE_CHANGE_EPSILON)
    {
        return Ok(false);
    }

    let replacement = create_glyph_atlas(font_family, font_size, scale_factor)?;
    before_replace(&replacement)?;
    *glyph_atlas = Some(replacement);
    *atlas_scale_factor = Some(scale_factor);
    Ok(true)
}

fn initial_window_dimensions(columns: u16, rows: u16) -> LogicalSize<u32> {
    LogicalSize::new(
        u32::from(columns).max(1) * INITIAL_CELL_WIDTH,
        u32::from(rows).max(1) * INITIAL_CELL_HEIGHT,
    )
}

fn drawable_dimensions(size: PhysicalSize<u32>) -> Option<(NonZeroU32, NonZeroU32)> {
    Some((NonZeroU32::new(size.width)?, NonZeroU32::new(size.height)?))
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
        arm_grid_redraw, atlas_cell_dimensions, begin_grid_redraw, classify_reader_exit,
        draw_cell_glyph, draw_grid_snapshot, drawable_dimensions, initial_window_dimensions,
        replace_glyph_atlas_for_scale, resolve_cell_colors, rgb_to_xrgb, rounded_i32,
        set_grid_cell_dimensions, snapshot_grid, terminal_color_query_value, GridAccessError,
        ReaderStatus, ResolvedCellColors,
    };
    use crate::glyph_atlas::{GlyphAtlas, GlyphEntry};
    use crate::grid::cell::{Cell, CellFlags, Color};
    use crate::grid::{CursorShape, Grid};
    use crate::terminal_colors::TerminalColors;
    use crate::terminal_reader::ReaderExit;
    use std::io;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use winit::dpi::PhysicalSize;

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
    fn zero_sized_windows_do_not_create_a_surface_extent() {
        assert!(drawable_dimensions(PhysicalSize::new(0, 480)).is_none());
        assert!(drawable_dimensions(PhysicalSize::new(800, 0)).is_none());
        assert!(drawable_dimensions(PhysicalSize::new(0, 0)).is_none());

        let (width, height) = drawable_dimensions(PhysicalSize::new(800, 480))
            .expect("non-zero dimensions should be drawable");
        assert_eq!(width.get(), 800);
        assert_eq!(height.get(), 480);
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
            |_| Ok(()),
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
            |_| Err("could not synchronize replacement metrics".to_string()),
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
            |_| Ok(()),
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
    fn grid_updates_coalesce_until_redraw_begins_then_rearm() {
        let redraw_pending = AtomicBool::new(false);

        assert!(arm_grid_redraw(&redraw_pending));
        assert!(!arm_grid_redraw(&redraw_pending));

        begin_grid_redraw(&redraw_pending);

        assert!(arm_grid_redraw(&redraw_pending));
        assert!(!arm_grid_redraw(&redraw_pending));
    }
}
