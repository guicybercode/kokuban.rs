use super::reader::ReaderWake;
use super::render_scheduler::RenderScheduler;
use crate::app::confirm::{self, ConfirmAction, ConfirmDialog, ConfirmResult};
use crate::config::OptionAsAlt;
use crate::glyph_atlas::GlyphAtlas;
use crate::grid::MouseTracking;
use crate::input::keybind::{KeyModifiers, KeybindMap, PaneAction};
use crate::input::macos::{
    key_modifiers_from_appkit, key_name_from_key_code, mouse_button_with_appkit_modifiers,
    option_acts_as_meta, terminal_key_from_key_code, translate_key_event,
};
use crate::input::mouse::{
    encode_alternate_scroll_steps, encode_mouse_event, mouse_wheel_route, MouseWheelRoute,
    MAX_WHEEL_STEPS_PER_EVENT, MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP,
};
use crate::layout::{PaneId, PixelRect};
use crate::notifications::NotificationController;
use crate::pane::actions::{apply_pane_action, PaneActionHost, ZoomDirection};
use crate::pane::cleanup::PaneCleanup;
use crate::pane::focus::{
    dispatch_pane_focus_transition_locked, focus_report_bytes, publish_focused_window_title,
};
use crate::pane::pane::Pane;
use crate::pane::PaneTree;
use crate::render_scene::{ChromeColors, ConfirmOverlayInfo, PaneRenderData, StatusBarLayout};
use crate::renderer::image_store::ImageStore;
use crate::renderer::metal::{FrameInputs, MetalRenderer};
use crate::search::SearchKey;
use crate::selection::GridPoint;
use crate::terminal_colors::TerminalColors;
use crate::terminal_writer::TerminalWriteQueueError;
use crate::update::{self, SharedUpdateStatus, UpdateStatus};
use crate::window_title::{sync_pending_window_title_with, WindowTitleMailbox, WINDOW_TITLE};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
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
    TooLarge {
        byte_count: usize,
        limit: usize,
    },
    AllocationFailed {
        byte_count: usize,
    },
    ConversionFailed {
        expected_bytes: usize,
        converted_bytes: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacKeyEquivalentRoute {
    PassThrough,
    Quit,
    Copy,
    Paste,
    ZoomIn,
    Keybind,
}

fn mac_key_equivalent_route(key_code: u16, modifiers: KeyModifiers) -> MacKeyEquivalentRoute {
    if !modifiers.contains(KeyModifiers::SUPER) {
        return MacKeyEquivalentRoute::PassThrough;
    }

    if modifiers == KeyModifiers::SUPER {
        return match key_code {
            12 => MacKeyEquivalentRoute::Quit,
            8 => MacKeyEquivalentRoute::Copy,
            9 => MacKeyEquivalentRoute::Paste,
            _ => MacKeyEquivalentRoute::Keybind,
        };
    }

    if key_code == 24 && modifiers == (KeyModifiers::SUPER | KeyModifiers::SHIFT) {
        return MacKeyEquivalentRoute::ZoomIn;
    }

    MacKeyEquivalentRoute::Keybind
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacScrollbackAction {
    PageUp,
    PageDown,
    Home,
    End,
}

fn mac_scrollback_action(key_code: u16, modifiers: KeyModifiers) -> Option<MacScrollbackAction> {
    if modifiers != KeyModifiers::SHIFT {
        return None;
    }

    match key_code {
        116 => Some(MacScrollbackAction::PageUp),
        121 => Some(MacScrollbackAction::PageDown),
        115 => Some(MacScrollbackAction::Home),
        119 => Some(MacScrollbackAction::End),
        _ => None,
    }
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
            let line_delta =
                sample.delta_y * f64::from(sample.scale_factor) / f64::from(sample.cell_height);
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
                i32::try_from(MAX_WHEEL_STEPS_PER_EVENT).expect("wheel step limit should fit i32")
            } else {
                -i32::try_from(MAX_WHEEL_STEPS_PER_EVENT).expect("wheel step limit should fit i32")
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

fn mac_scroll_phase(event_phase: NSEventPhase, momentum_phase: NSEventPhase) -> MacScrollPhase {
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
            let (column, row) =
                terminal_cell.expect("terminal mouse route should have a validated cell");
            let button = if steps.is_positive() {
                MOUSE_WHEEL_UP
            } else {
                MOUSE_WHEEL_DOWN
            };
            let button = mouse_button_with_appkit_modifiers(button, modifiers);
            let report = encode_mouse_event(button, column + 1, row + 1, true, mouse_encoding);
            let report_count = usize::try_from(steps.unsigned_abs().min(MAX_WHEEL_STEPS_PER_EVENT))
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

/// Marked (pre-edit) text supplied by the macOS input method.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MarkedText {
    text: String,
    /// Selection inside the marked text, in UTF-16 units as AppKit reports it.
    selected: (usize, usize),
}

thread_local! {
    /// Key event handed to `interpretKeyEvents:`; `doCommandBySelector:` and
    /// the fallback after interpretation take it so the key is sent at most once.
    static PENDING_KEY_EVENT: RefCell<Option<Retained<NSEvent>>> = const { RefCell::new(None) };
}

struct ViewState {
    pane_tree: Arc<Mutex<PaneTree>>,
    pane_cleanup: PaneCleanup,
    atlas: Arc<Mutex<GlyphAtlas>>,
    dirty: Arc<AtomicBool>,
    render_scheduler: Arc<RenderScheduler>,
    window_title: Arc<WindowTitleMailbox>,
    applied_window_title: String,
    should_close: Arc<AtomicBool>,
    reader_wake: Arc<ReaderWake>,
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
    status_bar_layout: StatusBarLayout,
    option_as_alt: OptionAsAlt,
    /// Text the input method is composing (marked text) for the focused pane.
    marked_text: Option<MarkedText>,
    /// Cmd is held: hyperlinks are underlined as a hint that Cmd+click opens them.
    cmd_held: bool,
    resize_step: f32,
    prompt_indicator_color: Option<(u8, u8, u8)>,
    image_store: Arc<Mutex<ImageStore>>,
    mouse_wheel_state: MacScrollState,
    last_window_focus: Option<bool>,
    window_is_key: Arc<AtomicBool>,
    confirm_dialog: Option<ConfirmDialog>,
    confirm_on_close_pane: bool,
    confirm_on_quit: bool,
    update_status: SharedUpdateStatus,
}

impl ViewState {
    /// Mark the terminal content changed so the next frame redraws it.
    fn request_render(&self) {
        self.render_scheduler.request_render();
    }

    /// Stop the app and wake the reader so it observes shutdown.
    fn request_close(&self) {
        self.should_close.store(true, Ordering::Relaxed);
        self.reader_wake.wake();
        // The frame callback performs termination on the main thread.
        self.render_scheduler.request_frame();
    }
}

thread_local! {
    static VIEW_STATE: RefCell<Option<ViewState>> = const { RefCell::new(None) };
}

fn record_window_focus_change(last_window_focus: &mut Option<bool>, focused: bool) -> bool {
    last_window_focus.replace(focused) != Some(focused)
}

fn pending_window_focus_report(
    last_window_focus: &mut Option<bool>,
    focused: bool,
) -> Option<Vec<u8>> {
    record_window_focus_change(last_window_focus, focused)
        .then(|| focus_report_bytes(focused).to_vec())
}

fn dispatch_window_focus_transition_with<State, Dispatch>(
    source: &Mutex<State>,
    last_window_focus: &mut Option<bool>,
    window_is_key: &AtomicBool,
    focused: bool,
    dispatch: Dispatch,
) -> bool
where
    Dispatch: FnOnce(&State, Vec<u8>) -> bool,
{
    let source = source
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    window_is_key.store(focused, Ordering::Release);
    let Some(bytes) = pending_window_focus_report(last_window_focus, focused) else {
        return false;
    };
    dispatch(&source, bytes)
}

fn dispatch_window_focus_transition(focused: bool) {
    VIEW_STATE.with(|view_state| {
        let mut view_state = view_state.borrow_mut();
        let Some(state) = view_state.as_mut() else {
            return;
        };
        let pane_tree = Arc::clone(&state.pane_tree);
        let window_is_key = Arc::clone(&state.window_is_key);
        dispatch_window_focus_transition_with(
            pane_tree.as_ref(),
            &mut state.last_window_focus,
            window_is_key.as_ref(),
            focused,
            |tree, bytes| {
                let Some(pane) = tree.focused_pane() else {
                    return false;
                };
                if !pane.grid.focus_events {
                    return false;
                }

                pane.queue_input(bytes);
                true
            },
        );
    });
}
/// Bounce the Dock icon when BEL arrived while the window was unfocused.
pub(super) fn apply_pending_user_attention(notifications: &NotificationController) {
    if notifications.window_focused() {
        notifications.on_focus_gained();
        return;
    }
    if !notifications.take_attention_request() {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    // InformationalRequest == 0; avoid depending on an extra AppKit feature flag.
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: `requestUserAttention:` takes an NSRequestUserAttentionType
    // (NSInteger; 0 is NSInformationalRequest) and returns NSInteger. The
    // shared NSApplication is messaged on the main thread (`mtm`).
    let _: usize = unsafe { msg_send![&app, requestUserAttention: 0usize] };
}

// SAFETY: TerminalView subclasses NSView without custom ivars and is
// MainThreadOnly, so AppKit only calls into it from the main thread. Every
// `#[unsafe(method(...))]` below uses the selector's declared AppKit signature
// (NSEvent, NSNotification, NSSize, BOOL, NSMenuItem) and the overrides keep
// NSView's invariants (super is called where the override replaces a
// superclass behavior that must still run, e.g. `setFrameSize:`).
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
            let modifiers = key_modifiers_from_appkit(event.modifierFlags());
            let route = mac_key_equivalent_route(key_code, modifiers);

            // If confirm dialog is active, route ALL keys to it (including Cmd+keys).
            let confirm_active = VIEW_STATE.with(|state| {
                state.borrow().as_ref().is_some_and(|s| s.confirm_dialog.is_some())
            });
            if confirm_active {
                let ch = event.charactersIgnoringModifiers()
                    .and_then(|s| s.to_string().chars().next());
                handle_confirm_key(key_code, ch);
                return Bool::YES;
            }

            if route == MacKeyEquivalentRoute::PassThrough {
                return Bool::NO;
            }

            // Cmd+Q: show quit confirmation (or quit immediately if disabled)
            if route == MacKeyEquivalentRoute::Quit {
                VIEW_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    if let Some(state) = state.as_mut() {
                        if state.confirm_on_quit {
                            state.confirm_dialog = Some(ConfirmDialog::new(
                                ConfirmAction::QuitApp,
                                None,
                            ));
                            state.request_render();
                        } else {
                            state.request_close();
                        }
                    }
                });
                return Bool::YES;
            }

            // Cmd+C: copy or ctrl-c
            if route == MacKeyEquivalentRoute::Copy {
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
                                state.request_render();
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
            if route == MacKeyEquivalentRoute::Paste {
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
                                        pane.max_nonfatal_input_bytes(),
                                    )
                                })
                            };
                            if let Some((pane_id, bracketed, max_paste_bytes)) = paste_target {
                                match encode_clipboard_paste(
                                    text.as_ref(),
                                    bracketed,
                                    max_paste_bytes,
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
            if route == MacKeyEquivalentRoute::ZoomIn {
                handle_pane_action(PaneAction::ZoomIn);
                return Bool::YES;
            }

            // Check configurable keybinds
            let characters = event
                .charactersIgnoringModifiers()
                .map(|characters| characters.to_string());
            let key_name = key_name_from_key_code(key_code, characters.as_deref());
            let action = key_name.and_then(|key_name| {
                VIEW_STATE.with(|state| {
                    let state = state.borrow();
                    state
                        .as_ref()
                        .and_then(|s| s.keybinds.lookup(key_name, modifiers))
                })
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
                        state.request_render();
                    }
                }
            });
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let blocked = VIEW_STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.confirm_dialog.is_some()));
            if blocked { return; }
            let has_shift = event.modifierFlags().contains(NSEventModifierFlags::Shift);
            let has_cmd = event.modifierFlags().contains(NSEventModifierFlags::Command);
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
                            let previous_focus = tree.focused_pane().map(|pane| pane.id);
                            tree.focused = pane_id;
                            dispatch_pane_focus_transition_locked(
                                &tree,
                                state.window_is_key.as_ref(),
                                previous_focus,
                                None,
                            );
                            publish_focused_window_title(&tree, state.window_title.as_ref());
                            tree.clear_focused_attention();
                        }
                        // Cmd+click opens an OSC 8 hyperlink under the pointer
                        // instead of reporting the press or starting a selection.
                        if has_cmd && !has_shift {
                            let url = pixel_to_grid_point(event, state, &tree, cell_w, cell_h)
                                .and_then(|(_id, point)| hyperlink_at_point(tree.pane(pane_id)?, point));
                            if let Some(url) = url {
                                if let Err(error) = crate::notifications::open_hyperlink(&url) {
                                    log::warn!("Failed to open hyperlink: {error}");
                                }
                                drop(tree);
                                return;
                            }
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
                    state.request_render();
                }
            });
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let blocked = VIEW_STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.confirm_dialog.is_some()));
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
                    state.request_render();
                }
            });
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let blocked = VIEW_STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.confirm_dialog.is_some()));
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

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            let cmd_held = event.modifierFlags().contains(NSEventModifierFlags::Command);
            VIEW_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    if state.cmd_held != cmd_held {
                        state.cmd_held = cmd_held;
                        state.request_render();
                    }
                }
            });
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let key_code = event.keyCode();
            let modifiers = event.modifierFlags();
            let key_modifiers = key_modifiers_from_appkit(modifiers);
            let has_cmd = modifiers.contains(NSEventModifierFlags::Command);

            // If confirm dialog is active, route keys to it
            let confirm_active = VIEW_STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.confirm_dialog.is_some()));
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

            // Busca aberta: as teclas vão para a consulta, não para o shell
            // (`deliver_key_event` e `ime_commit_text` escolhem o destino).
            let search_open = focused_search_open();

            // Exact Shift+PageUp/Down/Home/End: viewport scrolling
            if let Some(action) = mac_scrollback_action(key_code, key_modifiers).filter(|_| !search_open) {
                let handled = VIEW_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    if let Some(state) = state.as_mut() {
                        let mut tree = state.pane_tree.lock().unwrap();
                        if let Some(pane) = tree.focused_pane_mut() {
                            let rows = pane.grid.rows();
                            match action {
                                MacScrollbackAction::PageUp => { pane.grid.scroll_viewport_up(rows.saturating_sub(1)); }
                                MacScrollbackAction::PageDown => { pane.grid.scroll_viewport_down(rows.saturating_sub(1)); }
                                MacScrollbackAction::Home => { let max = pane.grid.scrollback_len(); pane.grid.scroll_viewport_up(max); }
                                MacScrollbackAction::End => { pane.grid.scroll_to_bottom(); }
                            }
                            drop(tree);
                            state.request_render();
                            return true;
                        }
                    }
                    false
                });
                if handled {
                    return;
                }
            }

            // Text keys go through the input method first so dead keys, CJK
            // composition and the emoji picker work; `insertText:` commits the
            // result, `setMarkedText:` shows the pre-edit and
            // `doCommandBySelector:` returns control keys to the terminal path.
            // Ctrl, Meta and named keys skip the IME unless text is being composed.
            let (composing, option_meta) = VIEW_STATE.with(|state| {
                let state = state.borrow();
                let state = state.as_ref();
                (
                    state.is_some_and(|s| s.marked_text.is_some()),
                    state.is_none_or(|s| option_acts_as_meta(modifiers, s.option_as_alt)),
                )
            });
            let has_ctrl = modifiers.contains(NSEventModifierFlags::Control);
            let named_key =
                terminal_key_from_key_code(key_code, modifiers.contains(NSEventModifierFlags::Shift)).is_some();
            if mac_key_route(composing, has_ctrl, option_meta, named_key) == MacKeyRoute::InputMethod {
                PENDING_KEY_EVENT.with(|pending| *pending.borrow_mut() = Some(event.retain()));
                let events = NSArray::from_slice(&[event]);
                self.interpretKeyEvents(&events);
                // Nothing claimed the event (no text, no command): send it as before.
                if let Some(event) = PENDING_KEY_EVENT.with(|pending| pending.borrow_mut().take()) {
                    deliver_key_event(&event);
                }
                return;
            }

            deliver_key_event(event);
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            sync_backing_scale(self);
        }

        // The atlas starts at `NSScreen::mainScreen`'s scale, but the window
        // can open on another display; match its backing scale as soon as the
        // view joins it instead of relying on a later backing notification.
        #[unsafe(method(viewDidMoveToWindow))]
        fn view_did_move_to_window(&self) {
            // SAFETY: forwards to NSView's implementation on the main thread;
            // the selector takes no arguments and returns void.
            let _: () = unsafe { msg_send![super(self), viewDidMoveToWindow] };
            sync_backing_scale(self);
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, new_size: NSSize) {
            // SAFETY: forwards the same NSSize to NSView's implementation;
            // `self` is a live main-thread view and the selector returns void.
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
                state.request_render();
            });
        }

        // App menu actions: the items have no target, so AppKit sends them
        // to this first responder.
        #[unsafe(method(checkForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            with_update_status(|status, scheduler| {
                update::spawn_check(status, true, move || scheduler.request_render());
            });
        }

        #[unsafe(method(installUpdate:))]
        fn install_update(&self, _sender: Option<&AnyObject>) {
            with_update_status(|status, scheduler| {
                let available = update::read_status(&status).available_tag().map(str::to_string);
                if let Some(tag) = available {
                    update::spawn_install(status, tag, move || scheduler.request_render());
                }
            });
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> Bool {
            let Some(status) = with_update_status(|status, _| update::read_status(&status)) else {
                return Bool::NO;
            };
            let busy = matches!(status, UpdateStatus::Checking | UpdateStatus::Installing(_));
            if item.action() == Some(objc2::sel!(installUpdate:)) {
                let title = match status.available_tag() {
                    Some(tag) => format!("Install Update {tag}…"),
                    None => "Install Update…".to_string(),
                };
                item.setTitle(&NSString::from_str(&title));
                return Bool::new(status.available_tag().is_some());
            }
            Bool::new(!busy)
        }
    }

    // SAFETY: NSObjectProtocol has no additional safety requirements.
    unsafe impl NSObjectProtocol for TerminalView {}

    // SAFETY: TerminalView is main-thread-only and both methods use the exact
    // signatures required by NSWindowDelegate.
    unsafe impl NSWindowDelegate for TerminalView {
        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            dispatch_window_focus_transition(true);
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            // A pre-edit cannot survive losing the key window: drop it on both sides.
            if ime_marked_text().is_some() {
                ime_set_marked_text(String::new(), (0, 0));
                if let Some(context) = self.inputContext() {
                    context.discardMarkedText();
                }
            }
            dispatch_window_focus_transition(false);
        }
    }

    // SAFETY: every method below matches the NSTextInputClient protocol
    // signatures generated by objc2-app-kit; all run on the main thread via
    // AppKit's key event dispatch.
    unsafe impl NSTextInputClient for TerminalView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text_replacement_range(&self, string: &AnyObject, _replacement_range: NSRange) {
            PENDING_KEY_EVENT.with(|pending| pending.borrow_mut().take());
            if let Some(text) = text_from_input_object(string) {
                ime_commit_text(&text);
            }
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, _selector: objc2::runtime::Sel) {
            // Arrows, Enter, Tab, Escape, Backspace...: the terminal (or the
            // open search) owns them.
            if let Some(event) = PENDING_KEY_EVENT.with(|pending| pending.borrow_mut().take()) {
                deliver_key_event(&event);
            }
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text_selected_range_replacement_range(
            &self,
            string: &AnyObject,
            selected_range: NSRange,
            _replacement_range: NSRange,
        ) {
            PENDING_KEY_EVENT.with(|pending| pending.borrow_mut().take());
            let text = text_from_input_object(string).unwrap_or_default();
            ime_set_marked_text(text, (selected_range.location, selected_range.length));
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            // AppKit asks to commit the current pre-edit as typed.
            if let Some(marked) = ime_marked_text() {
                ime_commit_text(&marked.text);
            }
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            match ime_marked_text() {
                Some(marked) => NSRange::new(marked.selected.0, marked.selected.1),
                None => NSRange::new(NSNotFound as usize, 0),
            }
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            match ime_marked_text() {
                Some(marked) => NSRange::new(0, utf16_len(&marked.text)),
                None => NSRange::new(NSNotFound as usize, 0),
            }
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            ime_marked_text().is_some()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring_for_proposed_range_actual_range(
            &self,
            _range: NSRange,
            _actual_range: NSRangePointer,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect_for_character_range_actual_range(
            &self,
            _range: NSRange,
            _actual_range: NSRangePointer,
        ) -> NSRect {
            let in_view = ime_cursor_rect_in_view();
            let in_window = self.convertRect_toView(in_view, None);
            match self.window() {
                Some(window) => window.convertRectToScreen(in_window),
                None => in_window,
            }
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> NSUInteger {
            NSNotFound as NSUInteger
        }
    }
);

/// Where a key press goes first: the input method (text, dead keys, CJK
/// composition) or straight to the key encoders (Ctrl, Meta, named keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacKeyRoute {
    InputMethod,
    Direct,
}

/// While text is being composed every key belongs to the input method.
fn mac_key_route(
    composing: bool,
    has_ctrl: bool,
    option_meta: bool,
    named_key: bool,
) -> MacKeyRoute {
    if composing || (!has_ctrl && !option_meta && !named_key) {
        MacKeyRoute::InputMethod
    } else {
        MacKeyRoute::Direct
    }
}

/// A key the input method did not turn into text: the open search of the
/// focused pane takes it, otherwise the pane's terminal does.
fn deliver_key_event(event: &NSEvent) {
    let key_modifiers = key_modifiers_from_appkit(event.modifierFlags());
    if !handle_search_key(event, event.keyCode(), key_modifiers) {
        forward_key_event_to_pane(event);
    }
}

/// Rebuild the glyph atlas, pane geometry and Metal layer when the window's
/// backing scale differs from the one the atlas was rasterized for.
fn sync_backing_scale(view: &TerminalView) {
    let Some(window) = view.window() else {
        return;
    };
    let reported = window.backingScaleFactor();
    VIEW_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        let Some(new_scale) = backing_scale_change(state.scale_factor, reported) else {
            return;
        };
        match GlyphAtlas::new(&state.font_family, state.font_size, new_scale) {
            Ok(mut replacement) => {
                let cell_w = replacement.cell_width;
                let cell_h = replacement.cell_height;
                let status_bar_height = if state.status_bar_enabled {
                    (cell_h * 1.5).ceil()
                } else {
                    0.0
                };
                let backing_size = view.convertSizeToBacking(view.bounds().size);

                log::info!("Scale factor changed to {new_scale}");
                let mut atlas = state.atlas.lock().unwrap();
                let mut tree = state.pane_tree.lock().unwrap();
                replacement.set_ligatures(atlas.ligatures_enabled());
                *atlas = replacement;
                update_pane_geometry(&mut tree, cell_w, cell_h, status_bar_height, backing_size);
                drop(tree);
                drop(atlas);

                state.scale_factor = new_scale;
                state.status_bar_height = status_bar_height;
                state.metal_layer.setContentsScale(f64::from(new_scale));
                state.metal_layer.setDrawableSize(backing_size);
                state.request_render();
            }
            Err(error) => log::error!(
                "Failed to rebuild glyph atlas for scale {new_scale}: {error}; keeping scale {}",
                state.scale_factor
            ),
        }
    });
}

/// The scale to switch to when AppKit reports `reported` for a window whose
/// atlas uses `current`; `None` keeps the current scale (same value, or a
/// value no display can have).
fn backing_scale_change(current: f32, reported: f64) -> Option<f32> {
    let reported = reported as f32;
    (reported.is_finite() && reported > 0.0 && (reported - current).abs() > 0.01)
        .then_some(reported)
}

/// Send one key event to the focused pane with that pane's current input mode.
fn forward_key_event_to_pane(event: &NSEvent) {
    VIEW_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if let Some(state) = state.as_mut() {
            let mut tree = state.pane_tree.lock().unwrap();
            if let Some(pane) = tree.focused_pane_mut() {
                pane.grid.scroll_to_bottom();
                if pane.attention {
                    pane.attention = false;
                    state.request_render();
                }
                if pane.selection.is_active() {
                    pane.selection.clear();
                    state.request_render();
                }
                if let Some(bytes) = translate_key_event(
                    event,
                    pane.grid.application_cursor_keys,
                    pane.grid.kitty_disambiguate(),
                    state.option_as_alt,
                ) {
                    pane.queue_input(bytes);
                }
            }
        }
    });
}

/// Text carried by an `NSTextInputClient` argument: an `NSString` or an
/// `NSAttributedString` (the input method may send either).
fn text_from_input_object(string: &AnyObject) -> Option<String> {
    if let Some(string) = string.downcast_ref::<NSString>() {
        return Some(string.to_string());
    }
    string
        .downcast_ref::<NSAttributedString>()
        .map(|attributed| attributed.string().to_string())
}

/// Commit text from the input method to the focused pane and drop the pre-edit.
fn ime_commit_text(text: &str) {
    VIEW_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if let Some(state) = state.as_mut() {
            state.marked_text = None;
            let mut tree = state.pane_tree.lock().unwrap();
            if let Some(pane) = tree.focused_pane_mut() {
                if pane.search.is_some() {
                    // Texto composto (acentos, kana) vai para a consulta aberta.
                    for key in search_text_keys(text) {
                        if pane.handle_search_key(key).is_none() {
                            break;
                        }
                    }
                } else {
                    pane.grid.scroll_to_bottom();
                    if pane.selection.is_active() {
                        pane.selection.clear();
                    }
                    if !text.is_empty() {
                        pane.queue_input(text.as_bytes().to_vec());
                    }
                }
            }
            drop(tree);
            state.request_render();
        }
    });
}

/// Replace the pre-edit shown at the cursor (empty text clears it).
fn ime_set_marked_text(text: String, selected: (usize, usize)) {
    VIEW_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if let Some(state) = state.as_mut() {
            state.marked_text = (!text.is_empty()).then_some(MarkedText { text, selected });
            state.request_render();
        }
    });
}

fn ime_marked_text() -> Option<MarkedText> {
    VIEW_STATE.with(|state| state.borrow().as_ref().and_then(|s| s.marked_text.clone()))
}

/// Rectangle of the focused pane's cursor cell in view coordinates (points,
/// flipped view), where the input method places its candidate window.
fn ime_cursor_rect_in_view() -> NSRect {
    VIEW_STATE.with(|state| {
        let state = state.borrow();
        let Some(state) = state.as_ref() else {
            return NSRect::ZERO;
        };
        let tree = state.pane_tree.lock().unwrap();
        let atlas = state.atlas.lock().unwrap();
        let scale = state.scale_factor.max(0.01) as f64;
        let Some(pane) = tree.focused_pane() else {
            return NSRect::ZERO;
        };
        let col = pane
            .grid
            .screen_cursor_col()
            .unwrap_or(pane.grid.cursor_col);
        let x = pane.rect.x + col as f32 * atlas.cell_width;
        let y = pane.rect.y + pane.grid.cursor_row as f32 * atlas.cell_height;
        NSRect::new(
            NSPoint::new(x as f64 / scale, y as f64 / scale),
            NSSize::new(
                atlas.cell_width as f64 / scale,
                atlas.cell_height as f64 / scale,
            ),
        )
    })
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Run `action` with the shared update status and the frame scheduler.
fn with_update_status<T>(
    action: impl FnOnce(SharedUpdateStatus, Arc<RenderScheduler>) -> T,
) -> Option<T> {
    let (status, scheduler) = VIEW_STATE.with(|state| {
        let state = state.borrow();
        let state = state.as_ref()?;
        Some((state.update_status.clone(), state.render_scheduler.clone()))
    })?;
    Some(action(status, scheduler))
}

/// Traduz um `NSEvent` para a busca. `None` quando a tecla não produz texto
/// nem é uma tecla de edição: a busca a engole sem redesenhar.
fn search_key_from_mac_event(
    key_code: u16,
    modifiers: KeyModifiers,
    characters: Option<&str>,
) -> Option<SearchKey> {
    const KEY_RETURN: u16 = 36;
    const KEY_TAB: u16 = 48;
    const KEY_DELETE: u16 = 51;
    const KEY_ESCAPE: u16 = 53;
    const KEY_KEYPAD_ENTER: u16 = 76;

    let shift = modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = modifiers.contains(KeyModifiers::CTRL);
    Some(match key_code {
        KEY_ESCAPE => SearchKey::Escape,
        KEY_RETURN | KEY_KEYPAD_ENTER if shift => SearchKey::ShiftEnter,
        KEY_RETURN | KEY_KEYPAD_ENTER => SearchKey::Enter,
        KEY_DELETE => SearchKey::Backspace,
        KEY_TAB => SearchKey::Ignore,
        _ if ctrl => SearchKey::Ignore,
        _ => {
            let typed = characters?.chars().find(|c| is_search_text_char(*c))?;
            SearchKey::Char(typed)
        }
    })
}

/// Setas e teclas de função chegam como caracteres de uso privado
/// (U+F700..U+F8FF) e, como os de controle, não entram na consulta.
fn is_search_text_char(character: char) -> bool {
    !character.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&character)
}

/// Caracteres de `text` que entram na consulta, na ordem digitada.
fn search_text_keys(text: &str) -> Vec<SearchKey> {
    text.chars()
        .filter(|c| is_search_text_char(*c))
        .map(SearchKey::Char)
        .collect()
}

/// Se o painel focado está com a busca aberta.
fn focused_search_open() -> bool {
    VIEW_STATE.with(|state| {
        state.borrow().as_ref().is_some_and(|state| {
            state
                .pane_tree
                .lock()
                .unwrap()
                .focused_pane()
                .is_some_and(|pane| pane.search.is_some())
        })
    })
}

/// Trata uma tecla enquanto a busca do painel focado está aberta. Retorna
/// `false` quando não há busca aberta, deixando a tecla seguir para o shell.
fn handle_search_key(event: &NSEvent, key_code: u16, modifiers: KeyModifiers) -> bool {
    VIEW_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return false;
        };
        let mut tree = state.pane_tree.lock().unwrap();
        let Some(pane) = tree.focused_pane_mut() else {
            return false;
        };
        if pane.search.is_none() {
            return false;
        }
        let characters = event.characters().map(|s| s.to_string());
        let typed: Vec<SearchKey> =
            match search_key_from_mac_event(key_code, modifiers, characters.as_deref()) {
                Some(SearchKey::Char(_)) => {
                    search_text_keys(characters.as_deref().unwrap_or_default())
                }
                Some(key) => vec![key],
                None => return true,
            };
        for key in typed {
            if pane.handle_search_key(key).is_none() {
                break;
            }
        }
        drop(tree);
        state.request_render();
        true
    })
}

/// macOS side of `apply_pane_action`: viewport from the Metal layer, cell
/// metrics from the atlas, zoom through `perform_zoom`, close confirmation
/// through `ConfirmDialog`.
struct MacPaneActionHost<'a> {
    state: &'a mut ViewState,
    cell_w: f32,
    cell_h: f32,
}

impl PaneActionHost for MacPaneActionHost<'_> {
    fn viewport(&self) -> PixelRect {
        let size = self.state.metal_layer.drawableSize();
        PixelRect {
            x: 0.0,
            y: 0.0,
            width: size.width as f32,
            height: size.height as f32,
        }
    }

    fn cell_size(&self) -> (f32, f32) {
        (self.cell_w, self.cell_h)
    }

    fn status_bar_height(&self) -> f32 {
        self.state.status_bar_height
    }

    fn resize_step(&self) -> f32 {
        self.state.resize_step
    }

    fn zoom(&mut self, tree: &mut PaneTree, direction: ZoomDirection) {
        let state = &mut *self.state;
        let new_size = match direction {
            ZoomDirection::In => (state.font_size + state.zoom_step).min(state.max_font_size),
            ZoomDirection::Out => (state.font_size - state.zoom_step).max(state.min_font_size),
            ZoomDirection::Reset => state.base_font_size,
        };
        perform_zoom(state, tree, new_size);
    }

    fn confirm_close(&mut self, id: PaneId, pane: Option<&Pane>) -> bool {
        if !self.state.confirm_on_close_pane {
            return false;
        }
        // Get foreground process name for the dialog
        let proc_name = pane.and_then(|p| confirm::foreground_process_name(p.pty.master_fd()));
        self.state.confirm_dialog =
            Some(ConfirmDialog::new(ConfirmAction::ClosePane(id), proc_name));
        self.state.request_render();
        true
    }
}

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

        let pane_tree = Arc::clone(&state.pane_tree);
        let mut tree = pane_tree.lock().unwrap();
        let outcome = {
            let mut host = MacPaneActionHost {
                state,
                cell_w,
                cell_h,
            };
            apply_pane_action(&mut tree, action, &mut host)
        };

        if outcome.terminate {
            dispatch_pane_focus_transition_locked(
                &tree,
                state.window_is_key.as_ref(),
                outcome.focus_changed_from,
                outcome.closed_pane.as_ref(),
            );
            drop(tree);
            if let Some(pane) = outcome.closed_pane {
                state.pane_cleanup.retire(pane);
            }
            state.request_close();
            return;
        }
        if !outcome.render {
            // A confirm dialog took over, or the zoom already rendered.
            return;
        }

        if let Some(previous_focus) = outcome.focus_changed_from {
            dispatch_pane_focus_transition_locked(
                &tree,
                state.window_is_key.as_ref(),
                Some(previous_focus),
                outcome.closed_pane.as_ref(),
            );
            publish_focused_window_title(&tree, state.window_title.as_ref());
        }
        tree.clear_focused_attention();
        drop(tree);
        if let Some(pane) = outcome.closed_pane {
            state.pane_cleanup.retire(pane);
        }
        // Splits and closes change the PTYs the reader must poll.
        state.reader_wake.wake();
        state.request_render();
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
                state.request_render();
                execute_confirm_action(state, action);
            }
            ConfirmResult::Cancelled => {
                state.confirm_dialog = None;
                state.request_render();
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
            let previous_focus = tree.focused_pane().map(|pane| pane.id);
            let outcome = tree.close(pane_id);
            let should_terminate = outcome.should_terminate;
            let closed_pane = outcome.closed_pane;
            if should_terminate {
                dispatch_pane_focus_transition_locked(
                    &tree,
                    state.window_is_key.as_ref(),
                    previous_focus,
                    closed_pane.as_ref(),
                );
                drop(tree);
                if let Some(pane) = closed_pane {
                    state.pane_cleanup.retire(pane);
                }
                state.request_close();
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
            if tree.focused_pane().map(|pane| pane.id) != previous_focus {
                dispatch_pane_focus_transition_locked(
                    &tree,
                    state.window_is_key.as_ref(),
                    previous_focus,
                    closed_pane.as_ref(),
                );
                publish_focused_window_title(&tree, state.window_title.as_ref());
            }
            drop(tree);
            if let Some(pane) = closed_pane {
                state.pane_cleanup.retire(pane);
            }
            state.reader_wake.wake();
        }
        ConfirmAction::QuitApp => {
            state.request_close();
        }
    }
}

/// The caller holds the pane tree lock (`tree`); the atlas is locked here.
fn perform_zoom(state: &mut ViewState, tree: &mut PaneTree, new_size: f32) {
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
    update_pane_geometry(tree, cell_w, cell_h, status_bar_height, drawable_size);
    drop(atlas);

    state.font_size = new_size;
    state.status_bar_height = status_bar_height;
    log::info!("Font zoom: {new_size}pt");

    state.request_render();
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
    Some((
        pane_id,
        col.min(pane.grid.cols().saturating_sub(1)),
        row.min(pane.grid.rows().saturating_sub(1)),
    ))
}

/// OSC 8 target under a retained-grid point, if the pane recorded one there.
fn hyperlink_at_point(pane: &Pane, point: GridPoint) -> Option<String> {
    let retained_row = usize::try_from(point.row).ok()?;
    pane.grid
        .hyperlink_at_retained(retained_row, point.col)
        .map(str::to_owned)
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

    Some((
        pane_id,
        crate::selection::point_from_viewport(&pane.grid, vis_row, col),
    ))
}

/// Place text queued by OSC 52 on the pasteboard (main thread only).
pub(super) fn apply_pending_clipboard_write(pending: &Mutex<Option<String>>) {
    let text = match pending.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => None,
    };
    if let Some(text) = text {
        copy_to_clipboard(&text);
    }
}

pub(crate) fn copy_to_clipboard(text: &str) {
    // SAFETY: plain AppKit message sends on the general pasteboard with a
    // valid, retained NSString and the framework-provided type constant;
    // called from the view's event handlers on the main thread.
    unsafe {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let ns_string = NSString::from_str(text);
        pasteboard.setString_forType(&ns_string, NSPasteboardTypeString);
    }
}

fn paste_from_clipboard() -> Option<Retained<NSString>> {
    // SAFETY: `stringForType:` on the general pasteboard with a valid type
    // constant returns a retained, possibly nil NSString; main thread only.
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
        // SAFETY: the destination is exactly `payload_len` writable bytes of
        // `bytes`, which was sized to `framed_len >= payload_start +
        // payload_len`; `maxLength` equals that slice length, `source_range`
        // covers the `utf16_len` UTF-16 units of `text`, and the two
        // out-pointers reference live locals.
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
        bytes[framed_len - BRACKETED_PASTE_END.len()..].copy_from_slice(BRACKETED_PASTE_END);
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

        // Clear before reading pane state: output decoded after this point
        // marks the next frame dirty instead of being overwritten below.
        state.dirty.store(false, Ordering::Relaxed);

        // Lock atlas FIRST (canonical order: atlas → tree → image_store)
        let mut atlas = state.atlas.lock().unwrap();
        let mut tree = state.pane_tree.lock().unwrap();

        let viewport = PixelRect {
            x: 0.0,
            y: 0.0,
            width: size.width as f32,
            height: size.height as f32,
        };
        let (layouts, dividers) = tree.layout_info(viewport);

        // Saída nova enquanto a busca está aberta: refaz os resultados antes
        // de desenhar, mantendo o resultado atual quando ele ainda existe.
        for (id, _) in &layouts {
            if let Some(pane) = tree.pane_mut(*id) {
                if let Some(search) = pane.search.as_mut() {
                    if search.needs_recompute(&pane.grid) {
                        search.recompute(&pane.grid);
                    }
                }
            }
        }

        let focused_id = tree.focused;
        let update_notice = update::read_status(&state.update_status).notice();
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
                    attention: pane.attention,
                    pane_index: i,
                    program: &pane.pty.program_name,
                    cwd: &pane.grid.cwd,
                    prompt_mark_rows,
                    show_cursor: pane.grid.cursor_visible && *id == focused_id,
                    update_notice: update_notice.as_deref().filter(|_| *id == focused_id),
                    search: pane.search.as_ref(),
                    underline_links: state.cmd_held,
                    preedit: state
                        .marked_text
                        .as_ref()
                        .filter(|_| *id == focused_id)
                        .map(|marked| marked.text.as_str()),
                });
            }
        }

        // Build confirm overlay info if dialog is active
        let confirm_overlay = state.confirm_dialog.as_ref().map(|dialog| {
            let region = match &dialog.action {
                ConfirmAction::ClosePane(pane_id) => {
                    // Find the pane rect for this pane
                    layouts
                        .iter()
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
        state.renderer.draw_frame(FrameInputs {
            panes: &pane_render_data,
            dividers: &dividers,
            atlas: &mut atlas,
            drawable: ProtocolObject::from_ref(&*drawable),
            texture: &texture,
            viewport_width: size.width as f32,
            viewport_height: size.height as f32,
            bg_opacity: state.bg_opacity,
            selection_fg: state.selection_fg,
            selection_bg: state.selection_bg,
            chrome: &state.chrome,
            status_bar_height: state.status_bar_height,
            status_bar_layout: state.status_bar_layout,
            prompt_indicator_color: state.prompt_indicator_color,
            image_store: Some(&img_store),
            confirm_overlay: confirm_overlay.as_ref(),
        });
        drop(img_store);

        drop(atlas);
        drop(tree);

        // Keep rendering during fade-in animation
        let still_animating = state
            .confirm_dialog
            .as_ref()
            .is_some_and(|d| d.is_animating());
        if still_animating {
            state.request_render();
        }
    });
}

pub fn render_if_dirty(dirty: &AtomicBool) {
    if dirty.load(Ordering::Relaxed) {
        render_frame();
    }
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

/// Everything `create_terminal_view` needs, grouped so the call site reads as
/// named fields instead of a positional list of thirty-odd arguments.
pub(super) struct TerminalViewParams<'a> {
    pub(super) mtm: MainThreadMarker,
    pub(super) device: &'a ProtocolObject<dyn MTLDevice>,
    pub(super) pane_tree: Arc<Mutex<PaneTree>>,
    pub(super) pane_cleanup: PaneCleanup,
    pub(super) atlas: Arc<Mutex<GlyphAtlas>>,
    pub(super) dirty: Arc<AtomicBool>,
    pub(super) render_scheduler: Arc<RenderScheduler>,
    pub(super) should_close: Arc<AtomicBool>,
    pub(super) reader_wake: Arc<ReaderWake>,
    pub(super) window_is_key: Arc<AtomicBool>,
    pub(super) terminal_colors: TerminalColors,
    pub(super) scale_factor: f32,
    pub(super) font_family: String,
    pub(super) font_size: f32,
    pub(super) zoom_step: f32,
    pub(super) min_font_size: f32,
    pub(super) max_font_size: f32,
    pub(super) selection_fg: (u8, u8, u8),
    pub(super) selection_bg: (u8, u8, u8),
    pub(super) chrome: ChromeColors,
    pub(super) keybinds: KeybindMap,
    pub(super) bg_opacity: f32,
    pub(super) status_bar_height: f32,
    pub(super) status_bar_enabled: bool,
    pub(super) status_bar_layout: StatusBarLayout,
    pub(super) option_as_alt: OptionAsAlt,
    pub(super) resize_step: f32,
    pub(super) prompt_indicator_color: Option<(u8, u8, u8)>,
    pub(super) image_store: Arc<Mutex<ImageStore>>,
    pub(super) window_title: Arc<WindowTitleMailbox>,
    pub(super) confirm_on_close_pane: bool,
    pub(super) confirm_on_quit: bool,
    pub(super) update_status: SharedUpdateStatus,
}

pub(super) fn create_terminal_view(params: TerminalViewParams<'_>) -> Retained<TerminalView> {
    let TerminalViewParams {
        mtm,
        device,
        pane_tree,
        pane_cleanup,
        atlas,
        dirty,
        render_scheduler,
        should_close,
        reader_wake,
        window_is_key,
        terminal_colors,
        scale_factor,
        font_family,
        font_size,
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
        status_bar_layout,
        option_as_alt,
        resize_step,
        prompt_indicator_color,
        image_store,
        window_title,
        confirm_on_close_pane,
        confirm_on_quit,
        update_status,
    } = params;
    let view = mtm.alloc::<TerminalView>().set_ivars(());
    // SAFETY: `view` is a fresh allocation with its (unit) ivars set; NSView's
    // designated `init` runs exactly once, on the main thread (`mtm`).
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
    let renderer = MetalRenderer::new(retained_device, terminal_colors);

    VIEW_STATE.with(|state| {
        *state.borrow_mut() = Some(ViewState {
            pane_tree,
            pane_cleanup,
            atlas,
            dirty,
            render_scheduler,
            window_title,
            applied_window_title: WINDOW_TITLE.to_string(),
            should_close,
            reader_wake,
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
            status_bar_layout,
            option_as_alt,
            marked_text: None,
            cmd_held: false,
            resize_step,
            prompt_indicator_color,
            image_store,
            mouse_wheel_state: MacScrollState::default(),
            last_window_focus: None,
            window_is_key,
            confirm_dialog: None,
            confirm_on_close_pane,
            confirm_on_quit,
            update_status,
        });
    });

    view
}

#[cfg(test)]
mod tests {
    use super::search_key_from_mac_event;
    use super::{backing_scale_change, mac_key_route, search_text_keys, MacKeyRoute};
    use super::{
        bracketed_paste_len, dispatch_window_focus_transition_with, encode_clipboard_paste,
        encode_macos_forwarded_wheel, focus_report_bytes, mac_key_equivalent_route,
        mac_scroll_phase, mac_scrollback_action, ClipboardPasteError, MacKeyEquivalentRoute,
        MacScrollPhase, MacScrollSample, MacScrollState, MacScrollbackAction, BRACKETED_PASTE_END,
        BRACKETED_PASTE_START,
    };
    use crate::grid::MouseEncoding;
    use crate::input::keybind::KeyModifiers;
    use crate::input::keyboard::{
        encode_terminal_key_with_modifiers, TerminalKey, TerminalKeyModifiers,
    };
    use crate::input::mouse::MouseWheelRoute;
    use crate::layout::PaneId;
    use crate::pane::focus::dispatch_pane_focus_transition_with;
    use objc2::AnyThread;
    use objc2_app_kit::{NSEventModifierFlags, NSEventPhase};
    use objc2_foundation::NSString;
    use std::cell::RefCell;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    #[test]
    fn ime_text_accepts_plain_and_attributed_strings() {
        use super::text_from_input_object;
        use objc2_foundation::{NSAttributedString, NSString};
        let plain = NSString::from_str("日本語");
        assert_eq!(text_from_input_object(&plain).as_deref(), Some("日本語"));
        let attributed = NSAttributedString::from_nsstring(&NSString::from_str("é"));
        assert_eq!(text_from_input_object(&attributed).as_deref(), Some("é"));
        let other = objc2_foundation::NSArray::<NSString>::new();
        assert_eq!(text_from_input_object(&other), None);
    }

    #[test]
    fn ime_marked_range_uses_utf16_units() {
        use super::utf16_len;
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("日本語"), 3);
        assert_eq!(utf16_len("😀"), 2);
    }

    #[derive(Clone, Copy)]
    struct FocusTarget {
        id: PaneId,
        enabled: bool,
    }

    #[derive(Clone, Copy)]
    enum FocusStep {
        Window(bool),
        Pane(PaneId),
    }

    fn run_focus_steps(initially_key: bool, steps: &[FocusStep]) -> Vec<(PaneId, Vec<u8>)> {
        let tree = Mutex::new(FocusTarget {
            id: 1,
            enabled: true,
        });
        let window_is_key = AtomicBool::new(initially_key);
        let mut last_window_focus = Some(initially_key);
        let writes = RefCell::new(Vec::new());

        for step in steps {
            match *step {
                FocusStep::Window(focused) => {
                    dispatch_window_focus_transition_with(
                        &tree,
                        &mut last_window_focus,
                        &window_is_key,
                        focused,
                        |pane, bytes| {
                            assert!(
                                tree.try_lock().is_err(),
                                "window state and enqueue must share the tree lock"
                            );
                            if !pane.enabled {
                                return false;
                            }
                            writes.borrow_mut().push((pane.id, bytes));
                            true
                        },
                    );
                }
                FocusStep::Pane(next_id) => {
                    let mut current = tree.lock().unwrap();
                    let previous = *current;
                    current.id = next_id;
                    let next = *current;
                    dispatch_pane_focus_transition_with(
                        window_is_key.load(Ordering::Acquire),
                        Some(previous.id),
                        Some(next.id),
                        Some(&previous),
                        Some(&next),
                        |pane| pane.enabled,
                        |pane, bytes| {
                            assert!(
                                tree.try_lock().is_err(),
                                "pane state and enqueue must share the tree lock"
                            );
                            writes.borrow_mut().push((pane.id, bytes));
                        },
                    );
                }
            }
        }

        writes.into_inner()
    }

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
    fn macos_focus_reports_use_exact_dec_private_mode_bytes() {
        assert_eq!(focus_report_bytes(true), b"\x1b[I");
        assert_eq!(focus_report_bytes(false), b"\x1b[O");
    }

    #[test]
    fn disabled_macos_focus_reporting_still_updates_deduplication_state() {
        let enabled = Mutex::new(false);
        let window_is_key = AtomicBool::new(false);
        let writes = RefCell::new(Vec::new());
        let mut last_window_focus = None;
        let dispatch = |last_window_focus: &mut Option<bool>, focused| {
            dispatch_window_focus_transition_with(
                &enabled,
                last_window_focus,
                &window_is_key,
                focused,
                |enabled, bytes| {
                    if !enabled {
                        return false;
                    }
                    writes.borrow_mut().push(bytes);
                    true
                },
            )
        };

        assert!(!dispatch(&mut last_window_focus, true));
        assert_eq!(last_window_focus, Some(true));
        assert!(window_is_key.load(Ordering::Acquire));
        assert!(writes.borrow().is_empty());

        *enabled.lock().unwrap() = true;
        assert!(!dispatch(&mut last_window_focus, true));
        assert!(dispatch(&mut last_window_focus, false));
        assert!(!window_is_key.load(Ordering::Acquire));
        assert_eq!(writes.into_inner(), [b"\x1b[O".to_vec()]);
    }

    #[test]
    fn macos_focus_reporting_deduplicates_and_preserves_transition_order() {
        let enabled = Mutex::new(true);
        let window_is_key = AtomicBool::new(false);
        let writes = RefCell::new(Vec::new());
        let mut last_window_focus = None;

        for (focused, expected_dispatch) in [
            (true, true),
            (true, false),
            (false, true),
            (false, false),
            (true, true),
        ] {
            let dispatched = dispatch_window_focus_transition_with(
                &enabled,
                &mut last_window_focus,
                &window_is_key,
                focused,
                |enabled, bytes| {
                    if !enabled {
                        return false;
                    }
                    writes.borrow_mut().push(bytes);
                    true
                },
            );
            assert_eq!(dispatched, expected_dispatch);
        }

        assert_eq!(
            writes.into_inner(),
            [b"\x1b[I".to_vec(), b"\x1b[O".to_vec(), b"\x1b[I".to_vec()]
        );
    }

    #[test]
    fn macos_focus_report_targets_the_current_pane_after_recording_the_transition() {
        let current_pane = Mutex::new((1_u8, true));
        let window_is_key = AtomicBool::new(false);
        let writes = RefCell::new(Vec::new());
        let mut last_window_focus = None;

        *current_pane.lock().unwrap() = (2, true);

        assert!(dispatch_window_focus_transition_with(
            &current_pane,
            &mut last_window_focus,
            &window_is_key,
            true,
            |&(pane_id, enabled), bytes| {
                assert!(
                    current_pane.try_lock().is_err(),
                    "pane selection and enqueue must share one tree lock"
                );
                assert!(window_is_key.load(Ordering::Acquire));
                if !enabled {
                    return false;
                }
                writes.borrow_mut().push((pane_id, bytes));
                true
            },
        ));
        assert_eq!(writes.into_inner(), [(2, b"\x1b[I".to_vec())]);
    }

    #[test]
    fn internal_focus_reports_old_then_new_for_all_mode_gate_combinations() {
        for (old_enabled, new_enabled) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let old = FocusTarget {
                id: 1,
                enabled: old_enabled,
            };
            let new = FocusTarget {
                id: 2,
                enabled: new_enabled,
            };
            let writes = RefCell::new(Vec::new());

            let dispatched = dispatch_pane_focus_transition_with(
                true,
                Some(old.id),
                Some(new.id),
                Some(&old),
                Some(&new),
                |pane| pane.enabled,
                |pane, bytes| writes.borrow_mut().push((pane.id, bytes)),
            );

            let mut expected = Vec::new();
            if old_enabled {
                expected.push((old.id, focus_report_bytes(false).to_vec()));
            }
            if new_enabled {
                expected.push((new.id, focus_report_bytes(true).to_vec()));
            }
            assert_eq!(dispatched, !expected.is_empty());
            assert_eq!(writes.into_inner(), expected);
        }
    }

    #[test]
    fn internal_focus_reports_ignore_non_key_and_unchanged_focus() {
        let old = FocusTarget {
            id: 1,
            enabled: true,
        };
        let new = FocusTarget {
            id: 2,
            enabled: true,
        };
        let writes = RefCell::new(Vec::new());

        for (window_is_key, previous, current) in [
            (false, Some(old.id), Some(new.id)),
            (true, Some(old.id), Some(old.id)),
            (false, None, Some(new.id)),
            (true, None, None),
        ] {
            assert!(!dispatch_pane_focus_transition_with(
                window_is_key,
                previous,
                current,
                Some(&old),
                Some(&new),
                |pane| pane.enabled,
                |pane, bytes| writes.borrow_mut().push((pane.id, bytes)),
            ));
        }
        assert!(writes.into_inner().is_empty());
    }

    #[test]
    fn detached_old_pane_reports_before_new_and_last_pane_reports_out_only() {
        let detached_old = FocusTarget {
            id: 7,
            enabled: true,
        };
        let current = FocusTarget {
            id: 9,
            enabled: true,
        };
        let writes = RefCell::new(Vec::new());

        assert!(dispatch_pane_focus_transition_with(
            true,
            Some(detached_old.id),
            Some(current.id),
            Some(&detached_old),
            Some(&current),
            |pane| pane.enabled,
            |pane, bytes| writes.borrow_mut().push((pane.id, bytes)),
        ));
        assert_eq!(
            writes.replace(Vec::new()),
            [
                (detached_old.id, focus_report_bytes(false).to_vec()),
                (current.id, focus_report_bytes(true).to_vec()),
            ]
        );

        assert!(dispatch_pane_focus_transition_with(
            true,
            Some(detached_old.id),
            None,
            Some(&detached_old),
            None,
            |pane| pane.enabled,
            |pane, bytes| writes.borrow_mut().push((pane.id, bytes)),
        ));
        assert_eq!(
            writes.into_inner(),
            [(detached_old.id, focus_report_bytes(false).to_vec())]
        );
    }

    #[test]
    fn mouse_focus_reports_precede_the_click_payload() {
        let old = FocusTarget {
            id: 1,
            enabled: true,
        };
        let clicked = FocusTarget {
            id: 2,
            enabled: true,
        };
        let writes = RefCell::new(Vec::new());

        dispatch_pane_focus_transition_with(
            true,
            Some(old.id),
            Some(clicked.id),
            Some(&old),
            Some(&clicked),
            |pane| pane.enabled,
            |pane, bytes| writes.borrow_mut().push((pane.id, bytes)),
        );
        writes
            .borrow_mut()
            .push((clicked.id, b"mouse-press".to_vec()));

        assert_eq!(
            writes.into_inner(),
            [
                (old.id, focus_report_bytes(false).to_vec()),
                (clicked.id, focus_report_bytes(true).to_vec()),
                (clicked.id, b"mouse-press".to_vec()),
            ]
        );
    }

    #[test]
    fn dead_pane_batch_reports_only_the_initial_and_final_focus() {
        let initial = FocusTarget {
            id: 1,
            enabled: true,
        };
        let final_pane = FocusTarget {
            id: 3,
            enabled: true,
        };
        let writes = RefCell::new(Vec::new());

        dispatch_pane_focus_transition_with(
            true,
            Some(initial.id),
            Some(final_pane.id),
            Some(&initial),
            Some(&final_pane),
            |pane| pane.enabled,
            |pane, bytes| writes.borrow_mut().push((pane.id, bytes)),
        );

        assert_eq!(
            writes.into_inner(),
            [
                (initial.id, focus_report_bytes(false).to_vec()),
                (final_pane.id, focus_report_bytes(true).to_vec()),
            ]
        );
    }

    #[test]
    fn macos_window_and_pane_focus_races_have_one_serialized_order() {
        assert_eq!(
            run_focus_steps(true, &[FocusStep::Pane(2), FocusStep::Window(false)],),
            [
                (1, focus_report_bytes(false).to_vec()),
                (2, focus_report_bytes(true).to_vec()),
                (2, focus_report_bytes(false).to_vec()),
            ]
        );
        assert_eq!(
            run_focus_steps(true, &[FocusStep::Window(false), FocusStep::Pane(2)],),
            [(1, focus_report_bytes(false).to_vec())]
        );
        assert_eq!(
            run_focus_steps(false, &[FocusStep::Pane(2), FocusStep::Window(true)],),
            [(2, focus_report_bytes(true).to_vec())]
        );
        assert_eq!(
            run_focus_steps(false, &[FocusStep::Window(true), FocusStep::Pane(2)],),
            [
                (1, focus_report_bytes(true).to_vec()),
                (1, focus_report_bytes(false).to_vec()),
                (2, focus_report_bytes(true).to_vec()),
            ]
        );
    }

    #[test]
    fn mac_key_equivalent_routes_require_exact_relevant_modifiers() {
        let cmd = KeyModifiers::SUPER;
        let cmd_shift = KeyModifiers::SUPER | KeyModifiers::SHIFT;

        for (key_code, modifiers, expected) in [
            (12, cmd, MacKeyEquivalentRoute::Quit),
            (8, cmd, MacKeyEquivalentRoute::Copy),
            (9, cmd, MacKeyEquivalentRoute::Paste),
            (24, cmd_shift, MacKeyEquivalentRoute::ZoomIn),
            (
                12,
                KeyModifiers::empty(),
                MacKeyEquivalentRoute::PassThrough,
            ),
            (8, KeyModifiers::SHIFT, MacKeyEquivalentRoute::PassThrough),
            (
                9,
                KeyModifiers::CTRL | KeyModifiers::ALT,
                MacKeyEquivalentRoute::PassThrough,
            ),
            (24, KeyModifiers::SHIFT, MacKeyEquivalentRoute::PassThrough),
            (12, cmd_shift, MacKeyEquivalentRoute::Keybind),
            (8, cmd | KeyModifiers::CTRL, MacKeyEquivalentRoute::Keybind),
            (9, cmd | KeyModifiers::ALT, MacKeyEquivalentRoute::Keybind),
            (24, cmd, MacKeyEquivalentRoute::Keybind),
            (
                24,
                cmd_shift | KeyModifiers::CTRL,
                MacKeyEquivalentRoute::Keybind,
            ),
            (0, cmd, MacKeyEquivalentRoute::Keybind),
        ] {
            assert_eq!(mac_key_equivalent_route(key_code, modifiers), expected);
        }
    }

    #[test]
    fn macos_scrollback_requires_exact_shift_for_all_local_keys() {
        for (key_code, expected) in [
            (116, MacScrollbackAction::PageUp),
            (121, MacScrollbackAction::PageDown),
            (115, MacScrollbackAction::Home),
            (119, MacScrollbackAction::End),
        ] {
            assert_eq!(
                mac_scrollback_action(key_code, KeyModifiers::SHIFT),
                Some(expected),
            );

            for modifiers in [
                KeyModifiers::SHIFT | KeyModifiers::CTRL,
                KeyModifiers::SHIFT | KeyModifiers::ALT,
            ] {
                assert_eq!(
                    mac_scrollback_action(key_code, modifiers),
                    None,
                    "{modifiers:?} with key code {key_code} must stay on the terminal input path",
                );
            }
        }

        assert_eq!(mac_scrollback_action(123, KeyModifiers::SHIFT), None,);
    }

    #[test]
    fn macos_modified_shift_page_up_keeps_xterm_sequences() {
        for (modifiers, terminal_modifiers, expected) in [
            (
                KeyModifiers::SHIFT | KeyModifiers::CTRL,
                TerminalKeyModifiers::new(true, false, true),
                b"\x1b[5;6~".as_slice(),
            ),
            (
                KeyModifiers::SHIFT | KeyModifiers::ALT,
                TerminalKeyModifiers::new(true, true, false),
                b"\x1b[5;4~".as_slice(),
            ),
        ] {
            assert_eq!(mac_scrollback_action(116, modifiers), None);
            assert_eq!(
                encode_terminal_key_with_modifiers(TerminalKey::PageUp, false, terminal_modifiers,)
                    .as_deref(),
                Some(expected),
            );
        }
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
        assert_eq!(bracketed_paste_len(usize::MAX - framing_bytes + 1), None);
        assert_eq!(bracketed_paste_len(usize::MAX), None);
    }

    #[test]
    fn clipboard_paste_rejects_unconvertible_utf16() {
        let unpaired_surrogate = [0xd800_u16];
        // SAFETY: the pointer references `unpaired_surrogate`, which outlives
        // the call, and `length` is its exact element count; NSString copies
        // the characters.
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
        for phase in [
            NSEventPhase::None,
            NSEventPhase::Changed,
            NSEventPhase::Stationary,
        ] {
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
    fn search_keys_map_edit_keys_text_and_swallowed_chords() {
        use crate::search::SearchKey;
        let none = KeyModifiers::empty();
        let cases = [
            (53, none, Some("\x1b"), Some(SearchKey::Escape)),
            (36, none, Some("\r"), Some(SearchKey::Enter)),
            (76, none, Some("\x03"), Some(SearchKey::Enter)),
            (
                36,
                KeyModifiers::SHIFT,
                Some("\r"),
                Some(SearchKey::ShiftEnter),
            ),
            (51, none, Some("\x7f"), Some(SearchKey::Backspace)),
            (48, none, Some("\t"), Some(SearchKey::Ignore)),
            (0, KeyModifiers::CTRL, Some("\x01"), Some(SearchKey::Ignore)),
            (0, none, Some("a"), Some(SearchKey::Char('a'))),
            (
                0,
                KeyModifiers::SHIFT,
                Some("A"),
                Some(SearchKey::Char('A')),
            ),
            (0, KeyModifiers::ALT, Some("å"), Some(SearchKey::Char('å'))),
            // Arrows and function keys arrive as private-use characters.
            (126, none, Some("\u{F700}"), None),
            (122, none, Some("\u{F704}"), None),
            (0, none, Some(""), None),
            (0, none, None, None),
        ];
        for (key_code, modifiers, characters, expected) in cases {
            assert_eq!(
                search_key_from_mac_event(key_code, modifiers, characters),
                expected,
                "key code {key_code} {modifiers:?} {characters:?}"
            );
        }
    }

    #[test]
    fn key_route_sends_text_to_the_input_method_and_chords_direct() {
        // (composing, ctrl, option as Meta, named key)
        for (composing, ctrl, meta, named, expected) in [
            // Plain text, and Option that is not Meta (the other side, or
            // "none"), compose through the input method.
            (false, false, false, false, MacKeyRoute::InputMethod),
            (false, true, false, false, MacKeyRoute::Direct),
            (false, false, true, false, MacKeyRoute::Direct),
            (false, false, false, true, MacKeyRoute::Direct),
            // Enter, Esc and Ctrl+C during composition belong to the input
            // method, so they never reach the PTY alongside the commit.
            (true, false, false, true, MacKeyRoute::InputMethod),
            (true, true, false, false, MacKeyRoute::InputMethod),
            (true, false, true, false, MacKeyRoute::InputMethod),
        ] {
            assert_eq!(
                mac_key_route(composing, ctrl, meta, named),
                expected,
                "composing={composing} ctrl={ctrl} meta={meta} named={named}"
            );
        }
    }

    #[test]
    fn composed_text_becomes_search_characters_in_order() {
        use crate::grid::Grid;
        use crate::parser::ansi::Utf8Parser;
        use crate::search::{SearchKey, SearchState};

        assert_eq!(
            search_text_keys("ção"),
            vec![
                SearchKey::Char('ç'),
                SearchKey::Char('ã'),
                SearchKey::Char('o')
            ]
        );
        assert_eq!(
            search_text_keys("漢\u{F700}\r字"),
            vec![SearchKey::Char('漢'), SearchKey::Char('字')]
        );
        assert!(search_text_keys("").is_empty());

        // A dead key commit ("´" then "a" -> "á") reaches the query and finds the row.
        let mut grid = Grid::new(20, 3, 100);
        Utf8Parser::new().feed("ação já\r\n".as_bytes(), &mut grid);
        let mut search = SearchState::new(String::new());
        for commit in ["a", "ç", "ã", "o"] {
            for key in search_text_keys(commit) {
                search.handle_key(key, &mut grid);
            }
        }
        assert_eq!(search.query, "ação");
        assert_eq!(search.matches.len(), 1);
    }

    #[test]
    fn backing_scale_changes_only_for_a_different_valid_scale() {
        assert_eq!(backing_scale_change(2.0, 1.0), Some(1.0));
        assert_eq!(backing_scale_change(1.0, 2.0), Some(2.0));
        assert_eq!(backing_scale_change(2.0, 3.0), Some(3.0));
        assert_eq!(backing_scale_change(2.0, 2.0), None);
        assert_eq!(backing_scale_change(2.0, 2.005), None);
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(backing_scale_change(2.0, invalid), None, "{invalid}");
        }
    }

    /// Selectors `TerminalView` itself overrides (not inherited from NSView).
    fn terminal_view_own_selectors() -> Vec<String> {
        use objc2::ClassType;
        super::TerminalView::class()
            .instance_methods()
            .iter()
            .map(|method| method.name().name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn terminal_view_tracks_backing_scale_when_joining_a_window() {
        let selectors = terminal_view_own_selectors();
        for selector in ["viewDidMoveToWindow", "viewDidChangeBackingProperties"] {
            assert!(
                selectors.iter().any(|name| name == selector),
                "TerminalView must override {selector}: {selectors:?}"
            );
        }
    }
}
