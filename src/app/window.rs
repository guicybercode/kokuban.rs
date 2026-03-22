use crate::grid::Grid;
use crate::input::keyboard::translate_key_event;
use crate::pty::Pty;
use crate::renderer::atlas::GlyphAtlas;
use crate::renderer::metal::MetalRenderer;

use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::*;
use objc2_foundation::*;
use objc2_metal::*;
use objc2_quartz_core::*;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct ViewState {
    grid: Arc<Mutex<Grid>>,
    atlas: Arc<Mutex<GlyphAtlas>>,
    pty: Arc<Pty>,
    dirty: Arc<AtomicBool>,
    renderer: MetalRenderer,
    metal_layer: Retained<CAMetalLayer>,
    scale_factor: f32,
    font_family: String,
    font_size: f32,
}

thread_local! {
    static VIEW_STATE: RefCell<Option<ViewState>> = RefCell::new(None);
}

define_class!(
    #[unsafe(super(NSView))]
    #[name = "TerminalView"]
    #[thread_kind = MainThreadOnly]
    pub struct TerminalView;

    impl TerminalView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> Bool {
            Bool::YES
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> Bool {
            Bool::YES
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> Bool {
            Bool::YES
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                let state = match state.as_mut() {
                    Some(s) => s,
                    None => return,
                };

                let drawable = state.metal_layer.nextDrawable();
                let drawable = match drawable {
                    Some(d) => d,
                    None => return,
                };

                let texture = drawable.texture();
                let size = state.metal_layer.drawableSize();

                let mut grid = state.grid.lock().unwrap();
                let mut atlas = state.atlas.lock().unwrap();

                state.renderer.draw(
                    &grid,
                    &mut atlas,
                    ProtocolObject::from_ref(&*drawable),
                    &texture,
                    size.width as f32,
                    size.height as f32,
                );

                grid.clear_dirty();
                state.dirty.store(false, Ordering::Relaxed);
            });
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if let Some(bytes) = translate_key_event(event) {
                VIEW_STATE.with(|state| {
                    let state = state.borrow();
                    if let Some(state) = state.as_ref() {
                        if let Err(e) = state.pty.write_all(&bytes) {
                            log::error!("Failed to write to PTY: {e}");
                        }
                    }
                });
            }
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    if let Some(window) = self.window() {
                        let new_scale = window.backingScaleFactor() as f32;
                        if (new_scale - state.scale_factor).abs() > 0.01 {
                            log::info!("Scale factor changed to {new_scale}");
                            state.scale_factor = new_scale;
                            state.metal_layer.setContentsScale(new_scale as f64);
                            // Recreate atlas at new scale
                            let mut atlas = state.atlas.lock().unwrap();
                            *atlas = GlyphAtlas::new(&state.font_family, state.font_size, new_scale);
                            state.dirty.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, new_size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: new_size] };

            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                let state = match state.as_mut() {
                    Some(s) => s,
                    None => return,
                };

                let scale = state.scale_factor;
                let pixel_w = new_size.width as f32 * scale;
                let pixel_h = new_size.height as f32 * scale;

                state.metal_layer.setDrawableSize(NSSize {
                    width: pixel_w as f64,
                    height: pixel_h as f64,
                });

                let atlas = state.atlas.lock().unwrap();
                let cell_w = atlas.cell_width;
                let cell_h = atlas.cell_height;
                drop(atlas);

                let new_cols = (pixel_w / cell_w).floor() as usize;
                let new_rows = (pixel_h / cell_h).floor() as usize;

                if new_cols > 0 && new_rows > 0 {
                    let mut grid = state.grid.lock().unwrap();
                    if new_cols != grid.cols() || new_rows != grid.rows() {
                        grid.resize(new_cols, new_rows);
                        log::debug!("Grid resized to {new_cols}x{new_rows}");
                        drop(grid);

                        if let Err(e) = state.pty.resize(new_cols as u16, new_rows as u16) {
                            log::error!("Failed to resize PTY: {e}");
                        }
                        state.dirty.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
    }
);

pub fn create_terminal_view(
    mtm: MainThreadMarker,
    device: &ProtocolObject<dyn MTLDevice>,
    grid: Arc<Mutex<Grid>>,
    atlas: Arc<Mutex<GlyphAtlas>>,
    pty: Arc<Pty>,
    dirty: Arc<AtomicBool>,
    default_fg: (u8, u8, u8),
    default_bg: (u8, u8, u8),
    scale_factor: f32,
    font_family: String,
    font_size: f32,
) -> Retained<TerminalView> {
    let view = mtm.alloc::<TerminalView>().set_ivars(());
    let view: Retained<TerminalView> = unsafe {
        msg_send![super(view), init]
    };

    view.setWantsLayer(true);

    // Create and configure CAMetalLayer
    let metal_layer = CAMetalLayer::new();
    metal_layer.setDevice(Some(device));
    metal_layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
    metal_layer.setContentsScale(scale_factor as f64);
    metal_layer.setFramebufferOnly(true);

    view.setLayer(Some(&metal_layer));

    // Set initial drawable size
    {
        let atlas = atlas.lock().unwrap();
        let grid = grid.lock().unwrap();
        let pixel_w = atlas.cell_width * grid.cols() as f32;
        let pixel_h = atlas.cell_height * grid.rows() as f32;
        metal_layer.setDrawableSize(NSSize {
            width: pixel_w as f64,
            height: pixel_h as f64,
        });
    }

    // Create renderer
    let retained_device: Retained<ProtocolObject<dyn MTLDevice>> = device.retain();
    let renderer = MetalRenderer::new(retained_device, default_fg, default_bg);

    VIEW_STATE.with(|state| {
        *state.borrow_mut() = Some(ViewState {
            grid,
            atlas,
            pty,
            dirty,
            renderer,
            metal_layer,
            scale_factor,
            font_family,
            font_size,
        });
    });

    view
}
