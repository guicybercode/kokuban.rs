use crate::config::{ColorConfig, Config};
use crate::glyph_atlas::{GlyphAtlas, GlyphKey};
use crate::grid::Grid;
use crate::pty::Pty;
use crate::software_raster::draw_glyph_a8;
use crate::terminal_reader::{ReaderExit, TerminalReader};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

const WINDOW_TITLE: &str = "黒板kokuban";
const INITIAL_CELL_WIDTH: u32 = 10;
const INITIAL_CELL_HEIGHT: u32 = 20;
const STARTUP_TEXT: &str = "kokuban";
const SCALE_CHANGE_EPSILON: f64 = 0.001;
const EXIT_AFTER_FIRST_FRAME_ENV: &str = "KOKUBAN_EXIT_AFTER_FIRST_FRAME";

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
    foreground: u32,
    font_family: String,
    font_size: f32,
    initial_size: LogicalSize<u32>,
    exit_after_first_frame: bool,
    first_frame_presented: bool,
    #[allow(dead_code)] // The grid becomes the Linux renderer's source in the next step.
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
            foreground: rgb_to_xrgb(foreground.0, foreground.1, foreground.2),
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
        replace_glyph_atlas_for_scale(
            &mut self.glyph_atlas,
            &mut self.atlas_scale_factor,
            &self.font_family,
            self.font_size,
            scale_factor,
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
        draw_startup_text(
            &mut buffer,
            (width.get(), height.get()),
            glyph_atlas,
            self.foreground,
        );
        let startup_text_visible =
            !self.exit_after_first_frame || buffer.iter().any(|&pixel| pixel != self.background);
        window.pre_present_notify();
        buffer
            .present()
            .map_err(|error| format!("could not present a Linux frame: {error}"))?;
        Ok(Some(startup_text_visible))
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
                    Ok(Some(startup_text_visible)) => {
                        self.first_frame_presented = true;
                        if self.exit_after_first_frame {
                            if startup_text_visible {
                                self.request_reader_shutdown();
                                event_loop.exit();
                            } else {
                                self.fail(
                                    event_loop,
                                    "smoke mode presented a Linux frame without visible glyphs"
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

fn replace_glyph_atlas_for_scale(
    glyph_atlas: &mut Option<GlyphAtlas>,
    atlas_scale_factor: &mut Option<f64>,
    font_family: &str,
    font_size: f32,
    scale_factor: f64,
) -> Result<bool, String> {
    if atlas_scale_factor
        .is_some_and(|current| (current - scale_factor).abs() <= SCALE_CHANGE_EPSILON)
    {
        return Ok(false);
    }

    let replacement = create_glyph_atlas(font_family, font_size, scale_factor)?;
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

fn draw_startup_text(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    foreground: u32,
) {
    let Some(cell_width) = rounded_positive_i32(atlas.cell_width) else {
        return;
    };
    let Some(cell_height) = rounded_positive_i32(atlas.cell_height) else {
        return;
    };
    let mut cell_x = cell_width;

    for character in STARTUP_TEXT.chars() {
        let glyph = atlas.get_or_insert(GlyphKey {
            c: character,
            bold: false,
            italic: false,
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
            (cell_x, cell_height),
            foreground,
        );

        let Some(next_x) = cell_x.checked_add(cell_width) else {
            return;
        };
        cell_x = next_x;
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
        arm_grid_redraw, begin_grid_redraw, classify_reader_exit, draw_cell_glyph,
        drawable_dimensions, initial_window_dimensions, replace_glyph_atlas_for_scale, rgb_to_xrgb,
        rounded_i32, terminal_color_query_value, ReaderStatus,
    };
    use crate::glyph_atlas::{GlyphAtlas, GlyphEntry};
    use crate::terminal_reader::ReaderExit;
    use std::io;
    use std::sync::atomic::AtomicBool;
    use winit::dpi::PhysicalSize;

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
    fn failed_scale_rebuild_preserves_the_previous_atlas() {
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
        )
        .is_err());

        let preserved = glyph_atlas
            .as_ref()
            .expect("failed replacement must keep the old atlas");
        assert_eq!(atlas_scale_factor, Some(1.0));
        assert_eq!(preserved.cell_width, original_cell_width);
        assert_eq!(preserved.glyphs.len(), original_glyph_count);
        assert!(!replace_glyph_atlas_for_scale(
            &mut glyph_atlas,
            &mut atlas_scale_factor,
            font_family,
            font_size,
            1.0,
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
