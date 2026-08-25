use crate::config::{ColorConfig, Config};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

const WINDOW_TITLE: &str = "黒板kokuban";
const INITIAL_CELL_WIDTH: u32 = 10;
const INITIAL_CELL_HEIGHT: u32 = 20;
const EXIT_AFTER_FIRST_FRAME_ENV: &str = "KOKUBAN_EXIT_AFTER_FIRST_FRAME";

type SoftwareSurface = Surface<Arc<Window>, Arc<Window>>;

pub(crate) fn launch(config: Config) -> Result<(), String> {
    if config.window.opacity < 1.0 {
        eprintln!(
            "kokuban: warning: window.opacity={} is not supported by the Linux software renderer; using an opaque window",
            config.window.opacity
        );
    }

    let event_loop = EventLoop::new()
        .map_err(|error| format!("could not connect to an X11 or Wayland display: {error}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let background = ColorConfig::parse_hex(&config.colors.background);
    let initial_size = initial_window_dimensions(config.window.columns, config.window.rows);
    let exit_after_first_frame = std::env::var(EXIT_AFTER_FIRST_FRAME_ENV).as_deref() == Ok("1");
    let mut application = LinuxWindow::new(background, initial_size, exit_after_first_frame);

    if let Err(error) = event_loop.run_app(&mut application) {
        return Err(format!("Linux event loop failed: {error}"));
    }

    application.finish()
}

struct LinuxWindow {
    // Drop the surface and its display connection before releasing the window.
    surface: Option<SoftwareSurface>,
    context: Option<Context<Arc<Window>>>,
    window: Option<Arc<Window>>,
    background: u32,
    initial_size: LogicalSize<u32>,
    exit_after_first_frame: bool,
    first_frame_presented: bool,
    error: Option<String>,
}

impl LinuxWindow {
    fn new(
        background: (u8, u8, u8),
        initial_size: LogicalSize<u32>,
        exit_after_first_frame: bool,
    ) -> Self {
        Self {
            surface: None,
            context: None,
            window: None,
            background: rgb_to_xrgb(background.0, background.1, background.2),
            initial_size,
            exit_after_first_frame,
            first_frame_presented: false,
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

        self.surface = Some(surface);
        self.context = Some(context);
        self.window = Some(window.clone());
        window.request_redraw();
        Ok(())
    }

    fn present_background(&mut self) -> Result<bool, String> {
        let window =
            self.window.as_ref().cloned().ok_or_else(|| {
                "redraw requested before the Linux window was created".to_string()
            })?;
        let size = window.inner_size();
        let Some((width, height)) = drawable_dimensions(size) else {
            return Ok(false);
        };
        let surface = self
            .surface
            .as_mut()
            .ok_or_else(|| "redraw requested before the software renderer was ready".to_string())?;

        surface
            .resize(width, height)
            .map_err(|error| format!("could not resize the software-rendering surface: {error}"))?;
        let mut buffer = surface
            .buffer_mut()
            .map_err(|error| format!("could not acquire the software-rendering buffer: {error}"))?;
        buffer.fill(self.background);
        window.pre_present_notify();
        buffer
            .present()
            .map_err(|error| format!("could not present a Linux frame: {error}"))?;
        Ok(true)
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: String) {
        if self.error.is_none() {
            self.error = Some(error);
        }
        event_loop.exit();
    }

    fn finish(self) -> Result<(), String> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.exit_after_first_frame && !self.first_frame_presented {
            return Err("smoke mode exited before the first Linux frame was presented".to_string());
        }
        Ok(())
    }
}

impl ApplicationHandler for LinuxWindow {
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
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => match self.present_background() {
                Ok(true) => {
                    self.first_frame_presented = true;
                    if self.exit_after_first_frame {
                        event_loop.exit();
                    }
                }
                Ok(false) => {}
                Err(error) => self.fail(event_loop, error),
            },
            _ => {}
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{drawable_dimensions, initial_window_dimensions, rgb_to_xrgb};
    use winit::dpi::PhysicalSize;

    #[test]
    fn converts_rgb_to_softbuffer_xrgb() {
        assert_eq!(rgb_to_xrgb(0x1a, 0x2b, 0x3c), 0x001a_2b3c);
        assert_eq!(rgb_to_xrgb(0xff, 0xff, 0xff) >> 24, 0);
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
}
