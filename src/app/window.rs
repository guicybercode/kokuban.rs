use crate::app::confirm::{self, ConfirmAction, ConfirmDialog, ConfirmResult};
use crate::app::selection::GridPoint;
use crate::grid::{MouseEncoding, MouseTracking};
use crate::input::keybind::{KeyModifiers, KeybindMap, PaneAction};
use crate::input::macos::translate_key_event;
use crate::pane::layout::PixelRect;
use crate::pane::tree::SplitDirection;
use crate::pane::PaneTree;
use crate::renderer::atlas::GlyphAtlas;
use crate::renderer::image_store::ImageStore;
use crate::renderer::metal::{ChromeColors, ConfirmOverlayInfo, MetalRenderer, PaneRenderData};

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
    pane_tree: Arc<Mutex<PaneTree>>,
    atlas: Arc<Mutex<GlyphAtlas>>,
    dirty: Arc<AtomicBool>,
    should_close: Arc<AtomicBool>,
    renderer: MetalRenderer,
    metal_layer: Retained<CAMetalLayer>,
    scale_factor: f32,
    font_family: String,
    font_size: f32,
    base_font_size: f32,
    zoom_step: f32,
    min_font_size: f32,
    max_font_size: f32,
    selection_fg: (u8, u8, u8),
    selection_bg: (u8, u8, u8),
    chrome: ChromeColors,
    keybinds: KeybindMap,
    bg_opacity: f32,
    status_bar_height: f32,
    status_bar_enabled: bool,
    resize_step: f32,
    prompt_indicator_color: Option<(u8, u8, u8)>,
    image_store: Arc<Mutex<ImageStore>>,
    confirm_dialog: Option<ConfirmDialog>,
    confirm_on_close_pane: bool,
    confirm_on_quit: bool,
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
            render_frame();
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> Bool {
            let key_code = event.keyCode();
            let modifiers = event.modifierFlags();
            let has_cmd = modifiers.contains(NSEventModifierFlags::Command);

            // If confirm dialog is active, route ALL keys to it (including Cmd+keys)
            let confirm_active = VIEW_STATE.with(|state| {
                state.borrow().as_ref().map_or(false, |s| s.confirm_dialog.is_some())
            });
            if confirm_active {
                let ch = event.charactersIgnoringModifiers()
                    .and_then(|s| s.to_string().chars().next());
                handle_confirm_key(key_code, ch);
                return Bool::YES;
            }

            if !has_cmd {
                return Bool::NO;
            }

            let has_shift = modifiers.contains(NSEventModifierFlags::Shift);

            // Build modifier flags for keybind lookup
            let mut km = KeyModifiers::CMD;
            if has_shift {
                km.insert(KeyModifiers::SHIFT);
            }

            // Cmd+Q: show quit confirmation (or quit immediately if disabled)
            if key_code == 12 && !has_shift {
                VIEW_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    if let Some(state) = state.as_mut() {
                        if state.confirm_on_quit {
                            state.confirm_dialog = Some(ConfirmDialog::new(
                                ConfirmAction::QuitApp,
                                None,
                            ));
                            state.dirty.store(true, Ordering::Relaxed);
                        } else {
                            state.should_close.store(true, Ordering::Relaxed);
                        }
                    }
                });
                return Bool::YES;
            }

            // Cmd+C: copy or ctrl-c
            if key_code == 8 && !has_shift {
                let handled = VIEW_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    if let Some(state) = state.as_mut() {
                        let mut tree = state.pane_tree.lock().unwrap();
                        if let Some(pane) = tree.focused_pane_mut() {
                            if pane.selection.is_active() {
                                let text = pane.selection.get_text(&pane.grid);
                                pane.selection.clear();
                                drop(tree);
                                if !text.is_empty() {
                                    copy_to_clipboard(&text);
                                }
                                state.dirty.store(true, Ordering::Relaxed);
                                return true;
                            }
                            // No selection: send Ctrl-C
                            if let Err(e) = pane.pty.write_all(&[0x03]) {
                                log::error!("Failed to write to PTY: {e}");
                            }
                        }
                    }
                    true // Always consume Cmd+C
                });
                if handled {
                    return Bool::YES;
                }
            }

            // Cmd+V: paste
            if key_code == 9 && !has_shift {
                VIEW_STATE.with(|state| {
                    let state = state.borrow();
                    if let Some(state) = state.as_ref() {
                        if let Some(text) = paste_from_clipboard() {
                            let tree = state.pane_tree.lock().unwrap();
                            if let Some(pane) = tree.focused_pane() {
                                let bracketed = pane.grid.bracketed_paste;
                                let mut bytes = Vec::new();
                                if bracketed {
                                    bytes.extend_from_slice(b"\x1b[200~");
                                }
                                bytes.extend_from_slice(text.as_bytes());
                                if bracketed {
                                    bytes.extend_from_slice(b"\x1b[201~");
                                }
                                if let Err(e) = pane.pty.write_all(&bytes) {
                                    log::error!("Failed to write to PTY: {e}");
                                }
                            }
                        }
                    }
                });
                return Bool::YES;
            }

            // Cmd+Shift+= (i.e. Cmd++) should also zoom in
            if key_code == 24 && has_shift {
                handle_pane_action(PaneAction::ZoomIn);
                return Bool::YES;
            }

            // Check configurable keybinds
            let action = VIEW_STATE.with(|state| {
                let state = state.borrow();
                state.as_ref().and_then(|s| s.keybinds.lookup(key_code, km))
            });

            if let Some(action) = action {
                handle_pane_action(action);
                return Bool::YES;
            }

            Bool::NO
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            // Block input while confirm dialog is active
            let blocked = VIEW_STATE.with(|s| s.borrow().as_ref().map_or(false, |s| s.confirm_dialog.is_some()));
            if blocked { return; }
            let delta_y = event.scrollingDeltaY();
            if delta_y.abs() < 0.01 { return; }
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    let atlas = state.atlas.lock().unwrap();
                    let cell_w = atlas.cell_width;
                    let cell_h = atlas.cell_height;
                    drop(atlas);
                    let mut tree = state.pane_tree.lock().unwrap();
                    let cell_info = pixel_to_cell(event, state, &tree, cell_w, cell_h);
                    let tracking = tree.focused_pane().map(|p| (p.grid.mouse_tracking, p.grid.mouse_encoding == MouseEncoding::Sgr));
                    if let (Some(mt), Some((_id, gc, gr))) = (tracking, cell_info) {
                        if mt.0 != MouseTracking::None {
                            let button = if delta_y > 0.0 { 64u8 } else { 65u8 };
                            let seq = encode_sgr_mouse(button, gc + 1, gr + 1, true, mt.1);
                            if let Some(pane) = tree.focused_pane() {
                                pane.pty.write_all(&seq).ok();
                            }
                        } else if let Some(pane) = tree.focused_pane_mut() {
                            let lines = (delta_y.abs() / 3.0).max(1.0) as usize;
                            if delta_y > 0.0 { pane.grid.scroll_viewport_up(lines); }
                            else { pane.grid.scroll_viewport_down(lines); }
                        }
                    } else if let Some(pane) = tree.focused_pane_mut() {
                        let lines = (delta_y.abs() / 3.0).max(1.0) as usize;
                        if delta_y > 0.0 { pane.grid.scroll_viewport_up(lines); }
                        else { pane.grid.scroll_viewport_down(lines); }
                    }
                    drop(tree);
                    state.dirty.store(true, Ordering::Relaxed);
                }
            });
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let blocked = VIEW_STATE.with(|s| s.borrow().as_ref().map_or(false, |s| s.confirm_dialog.is_some()));
            if blocked { return; }
            let has_shift = event.modifierFlags().contains(NSEventModifierFlags::Shift);
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    let atlas = state.atlas.lock().unwrap();
                    let cell_w = atlas.cell_width;
                    let cell_h = atlas.cell_height;
                    drop(atlas);
                    let mut tree = state.pane_tree.lock().unwrap();
                    let cell_info = pixel_to_cell(event, state, &tree, cell_w, cell_h);
                    if let Some((pane_id, gc, gr)) = cell_info {
                        tree.focused = pane_id;
                        let tracking = tree.pane(pane_id).map(|p| (p.grid.mouse_tracking, p.grid.mouse_encoding == MouseEncoding::Sgr));
                        if let Some((mt, sgr)) = tracking {
                            if mt != MouseTracking::None && !has_shift {
                                let seq = encode_sgr_mouse(0, gc + 1, gr + 1, true, sgr);
                                if let Some(pane) = tree.pane(pane_id) {
                                    pane.pty.write_all(&seq).ok();
                                }
                            } else {
                                let grid_pt = pixel_to_grid_point(event, state, &tree, cell_w, cell_h);
                                if let Some((_id, point)) = grid_pt {
                                    if let Some(pane) = tree.pane_mut(pane_id) {
                                        pane.selection.start(point);
                                    }
                                }
                            }
                        }
                    }
                    drop(tree);
                    state.dirty.store(true, Ordering::Relaxed);
                }
            });
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let blocked = VIEW_STATE.with(|s| s.borrow().as_ref().map_or(false, |s| s.confirm_dialog.is_some()));
            if blocked { return; }
            let has_shift = event.modifierFlags().contains(NSEventModifierFlags::Shift);
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    let atlas = state.atlas.lock().unwrap();
                    let cell_w = atlas.cell_width;
                    let cell_h = atlas.cell_height;
                    drop(atlas);
                    let mut tree = state.pane_tree.lock().unwrap();
                    let cell_info = pixel_to_cell(event, state, &tree, cell_w, cell_h);
                    let focused = tree.focused;
                    let tracking = tree.focused_pane().map(|p| (p.grid.mouse_tracking, p.grid.mouse_encoding == MouseEncoding::Sgr));
                    if let Some((mt, sgr)) = tracking {
                        let forward = (mt == MouseTracking::ButtonEvent || mt == MouseTracking::AnyEvent) && !has_shift;
                        if forward {
                            if let Some((_id, gc, gr)) = cell_info {
                                let seq = encode_sgr_mouse(32, gc + 1, gr + 1, true, sgr);
                                if let Some(pane) = tree.pane(focused) {
                                    pane.pty.write_all(&seq).ok();
                                }
                            }
                        } else {
                            let grid_pt = pixel_to_grid_point(event, state, &tree, cell_w, cell_h);
                            if let Some((_id, point)) = grid_pt {
                                if let Some(pane) = tree.focused_pane_mut() {
                                    pane.selection.update(point);
                                }
                            }
                        }
                    }
                    drop(tree);
                    state.dirty.store(true, Ordering::Relaxed);
                }
            });
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let blocked = VIEW_STATE.with(|s| s.borrow().as_ref().map_or(false, |s| s.confirm_dialog.is_some()));
            if blocked { return; }
            VIEW_STATE.with(|state| {
                let state = state.borrow();
                if let Some(state) = state.as_ref() {
                    let atlas = state.atlas.lock().unwrap();
                    let cell_w = atlas.cell_width;
                    let cell_h = atlas.cell_height;
                    drop(atlas);
                    let tree = state.pane_tree.lock().unwrap();
                    let cell_info = pixel_to_cell(event, state, &tree, cell_w, cell_h);
                    if let Some(pane) = tree.focused_pane() {
                        if pane.grid.mouse_tracking != MouseTracking::None {
                            if let Some((_id, gc, gr)) = cell_info {
                                let sgr = pane.grid.mouse_encoding == MouseEncoding::Sgr;
                                let seq = encode_sgr_mouse(0, gc + 1, gr + 1, false, sgr);
                                pane.pty.write_all(&seq).ok();
                            }
                        }
                    }
                }
            });
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let key_code = event.keyCode();
            let modifiers = event.modifierFlags();
            let has_shift = modifiers.contains(NSEventModifierFlags::Shift);
            let has_cmd = modifiers.contains(NSEventModifierFlags::Command);

            // If confirm dialog is active, route keys to it
            let confirm_active = VIEW_STATE.with(|s| s.borrow().as_ref().map_or(false, |s| s.confirm_dialog.is_some()));
            if confirm_active {
                let ch = event.charactersIgnoringModifiers()
                    .and_then(|s| s.to_string().chars().next());
                handle_confirm_key(key_code, ch);
                return;
            }

            // Cmd keys are handled in performKeyEquivalent
            if has_cmd {
                return;
            }

            // Shift+PageUp/Down/Home/End: viewport scrolling
            if has_shift {
                let handled = VIEW_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    if let Some(state) = state.as_mut() {
                        let mut tree = state.pane_tree.lock().unwrap();
                        if let Some(pane) = tree.focused_pane_mut() {
                            let rows = pane.grid.rows();
                            match key_code {
                                116 => { pane.grid.scroll_viewport_up(rows.saturating_sub(1)); }
                                121 => { pane.grid.scroll_viewport_down(rows.saturating_sub(1)); }
                                115 => { let max = pane.grid.scrollback_len(); pane.grid.scroll_viewport_up(max); }
                                119 => { pane.grid.scroll_to_bottom(); }
                                _ => return false,
                            }
                            drop(tree);
                            state.dirty.store(true, Ordering::Relaxed);
                            return true;
                        }
                    }
                    false
                });
                if handled {
                    return;
                }
            }

            // Forward to the focused pane using that pane's current input mode.
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    let mut tree = state.pane_tree.lock().unwrap();
                    if let Some(pane) = tree.focused_pane_mut() {
                        pane.grid.scroll_to_bottom();
                        if pane.selection.is_active() {
                            pane.selection.clear();
                            state.dirty.store(true, Ordering::Relaxed);
                        }
                        if let Some(bytes) =
                            translate_key_event(event, pane.grid.application_cursor_keys)
                        {
                            if let Err(e) = pane.pty.write_all(&bytes) {
                                log::error!("Failed to write to PTY: {e}");
                            }
                        }
                    }
                }
            });
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
                            let mut atlas = state.atlas.lock().unwrap();
                            *atlas = GlyphAtlas::new(&state.font_family, state.font_size, new_scale);
                            drop(atlas);
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

                let viewport = PixelRect {
                    x: 0.0,
                    y: 0.0,
                    width: pixel_w,
                    height: pixel_h,
                };

                let mut tree = state.pane_tree.lock().unwrap();
                tree.relayout(viewport, cell_w, cell_h, state.status_bar_height);
                drop(tree);
                state.dirty.store(true, Ordering::Relaxed);
            });
        }
    }
);

fn handle_pane_action(action: PaneAction) {
    VIEW_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let state = match state.as_mut() {
            Some(s) => s,
            None => return,
        };

        let atlas = state.atlas.lock().unwrap();
        let cell_w = atlas.cell_width;
        let cell_h = atlas.cell_height;
        drop(atlas);

        let mut tree = state.pane_tree.lock().unwrap();

        match action {
            PaneAction::SplitVertical => {
                if let Err(e) = tree.split(SplitDirection::Vertical, cell_w, cell_h) {
                    log::error!("Failed to split vertical: {e}");
                }
                // Relayout
                let size = state.metal_layer.drawableSize();
                let viewport = PixelRect {
                    x: 0.0,
                    y: 0.0,
                    width: size.width as f32,
                    height: size.height as f32,
                };
                tree.relayout(viewport, cell_w, cell_h, state.status_bar_height);
            }
            PaneAction::SplitHorizontal => {
                if let Err(e) = tree.split(SplitDirection::Horizontal, cell_w, cell_h) {
                    log::error!("Failed to split horizontal: {e}");
                }
                let size = state.metal_layer.drawableSize();
                let viewport = PixelRect {
                    x: 0.0,
                    y: 0.0,
                    width: size.width as f32,
                    height: size.height as f32,
                };
                tree.relayout(viewport, cell_w, cell_h, state.status_bar_height);
            }
            PaneAction::ClosePane => {
                if state.confirm_on_close_pane {
                    let id = tree.focused;
                    // Get foreground process name for the dialog
                    let proc_name = tree.pane(id).and_then(|p| {
                        confirm::foreground_process_name(p.pty.master_fd())
                    });
                    drop(tree);
                    state.confirm_dialog = Some(ConfirmDialog::new(
                        ConfirmAction::ClosePane(id),
                        proc_name,
                    ));
                    state.dirty.store(true, Ordering::Relaxed);
                    return;
                }
                // No confirmation — close immediately
                let id = tree.focused;
                let should_terminate = tree.close(id);
                if should_terminate {
                    drop(tree);
                    state.should_close.store(true, Ordering::Relaxed);
                    return;
                }
                let size = state.metal_layer.drawableSize();
                let viewport = PixelRect {
                    x: 0.0,
                    y: 0.0,
                    width: size.width as f32,
                    height: size.height as f32,
                };
                tree.relayout(viewport, cell_w, cell_h, state.status_bar_height);
            }
            PaneAction::FocusLeft => {
                tree.focus_neighbor(SplitDirection::Vertical, false);
            }
            PaneAction::FocusRight => {
                tree.focus_neighbor(SplitDirection::Vertical, true);
            }
            PaneAction::FocusUp => {
                tree.focus_neighbor(SplitDirection::Horizontal, false);
            }
            PaneAction::FocusDown => {
                tree.focus_neighbor(SplitDirection::Horizontal, true);
            }
            PaneAction::ResizeLeft => {
                let delta = -(state.resize_step / cell_w) * 0.01;
                tree.resize_focused(delta);
                let size = state.metal_layer.drawableSize();
                let viewport = PixelRect { x: 0.0, y: 0.0, width: size.width as f32, height: size.height as f32 };
                tree.relayout(viewport, cell_w, cell_h, state.status_bar_height);
            }
            PaneAction::ResizeRight => {
                let delta = (state.resize_step / cell_w) * 0.01;
                tree.resize_focused(delta);
                let size = state.metal_layer.drawableSize();
                let viewport = PixelRect { x: 0.0, y: 0.0, width: size.width as f32, height: size.height as f32 };
                tree.relayout(viewport, cell_w, cell_h, state.status_bar_height);
            }
            PaneAction::ResizeUp => {
                let delta = -(state.resize_step / cell_h) * 0.01;
                tree.resize_focused(delta);
                let size = state.metal_layer.drawableSize();
                let viewport = PixelRect { x: 0.0, y: 0.0, width: size.width as f32, height: size.height as f32 };
                tree.relayout(viewport, cell_w, cell_h, state.status_bar_height);
            }
            PaneAction::ResizeDown => {
                let delta = (state.resize_step / cell_h) * 0.01;
                tree.resize_focused(delta);
                let size = state.metal_layer.drawableSize();
                let viewport = PixelRect { x: 0.0, y: 0.0, width: size.width as f32, height: size.height as f32 };
                tree.relayout(viewport, cell_w, cell_h, state.status_bar_height);
            }
            PaneAction::ZoomIn => {
                drop(tree);
                let new_size = (state.font_size + state.zoom_step).min(state.max_font_size);
                perform_zoom(state, new_size);
                return;
            }
            PaneAction::ZoomOut => {
                drop(tree);
                let new_size = (state.font_size - state.zoom_step).max(state.min_font_size);
                perform_zoom(state, new_size);
                return;
            }
            PaneAction::ZoomReset => {
                drop(tree);
                perform_zoom(state, state.base_font_size);
                return;
            }
            PaneAction::PrevPrompt => {
                if let Some(pane) = tree.focused_pane_mut() {
                    let current_abs = if pane.grid.scroll_offset > 0 {
                        let sb_len = pane.grid.scrollback_len();
                        sb_len.saturating_sub(pane.grid.scroll_offset)
                    } else {
                        pane.grid.current_absolute_row()
                    };
                    if let Some(target_row) = pane.grid.marks.prev_prompt(current_abs) {
                        let sb_len = pane.grid.scrollback_len();
                        let total_pushed = pane.grid.total_lines_pushed;
                        // target_row is absolute. Convert to scroll_offset.
                        // absolute row in scrollback = target_row, but scrollback may have evicted lines.
                        // scrollback stores the last sb_len lines. The absolute range in scrollback is
                        // [total_pushed - sb_len .. total_pushed). The visible buffer starts at total_pushed.
                        let evicted = total_pushed.saturating_sub(sb_len);
                        if target_row >= evicted {
                            let sb_index = target_row - evicted;
                            // scroll_offset = sb_len - sb_index (with 2 lines context)
                            let offset = sb_len.saturating_sub(sb_index).saturating_sub(2);
                            pane.grid.scroll_offset = offset.min(sb_len);
                            pane.grid.mark_all_dirty();
                        }
                    }
                }
            }
            PaneAction::NextPrompt => {
                if let Some(pane) = tree.focused_pane_mut() {
                    let current_abs = if pane.grid.scroll_offset > 0 {
                        let sb_len = pane.grid.scrollback_len();
                        sb_len.saturating_sub(pane.grid.scroll_offset)
                    } else {
                        pane.grid.current_absolute_row()
                    };
                    if let Some(target_row) = pane.grid.marks.next_prompt(current_abs) {
                        let sb_len = pane.grid.scrollback_len();
                        let total_pushed = pane.grid.total_lines_pushed;
                        let evicted = total_pushed.saturating_sub(sb_len);
                        if target_row >= evicted && target_row < total_pushed {
                            let sb_index = target_row - evicted;
                            let offset = sb_len.saturating_sub(sb_index).saturating_sub(2);
                            pane.grid.scroll_offset = offset.min(sb_len);
                            pane.grid.mark_all_dirty();
                        } else {
                            // Target is in the visible buffer or beyond — snap to bottom
                            pane.grid.scroll_to_bottom();
                        }
                    } else {
                        pane.grid.scroll_to_bottom();
                    }
                }
            }
        }

        drop(tree);
        state.dirty.store(true, Ordering::Relaxed);
    });
}

fn handle_confirm_key(key_code: u16, character: Option<char>) {
    VIEW_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let state = match state.as_mut() {
            Some(s) => s,
            None => return,
        };

        let result = match &state.confirm_dialog {
            Some(dialog) => dialog.handle_input(key_code, character),
            None => return,
        };

        match result {
            ConfirmResult::Confirmed => {
                let action = state.confirm_dialog.take().unwrap().action;
                state.dirty.store(true, Ordering::Relaxed);
                execute_confirm_action(state, action);
            }
            ConfirmResult::Cancelled => {
                state.confirm_dialog = None;
                state.dirty.store(true, Ordering::Relaxed);
            }
            ConfirmResult::Pending => {
                // Ignore unrecognized keys
            }
        }
    });
}

fn execute_confirm_action(state: &mut ViewState, action: ConfirmAction) {
    match action {
        ConfirmAction::ClosePane(pane_id) => {
            let atlas = state.atlas.lock().unwrap();
            let cell_w = atlas.cell_width;
            let cell_h = atlas.cell_height;
            drop(atlas);

            let mut tree = state.pane_tree.lock().unwrap();
            let should_terminate = tree.close(pane_id);
            if should_terminate {
                drop(tree);
                state.should_close.store(true, Ordering::Relaxed);
                return;
            }
            let size = state.metal_layer.drawableSize();
            let viewport = PixelRect {
                x: 0.0,
                y: 0.0,
                width: size.width as f32,
                height: size.height as f32,
            };
            tree.relayout(viewport, cell_w, cell_h, state.status_bar_height);
        }
        ConfirmAction::QuitApp => {
            state.should_close.store(true, Ordering::Relaxed);
        }
    }
}

fn perform_zoom(state: &mut ViewState, new_size: f32) {
    if (new_size - state.font_size).abs() < 0.01 {
        return;
    }
    state.font_size = new_size;
    log::info!("Font zoom: {new_size}pt");

    // Recreate atlas at new size
    let mut atlas = state.atlas.lock().unwrap();
    atlas.clear_and_resize(new_size);
    let cell_w = atlas.cell_width;
    let cell_h = atlas.cell_height;
    drop(atlas);

    // Recalculate status bar height
    if state.status_bar_enabled {
        state.status_bar_height = (cell_h * 1.5).ceil();
    }

    // Relayout all panes with new cell dimensions
    let size = state.metal_layer.drawableSize();
    let viewport = PixelRect {
        x: 0.0,
        y: 0.0,
        width: size.width as f32,
        height: size.height as f32,
    };
    let mut tree = state.pane_tree.lock().unwrap();
    tree.relayout(viewport, cell_w, cell_h, state.status_bar_height);
    drop(tree);

    state.dirty.store(true, Ordering::Relaxed);
}

fn pixel_to_cell(event: &NSEvent, state: &ViewState, tree: &PaneTree, cell_w: f32, cell_h: f32) -> Option<(crate::pane::PaneId, usize, usize)> {
    let loc = event.locationInWindow();
    let scale = state.scale_factor;
    let size = state.metal_layer.drawableSize();
    let view_h = size.height as f32 / scale;
    let px = loc.x as f32 * scale;
    let py = (view_h - loc.y as f32) * scale;
    let pane_id = tree.pane_at(px, py).unwrap_or(tree.focused);
    let pane = tree.pane(pane_id)?;
    let col = ((px - pane.rect.x) / cell_w).max(0.0) as usize;
    let row = ((py - pane.rect.y) / cell_h).max(0.0) as usize;
    Some((pane_id, col.min(pane.grid.cols().saturating_sub(1)), row.min(pane.grid.rows().saturating_sub(1))))
}

fn encode_sgr_mouse(button: u8, col: usize, row: usize, press: bool, sgr: bool) -> Vec<u8> {
    if sgr {
        let suffix = if press { 'M' } else { 'm' };
        format!("\x1b[<{button};{col};{row}{suffix}").into_bytes()
    } else {
        // Legacy X10 encoding
        let cb = button + 32;
        let cx = (col as u8).min(223) + 32;
        let cy = (row as u8).min(223) + 32;
        if press {
            vec![0x1b, b'[', b'M', cb, cx, cy]
        } else {
            vec![0x1b, b'[', b'M', 3 + 32, cx, cy] // release = button 3
        }
    }
}

fn pixel_to_grid_point(event: &NSEvent, state: &ViewState, tree: &PaneTree, cell_w: f32, cell_h: f32) -> Option<(crate::pane::PaneId, GridPoint)> {
    let loc = event.locationInWindow();
    let scale = state.scale_factor;

    let size = state.metal_layer.drawableSize();
    let view_h = size.height as f32 / scale;

    let px = loc.x as f32 * scale;
    let py = (view_h - loc.y as f32) * scale;

    let pane_id = tree.pane_at(px, py).unwrap_or(tree.focused);
    let pane = tree.pane(pane_id)?;
    let rect = pane.rect;

    let local_x = px - rect.x;
    let local_y = py - rect.y;

    let col = (local_x / cell_w) as usize;
    let vis_row = (local_y / cell_h) as usize;

    let sb_len = pane.grid.scrollback_len();
    let scroll_offset = pane.grid.scroll_offset;
    let abs_row = sb_len as i64 - scroll_offset as i64 + vis_row as i64;

    Some((pane_id, GridPoint { row: abs_row, col }))
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

fn render_frame() {
    VIEW_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let state = match state.as_mut() {
            Some(s) => s,
            None => return,
        };

        let size = state.metal_layer.drawableSize();
        let drawable = match state.metal_layer.nextDrawable() {
            Some(d) => d,
            None => return,
        };
        let texture = drawable.texture();

        // Lock atlas FIRST (canonical order: atlas → tree → image_store)
        let mut atlas = state.atlas.lock().unwrap();
        let tree = state.pane_tree.lock().unwrap();

        let viewport = PixelRect {
            x: 0.0,
            y: 0.0,
            width: size.width as f32,
            height: size.height as f32,
        };
        let (layouts, dividers) = tree.layout_info(viewport);

        let focused_id = tree.focused;
        let mut pane_render_data: Vec<PaneRenderData> = Vec::new();

        for (i, (id, rect)) in layouts.iter().enumerate() {
            if let Some(pane) = tree.pane(*id) {
                let sel = if pane.selection.is_active() {
                    Some(&pane.selection)
                } else {
                    None
                };
                let prompt_mark_rows = if state.prompt_indicator_color.is_some() {
                    pane.grid.marks.visible_prompt_rows(
                        pane.grid.scroll_offset,
                        pane.grid.scrollback_len(),
                        pane.grid.rows(),
                    )
                } else {
                    Vec::new()
                };
                pane_render_data.push(PaneRenderData {
                    grid: &pane.grid,
                    rect: *rect,
                    selection: sel,
                    is_focused: *id == focused_id,
                    pane_index: i,
                    cwd: &pane.grid.cwd,
                    prompt_mark_rows,
                    show_cursor: pane.grid.cursor_visible && *id == focused_id,
                });
            }
        }

        // Build confirm overlay info if dialog is active
        let confirm_overlay = state.confirm_dialog.as_ref().map(|dialog| {
            let region = match &dialog.action {
                ConfirmAction::ClosePane(pane_id) => {
                    // Find the pane rect for this pane
                    layouts.iter()
                        .find(|(id, _)| id == pane_id)
                        .map(|(_, r)| *r)
                        .unwrap_or(viewport)
                }
                ConfirmAction::QuitApp => viewport,
            };
            ConfirmOverlayInfo {
                region,
                title: dialog.title_text().to_string(),
                process_text: dialog.process_text(),
                opacity: dialog.opacity(),
            }
        });

        let img_store = state.image_store.lock().unwrap();
        state.renderer.draw_frame(
            &pane_render_data,
            &dividers,
            &mut atlas,
            ProtocolObject::from_ref(&*drawable),
            &texture,
            size.width as f32,
            size.height as f32,
            state.bg_opacity,
            state.selection_fg,
            state.selection_bg,
            &state.chrome,
            state.status_bar_height,
            state.prompt_indicator_color,
            Some(&img_store),
            confirm_overlay.as_ref(),
        );
        drop(img_store);

        drop(atlas);
        drop(tree);

        // Keep rendering during fade-in animation
        let still_animating = state.confirm_dialog.as_ref().map_or(false, |d| d.is_animating());
        if still_animating {
            state.dirty.store(true, Ordering::Relaxed);
        } else {
            state.dirty.store(false, Ordering::Relaxed);
        }
    });
}

pub fn render_if_dirty(dirty: &AtomicBool) {
    if dirty.load(Ordering::Relaxed) {
        render_frame();
    }
}

pub fn create_terminal_view(
    mtm: MainThreadMarker,
    device: &ProtocolObject<dyn MTLDevice>,
    pane_tree: Arc<Mutex<PaneTree>>,
    atlas: Arc<Mutex<GlyphAtlas>>,
    dirty: Arc<AtomicBool>,
    should_close: Arc<AtomicBool>,
    default_fg: (u8, u8, u8),
    default_bg: (u8, u8, u8),
    scale_factor: f32,
    font_family: String,
    font_size: f32,
    zoom_step: f32,
    min_font_size: f32,
    max_font_size: f32,
    selection_fg: (u8, u8, u8),
    selection_bg: (u8, u8, u8),
    chrome: ChromeColors,
    keybinds: KeybindMap,
    bg_opacity: f32,
    status_bar_height: f32,
    status_bar_enabled: bool,
    resize_step: f32,
    prompt_indicator_color: Option<(u8, u8, u8)>,
    image_store: Arc<Mutex<ImageStore>>,
    confirm_on_close_pane: bool,
    confirm_on_quit: bool,
) -> Retained<TerminalView> {
    let view = mtm.alloc::<TerminalView>().set_ivars(());
    let view: Retained<TerminalView> = unsafe { msg_send![super(view), init] };

    view.setWantsLayer(true);

    let metal_layer = CAMetalLayer::new();
    metal_layer.setDevice(Some(device));
    metal_layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
    metal_layer.setContentsScale(scale_factor as f64);
    metal_layer.setFramebufferOnly(true);

    if bg_opacity < 1.0 {
        metal_layer.setOpaque(false);
    }

    view.setLayer(Some(&metal_layer));

    // Set initial drawable size
    {
        let atlas = atlas.lock().unwrap();
        let tree = pane_tree.lock().unwrap();
        // Use first pane's grid size for initial window
        let pane_ids = tree.pane_ids();
        if let Some(pane) = pane_ids.first().and_then(|id| tree.pane(*id)) {
            let pixel_w = atlas.cell_width * pane.grid.cols() as f32;
            let pixel_h = atlas.cell_height * pane.grid.rows() as f32 + status_bar_height;
            metal_layer.setDrawableSize(NSSize {
                width: pixel_w as f64,
                height: pixel_h as f64,
            });
        }
    }

    let retained_device: Retained<ProtocolObject<dyn MTLDevice>> = device.retain();
    let renderer = MetalRenderer::new(retained_device, default_fg, default_bg);

    VIEW_STATE.with(|state| {
        *state.borrow_mut() = Some(ViewState {
            pane_tree,
            atlas,
            dirty,
            should_close,
            renderer,
            metal_layer,
            scale_factor,
            font_family,
            font_size,
            base_font_size: font_size,
            zoom_step,
            min_font_size,
            max_font_size,
            selection_fg,
            selection_bg,
            chrome,
            keybinds,
            bg_opacity,
            status_bar_height,
            status_bar_enabled,
            resize_step,
            prompt_indicator_color,
            image_store,
            confirm_dialog: None,
            confirm_on_close_pane,
            confirm_on_quit,
        });
    });

    view
}
