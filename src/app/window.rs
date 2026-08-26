use super::PaneCleanup;
use crate::app::confirm::{self, ConfirmAction, ConfirmDialog, ConfirmResult};
use crate::glyph_atlas::GlyphAtlas;
use crate::grid::MouseTracking;
use crate::input::keybind::{KeyModifiers, KeybindMap, PaneAction};
use crate::input::macos::{mouse_button_with_appkit_modifiers, translate_key_event};
use crate::input::mouse::{
    encode_alternate_scroll_steps, encode_mouse_event, mouse_wheel_route, MouseWheelRoute,
    MAX_WHEEL_STEPS_PER_EVENT, MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP,
};
use crate::layout::{PaneId, PixelRect, SplitDirection};
use crate::pane::PaneTree;
use crate::render_scene::{ChromeColors, ConfirmOverlayInfo, PaneRenderData};
use crate::renderer::image_store::ImageStore;
use crate::renderer::metal::MetalRenderer;
use crate::selection::GridPoint;
use crate::terminal_writer::TerminalWriteQueueError;
use crate::window_title::{normalized_window_title, sync_window_title_with, WINDOW_TITLE};

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

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardPasteError {
    TooLarge { byte_count: usize, limit: usize },
    AllocationFailed { byte_count: usize },
    ConversionFailed {
        expected_bytes: usize,
        converted_bytes: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacScrollPhase {
    Started,
    Continued,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacScrollUnits {
    Lines,
    Points {
        scale_factor_bits: u32,
        cell_height_bits: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MacScrollContext {
    pane_id: PaneId,
    route: MouseWheelRoute,
    units: MacScrollUnits,
    modifier_mask: u8,
}

#[derive(Debug, Clone, Copy)]
struct MacScrollSample {
    delta_y: f64,
    precise: bool,
    scale_factor: f32,
    cell_height: f32,
    phase: MacScrollPhase,
    pane_id: PaneId,
    route: MouseWheelRoute,
    modifier_mask: u8,
}

#[derive(Debug, Default)]
struct MacScrollState {
    line_remainder: f64,
    context: Option<MacScrollContext>,
}

impl MacScrollState {
    fn reset(&mut self) {
        self.line_remainder = 0.0;
        self.context = None;
    }

    fn consume(&mut self, sample: MacScrollSample) -> i32 {
        if !sample.delta_y.is_finite() {
            self.reset();
            return 0;
        }

        let (line_delta, units) = if sample.precise {
            if !sample.scale_factor.is_finite()
                || sample.scale_factor <= 0.0
                || !sample.cell_height.is_finite()
                || sample.cell_height <= 0.0
            {
                self.reset();
                return 0;
            }
            let line_delta = sample.delta_y * f64::from(sample.scale_factor)
                / f64::from(sample.cell_height);
            if !line_delta.is_finite() {
                self.reset();
                return 0;
            }
            (
                line_delta,
                MacScrollUnits::Points {
                    scale_factor_bits: sample.scale_factor.to_bits(),
                    cell_height_bits: sample.cell_height.to_bits(),
                },
            )
        } else {
            (sample.delta_y, MacScrollUnits::Lines)
        };

        let context = MacScrollContext {
            pane_id: sample.pane_id,
            route: sample.route,
            units,
            modifier_mask: if matches!(sample.route, MouseWheelRoute::Terminal(_)) {
                sample.modifier_mask
            } else {
                0
            },
        };
        if sample.phase == MacScrollPhase::Started || self.context != Some(context) {
            self.line_remainder = 0.0;
        }
        self.context = Some(context);

        let total = self.line_remainder + line_delta;
        if !total.is_finite() {
            self.reset();
            return 0;
        }
        let whole_steps = total.trunc();
        let limit = f64::from(MAX_WHEEL_STEPS_PER_EVENT);
        let steps = if whole_steps.abs() > limit {
            self.line_remainder = 0.0;
            if whole_steps.is_sign_positive() {
                i32::try_from(MAX_WHEEL_STEPS_PER_EVENT)
                    .expect("wheel step limit should fit i32")
            } else {
                -i32::try_from(MAX_WHEEL_STEPS_PER_EVENT)
                    .expect("wheel step limit should fit i32")
            }
        } else {
            self.line_remainder = total - whole_steps;
            whole_steps as i32
        };

        if sample.phase == MacScrollPhase::Finished {
            self.reset();
        }
        steps
    }
}

fn mac_scroll_phase(
    event_phase: NSEventPhase,
    momentum_phase: NSEventPhase,
) -> MacScrollPhase {
    let started = NSEventPhase::MayBegin | NSEventPhase::Began;
    let finished = NSEventPhase::Ended | NSEventPhase::Cancelled;
    if momentum_phase.intersects(started) {
        MacScrollPhase::Started
    } else if momentum_phase.intersects(finished) {
        MacScrollPhase::Finished
    } else if event_phase.intersects(started) {
        MacScrollPhase::Started
    } else if event_phase.intersects(finished) {
        MacScrollPhase::Finished
    } else {
        MacScrollPhase::Continued
    }
}

fn encode_macos_forwarded_wheel(
    route: MouseWheelRoute,
    steps: i32,
    terminal_cell: Option<(usize, usize)>,
    modifiers: NSEventModifierFlags,
) -> Option<Vec<u8>> {
    match route {
        MouseWheelRoute::Terminal(mouse_encoding) => {
            let (column, row) = terminal_cell
                .expect("terminal mouse route should have a validated cell");
            let button = if steps.is_positive() {
                MOUSE_WHEEL_UP
            } else {
                MOUSE_WHEEL_DOWN
            };
            let button = mouse_button_with_appkit_modifiers(button, modifiers);
            let report = encode_mouse_event(
                button,
                column + 1,
                row + 1,
                true,
                mouse_encoding,
            );
            let report_count = usize::try_from(
                steps.unsigned_abs().min(MAX_WHEEL_STEPS_PER_EVENT),
            )
                .expect("bounded wheel report count should fit usize");
            Some(report.repeat(report_count))
        }
        MouseWheelRoute::AlternateScroll {
            application_cursor_keys,
        } => Some(encode_alternate_scroll_steps(
            steps,
            application_cursor_keys,
        )),
        MouseWheelRoute::Scrollback => None,
    }
}

struct ViewState {
    pane_tree: Arc<Mutex<PaneTree>>,
    pane_cleanup: PaneCleanup,
    atlas: Arc<Mutex<GlyphAtlas>>,
    dirty: Arc<AtomicBool>,
    window_title: Arc<WindowTitleMailbox>,
    applied_window_title: String,
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
    mouse_wheel_state: MacScrollState,
    confirm_dialog: Option<ConfirmDialog>,
    confirm_on_close_pane: bool,
    confirm_on_quit: bool,
}

// Keep AppKit title updates independent from the potentially long-held PaneTree lock.
pub(super) struct WindowTitleMailbox {
    desired_title: Mutex<String>,
    pending: AtomicBool,
}

impl WindowTitleMailbox {
    pub(super) fn new() -> Self {
        Self {
            desired_title: Mutex::new(WINDOW_TITLE.to_string()),
            pending: AtomicBool::new(true),
        }
    }

    fn publish(&self, raw_title: &str) -> bool {
        let next_title = normalized_window_title(raw_title);
        let mut desired_title = self
            .desired_title
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if desired_title.as_str() == next_title.as_ref() {
            return false;
        }

        desired_title.clear();
        desired_title.push_str(next_title.as_ref());
        drop(desired_title);
        self.pending.store(true, Ordering::Release);
        true
    }

    fn has_pending_update(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    fn take_pending_title(&self) -> Option<String> {
        if !self.pending.swap(false, Ordering::AcqRel) {
            return None;
        }

        Some(
            self.desired_title
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        )
    }
}

pub(super) fn publish_focused_window_title(
    tree: &PaneTree,
    window_title: &WindowTitleMailbox,
) -> bool {
    let title = tree
        .focused_pane()
        .map(|pane| pane.grid.title())
        .unwrap_or_default();
    window_title.publish(title)
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
                            pane.queue_input(vec![0x03]);
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
                            let paste_target = {
                                let tree = state.pane_tree.lock().unwrap();
                                tree.focused_pane().map(|pane| {
                                    (
                                        pane.id,
                                        pane.grid.bracketed_paste,
                                        pane.max_input_bytes(),
                                    )
                                })
                            };
                            if let Some((pane_id, bracketed, max_input_bytes)) = paste_target {
                                match encode_clipboard_paste(
                                    text.as_ref(),
                                    bracketed,
                                    max_input_bytes,
                                ) {
                                    Ok(bytes) => {
                                        let byte_count = bytes.len();
                                        let tree = state.pane_tree.lock().unwrap();
                                        match tree.pane(pane_id) {
                                            Some(pane)
                                                if pane.grid.bracketed_paste == bracketed => {
                                                match pane.queue_paste_input(bytes) {
                                                    Ok(()) => {}
                                                    Err(TerminalWriteQueueError::Full) => {
                                                        log::warn!(
                                                            "dropping macOS clipboard paste of {byte_count} bytes: terminal input queue is full"
                                                        );
                                                    }
                                                    Err(TerminalWriteQueueError::Disconnected) => {
                                                        log::warn!(
                                                            "macOS clipboard paste was not queued: terminal input is disconnected"
                                                        );
                                                    }
                                                }
                                            }
                                            Some(_) => {
                                                log::warn!(
                                                    "dropping macOS clipboard paste: bracketed paste mode changed while encoding"
                                                );
                                            }
                                            None => {
                                                log::warn!(
                                                    "dropping macOS clipboard paste: target pane closed while encoding"
                                                );
                                            }
                                        }
                                    }
                                    Err(ClipboardPasteError::TooLarge { byte_count, limit }) => {
                                        log::warn!(
                                            "dropping macOS clipboard paste requiring at least {byte_count} bytes; limit is {limit}"
                                        );
                                    }
                                    Err(ClipboardPasteError::AllocationFailed { byte_count }) => {
                                        log::warn!(
                                            "dropping macOS clipboard paste: failed to allocate {byte_count} bytes"
                                        );
                                    }
                                    Err(ClipboardPasteError::ConversionFailed {
                                        expected_bytes,
                                        converted_bytes,
                                    }) => {
                                        log::warn!(
                                            "dropping macOS clipboard paste: converted {converted_bytes} of {expected_bytes} UTF-8 bytes"
                                        );
                                    }
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
            let blocked = VIEW_STATE.with(|slot| {
                let mut slot = slot.borrow_mut();
                if let Some(state) = slot.as_mut() {
                    if state.confirm_dialog.is_some() {
                        state.mouse_wheel_state.reset();
                        return true;
                    }
                }
                false
            });
            if blocked { return; }
            let delta_y = event.scrollingDeltaY();
            let precise = event.hasPreciseScrollingDeltas();
            let phase = mac_scroll_phase(event.phase(), event.momentumPhase());
            let modifiers = event.modifierFlags();
            let modifier_mask = mouse_button_with_appkit_modifiers(0, modifiers);
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    let atlas = state.atlas.lock().unwrap();
                    let cell_w = atlas.cell_width;
                    let cell_h = atlas.cell_height;
                    drop(atlas);
                    let mut tree = state.pane_tree.lock().unwrap();
                    let Some((pane_id, route)) = tree
                        .focused_pane()
                        .map(|pane| (pane.id, mouse_wheel_route(&pane.grid)))
                    else {
                        state.mouse_wheel_state.reset();
                        return;
                    };
                    let terminal_cell = match route {
                        MouseWheelRoute::Terminal(_) => {
                            match pixel_to_cell(event, state, &tree, cell_w, cell_h) {
                                Some((cell_pane_id, column, row)) if cell_pane_id == pane_id => {
                                    Some((column, row))
                                }
                                _ => {
                                    state.mouse_wheel_state.reset();
                                    return;
                                }
                            }
                        }
                        MouseWheelRoute::Scrollback | MouseWheelRoute::AlternateScroll { .. } => {
                            None
                        }
                    };
                    let steps = state.mouse_wheel_state.consume(MacScrollSample {
                        delta_y,
                        precise,
                        scale_factor: state.scale_factor,
                        cell_height: cell_h,
                        phase,
                        pane_id,
                        route,
                        modifier_mask,
                    });
                    if steps == 0 {
                        return;
                    }

                    let mut viewport_changed = false;
                    match route {
                        MouseWheelRoute::Terminal(_)
                        | MouseWheelRoute::AlternateScroll { .. } => {
                            let reports = encode_macos_forwarded_wheel(
                                route,
                                steps,
                                terminal_cell,
                                modifiers,
                            )
                            .expect("forwarded wheel route should produce terminal bytes");
                            if let Some(pane) = tree.focused_pane() {
                                pane.queue_input(reports);
                            }
                        }
                        MouseWheelRoute::Scrollback => {
                            if let Some(pane) = tree.focused_pane_mut() {
                                let previous_offset = pane.grid.scroll_offset;
                                let lines = usize::try_from(steps.unsigned_abs())
                                    .expect("bounded scrollback steps should fit usize");
                                if steps.is_positive() { pane.grid.scroll_viewport_up(lines); }
                                else { pane.grid.scroll_viewport_down(lines); }
                                viewport_changed = pane.grid.scroll_offset != previous_offset;
                            }
                        }
                    }
                    drop(tree);
                    if viewport_changed {
                        state.dirty.store(true, Ordering::Relaxed);
                    }
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
                        if tree.focused != pane_id {
                            tree.focused = pane_id;
                            publish_focused_window_title(&tree, state.window_title.as_ref());
                        }
                        let tracking = tree.pane(pane_id).map(|p| (p.grid.mouse_tracking, p.grid.mouse_encoding));
                        if let Some((mt, encoding)) = tracking {
                            if mt != MouseTracking::None && !has_shift {
                                let seq = encode_mouse_event(0, gc + 1, gr + 1, true, encoding);
                                if let Some(pane) = tree.pane(pane_id) {
                                    pane.queue_input(seq);
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
                    let tracking = tree.focused_pane().map(|p| (p.grid.mouse_tracking, p.grid.mouse_encoding));
                    if let Some((mt, encoding)) = tracking {
                        let forward = (mt == MouseTracking::ButtonEvent || mt == MouseTracking::AnyEvent) && !has_shift;
                        if forward {
                            if let Some((_id, gc, gr)) = cell_info {
                                let seq = encode_mouse_event(32, gc + 1, gr + 1, true, encoding);
                                if let Some(pane) = tree.pane(focused) {
                                    pane.queue_motion_input(seq);
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
                                let seq = encode_mouse_event(0, gc + 1, gr + 1, false, pane.grid.mouse_encoding);
                                pane.queue_input(seq);
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
                            pane.queue_input(bytes);
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
                            match GlyphAtlas::new(
                                &state.font_family,
                                state.font_size,
                                new_scale,
                            ) {
                                Ok(replacement) => {
                                    let cell_w = replacement.cell_width;
                                    let cell_h = replacement.cell_height;
                                    let status_bar_height = if state.status_bar_enabled {
                                        (cell_h * 1.5).ceil()
                                    } else {
                                        0.0
                                    };
                                    let backing_size =
                                        self.convertSizeToBacking(self.bounds().size);

                                    log::info!("Scale factor changed to {new_scale}");
                                    let mut atlas = state.atlas.lock().unwrap();
                                    let mut tree = state.pane_tree.lock().unwrap();
                                    *atlas = replacement;
                                    update_pane_geometry(
                                        &mut tree,
                                        cell_w,
                                        cell_h,
                                        status_bar_height,
                                        backing_size,
                                    );
                                    drop(tree);
                                    drop(atlas);

                                    state.scale_factor = new_scale;
                                    state.status_bar_height = status_bar_height;
                                    state.metal_layer.setContentsScale(new_scale as f64);
                                    state.metal_layer.setDrawableSize(backing_size);
                                    state.dirty.store(true, Ordering::Relaxed);
                                }
                                Err(error) => log::error!(
                                    "Failed to rebuild glyph atlas for scale {new_scale}: {error}; \
                                     keeping scale {}",
                                    state.scale_factor
                                ),
                            }
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
        let previous_focus = tree.focused;
        let mut closed_pane = None;

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
                let outcome = tree.close(id);
                closed_pane = outcome.closed_pane;
                if outcome.should_terminate {
                    drop(tree);
                    if let Some(pane) = closed_pane {
                        state.pane_cleanup.retire(pane);
                    }
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

        if tree.focused != previous_focus {
            publish_focused_window_title(&tree, state.window_title.as_ref());
        }
        drop(tree);
        if let Some(pane) = closed_pane {
            state.pane_cleanup.retire(pane);
        }
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
            let previous_focus = tree.focused;
            let outcome = tree.close(pane_id);
            let should_terminate = outcome.should_terminate;
            let closed_pane = outcome.closed_pane;
            if should_terminate {
                drop(tree);
                if let Some(pane) = closed_pane {
                    state.pane_cleanup.retire(pane);
                }
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
            if tree.focused != previous_focus {
                publish_focused_window_title(&tree, state.window_title.as_ref());
            }
            drop(tree);
            if let Some(pane) = closed_pane {
                state.pane_cleanup.retire(pane);
            }
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

    let drawable_size = state.metal_layer.drawableSize();
    let mut atlas = state.atlas.lock().unwrap();
    if let Err(error) = atlas.clear_and_resize(new_size) {
        log::error!("Failed to resize glyph atlas to {new_size}pt: {error}");
        return;
    }
    let cell_w = atlas.cell_width;
    let cell_h = atlas.cell_height;
    let status_bar_height = if state.status_bar_enabled {
        (cell_h * 1.5).ceil()
    } else {
        0.0
    };
    let mut tree = state.pane_tree.lock().unwrap();
    update_pane_geometry(
        &mut tree,
        cell_w,
        cell_h,
        status_bar_height,
        drawable_size,
    );
    drop(tree);
    drop(atlas);

    state.font_size = new_size;
    state.status_bar_height = status_bar_height;
    log::info!("Font zoom: {new_size}pt");

    state.dirty.store(true, Ordering::Relaxed);
}

fn update_pane_geometry(
    tree: &mut PaneTree,
    cell_w: f32,
    cell_h: f32,
    status_bar_height: f32,
    drawable_size: NSSize,
) {
    for id in tree.pane_ids() {
        if let Some(pane) = tree.pane_mut(id) {
            pane.grid.cell_pixel_width = cell_w as u16;
            pane.grid.cell_pixel_height = cell_h as u16;
        }
    }

    let viewport = PixelRect {
        x: 0.0,
        y: 0.0,
        width: drawable_size.width as f32,
        height: drawable_size.height as f32,
    };
    tree.relayout(viewport, cell_w, cell_h, status_bar_height);
}

fn pixel_to_cell(
    event: &NSEvent,
    state: &ViewState,
    tree: &PaneTree,
    cell_w: f32,
    cell_h: f32,
) -> Option<(PaneId, usize, usize)> {
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

fn pixel_to_grid_point(
    event: &NSEvent,
    state: &ViewState,
    tree: &PaneTree,
    cell_w: f32,
    cell_h: f32,
) -> Option<(PaneId, GridPoint)> {
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

fn paste_from_clipboard() -> Option<Retained<NSString>> {
    unsafe {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.stringForType(NSPasteboardTypeString)
    }
}

fn encode_clipboard_paste(
    text: &NSString,
    bracketed: bool,
    capacity_budget: usize,
) -> Result<Vec<u8>, ClipboardPasteError> {
    let utf16_len = text.len_utf16();
    let minimum_framed_len = if bracketed {
        bracketed_paste_len(utf16_len).unwrap_or(usize::MAX)
    } else {
        utf16_len
    };
    if minimum_framed_len > capacity_budget {
        return Err(ClipboardPasteError::TooLarge {
            byte_count: minimum_framed_len,
            limit: capacity_budget,
        });
    }

    let payload_len = text.len();
    let framed_len = if bracketed {
        bracketed_paste_len(payload_len).ok_or(ClipboardPasteError::TooLarge {
            byte_count: usize::MAX,
            limit: capacity_budget,
        })?
    } else {
        payload_len
    };
    if framed_len > capacity_budget {
        return Err(ClipboardPasteError::TooLarge {
            byte_count: framed_len,
            limit: capacity_budget,
        });
    }

    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(framed_len)
        .map_err(|_| ClipboardPasteError::AllocationFailed {
            byte_count: framed_len,
        })?;
    if bytes.capacity() > capacity_budget {
        return Err(ClipboardPasteError::TooLarge {
            byte_count: bytes.capacity(),
            limit: capacity_budget,
        });
    }
    bytes.resize(framed_len, 0);

    let payload_start = if bracketed {
        BRACKETED_PASTE_START.len()
    } else {
        0
    };
    if payload_len == 0 {
        if utf16_len != 0 {
            return Err(ClipboardPasteError::ConversionFailed {
                expected_bytes: payload_len,
                converted_bytes: 0,
            });
        }
    } else {
        let mut converted_bytes = 0;
        let mut remaining_range = NSRange::default();
        let source_range = NSRange::new(0, utf16_len);
        let converted = unsafe {
            text.getBytes_maxLength_usedLength_encoding_options_range_remainingRange(
                bytes[payload_start..payload_start + payload_len]
                    .as_mut_ptr()
                    .cast(),
                payload_len,
                &mut converted_bytes,
                NSUTF8StringEncoding,
                NSStringEncodingConversionOptions::empty(),
                source_range,
                &mut remaining_range,
            )
        };
        if !converted || converted_bytes != payload_len || !remaining_range.is_empty() {
            return Err(ClipboardPasteError::ConversionFailed {
                expected_bytes: payload_len,
                converted_bytes,
            });
        }
    }

    if bracketed {
        bytes[..BRACKETED_PASTE_START.len()].copy_from_slice(BRACKETED_PASTE_START);
        bytes[framed_len - BRACKETED_PASTE_END.len()..]
            .copy_from_slice(BRACKETED_PASTE_END);
    }
    Ok(bytes)
}

fn bracketed_paste_len(payload_len: usize) -> Option<usize> {
    payload_len
        .checked_add(BRACKETED_PASTE_START.len())?
        .checked_add(BRACKETED_PASTE_END.len())
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

fn sync_pending_window_title_with<F>(
    window_title: &WindowTitleMailbox,
    applied_title: &mut String,
    set_title: F,
) -> bool
where
    F: FnOnce(&str),
{
    let Some(title) = window_title.take_pending_title() else {
        return false;
    };
    sync_window_title_with(applied_title, &title, set_title)
}

pub fn sync_window_title(window_number: NSInteger) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::error!("refusing to update the macOS window title outside the main thread");
        return;
    };
    let update_pending = VIEW_STATE.with(|view_state| {
        view_state
            .borrow()
            .as_ref()
            .is_some_and(|state| state.window_title.has_pending_update())
    });
    if !update_pending {
        return;
    }
    let app = NSApplication::sharedApplication(mtm);
    let Some(window) = app.windowWithWindowNumber(window_number) else {
        return;
    };
    let mut title_to_apply = None;

    VIEW_STATE.with(|view_state| {
        let mut view_state = view_state.borrow_mut();
        let Some(state) = view_state.as_mut() else {
            return;
        };
        if !state.window_title.has_pending_update() {
            return;
        }

        let window_title = state.window_title.clone();
        sync_pending_window_title_with(
            window_title.as_ref(),
            &mut state.applied_window_title,
            |title| title_to_apply = Some(title.to_string()),
        );
    });

    if let Some(title) = title_to_apply {
        window.setTitle(&NSString::from_str(&title));
    }
}

pub(super) fn create_terminal_view(
    mtm: MainThreadMarker,
    device: &ProtocolObject<dyn MTLDevice>,
    pane_tree: Arc<Mutex<PaneTree>>,
    pane_cleanup: PaneCleanup,
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
    window_title: Arc<WindowTitleMailbox>,
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
            pane_cleanup,
            atlas,
            dirty,
            window_title,
            applied_window_title: WINDOW_TITLE.to_string(),
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
            mouse_wheel_state: MacScrollState::default(),
            confirm_dialog: None,
            confirm_on_close_pane,
            confirm_on_quit,
        });
    });

    view
}

#[cfg(test)]
mod tests {
    use super::{
        bracketed_paste_len, encode_clipboard_paste, encode_macos_forwarded_wheel,
        mac_scroll_phase, sync_pending_window_title_with, ClipboardPasteError, MacScrollPhase,
        MacScrollSample, MacScrollState, WindowTitleMailbox, BRACKETED_PASTE_END,
        BRACKETED_PASTE_START, WINDOW_TITLE,
    };
    use crate::grid::MouseEncoding;
    use crate::input::mouse::MouseWheelRoute;
    use objc2::AnyThread;
    use objc2_app_kit::{NSEventModifierFlags, NSEventPhase};
    use objc2_foundation::NSString;
    use std::cell::{Cell, RefCell};
    use std::ptr::NonNull;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::thread;

    fn scroll_sample(delta_y: f64) -> MacScrollSample {
        MacScrollSample {
            delta_y,
            precise: false,
            scale_factor: 2.0,
            cell_height: 20.0,
            phase: MacScrollPhase::Continued,
            pane_id: 1,
            route: MouseWheelRoute::Scrollback,
            modifier_mask: 0,
        }
    }

    fn assert_remainder(state: &MacScrollState, expected: f64) {
        assert!(
            (state.line_remainder - expected).abs() < f64::EPSILON,
            "expected remainder {expected}, got {}",
            state.line_remainder,
        );
    }

    #[test]
    fn clipboard_paste_encoding_copies_utf8_and_embedded_nul_directly() {
        let plain_text = NSString::from_str("plain\0é");
        let plain = encode_clipboard_paste(plain_text.as_ref(), false, 64)
            .expect("plain UTF-8 paste should fit its budget");
        assert_eq!(plain, "plain\0é".as_bytes());

        let supplementary_text = NSString::from_str("😀");
        assert_eq!(supplementary_text.len_utf16(), 2);
        assert_eq!(supplementary_text.len(), 4);
        assert_eq!(
            encode_clipboard_paste(supplementary_text.as_ref(), false, 64)
                .expect("a supplementary scalar should convert across its full UTF-16 range"),
            "😀".as_bytes()
        );

        let bracketed_text = NSString::from_str("a\0é");
        let framed = encode_clipboard_paste(bracketed_text.as_ref(), true, 64)
            .expect("bracketed UTF-8 paste should fit its budget");
        let mut expected = BRACKETED_PASTE_START.to_vec();
        expected.extend_from_slice("a\0é".as_bytes());
        expected.extend_from_slice(BRACKETED_PASTE_END);
        assert_eq!(framed, expected);

        let empty = NSString::from_str("");
        assert!(encode_clipboard_paste(empty.as_ref(), false, 0)
            .expect("empty plain paste should not allocate")
            .is_empty());
        assert_eq!(
            encode_clipboard_paste(
                empty.as_ref(),
                true,
                BRACKETED_PASTE_START.len() + BRACKETED_PASTE_END.len(),
            )
            .expect("empty bracketed paste should contain only its frame"),
            [BRACKETED_PASTE_START, BRACKETED_PASTE_END].concat()
        );
    }

    #[test]
    fn clipboard_paste_preflight_accepts_exact_budget_and_rejects_one_less() {
        let text = NSString::from_str("é");
        let required_bytes = text.len() + BRACKETED_PASTE_START.len() + BRACKETED_PASTE_END.len();
        assert_eq!(
            encode_clipboard_paste(text.as_ref(), true, required_bytes)
                .expect("the exact retained-byte budget should be accepted")
                .len(),
            required_bytes
        );
        assert_eq!(
            encode_clipboard_paste(text.as_ref(), true, required_bytes - 1),
            Err(ClipboardPasteError::TooLarge {
                byte_count: required_bytes,
                limit: required_bytes - 1,
            })
        );

        let framing_bytes = BRACKETED_PASTE_START.len() + BRACKETED_PASTE_END.len();
        assert_eq!(
            bracketed_paste_len(usize::MAX - framing_bytes),
            Some(usize::MAX)
        );
        assert_eq!(
            bracketed_paste_len(usize::MAX - framing_bytes + 1),
            None
        );
        assert_eq!(bracketed_paste_len(usize::MAX), None);
    }

    #[test]
    fn clipboard_paste_rejects_unconvertible_utf16() {
        let unpaired_surrogate = [0xd800_u16];
        let text = unsafe {
            NSString::initWithCharacters_length(
                NSString::alloc(),
                NonNull::from(&unpaired_surrogate[0]),
                unpaired_surrogate.len(),
            )
        };
        assert_eq!(text.len_utf16(), 1);
        assert_eq!(
            encode_clipboard_paste(text.as_ref(), false, 64),
            Err(ClipboardPasteError::ConversionFailed {
                expected_bytes: 0,
                converted_bytes: 0,
            })
        );
    }

    #[test]
    fn coarse_scroll_deltas_are_lines_and_are_bounded_per_event() {
        let mut state = MacScrollState::default();
        assert_eq!(state.consume(scroll_sample(3.0)), 3);
        assert_eq!(state.consume(scroll_sample(-6.0)), -6);

        let mut geometry_independent = scroll_sample(1.0);
        geometry_independent.scale_factor = f32::NAN;
        geometry_independent.cell_height = 0.0;
        assert_eq!(state.consume(geometry_independent), 1);

        assert_eq!(state.consume(scroll_sample(f64::MAX)), 32);
        assert_remainder(&state, 0.0);
        assert_eq!(state.consume(scroll_sample(-f64::MAX)), -32);
        assert_remainder(&state, 0.0);
    }

    #[test]
    fn precise_scroll_converts_points_to_physical_pixels_and_accumulates_lines() {
        let mut state = MacScrollState::default();
        let mut sample = scroll_sample(5.0);
        sample.precise = true;
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.5);
        assert_eq!(state.consume(sample), 1);
        assert_remainder(&state, 0.0);

        sample.delta_y = -5.0;
        sample.phase = MacScrollPhase::Started;
        assert_eq!(state.consume(sample), 0);
        sample.phase = MacScrollPhase::Continued;
        assert_eq!(state.consume(sample), -1);

        sample.delta_y = 5.0;
        sample.phase = MacScrollPhase::Started;
        assert_eq!(state.consume(sample), 0);
        sample.delta_y = -5.0;
        sample.phase = MacScrollPhase::Continued;
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.0);

        sample.delta_y = 0.005;
        sample.phase = MacScrollPhase::Started;
        assert_eq!(state.consume(sample), 0);
        assert!(state.line_remainder > 0.0);
    }

    #[test]
    fn gesture_and_momentum_boundaries_reset_even_when_delta_is_zero() {
        let none = NSEventPhase::None;
        for phase in [NSEventPhase::None, NSEventPhase::Changed, NSEventPhase::Stationary] {
            assert_eq!(mac_scroll_phase(phase, none), MacScrollPhase::Continued);
            assert_eq!(mac_scroll_phase(none, phase), MacScrollPhase::Continued);
        }
        for phase in [NSEventPhase::MayBegin, NSEventPhase::Began] {
            assert_eq!(mac_scroll_phase(phase, none), MacScrollPhase::Started);
            assert_eq!(mac_scroll_phase(none, phase), MacScrollPhase::Started);
        }
        for phase in [NSEventPhase::Ended, NSEventPhase::Cancelled] {
            assert_eq!(mac_scroll_phase(phase, none), MacScrollPhase::Finished);
            assert_eq!(mac_scroll_phase(none, phase), MacScrollPhase::Finished);
        }
        assert_eq!(
            mac_scroll_phase(NSEventPhase::Ended, NSEventPhase::Began),
            MacScrollPhase::Started
        );
        assert_eq!(
            mac_scroll_phase(NSEventPhase::Began, NSEventPhase::Ended),
            MacScrollPhase::Finished
        );

        let mut state = MacScrollState::default();
        assert_eq!(state.consume(scroll_sample(0.75)), 0);
        assert_remainder(&state, 0.75);
        let mut boundary = scroll_sample(0.0);
        boundary.phase = MacScrollPhase::Started;
        assert_eq!(state.consume(boundary), 0);
        assert_remainder(&state, 0.0);

        assert_eq!(state.consume(scroll_sample(0.75)), 0);
        boundary.delta_y = 0.25;
        boundary.phase = MacScrollPhase::Finished;
        assert_eq!(state.consume(boundary), 1);
        assert_remainder(&state, 0.0);
        assert_eq!(state.context, None);

        assert_eq!(state.consume(scroll_sample(0.75)), 0);
        boundary.delta_y = 0.0;
        assert_eq!(state.consume(boundary), 0);
        assert_remainder(&state, 0.0);
        assert_eq!(state.context, None);
    }

    #[test]
    fn invalid_scroll_samples_clear_state_and_precise_steps_are_bounded() {
        for delta_y in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut state = MacScrollState::default();
            assert_eq!(state.consume(scroll_sample(0.75)), 0);
            let mut invalid = scroll_sample(delta_y);
            invalid.precise = true;
            assert_eq!(state.consume(invalid), 0);
            assert_remainder(&state, 0.0);
            assert_eq!(state.context, None);
        }

        for (scale_factor, cell_height) in [
            (0.0, 20.0),
            (-1.0, 20.0),
            (f32::NAN, 20.0),
            (f32::INFINITY, 20.0),
            (2.0, 0.0),
            (2.0, -1.0),
            (2.0, f32::NAN),
            (2.0, f32::INFINITY),
        ] {
            let mut state = MacScrollState::default();
            let mut sample = scroll_sample(5.0);
            sample.precise = true;
            sample.scale_factor = scale_factor;
            sample.cell_height = cell_height;
            assert_eq!(state.consume(sample), 0);
            assert_eq!(state.context, None);
        }

        let mut state = MacScrollState::default();
        let mut huge = scroll_sample(1_000_000.0);
        huge.precise = true;
        assert_eq!(state.consume(huge), 32);
        assert_remainder(&state, 0.0);
        huge.delta_y = -1_000_000.0;
        assert_eq!(state.consume(huge), -32);
        assert_remainder(&state, 0.0);
    }

    #[test]
    fn scroll_remainder_is_scoped_to_route_pane_modifiers_and_precise_geometry() {
        let mut state = MacScrollState::default();
        let mut sample = scroll_sample(7.5);
        sample.precise = true;
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.75);

        sample.delta_y = 2.5;
        sample.route = MouseWheelRoute::Terminal(MouseEncoding::Sgr);
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.25);

        sample.delta_y = 7.5;
        sample.modifier_mask = 4;
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.75);

        sample.delta_y = 2.5;
        sample.modifier_mask = 8;
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.25);

        sample.delta_y = 7.5;
        sample.route = MouseWheelRoute::AlternateScroll {
            application_cursor_keys: false,
        };
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.75);

        sample.delta_y = 2.5;
        sample.modifier_mask = 16;
        assert_eq!(state.consume(sample), 1);
        assert_remainder(&state, 0.0);

        sample.delta_y = 2.5;
        sample.route = MouseWheelRoute::AlternateScroll {
            application_cursor_keys: true,
        };
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.25);

        sample.delta_y = 7.5;
        sample.pane_id = 2;
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.75);

        sample.delta_y = 5.0;
        sample.cell_height = 40.0;
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.25);

        sample.delta_y = 10.0;
        sample.scale_factor = 1.0;
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.25);

        sample.delta_y = 0.75;
        sample.precise = false;
        assert_eq!(state.consume(sample), 0);
        assert_remainder(&state, 0.75);
    }

    #[test]
    fn macos_tracking_wheel_preserves_modifiers_in_sgr_and_legacy_reports() {
        let modifiers = NSEventModifierFlags::Shift
            | NSEventModifierFlags::Option
            | NSEventModifierFlags::Control;
        assert_eq!(
            encode_macos_forwarded_wheel(
                MouseWheelRoute::Terminal(MouseEncoding::Sgr),
                2,
                Some((1, 2)),
                modifiers,
            )
            .as_deref(),
            Some(b"\x1b[<92;2;3M\x1b[<92;2;3M".as_slice()),
        );
        assert_eq!(
            encode_macos_forwarded_wheel(
                MouseWheelRoute::Terminal(MouseEncoding::Sgr),
                -1,
                Some((1, 2)),
                modifiers,
            )
            .as_deref(),
            Some(b"\x1b[<93;2;3M".as_slice()),
        );
        assert_eq!(
            encode_macos_forwarded_wheel(
                MouseWheelRoute::Terminal(MouseEncoding::Default),
                2,
                Some((1, 2)),
                modifiers,
            ),
            Some([27, 91, 77, 124, 34, 35].repeat(2)),
        );
    }

    #[test]
    fn macos_alternate_scroll_ignores_mouse_modifiers() {
        let modifiers = NSEventModifierFlags::Shift
            | NSEventModifierFlags::Option
            | NSEventModifierFlags::Control
            | NSEventModifierFlags::Command;
        assert_eq!(
            encode_macos_forwarded_wheel(
                MouseWheelRoute::AlternateScroll {
                    application_cursor_keys: false,
                },
                2,
                None,
                modifiers,
            )
            .as_deref(),
            Some(b"\x1b[A\x1b[A".as_slice()),
        );
        assert_eq!(
            encode_macos_forwarded_wheel(
                MouseWheelRoute::AlternateScroll {
                    application_cursor_keys: true,
                },
                -2,
                None,
                modifiers,
            )
            .as_deref(),
            Some(b"\x1bOB\x1bOB".as_slice()),
        );
    }

    #[test]
    fn window_title_sync_skips_work_without_a_pending_update() {
        let window_title = WindowTitleMailbox::new();
        let mut applied_title = WINDOW_TITLE.to_string();
        let setters = Cell::new(0);

        assert!(!sync_pending_window_title_with(
            &window_title,
            &mut applied_title,
            |_| setters.set(setters.get() + 1),
        ));
        assert!(!window_title.has_pending_update());

        assert!(!sync_pending_window_title_with(
            &window_title,
            &mut applied_title,
            |_| setters.set(setters.get() + 1),
        ));
        assert_eq!(setters.get(), 0);
    }

    #[test]
    fn window_title_mailbox_coalesces_to_the_latest_normalized_value() {
        let window_title = WindowTitleMailbox::new();
        let mut applied_title = WINDOW_TITLE.to_string();
        let applied = RefCell::new(Vec::new());

        assert!(window_title.publish("first"));
        assert!(window_title.publish("latest\n"));
        assert!(!window_title.publish("latest"));
        assert!(sync_pending_window_title_with(
            &window_title,
            &mut applied_title,
            |title| applied.borrow_mut().push(title.to_string()),
        ));

        assert!(!window_title.has_pending_update());
        assert_eq!(applied.into_inner(), ["latest"]);
    }

    #[test]
    fn window_title_update_after_take_remains_pending_for_the_next_tick() {
        let window_title = WindowTitleMailbox::new();
        let mut applied_title = WINDOW_TITLE.to_string();
        let applied = RefCell::new(Vec::new());

        window_title.publish("first");
        let first = window_title
            .take_pending_title()
            .expect("the first update should be pending");
        assert!(!window_title.has_pending_update());

        window_title.publish("latest");
        assert!(window_title.has_pending_update());
        assert!(crate::window_title::sync_window_title_with(
            &mut applied_title,
            &first,
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert!(sync_pending_window_title_with(
            &window_title,
            &mut applied_title,
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert!(!window_title.has_pending_update());
        assert_eq!(applied.into_inner(), ["first", "latest"]);
    }

    #[test]
    fn window_title_publish_between_swap_and_clone_keeps_the_latest_value_pending() {
        let window_title = Arc::new(WindowTitleMailbox::new());
        let mut applied_title = WINDOW_TITLE.to_string();
        let applied = RefCell::new(Vec::new());

        window_title.publish("first");
        let mut desired_title = window_title
            .desired_title
            .lock()
            .expect("the title mailbox should lock");
        let consumer = window_title.clone();
        let take = thread::spawn(move || consumer.take_pending_title());

        while window_title.has_pending_update() {
            thread::yield_now();
        }

        desired_title.clear();
        desired_title.push_str("latest");
        drop(desired_title);
        window_title.pending.store(true, Ordering::Release);

        let taken = take
            .join()
            .expect("the title consumer should not panic")
            .expect("the raced title should be available");
        assert_eq!(taken, "latest");
        assert!(window_title.has_pending_update());

        assert!(crate::window_title::sync_window_title_with(
            &mut applied_title,
            &taken,
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert!(!sync_pending_window_title_with(
            window_title.as_ref(),
            &mut applied_title,
            |_| panic!("the redundant pending update must be deduplicated"),
        ));
        assert!(!window_title.has_pending_update());
        assert_eq!(applied.into_inner(), ["latest"]);
    }

    #[test]
    fn window_title_setter_runs_after_the_mailbox_lock_is_released() {
        let window_title = WindowTitleMailbox::new();
        let mut applied_title = WINDOW_TITLE.to_string();

        window_title.publish("main thread");
        assert!(sync_pending_window_title_with(
            &window_title,
            &mut applied_title,
            |_| {
                assert!(
                    window_title.desired_title.try_lock().is_ok(),
                    "the title mailbox must be unlocked before calling AppKit"
                );
            },
        ));
    }
}
