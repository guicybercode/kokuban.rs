use crate::app::selection::{GridPoint, SelectionState};
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
    selection: SelectionState,
    selection_fg: (u8, u8, u8),
    selection_bg: (u8, u8, u8),
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
            log::debug!("[render] updateLayer called");
            render_frame();
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let delta_y = event.scrollingDeltaY();
            if delta_y.abs() < 0.01 {
                return;
            }
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    let lines = (delta_y.abs() / 3.0).max(1.0) as usize;
                    let mut grid = state.grid.lock().unwrap();
                    if delta_y > 0.0 {
                        grid.scroll_viewport_up(lines);
                    } else {
                        grid.scroll_viewport_down(lines);
                    }
                    drop(grid);
                    state.dirty.store(true, Ordering::Relaxed);
                }
            });
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    if let Some(point) = pixel_to_grid_point(event, state) {
                        state.selection.start(point);
                        state.dirty.store(true, Ordering::Relaxed);
                    }
                }
            });
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    if let Some(point) = pixel_to_grid_point(event, state) {
                        state.selection.update(point);
                        state.dirty.store(true, Ordering::Relaxed);
                    }
                }
            });
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            // Selection stays until cleared by keypress
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let key_code = event.keyCode();
            let modifiers = event.modifierFlags();
            let has_shift = modifiers.contains(NSEventModifierFlags::Shift);
            let has_cmd = modifiers.contains(NSEventModifierFlags::Command);

            // Cmd+C: copy
            if has_cmd && key_code == 8 {
                VIEW_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    if let Some(state) = state.as_mut() {
                        if state.selection.is_active() {
                            let grid = state.grid.lock().unwrap();
                            let text = state.selection.get_text(&grid);
                            drop(grid);
                            if !text.is_empty() {
                                copy_to_clipboard(&text);
                            }
                            state.selection.clear();
                            state.dirty.store(true, Ordering::Relaxed);
                            return;
                        }
                        // No selection — send Ctrl-C
                        if let Err(e) = state.pty.write_all(&[0x03]) {
                            log::error!("Failed to write to PTY: {e}");
                        }
                    }
                });
                return;
            }

            // Cmd+V: paste
            if has_cmd && key_code == 9 {
                VIEW_STATE.with(|state| {
                    let state = state.borrow();
                    if let Some(state) = state.as_ref() {
                        if let Some(text) = paste_from_clipboard() {
                            let grid = state.grid.lock().unwrap();
                            let bracketed = grid.bracketed_paste;
                            drop(grid);

                            let mut bytes = Vec::new();
                            if bracketed {
                                bytes.extend_from_slice(b"\x1b[200~");
                            }
                            bytes.extend_from_slice(text.as_bytes());
                            if bracketed {
                                bytes.extend_from_slice(b"\x1b[201~");
                            }
                            if let Err(e) = state.pty.write_all(&bytes) {
                                log::error!("Failed to write to PTY: {e}");
                            }
                        }
                    }
                });
                return;
            }

            // Shift+PageUp/PageDown/Home/End: viewport scrolling
            if has_shift {
                let handled = VIEW_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    if let Some(state) = state.as_mut() {
                        let mut grid = state.grid.lock().unwrap();
                        let rows = grid.rows();
                        match key_code {
                            116 => { // PageUp
                                grid.scroll_viewport_up(rows.saturating_sub(1));
                                state.dirty.store(true, Ordering::Relaxed);
                                return true;
                            }
                            121 => { // PageDown
                                grid.scroll_viewport_down(rows.saturating_sub(1));
                                state.dirty.store(true, Ordering::Relaxed);
                                return true;
                            }
                            115 => { // Home
                                let max = grid.scrollback_len();
                                grid.scroll_viewport_up(max);
                                state.dirty.store(true, Ordering::Relaxed);
                                return true;
                            }
                            119 => { // End
                                grid.scroll_to_bottom();
                                state.dirty.store(true, Ordering::Relaxed);
                                return true;
                            }
                            _ => {}
                        }
                    }
                    false
                });
                if handled {
                    return;
                }
            }

            // Any non-modifier keypress: snap to bottom and clear selection
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    let mut grid = state.grid.lock().unwrap();
                    grid.scroll_to_bottom();
                    drop(grid);
                    if state.selection.is_active() {
                        state.selection.clear();
                        state.dirty.store(true, Ordering::Relaxed);
                    }
                }
            });

            // Normal key handling
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

                            // Update drawable size to match new atlas cell dimensions
                            let grid = state.grid.lock().unwrap();
                            let pixel_w = atlas.cell_width * grid.cols() as f32;
                            let pixel_h = atlas.cell_height * grid.rows() as f32;
                            log::debug!("[render] scale change: updating drawableSize to {pixel_w}x{pixel_h}");
                            state.metal_layer.setDrawableSize(NSSize {
                                width: pixel_w as f64,
                                height: pixel_h as f64,
                            });

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

fn pixel_to_grid_point(event: &NSEvent, state: &ViewState) -> Option<GridPoint> {
    let loc = event.locationInWindow();
    // NSView is flipped, but locationInWindow is in window coords (origin bottom-left).
    // We need to convert to view coords.
    let scale = state.scale_factor;
    let atlas = state.atlas.lock().unwrap();
    let cell_w = atlas.cell_width;
    let cell_h = atlas.cell_height;
    drop(atlas);

    // locationInWindow has Y=0 at bottom. Our view is flipped so Y=0 is at top.
    // Get the view frame height to flip.
    let grid = state.grid.lock().unwrap();
    let view_pixel_h = cell_h * grid.rows() as f32 / scale;
    let scroll_offset = grid.scroll_offset;
    let sb_len = grid.scrollback_len();
    drop(grid);

    let x = loc.x as f32 * scale;
    let y = (view_pixel_h - loc.y as f32) * scale;

    let col = (x / cell_w) as usize;
    let vis_row = (y / cell_h) as usize;

    // Convert to absolute row
    let abs_row = sb_len as i64 - scroll_offset as i64 + vis_row as i64;

    Some(GridPoint { row: abs_row, col })
}

fn copy_to_clipboard(text: &str) {
    unsafe {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let ns_string = NSString::from_str(text);
        pasteboard.setString_forType(&ns_string, NSPasteboardTypeString);
    }
}

fn paste_from_clipboard() -> Option<String> {
    unsafe {
        let pasteboard = NSPasteboard::generalPasteboard();
        let types = NSArray::from_retained_slice(&[NSPasteboardTypeString.copy()]);
        let available = pasteboard.availableTypeFromArray(&types);
        if available.is_some() {
            if let Some(s) = pasteboard.stringForType(NSPasteboardTypeString) {
                return Some(s.to_string());
            }
        }
        None
    }
}

/// Perform one render frame. Called from both `updateLayer` and the timer.
fn render_frame() {
    VIEW_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let state = match state.as_mut() {
            Some(s) => s,
            None => {
                log::warn!("[render] VIEW_STATE is None");
                return;
            }
        };

        let size = state.metal_layer.drawableSize();
        log::debug!("[render] drawableSize: {}x{}", size.width, size.height);

        let drawable = state.metal_layer.nextDrawable();
        let drawable = match drawable {
            Some(d) => d,
            None => {
                log::warn!("[render] nextDrawable returned None");
                return;
            }
        };

        let texture = drawable.texture();

        let grid = state.grid.lock().unwrap();
        let mut atlas = state.atlas.lock().unwrap();

        let sel = if state.selection.is_active() {
            Some(&state.selection)
        } else {
            None
        };

        state.renderer.draw(
            &grid,
            &mut atlas,
            ProtocolObject::from_ref(&*drawable),
            &texture,
            size.width as f32,
            size.height as f32,
            sel,
            state.selection_fg,
            state.selection_bg,
        );

        drop(atlas);
        drop(grid);
        state.dirty.store(false, Ordering::Relaxed);
        log::debug!("[render] frame presented");
    });
}

/// Called directly from the timer to render without relying on setNeedsDisplay.
pub fn render_if_dirty(dirty: &AtomicBool) {
    if dirty.load(Ordering::Relaxed) {
        log::trace!("[render] timer tick: dirty, rendering directly");
        render_frame();
    }
}

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
    selection_fg: (u8, u8, u8),
    selection_bg: (u8, u8, u8),
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
            selection: SelectionState::default(),
            selection_fg,
            selection_bg,
        });
    });

    view
}
