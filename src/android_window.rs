//! Android owns window lifetime and input; terminal state lives independently.
use crate::android_controls::{
    mask_outside, Control, Rect, ToolbarLayout, TouchContacts, TouchGesture, TouchRegion,
};
use crate::android_images::AndroidImages;
use crate::android_ime::{AccessibleControl, AndroidIme};
use crate::android_input::{self, ImeEvent, InputModifiers, InputState};
use crate::android_metrics::FrameMetrics;
use crate::android_runtime::AndroidRuntime;
use crate::config::{ColorConfig, Config};
use crate::glyph_atlas::{GlyphAtlas, GlyphKey};
use crate::grid::cell::CellFlags;
use crate::grid::{Grid, MouseEncoding, MouseTracking};
use crate::input::keyboard::TerminalKey;
use crate::input::mouse::{
    encode_alternate_scroll_steps, encode_mouse_event, mouse_button_with_modifier_flags,
    mouse_wheel_route, MouseWheelRoute, MAX_WHEEL_STEPS_PER_EVENT, MOUSE_WHEEL_DOWN,
    MOUSE_WHEEL_UP,
};
use crate::parser::ansi::GraphicsSupport;
use crate::pty::Pty;
use crate::selection::{GridPoint, SelectionState};
use crate::software_graphics::SoftwareGraphics;
use crate::software_raster::{draw_glyph_a8, draw_image_rgba, fill_rect};
use crate::terminal_colors::TerminalColors;
use crate::terminal_reader::{ReaderExit, TerminalReader};
use crate::terminal_writer::{TerminalWriter, WriterExit};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{
    DeviceId, ElementState, Ime, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::android::{activity::AndroidApp, EventLoopBuilderExtAndroid};
use winit::window::{Window, WindowId};

#[derive(Debug)]
enum Event {
    Updated,
    Finished(Option<String>),
    Input(ImeEvent),
}

pub(crate) fn launch(app: AndroidApp) -> Result<(), String> {
    let path = app
        .internal_data_path()
        .ok_or("Android private storage unavailable")?;
    let mut runtime = AndroidRuntime::prepare(&path).map_err(|e| e.to_string())?;
    let config = match std::fs::read_to_string(runtime.config_path()) {
        Ok(text) => toml::from_str::<Config>(&text).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(format!("Cannot read Android config: {e}")),
    };
    let event_loop = EventLoop::<Event>::with_user_event()
        .with_android_app(app.clone())
        .build()
        .map_err(|e| e.to_string())?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let input_proxy = event_loop.create_proxy();
    let ime = AndroidIme::new(app.clone(), move |event| {
        let _ = input_proxy.send_event(Event::Input(event));
    })?;
    let native_libraries = ime.native_library_dir()?;
    if native_libraries.join("libkokuban_ssh.so").is_file() {
        if let Err(error) = runtime.enable_native_tools(&native_libraries) {
            let message = format!("Could not enable packaged tools: {error}");
            let _ = ime.show_error(&message);
            return Err(message);
        }
    } else {
        log::warn!("Packaged SSH executable is unavailable in this APK");
    }
    let metrics = FrameMetrics::new(
        runtime
            .config_path()
            .with_file_name("trace-frames")
            .exists(),
    );
    let columns = config.window.columns.clamp(1, 512);
    let rows = config.window.rows.clamp(1, 256);
    let mut grid = Grid::new(
        columns as usize,
        rows as usize,
        config.window.scrollback_lines.min(50_000),
    );
    let foreground = ColorConfig::parse_hex(&config.colors.foreground);
    let background = ColorConfig::parse_hex(&config.colors.background);
    grid.default_fg_hex = format!(
        "#{:02x}{:02x}{:02x}",
        foreground.0, foreground.1, foreground.2
    );
    grid.default_bg_hex = format!(
        "#{:02x}{:02x}{:02x}",
        background.0, background.1, background.2
    );
    let grid = Arc::new(Mutex::new(grid));
    let support = GraphicsSupport {
        kitty: config.images.kitty_graphics_enabled(),
        sixel: config.images.sixel_graphics_enabled(),
    };
    let mut images = AndroidImages::new(&config.images);
    let image_store = images.store.clone();
    let pty = Arc::new(
        runtime
            .spawn_shell(columns, rows, support.kitty, support.sixel)
            .map_err(|e| e.to_string())?,
    );
    log::info!("shell started");
    let pending = Arc::new(AtomicBool::new(false));
    let proxy = event_loop.create_proxy();
    let writer = TerminalWriter::spawn(pty.clone(), move |exit| {
        if !matches!(exit, WriterExit::Shutdown) {
            let _ = proxy.send_event(Event::Finished(Some(format!("PTY writer: {exit:?}"))));
        }
    })
    .map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let exit_proxy = proxy.clone();
    let reader_pending = pending.clone();
    let reader = TerminalReader::spawn_with_graphics(
        pty.clone(),
        grid.clone(),
        support,
        move |event, grid| images.process(event, grid),
        move || {
            if !reader_pending.swap(true, Ordering::AcqRel) {
                let _ = proxy.send_event(Event::Updated);
            }
        },
        move |exit| {
            let error = match exit {
                ReaderExit::Shutdown | ReaderExit::Eof => None,
                _ => Some(format!("PTY reader: {exit:?}")),
            };
            let _ = exit_proxy.send_event(Event::Finished(error));
        },
    )
    .map_err(|e| e.to_string())?;
    let mut terminal = AndroidWindow {
        surface: None,
        context: None,
        window: None,
        atlas: None,
        toolbar_atlas: None,
        app,
        ime,
        input: InputState::default(),
        viewport: None,
        config,
        colors: TerminalColors::new(foreground, background),
        grid,
        image_store,
        pty,
        writer: Some(writer),
        reader: Some(reader),
        pending,
        metrics,
        modifiers: ModifiersState::empty(),
        control: false,
        keyboard: false,
        touch: None,
        touch_contacts: TouchContacts::default(),
        accessible_controls: Vec::new(),
        accessible_focus: None,
        toolbar_page: 0,
        selecting: false,
        selection: SelectionState::default(),
        mouse_position: None,
        mouse_press: None,
        mouse_click_origin: None,
        mouse_click_canceled: false,
        last_mouse_cell: None,
        wheel_remainder: 0.0,
        wheel_route: None,
        focused: None,
        surface_size: (0, 0),
        first_frame: false,
        error: None,
    };
    let result = event_loop.run_app(&mut terminal).map_err(|e| e.to_string());
    terminal.shutdown();
    result?;
    terminal.error.take().map_or(Ok(()), Err)
}

struct AndroidWindow {
    // This order also ensures native window references drop after the surface.
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    context: Option<Context<Arc<Window>>>,
    window: Option<Arc<Window>>,
    atlas: Option<GlyphAtlas>,
    toolbar_atlas: Option<GlyphAtlas>,
    app: AndroidApp,
    ime: AndroidIme,
    input: InputState,
    viewport: Option<[u32; 4]>,
    config: Config,
    colors: TerminalColors,
    grid: Arc<Mutex<Grid>>,
    image_store: Arc<Mutex<SoftwareGraphics>>,
    pty: Arc<Pty>,
    writer: Option<TerminalWriter>,
    reader: Option<TerminalReader>,
    pending: Arc<AtomicBool>,
    metrics: FrameMetrics,
    modifiers: ModifiersState,
    control: bool,
    keyboard: bool,
    touch: Option<TouchGesture>,
    touch_contacts: TouchContacts,
    accessible_controls: Vec<AccessibleControl>,
    accessible_focus: Option<Control>,
    toolbar_page: usize,
    selecting: bool,
    selection: SelectionState,
    mouse_position: Option<(DeviceId, f64, f64)>,
    mouse_press: Option<(DeviceId, MouseAction)>,
    mouse_click_origin: Option<(f64, f64)>,
    mouse_click_canceled: bool,
    last_mouse_cell: Option<(u8, usize, usize)>,
    wheel_remainder: f64,
    wheel_route: Option<MouseWheelRoute>,
    focused: Option<bool>,
    surface_size: (u32, u32),
    first_frame: bool,
    error: Option<String>,
}

#[derive(Clone, Copy)]
enum MouseAction {
    Toolbar(Control),
    Selection,
    Terminal { button: u8, encoding: MouseEncoding },
}

impl AndroidWindow {
    fn shutdown(&mut self) {
        if let Some(reader) = &self.reader {
            reader.request_shutdown();
        }
        if let Some(writer) = &mut self.writer {
            writer.request_shutdown();
        }
        if let Some(reader) = self.reader.take() {
            if reader.shutdown_and_join().is_err() {
                log::error!("reader panicked during shutdown");
            }
        }
        if let Some(writer) = self.writer.take() {
            if writer.shutdown_and_join().is_err() {
                log::error!("writer panicked during shutdown");
            }
        }
    }

    fn redraw(&self) {
        if self.surface.is_some() {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    fn send(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(writer) = &self.writer {
            if let Err(error) = writer.enqueue(bytes) {
                self.error = Some(format!("Input queue: {error}"));
                log::error!("{}", self.error.as_deref().unwrap_or("Input failed"));
            } else {
                self.metrics.input();
                self.selection.clear();
                if let Ok(mut grid) = self.grid.lock() {
                    grid.scroll_to_bottom();
                }
            }
        }
        self.redraw();
    }

    fn input_modifiers(&self) -> InputModifiers {
        InputModifiers::new(
            self.modifiers.shift_key(),
            self.modifiers.alt_key(),
            self.control || self.modifiers.control_key(),
        )
    }

    fn key(&mut self, key: TerminalKey) {
        let application = self
            .grid
            .lock()
            .map(|g| g.application_cursor_keys)
            .unwrap_or(false);
        let bytes = android_input::encode_key(key, application, self.input_modifiers());
        if !bytes.is_empty() {
            self.control = false;
            self.send(bytes);
        }
    }

    fn text(&mut self, text: &str) {
        let bytes = android_input::encode_text(text, self.input_modifiers());
        if !bytes.is_empty() {
            self.control = false;
            self.send(bytes);
        }
    }

    fn input_event(&mut self, event: ImeEvent) {
        match &event {
            ImeEvent::Control { id, action } => {
                if let Some(control) = Control::from_id(*id) {
                    match action {
                        0 => self.activate_control(control),
                        1 => self.accessible_focus = Some(control),
                        2 if self.accessible_focus == Some(control) => self.accessible_focus = None,
                        _ => {}
                    }
                }
            }
            ImeEvent::Viewport {
                left,
                top,
                right,
                bottom,
                keyboard,
            } => {
                self.viewport = Some([*left, *top, *right, *bottom]);
                self.keyboard = *keyboard;
            }
            ImeEvent::Clipboard(text) => {
                let bracketed = self.grid.lock().map(|g| g.bracketed_paste).unwrap_or(false);
                if text.len() + 12 <= crate::terminal_writer::TERMINAL_INPUT_LOSSLESS_BYTE_HEADROOM
                {
                    let mut bytes = Vec::with_capacity(text.len() + 12);
                    if bracketed {
                        bytes.extend_from_slice(b"\x1b[200~");
                    }
                    bytes.extend_from_slice(text.as_bytes());
                    if bracketed {
                        bytes.extend_from_slice(b"\x1b[201~");
                    }
                    self.send(bytes);
                }
            }
            _ => {
                let application = self
                    .grid
                    .lock()
                    .map(|g| g.application_cursor_keys)
                    .unwrap_or(false);
                let bytes = self
                    .input
                    .handle(&event, application, self.input_modifiers());
                if !bytes.is_empty() {
                    self.control = false;
                    self.send(bytes);
                }
            }
        }
        self.redraw();
    }

    fn content_bounds(&self, width: u32, height: u32) -> [u32; 4] {
        let rect = self.app.content_rect();
        let [left, top, right, bottom] = self.viewport.unwrap_or([
            rect.left.max(0) as u32,
            rect.top.max(0) as u32,
            rect.right.max(0) as u32,
            rect.bottom.max(0) as u32,
        ]);
        let right = right.min(width);
        let bottom = bottom.min(height);
        if left < right && top < bottom {
            [left, top, right, bottom]
        } else {
            [0, 0, width, height]
        }
    }

    fn controls(&self) -> Option<ToolbarLayout> {
        let window = self.window.as_ref()?;
        let size = window.inner_size();
        Some(ToolbarLayout::new(
            Rect::from_bounds(self.content_bounds(size.width, size.height)),
            window.scale_factor(),
            self.toolbar_page,
        ))
    }

    fn draw(&mut self) -> Result<(), String> {
        let render_started = Instant::now();
        self.pending.store(false, Ordering::Release);
        let Some(window) = &self.window else {
            return Ok(());
        };
        if self.surface.is_none() {
            return Ok(());
        }
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        // Bound the two full-frame CPU buffers before allocating either one.
        if u64::from(size.width) * u64::from(size.height) > 16_777_216 {
            return Err("Android surface exceeds the 16 megapixel memory budget".into());
        }
        let controls = self.controls().ok_or("Controls unavailable")?;
        let Rect {
            left,
            top,
            right,
            bottom,
        } = controls.terminal;
        let view_width = right - left;
        let view_height = bottom - top;
        let atlas = self.atlas.as_mut().ok_or("Font atlas unavailable")?;
        let cell_width = atlas.cell_width.ceil().max(1.0) as u32;
        let cell_height = atlas.cell_height.ceil().max(1.0) as u32;
        let columns = (view_width / cell_width).clamp(1, 512) as u16;
        let rows = (view_height / cell_height).clamp(1, 256) as u16;
        let (cells, cursor, images, selected) = {
            let mut grid = self.grid.lock().map_err(|_| "Grid lock poisoned")?;
            if grid.cols() != columns as usize || grid.rows() != rows as usize {
                // Do not commit new grid dimensions if the PTY resize fails.
                self.pty.resize(columns, rows).map_err(|e| e.to_string())?;
                grid.resize(columns as usize, rows as usize);
                self.selection.clear();
                log::info!("terminal resized: {columns}x{rows}");
            }
            grid.cell_pixel_width = cell_width as u16;
            grid.cell_pixel_height = cell_height as u16;
            let cells = (0..grid.rows())
                .flat_map(|row| {
                    let grid = &grid;
                    (0..grid.cols()).map(move |col| *grid.visible_cell(row, col))
                })
                .collect::<Vec<_>>();
            let cursor = (grid.cursor_visible && grid.scroll_offset == 0)
                .then_some((grid.cursor_col as u32, grid.cursor_row as u32));
            let selected = cells
                .iter()
                .enumerate()
                .map(|(index, cell)| {
                    let row = index / usize::from(columns);
                    let col = index % usize::from(columns);
                    self.selection
                        .contains(row, col, grid.scroll_offset, grid.scrollback_len())
                        || (cell.flags.contains(CellFlags::WIDE_CONT)
                            && col > 0
                            && self.selection.contains(
                                row,
                                col - 1,
                                grid.scroll_offset,
                                grid.scrollback_len(),
                            ))
                })
                .collect::<Vec<_>>();
            (
                cells,
                cursor,
                self.image_store
                    .lock()
                    .map_err(|_| "Image store lock poisoned")?
                    .snapshot(&grid, (cell_width as u16, cell_height as u16)),
                selected,
            )
        };
        let surface = self.surface.as_mut().ok_or("Surface unavailable")?;
        if self.surface_size != (size.width, size.height) {
            surface
                .resize(
                    NonZeroU32::new(size.width).unwrap(),
                    NonZeroU32::new(size.height).unwrap(),
                )
                .map_err(|e| e.to_string())?;
            self.surface_size = (size.width, size.height);
        }
        let mut frame = surface.buffer_mut().map_err(|e| e.to_string())?;
        let frame_size = (size.width, size.height);
        frame.fill(rgb(self.colors.default_background()));
        for (index, cell) in cells.iter().enumerate() {
            let x = left + (index as u32 % columns as u32) * cell_width;
            let y = top + (index as u32 / columns as u32) * cell_height;
            let colors = self
                .colors
                .resolve_cell_colors(cell.fg, cell.bg, cell.flags);
            fill_rect(
                &mut frame,
                frame_size,
                (x as i32, y as i32),
                (cell_width, cell_height),
                rgb(colors.background),
                255,
            );
        }
        for image in images.iter().filter(|image| image.z_index < 0) {
            draw_image_rgba(
                &mut frame,
                frame_size,
                &image.pixels,
                image.size,
                (
                    image.rectangle.0 + left as f32,
                    image.rectangle.1 + top as f32,
                    image.rectangle.2,
                    image.rectangle.3,
                ),
            );
        }
        for (index, cell) in cells.iter().enumerate() {
            let x = left + (index as u32 % columns as u32) * cell_width;
            let y = top + (index as u32 / columns as u32) * cell_height;
            let colors = self
                .colors
                .resolve_cell_colors(cell.fg, cell.bg, cell.flags);
            if !cell
                .flags
                .intersects(CellFlags::HIDDEN | CellFlags::WIDE_CONT)
            {
                let glyph = atlas.get_or_insert(GlyphKey {
                    c: cell.c,
                    bold: cell.flags.contains(CellFlags::BOLD),
                    italic: cell.flags.contains(CellFlags::ITALIC),
                });
                draw_glyph_a8(
                    &mut frame,
                    frame_size,
                    &atlas.pixels,
                    (atlas.width, atlas.height),
                    glyph,
                    (
                        x as i32 + glyph.bearing_x,
                        y as i32 + atlas.ascent.round() as i32 + glyph.bearing_y,
                    ),
                    rgb(colors.foreground),
                );
                if cell.flags.contains(CellFlags::UNDERLINE) {
                    fill_rect(
                        &mut frame,
                        frame_size,
                        (x as i32, (y + cell_height - 2) as i32),
                        (cell_width, 1),
                        rgb(colors.foreground),
                        255,
                    );
                }
            }
        }
        for image in images.iter().filter(|image| image.z_index >= 0) {
            draw_image_rgba(
                &mut frame,
                frame_size,
                &image.pixels,
                image.size,
                (
                    image.rectangle.0 + left as f32,
                    image.rectangle.1 + top as f32,
                    image.rectangle.2,
                    image.rectangle.3,
                ),
            );
        }
        for (index, selected) in selected.iter().enumerate() {
            if *selected {
                fill_rect(
                    &mut frame,
                    frame_size,
                    (
                        (left + index as u32 % u32::from(columns) * cell_width) as i32,
                        (top + index as u32 / u32::from(columns) * cell_height) as i32,
                    ),
                    (cell_width, cell_height),
                    0x5992bd,
                    150,
                );
            }
        }
        if let Some((col, row)) = cursor {
            let mut preedit_col = col;
            let mut cluster_col = col;
            for c in self.input.preedit().chars().take(columns as usize * 4) {
                let width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1) as u32;
                if width > 0 && preedit_col >= u32::from(columns) {
                    break;
                }
                if width > 0 {
                    cluster_col = preedit_col;
                }
                let glyph = atlas.get_or_insert(GlyphKey {
                    c,
                    bold: false,
                    italic: false,
                });
                let x = left + cluster_col * cell_width;
                let y = top + row * cell_height;
                if width > 0 {
                    fill_rect(
                        &mut frame,
                        frame_size,
                        (x as i32, y as i32),
                        (width * cell_width, cell_height),
                        0x272d32,
                        255,
                    );
                }
                draw_glyph_a8(
                    &mut frame,
                    frame_size,
                    &atlas.pixels,
                    (atlas.width, atlas.height),
                    glyph,
                    (
                        x as i32 + glyph.bearing_x,
                        y as i32 + atlas.ascent.round() as i32 + glyph.bearing_y,
                    ),
                    0xffffff,
                );
                fill_rect(
                    &mut frame,
                    frame_size,
                    (x as i32, (y + cell_height - 2) as i32),
                    (width * cell_width, 2),
                    0x6cd4a0,
                    255,
                );
                preedit_col += width;
            }
            fill_rect(
                &mut frame,
                frame_size,
                (
                    (left + col * cell_width) as i32,
                    (top + (row + 1) * cell_height - 2) as i32,
                ),
                (cell_width, 2),
                0xffffff,
                255,
            );
        }
        // Native raster helpers clip to the surface. Restore safe-area/toolbar
        // pixels after media composition so oversized images cannot cover controls.
        mask_outside(
            &mut frame,
            frame_size,
            controls.terminal,
            rgb(self.colors.default_background()),
        );
        let toolbar_atlas = self
            .toolbar_atlas
            .as_mut()
            .ok_or("Toolbar font unavailable")?;
        let toolbar_cell_width = toolbar_atlas.cell_width.ceil().max(1.0) as u32;
        let toolbar_cell_height = toolbar_atlas.cell_height.ceil().max(1.0) as u32;
        let pressed = self
            .touch
            .as_ref()
            .and_then(|gesture| match gesture.region {
                TouchRegion::Toolbar(control) if !gesture.dragged => Some(control),
                _ => None,
            })
            .or_else(|| {
                self.mouse_press.and_then(|(_, action)| match action {
                    MouseAction::Toolbar(control) if !self.mouse_click_canceled => Some(control),
                    _ => None,
                })
            });
        for button in &controls.buttons {
            let selected = match button.control {
                Control::Control => self.control,
                Control::Keyboard => self.keyboard,
                Control::Select => self.selecting,
                _ => false,
            };
            let disabled = button.control == Control::Copy && !self.selection.is_active();
            let rect = button.rect;
            fill_rect(
                &mut frame,
                frame_size,
                (rect.left as i32, rect.top as i32),
                (
                    rect.width().saturating_sub(1),
                    rect.height().saturating_sub(1),
                ),
                if pressed == Some(button.control) {
                    0x526674
                } else if selected {
                    0x315675
                } else {
                    0x272d32
                },
                255,
            );
            // Active toggles have a persistent underline in addition to color.
            if selected {
                fill_rect(
                    &mut frame,
                    frame_size,
                    (rect.left as i32, rect.bottom.saturating_sub(4) as i32),
                    (rect.width().saturating_sub(1), 3),
                    0x6cd4a0,
                    255,
                );
            }
            let label = button.control.label();
            if self.accessible_focus == Some(button.control) {
                for (origin, size) in [
                    ((rect.left, rect.top), (rect.width(), 3)),
                    (
                        (rect.left, rect.bottom.saturating_sub(3)),
                        (rect.width(), 3),
                    ),
                    ((rect.left, rect.top), (3, rect.height())),
                    ((rect.right.saturating_sub(3), rect.top), (3, rect.height())),
                ] {
                    fill_rect(
                        &mut frame,
                        frame_size,
                        (origin.0 as i32, origin.1 as i32),
                        size,
                        0xffffff,
                        255,
                    );
                }
            }
            let text_width = label.len() as u32 * toolbar_cell_width;
            let text_left = rect.left + rect.width().saturating_sub(text_width) / 2;
            for (index, c) in label
                .chars()
                .take((rect.width() / toolbar_cell_width) as usize)
                .enumerate()
            {
                let glyph = toolbar_atlas.get_or_insert(GlyphKey {
                    c,
                    bold: selected,
                    italic: false,
                });
                draw_glyph_a8(
                    &mut frame,
                    frame_size,
                    &toolbar_atlas.pixels,
                    (toolbar_atlas.width, toolbar_atlas.height),
                    glyph,
                    (
                        (text_left + index as u32 * toolbar_cell_width) as i32 + glyph.bearing_x,
                        (rect.top + rect.height().saturating_sub(toolbar_cell_height) / 2) as i32
                            + toolbar_atlas.ascent.round() as i32
                            + glyph.bearing_y,
                    ),
                    if disabled { 0xa4abb0 } else { 0xf0f0f0 },
                );
            }
        }
        window.pre_present_notify();
        frame.present().map_err(|e| e.to_string())?;
        self.metrics.presented(render_started);
        let accessible_controls = controls
            .buttons
            .iter()
            .map(|button| AccessibleControl {
                id: button.control as i32,
                label: button.control.accessibility_label(),
                bounds: [
                    button.rect.left,
                    button.rect.top,
                    button.rect.right,
                    button.rect.bottom,
                ],
                selected: match button.control {
                    Control::Control => self.control,
                    Control::Keyboard => self.keyboard,
                    Control::Select => self.selecting,
                    _ => false,
                },
                enabled: button.control != Control::Copy || self.selection.is_active(),
                toggle: matches!(
                    button.control,
                    Control::Control | Control::Keyboard | Control::Select
                ),
            })
            .collect::<Vec<_>>();
        if accessible_controls != self.accessible_controls {
            match self.ime.update_controls(&accessible_controls) {
                Ok(()) => self.accessible_controls = accessible_controls,
                Err(error) => log::warn!("Accessibility controls: {error}"),
            }
        }
        if !self.first_frame {
            log::info!("first frame presented");
            self.first_frame = true;
        }
        Ok(())
    }

    fn platform_error(&self, result: Result<(), String>) {
        if let Err(error) = result {
            log::error!("{error}");
            let _ = self.ime.show_error(&error);
        }
    }

    fn activate_control(&mut self, control: Control) {
        match control {
            Control::Escape if self.selecting => {
                self.selecting = false;
                self.selection.clear();
            }
            Control::Escape => self.key(TerminalKey::Escape),
            Control::Tab => self.key(TerminalKey::Tab),
            Control::Control => self.control = !self.control,
            Control::Left => self.key(TerminalKey::Left),
            Control::Down => self.key(TerminalKey::Down),
            Control::Up => self.key(TerminalKey::Up),
            Control::Right => self.key(TerminalKey::Right),
            Control::PageUp => self.key(TerminalKey::PageUp),
            Control::PageDown => self.key(TerminalKey::PageDown),
            Control::Home => self.key(TerminalKey::Home),
            Control::End => self.key(TerminalKey::End),
            Control::Insert => self.key(TerminalKey::Insert),
            Control::Delete => self.key(TerminalKey::Delete),
            Control::Keyboard => {
                self.keyboard = !self.keyboard;
                self.platform_error(self.ime.set_visible(self.keyboard));
            }
            Control::Select => {
                self.selecting = !self.selecting;
                self.selection.clear();
            }
            Control::Copy => {
                if self.selection.is_active() {
                    let text = self.grid.lock().map(|grid| self.selection.get_text(&grid));
                    if let Ok(text) = text {
                        self.platform_error(self.ime.copy(&text));
                    }
                }
            }
            Control::Paste => self.platform_error(self.ime.paste()),
            Control::More => {
                if let Some(layout) = self.controls() {
                    self.toolbar_page = (self.toolbar_page + 1) % layout.page_count;
                }
            }
        }
        self.redraw();
    }

    fn visible_cell(&self, x: f64, y: f64, clamp: bool) -> Option<(usize, usize)> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let rect = self.controls()?.terminal;
        if rect.width() == 0 || rect.height() == 0 || (!clamp && !rect.contains(x, y)) {
            return None;
        }
        let atlas = self.atlas.as_ref()?;
        let x = x.clamp(f64::from(rect.left), f64::from(rect.right - 1)) - f64::from(rect.left);
        let y = y.clamp(f64::from(rect.top), f64::from(rect.bottom - 1)) - f64::from(rect.top);
        let grid = self.grid.lock().ok()?;
        Some((
            (x / f64::from(atlas.cell_width.ceil().max(1.0))) as usize,
            (y / f64::from(atlas.cell_height.ceil().max(1.0))) as usize,
        ))
        .map(|(col, row)| {
            (
                col.min(grid.cols().saturating_sub(1)),
                row.min(grid.rows().saturating_sub(1)),
            )
        })
    }

    fn selection_point(&self, x: f64, y: f64) -> Option<GridPoint> {
        let (mut col, row) = self.visible_cell(x, y, true)?;
        let grid = self.grid.lock().ok()?;
        if col > 0
            && grid
                .visible_cell(row, col)
                .flags
                .contains(CellFlags::WIDE_CONT)
        {
            col -= 1;
        }
        Some(GridPoint {
            row: grid.scrollback_len() as i64 - grid.scroll_offset as i64 + row as i64,
            col,
        })
    }

    fn touch_event(&mut self, id: u64, phase: TouchPhase, x: f64, y: f64) {
        let accepted = match phase {
            TouchPhase::Started => self.touch_contacts.begin(id),
            TouchPhase::Ended | TouchPhase::Cancelled => self.touch_contacts.end(id),
            TouchPhase::Moved => true,
        };
        if !accepted {
            self.touch = None;
            self.redraw();
            return;
        }
        let Some(layout) = self.controls() else {
            return;
        };
        let scale = self
            .window
            .as_ref()
            .map_or(1.0, |window| window.scale_factor());
        let line_height = self
            .atlas
            .as_ref()
            .map_or(20.0, |atlas| f64::from(atlas.cell_height.ceil()));
        match phase {
            TouchPhase::Started => {
                if self.touch.is_some() {
                    self.touch = None;
                    self.redraw();
                    return;
                }
                let region = if let Some(control) = layout.hit(x, y) {
                    TouchRegion::Toolbar(control)
                } else if layout.terminal.contains(x, y) {
                    TouchRegion::Terminal
                } else {
                    return;
                };
                if matches!(region, TouchRegion::Terminal) && self.selecting {
                    if let Some(point) = self.selection_point(x, y) {
                        self.selection.start(point);
                    }
                }
                self.touch = Some(TouchGesture::new(id, region, x, y));
            }
            TouchPhase::Moved => {
                let Some(gesture) = &mut self.touch else {
                    return;
                };
                if gesture.id != id {
                    return;
                }
                let lines = gesture.move_to(x, y, 8.0 * scale, line_height);
                let terminal = matches!(gesture.region, TouchRegion::Terminal);
                if terminal && self.selecting {
                    if let Some(point) = self.selection_point(x, y) {
                        self.selection.update(point);
                    }
                } else if terminal && lines != 0 {
                    self.scroll(lines, x, y);
                }
            }
            TouchPhase::Ended => {
                if self.touch.as_ref().is_none_or(|gesture| gesture.id != id) {
                    return;
                }
                let Some(mut gesture) = self.touch.take() else {
                    return;
                };
                let lines = gesture.move_to(x, y, 8.0 * scale, line_height);
                if let Some(control) = gesture.released_control(&layout, x, y) {
                    self.activate_control(control);
                } else if matches!(gesture.region, TouchRegion::Terminal) {
                    if self.selecting {
                        if let Some(point) = self.selection_point(x, y) {
                            self.selection.update(point);
                        }
                    } else if lines != 0 {
                        self.scroll(lines, x, y);
                    } else if !gesture.dragged && layout.terminal.contains(x, y) {
                        self.keyboard = true;
                        self.platform_error(self.ime.set_visible(true));
                    }
                }
            }
            TouchPhase::Cancelled => {
                if self.touch.as_ref().is_some_and(|gesture| gesture.id == id) {
                    self.touch = None;
                }
            }
        }
        self.redraw();
    }

    // Focus and mouse protocol reports must not reset local selection/scrollback.
    fn report(&mut self, bytes: Vec<u8>, lossy_motion: bool) {
        if bytes.is_empty() {
            return;
        }
        if let Some(writer) = &self.writer {
            let result = if lossy_motion {
                writer.enqueue_nonfatal(bytes)
            } else {
                writer.enqueue(bytes)
            };
            if let Err(error) = result {
                if !lossy_motion {
                    self.error = Some(format!("Input queue: {error}"));
                }
                log::warn!("Input report: {error}");
            }
        }
    }

    fn mouse_button_flags(&self, button: u8) -> u8 {
        mouse_button_with_modifier_flags(
            button,
            self.modifiers.shift_key(),
            self.modifiers.alt_key(),
            self.modifiers.control_key(),
        )
    }

    fn scroll(&mut self, lines: i32, x: f64, y: f64) {
        if lines == 0 {
            return;
        }
        let route = self
            .grid
            .lock()
            .map(|grid| {
                if self.selecting || self.modifiers.shift_key() {
                    MouseWheelRoute::Scrollback
                } else {
                    mouse_wheel_route(&grid)
                }
            })
            .unwrap_or(MouseWheelRoute::Scrollback);
        match route {
            MouseWheelRoute::Scrollback => {
                if let Ok(mut grid) = self.grid.lock() {
                    if lines > 0 {
                        grid.scroll_viewport_up(lines as usize);
                    } else {
                        grid.scroll_viewport_down(lines.unsigned_abs() as usize);
                    }
                }
                self.redraw();
            }
            MouseWheelRoute::AlternateScroll {
                application_cursor_keys,
            } => {
                self.report(
                    encode_alternate_scroll_steps(lines, application_cursor_keys),
                    false,
                );
            }
            MouseWheelRoute::Terminal(encoding) => {
                if let Some((col, row)) = self.visible_cell(x, y, false) {
                    let button = self.mouse_button_flags(if lines > 0 {
                        MOUSE_WHEEL_UP
                    } else {
                        MOUSE_WHEEL_DOWN
                    });
                    let bytes = encode_mouse_event(button, col + 1, row + 1, true, encoding)
                        .repeat(lines.unsigned_abs().min(MAX_WHEEL_STEPS_PER_EVENT) as usize);
                    self.report(bytes, false);
                }
            }
        }
    }

    fn mouse_wheel(&mut self, device: DeviceId, delta: MouseScrollDelta, phase: TouchPhase) {
        if phase == TouchPhase::Cancelled {
            self.wheel_remainder = 0.0;
            return;
        }
        if self
            .mouse_press
            .is_some_and(|(pressed, _)| pressed != device)
        {
            return;
        }
        let route = self.grid.lock().ok().map(|grid| mouse_wheel_route(&grid));
        if route != self.wheel_route || phase == TouchPhase::Started {
            self.wheel_remainder = 0.0;
        }
        self.wheel_route = route;
        let delta = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y),
            MouseScrollDelta::PixelDelta(position) => {
                position.y
                    / self
                        .atlas
                        .as_ref()
                        .map_or(20.0, |atlas| f64::from(atlas.cell_height.ceil().max(1.0)))
            }
        };
        if !delta.is_finite() {
            return;
        }
        self.wheel_remainder += delta;
        let lines = self.wheel_remainder.trunc().clamp(-32.0, 32.0) as i32;
        self.wheel_remainder -= f64::from(lines);
        if let Some((pointer, x, y)) = self.mouse_position {
            if pointer == device {
                self.scroll(lines, x, y);
            }
        } else if let Some(layout) = self.controls() {
            self.scroll(
                lines,
                f64::from(layout.terminal.left),
                f64::from(layout.terminal.top),
            );
        }
    }

    fn mouse_moved(&mut self, device: DeviceId, x: f64, y: f64) {
        if self
            .mouse_press
            .is_some_and(|(pressed, _)| pressed != device)
        {
            return;
        }
        self.mouse_position = Some((device, x, y));
        if let Some((start_x, start_y)) = self.mouse_click_origin {
            let scale = self
                .window
                .as_ref()
                .map_or(1.0, |window| window.scale_factor());
            self.mouse_click_canceled |= (x - start_x).hypot(y - start_y) > 8.0 * scale;
        }
        if matches!(self.mouse_press, Some((_, MouseAction::Selection))) {
            if let Some(point) = self.selection_point(x, y) {
                self.selection.update(point);
                self.redraw();
            }
            return;
        }
        if matches!(self.mouse_press, Some((_, MouseAction::Toolbar(_)))) {
            self.redraw();
            return;
        }
        let Some((col, row)) = self.visible_cell(x, y, false) else {
            self.last_mouse_cell = None;
            return;
        };
        let mode = self
            .grid
            .lock()
            .ok()
            .map(|grid| (grid.mouse_tracking, grid.mouse_encoding));
        let Some((tracking, encoding)) = mode else {
            return;
        };
        if self.selecting || self.modifiers.shift_key() {
            return;
        }
        let button = match self.mouse_press {
            Some((_, MouseAction::Terminal { button, .. }))
                if matches!(
                    tracking,
                    MouseTracking::ButtonEvent | MouseTracking::AnyEvent
                ) =>
            {
                button
            }
            None if tracking == MouseTracking::AnyEvent => 3,
            _ => return,
        };
        let button = self.mouse_button_flags(button | 32);
        let position = (button, col, row);
        if self.last_mouse_cell == Some(position) {
            return;
        }
        self.last_mouse_cell = Some(position);
        self.report(
            encode_mouse_event(button, col + 1, row + 1, true, encoding),
            true,
        );
    }

    fn mouse_button(&mut self, device: DeviceId, state: ElementState, button: MouseButton) {
        let Some((pointer, x, y)) = self.mouse_position else {
            return;
        };
        if pointer != device {
            return;
        }
        if state == ElementState::Released {
            if self
                .mouse_press
                .is_none_or(|(pressed, _)| pressed != device)
            {
                return;
            }
            let expected = match self.mouse_press {
                Some((_, MouseAction::Terminal { button: 1, .. })) => MouseButton::Middle,
                Some((_, MouseAction::Terminal { button: 2, .. })) => MouseButton::Right,
                _ => MouseButton::Left,
            };
            if button != expected {
                return;
            }
            let Some((_, action)) = self.mouse_press.take() else {
                return;
            };
            match action {
                MouseAction::Toolbar(control) if !self.mouse_click_canceled => {
                    if self
                        .controls()
                        .is_some_and(|layout| layout.hit(x, y) == Some(control))
                    {
                        self.activate_control(control);
                    }
                }
                MouseAction::Terminal { button, encoding } => {
                    if let Some((col, row)) = self.visible_cell(x, y, true) {
                        self.report(
                            encode_mouse_event(
                                self.mouse_button_flags(button),
                                col + 1,
                                row + 1,
                                false,
                                encoding,
                            ),
                            false,
                        );
                    }
                }
                _ => {}
            }
            self.mouse_click_origin = None;
            self.last_mouse_cell = None;
            self.redraw();
            return;
        }
        if self.mouse_press.is_some() {
            return;
        }
        self.mouse_click_origin = Some((x, y));
        self.mouse_click_canceled = false;
        if let Some(control) = self.controls().and_then(|layout| layout.hit(x, y)) {
            if button == MouseButton::Left {
                self.mouse_press = Some((device, MouseAction::Toolbar(control)));
                self.redraw();
            }
            return;
        }
        let Some((col, row)) = self.visible_cell(x, y, false) else {
            return;
        };
        let mode = self
            .grid
            .lock()
            .ok()
            .map(|grid| (grid.mouse_tracking, grid.mouse_encoding));
        let Some((tracking, encoding)) = mode else {
            return;
        };
        if tracking != MouseTracking::None && !self.selecting && !self.modifiers.shift_key() {
            let code = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                _ => return,
            };
            self.mouse_press = Some((
                device,
                MouseAction::Terminal {
                    button: code,
                    encoding,
                },
            ));
            self.last_mouse_cell = None;
            self.report(
                encode_mouse_event(
                    self.mouse_button_flags(code),
                    col + 1,
                    row + 1,
                    true,
                    encoding,
                ),
                false,
            );
        } else if button == MouseButton::Left {
            if let Some(point) = self.selection_point(x, y) {
                self.selection.start(point);
                self.mouse_press = Some((device, MouseAction::Selection));
                self.redraw();
            }
        } else if button == MouseButton::Middle {
            self.platform_error(self.ime.paste());
        }
    }

    fn focus_changed(&mut self, focused: bool) {
        if self.focused == Some(focused) {
            return;
        }
        self.focused = Some(focused);
        if !focused {
            self.touch = None;
            self.touch_contacts.clear();
            self.mouse_press = None;
            self.mouse_click_origin = None;
            self.mouse_position = None;
            self.last_mouse_cell = None;
            self.wheel_remainder = 0.0;
            self.control = false;
            self.modifiers = ModifiersState::empty();
        }
        if self.grid.lock().is_ok_and(|grid| grid.focus_events) {
            self.report(
                if focused {
                    b"\x1b[I".to_vec()
                } else {
                    b"\x1b[O".to_vec()
                },
                false,
            );
        }
        self.redraw();
    }
}

impl Drop for AndroidWindow {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl ApplicationHandler<Event> for AndroidWindow {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Wait);
        if event_loop.exiting() || self.surface.is_none() {
            return;
        }
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let update = (|| -> Result<_, String> {
            // Same grid -> graphics lock order as the PTY reader. Only visible
            // animations schedule work; suspension drops the surface above.
            let grid = self.grid.lock().map_err(|_| "Grid lock poisoned")?;
            let mut graphics = self
                .image_store
                .lock()
                .map_err(|_| "Image store lock poisoned")?;
            Ok(graphics.advance_animations(
                &grid,
                (grid.cell_pixel_width, grid.cell_pixel_height),
                Instant::now(),
            ))
        })();
        match update {
            Ok(update) => {
                if update.changed {
                    window.request_redraw();
                }
                if let Some(deadline) = update.next_deadline {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
            }
            Err(error) => {
                log::error!("Animation failed: {error}");
                let _ = self.ime.show_error(&error);
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        log::info!("resumed");
        if self.surface.is_some() {
            return;
        }
        let result = (|| -> Result<(), String> {
            let window = match &self.window {
                Some(window) => window.clone(),
                None => Arc::new(
                    event_loop
                        .create_window(Window::default_attributes().with_title("Kokuban"))
                        .map_err(|e| e.to_string())?,
                ),
            };
            let context = Context::new(window.clone()).map_err(|e| e.to_string())?;
            let surface = Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;
            let atlas = GlyphAtlas::new(
                &self.config.font.family,
                self.config.font.size.clamp(8.0, 32.0),
                window.scale_factor() as f32,
            )
            .map_err(|e| e.to_string())?;
            self.window = Some(window);
            self.context = Some(context);
            self.surface = Some(surface);
            self.atlas = Some(atlas);
            self.toolbar_atlas = Some(
                GlyphAtlas::new(
                    &self.config.font.family,
                    14.0,
                    self.window
                        .as_ref()
                        .map_or(1.0, |window| window.scale_factor() as f32),
                )
                .map_err(|error| error.to_string())?,
            );
            self.surface_size = (0, 0);
            self.redraw();
            Ok(())
        })();
        if let Err(error) = result {
            log::error!("Could not resume terminal: {error}");
            let _ = self.ime.show_error(&error);
            self.error = Some(error);
            event_loop.exit();
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        log::info!("suspended");
        self.focus_changed(false);
        self.metrics.suspend();
        self.surface = None;
        self.context = None;
        self.touch = None;
        self.modifiers = ModifiersState::empty();
        self.keyboard = false;
        self.surface_size = (0, 0);
        self.viewport = None;
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        match event {
            Event::Input(event) => self.input_event(event),
            Event::Updated => {
                self.metrics.output();
                self.pending.store(false, Ordering::Release);
                self.redraw();
            }
            Event::Finished(error) => {
                if let Some(error) = error {
                    log::error!("{error}");
                    self.error = Some(error);
                }
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.draw() {
                    log::error!("Presentation failed: {error}");
                    let _ = self.ime.show_error(&error);
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(_) => {
                self.touch = None;
                self.touch_contacts.clear();
                if matches!(self.mouse_press, Some((_, MouseAction::Toolbar(_)))) {
                    self.mouse_press = None;
                }
                self.last_mouse_cell = None;
                self.redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let fonts = GlyphAtlas::new(
                    &self.config.font.family,
                    self.config.font.size.clamp(8.0, 32.0),
                    scale_factor as f32,
                )
                .and_then(|atlas| {
                    GlyphAtlas::new(&self.config.font.family, 14.0, scale_factor as f32)
                        .map(|toolbar| (atlas, toolbar))
                });
                match fonts {
                    Ok((atlas, toolbar)) => {
                        self.atlas = Some(atlas);
                        self.toolbar_atlas = Some(toolbar);
                        self.redraw();
                    }
                    Err(error) => {
                        let message = format!("Could not scale terminal fonts: {error}");
                        let _ = self.ime.show_error(&message);
                        self.error = Some(message);
                        event_loop.exit();
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed => {
                let shortcut = if self.modifiers.control_key() && self.modifiers.shift_key() {
                    match &event.logical_key {
                        Key::Character(text) if text.eq_ignore_ascii_case("c") => {
                            Some(Control::Copy)
                        }
                        Key::Character(text) if text.eq_ignore_ascii_case("v") => {
                            Some(Control::Paste)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(control) = shortcut {
                    self.activate_control(control);
                } else if let Key::Named(named) = event.logical_key {
                    if let Some(key) = named_terminal_key(named) {
                        self.key(key);
                    } else if let Some(text) = event.text {
                        self.text(&text);
                    }
                } else if let Some(text) = event.text {
                    self.text(&text);
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => self.text(&text),
            WindowEvent::Focused(focused) => self.focus_changed(focused),
            WindowEvent::Touch(touch) => {
                self.touch_event(touch.id, touch.phase, touch.location.x, touch.location.y)
            }
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => self.mouse_moved(device_id, position.x, position.y),
            WindowEvent::CursorLeft { device_id } => {
                if self
                    .mouse_position
                    .is_some_and(|(pointer, _, _)| pointer == device_id)
                {
                    if self.mouse_press.is_none() {
                        self.mouse_position = None;
                    }
                    self.mouse_click_canceled = true;
                    self.last_mouse_cell = None;
                }
            }
            WindowEvent::MouseInput {
                device_id,
                state,
                button,
            } => self.mouse_button(device_id, state, button),
            WindowEvent::MouseWheel {
                device_id,
                delta,
                phase,
            } => self.mouse_wheel(device_id, delta, phase),
            _ => {}
        }
    }
}

fn rgb(color: (u8, u8, u8)) -> u32 {
    (u32::from(color.0) << 16) | (u32::from(color.1) << 8) | u32::from(color.2)
}

fn named_terminal_key(key: NamedKey) -> Option<TerminalKey> {
    Some(match key {
        NamedKey::Enter => TerminalKey::Enter,
        NamedKey::Backspace => TerminalKey::Backspace,
        NamedKey::Tab => TerminalKey::Tab,
        NamedKey::Escape => TerminalKey::Escape,
        NamedKey::ArrowUp => TerminalKey::Up,
        NamedKey::ArrowDown => TerminalKey::Down,
        NamedKey::ArrowLeft => TerminalKey::Left,
        NamedKey::ArrowRight => TerminalKey::Right,
        NamedKey::Home => TerminalKey::Home,
        NamedKey::End => TerminalKey::End,
        NamedKey::PageUp => TerminalKey::PageUp,
        NamedKey::PageDown => TerminalKey::PageDown,
        NamedKey::Insert => TerminalKey::Insert,
        NamedKey::Delete => TerminalKey::Delete,
        NamedKey::F1 => TerminalKey::Function(1),
        NamedKey::F2 => TerminalKey::Function(2),
        NamedKey::F3 => TerminalKey::Function(3),
        NamedKey::F4 => TerminalKey::Function(4),
        NamedKey::F5 => TerminalKey::Function(5),
        NamedKey::F6 => TerminalKey::Function(6),
        NamedKey::F7 => TerminalKey::Function(7),
        NamedKey::F8 => TerminalKey::Function(8),
        NamedKey::F9 => TerminalKey::Function(9),
        NamedKey::F10 => TerminalKey::Function(10),
        NamedKey::F11 => TerminalKey::Function(11),
        NamedKey::F12 => TerminalKey::Function(12),
        _ => return None,
    })
}
