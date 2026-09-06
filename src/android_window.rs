//! Android owns window lifetime and input; terminal state lives independently.
use crate::android_images::{image_store::ImageStore, AndroidImages};
use crate::android_ime::AndroidIme;
use crate::android_input::{self, ImeEvent, InputModifiers, InputState};
use crate::android_runtime::AndroidRuntime;
use crate::config::{ColorConfig, Config};
use crate::glyph_atlas::{GlyphAtlas, GlyphKey};
use crate::grid::cell::CellFlags;
use crate::grid::Grid;
use crate::input::keyboard::TerminalKey;
use crate::parser::ansi::GraphicsSupport;
use crate::pty::Pty;
use crate::software_raster::{draw_glyph_a8, draw_image_rgba, fill_rect};
use crate::terminal_colors::TerminalColors;
use crate::terminal_reader::{ReaderExit, TerminalReader};
use crate::terminal_writer::{TerminalWriter, WriterExit};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, TouchPhase, WindowEvent};
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
    let runtime = AndroidRuntime::prepare(&path).map_err(|e| e.to_string())?;
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
        modifiers: ModifiersState::empty(),
        control: false,
        keyboard: false,
        touch: None,
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
    app: AndroidApp,
    ime: AndroidIme,
    input: InputState,
    viewport: Option<[u32; 4]>,
    config: Config,
    colors: TerminalColors,
    grid: Arc<Mutex<Grid>>,
    image_store: Arc<Mutex<ImageStore>>,
    pty: Arc<Pty>,
    writer: Option<TerminalWriter>,
    reader: Option<TerminalReader>,
    pending: Arc<AtomicBool>,
    modifiers: ModifiersState,
    control: bool,
    keyboard: bool,
    touch: Option<(u64, f64, f64, bool)>,
    surface_size: (u32, u32),
    first_frame: bool,
    error: Option<String>,
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
            } else if let Ok(mut grid) = self.grid.lock() {
                grid.scroll_to_bottom();
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

    fn toolbar_height(&self) -> u32 {
        (self.window.as_ref().map_or(1.0, |w| w.scale_factor()) * 48.0).ceil() as u32
    }

    fn draw(&mut self) -> Result<(), String> {
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
        let [left, top, right, bottom] = self.content_bounds(size.width, size.height);
        let view_width = right - left;
        let view_height = bottom - top;
        let toolbar_height = self.toolbar_height().min(view_height);
        let atlas = self.atlas.as_mut().ok_or("Font atlas unavailable")?;
        let cell_width = atlas.cell_width.ceil().max(1.0) as u32;
        let cell_height = atlas.cell_height.ceil().max(1.0) as u32;
        let columns = (view_width / cell_width).clamp(1, 512) as u16;
        let rows = (view_height.saturating_sub(toolbar_height) / cell_height).clamp(1, 256) as u16;
        let (cells, cursor, images) = {
            let mut grid = self.grid.lock().map_err(|_| "Grid lock poisoned")?;
            if grid.cols() != columns as usize || grid.rows() != rows as usize {
                // Do not commit new grid dimensions if the PTY resize fails.
                self.pty.resize(columns, rows).map_err(|e| e.to_string())?;
                grid.resize(columns as usize, rows as usize);
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
            (
                cells,
                cursor,
                crate::android_images::snapshot(&grid, &self.image_store)?,
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
        for image in images.iter().filter(|image| image.z < 0) {
            draw_image_rgba(
                &mut frame,
                frame_size,
                &image.pixels,
                image.size,
                (
                    image.rect.0 + left as f32,
                    image.rect.1 + top as f32,
                    image.rect.2,
                    image.rect.3,
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
        for image in images.iter().filter(|image| image.z >= 0) {
            draw_image_rgba(
                &mut frame,
                frame_size,
                &image.pixels,
                image.size,
                (
                    image.rect.0 + left as f32,
                    image.rect.1 + top as f32,
                    image.rect.2,
                    image.rect.3,
                ),
            );
        }
        if let Some((col, row)) = cursor {
            let mut preedit_col = col;
            for c in self.input.preedit().chars().take(columns as usize) {
                if preedit_col >= u32::from(columns) {
                    break;
                }
                let glyph = atlas.get_or_insert(GlyphKey {
                    c,
                    bold: false,
                    italic: false,
                });
                let x = left + preedit_col * cell_width;
                let y = top + row * cell_height;
                let width = unicode_width::UnicodeWidthChar::width(c)
                    .unwrap_or(1)
                    .max(1) as u32;
                fill_rect(
                    &mut frame,
                    frame_size,
                    (x as i32, y as i32),
                    (width * cell_width, cell_height),
                    0x272d32,
                    255,
                );
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
        let toolbar_y = bottom - toolbar_height;
        // Seven 48 dp targets on normal phones; terminal retains the rest.
        for (index, label) in ["Esc", "Tab", "Ctrl", "<", "v", "^", ">"]
            .iter()
            .enumerate()
        {
            let x0 = left + view_width * index as u32 / 7;
            let x1 = left + view_width * (index as u32 + 1) / 7;
            let selected = self.control && index == 2;
            fill_rect(
                &mut frame,
                frame_size,
                (x0 as i32, toolbar_y as i32),
                ((x1 - x0).saturating_sub(1), toolbar_height),
                if selected { 0x7d3434 } else { 0x272d32 },
                255,
            );
            let text_width = label.len() as u32 * cell_width;
            let text_left = x0 + (x1 - x0).saturating_sub(text_width) / 2;
            for (i, c) in label.chars().enumerate() {
                let glyph = atlas.get_or_insert(GlyphKey {
                    c,
                    bold: selected,
                    italic: false,
                });
                draw_glyph_a8(
                    &mut frame,
                    frame_size,
                    &atlas.pixels,
                    (atlas.width, atlas.height),
                    glyph,
                    (
                        (text_left + i as u32 * cell_width) as i32 + glyph.bearing_x,
                        (toolbar_y + toolbar_height.saturating_sub(cell_height) / 2) as i32
                            + atlas.ascent.round() as i32
                            + glyph.bearing_y,
                    ),
                    0xf0f0f0,
                );
            }
        }
        window.pre_present_notify();
        frame.present().map_err(|e| e.to_string())?;
        if !self.first_frame {
            log::info!("first frame presented");
            self.first_frame = true;
        }
        Ok(())
    }

    fn tap(&mut self, x: f64, y: f64) {
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        let [left, top, right, bottom] = self.content_bounds(size.width, size.height);
        if x < f64::from(left)
            || x >= f64::from(right)
            || y < f64::from(top)
            || y >= f64::from(bottom)
        {
            return;
        }
        if y >= f64::from(bottom.saturating_sub(self.toolbar_height())) {
            match ((x as u32 - left).saturating_mul(7) / (right - left).max(1)).min(6) {
                0 => self.key(TerminalKey::Escape),
                1 => self.key(TerminalKey::Tab),
                2 => {
                    self.control = !self.control;
                    self.redraw();
                }
                3 => self.key(TerminalKey::Left),
                4 => self.key(TerminalKey::Down),
                5 => self.key(TerminalKey::Up),
                _ => self.key(TerminalKey::Right),
            }
        } else {
            self.keyboard = !self.keyboard;
            if let Err(error) = self.ime.set_visible(self.keyboard) {
                log::error!("{error}");
            }
        }
    }
}

impl Drop for AndroidWindow {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl ApplicationHandler<Event> for AndroidWindow {
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
            WindowEvent::Resized(_) => self.redraw(),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed => match event.logical_key {
                Key::Named(NamedKey::Enter) => self.key(TerminalKey::Enter),
                Key::Named(NamedKey::Backspace) => self.key(TerminalKey::Backspace),
                Key::Named(NamedKey::Tab) => self.key(TerminalKey::Tab),
                Key::Named(NamedKey::Escape) => self.key(TerminalKey::Escape),
                Key::Named(NamedKey::ArrowUp) => self.key(TerminalKey::Up),
                Key::Named(NamedKey::ArrowDown) => self.key(TerminalKey::Down),
                Key::Named(NamedKey::ArrowLeft) => self.key(TerminalKey::Left),
                Key::Named(NamedKey::ArrowRight) => self.key(TerminalKey::Right),
                _ => {
                    if let Some(text) = event.text {
                        self.text(&text);
                    }
                }
            },
            WindowEvent::Ime(Ime::Commit(text)) => self.text(&text),
            WindowEvent::Touch(touch) => match touch.phase {
                TouchPhase::Started if self.touch.is_none() => {
                    self.touch = Some((touch.id, touch.location.x, touch.location.y, false));
                }
                TouchPhase::Moved => {
                    if let Some((id, _, last_y, moved)) = &mut self.touch {
                        if *id == touch.id {
                            let height = self.atlas.as_ref().map_or(20.0, |a| a.cell_height) as f64;
                            let lines = ((touch.location.y - *last_y) / height) as i32;
                            if lines != 0 {
                                if let Ok(mut grid) = self.grid.lock() {
                                    if lines > 0 {
                                        grid.scroll_viewport_up(lines as usize);
                                    } else {
                                        grid.scroll_viewport_down(lines.unsigned_abs() as usize);
                                    }
                                }
                                *last_y += lines as f64 * height;
                                *moved = true;
                                self.redraw();
                            }
                        }
                    }
                }
                TouchPhase::Ended => {
                    if let Some((id, x, y, moved)) = self.touch {
                        if id == touch.id {
                            self.touch = None;
                            if !moved {
                                self.tap(x, y);
                            }
                        }
                    }
                }
                TouchPhase::Cancelled => self.touch = None,
                _ => {}
            },
            _ => {}
        }
    }
}

fn rgb(color: (u8, u8, u8)) -> u32 {
    (u32::from(color.0) << 16) | (u32::from(color.1) << 8) | u32::from(color.2)
}
