//! Android input bookkeeping, deliberately independent of NDK types so it can
//! be included as a module in a small host `rustc --test` harness.

use std::collections::BTreeMap;

const SOURCE_MOUSE: u32 = 0x0000_2002;
const PRIMARY: u32 = 1;
const MOUSE_BUTTONS: u32 = 0x1f;
pub(super) const CTRL_MASK: u32 = 0x7000;

pub(super) fn is_mouse(source: u32) -> bool {
    source & SOURCE_MOUSE == SOURCE_MOUSE
}

pub(super) fn normalize_meta(meta: u32) -> u32 {
    // Android AMETA_* aggregate and left/right bits. Lock keys are not winit
    // modifiers. Some injected events supply only a left/right bit.
    let mut normalized = meta & (0xc1 | 0x32 | CTRL_MASK | 0x70000);
    for (mask, aggregate) in [(0xc1, 1), (0x32, 2), (CTRL_MASK, 0x1000), (0x70000, 0x10000)] {
        if meta & mask != 0 {
            normalized |= aggregate;
        }
    }
    normalized
}

pub(super) fn scroll_delta(horizontal: f32, vertical: f32) -> Option<(f32, f32)> {
    if !horizontal.is_finite() || !vertical.is_finite() || (horizontal == 0.0 && vertical == 0.0) {
        return None;
    }
    Some((-horizontal, vertical))
}

#[derive(Debug)]
struct ButtonSnapshot {
    buttons: u32,
    inferred_primary: bool,
}

#[derive(Debug, Default)]
pub(super) struct InputState {
    modifiers: u32,
    buttons: BTreeMap<i32, ButtonSnapshot>,
}

impl InputState {
    pub(super) fn modifiers_changed(&mut self, meta: u32) -> Option<u32> {
        let next = normalize_meta(meta);
        if next == self.modifiers {
            return None;
        }
        self.modifiers = next;
        Some(next)
    }

    /// Reconcile the complete button snapshot. Android can deliver both DOWN
    /// and BUTTON_PRESS for the same transition, and similarly on release.
    pub(super) fn buttons_changed(
        &mut self,
        device: i32,
        buttons: u32,
        down: bool,
        up: bool,
        cancel: bool,
    ) -> Vec<(u32, bool)> {
        let previous = self.buttons.remove(&device);
        let mut next = buttons & MOUSE_BUTTONS;
        let mut inferred_primary = false;
        if cancel {
            next = 0;
        } else if next == 0
            && (down || (!up && previous.as_ref().is_some_and(|p| p.inferred_primary)))
        {
            // `adb shell input mouse tap` and some synthetic input sources
            // provide DOWN/UP without a button-state snapshot.
            next = PRIMARY;
            inferred_primary = true;
        }
        let previous = previous.map_or(0, |p| p.buttons);
        if next != 0 {
            self.buttons.insert(device, ButtonSnapshot { buttons: next, inferred_primary });
        }
        (0..5)
            .map(|bit| 1 << bit)
            .filter(|bit| (previous ^ next) & bit != 0)
            .map(|bit| (bit, next & bit != 0))
            .collect()
    }

    pub(super) fn release_all(&mut self) -> Vec<(i32, u32)> {
        std::mem::take(&mut self.buttons)
            .into_iter()
            .map(|(device, state)| (device, state.buttons))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_source_does_not_capture_fingers_stylus_or_relative_motion() {
        assert!(is_mouse(0x2002));
        assert!(is_mouse(0x2002 | 0x0100_0000));
        for source in [0x1002, 0x4002, 0xc002, 0x20004, 0x100008] {
            assert!(!is_mouse(source));
        }
    }

    #[test]
    fn modifiers_normalize_side_bits_ignore_locks_and_report_releases() {
        let mut state = InputState::default();
        assert_eq!(state.modifiers_changed(0x40 | 0x2000), Some(0x41 | 0x3000));
        assert_eq!(state.modifiers_changed(0x41 | 0x3000 | 0x100000), None);
        assert_eq!(state.modifiers_changed(0), Some(0));
        assert_eq!(state.modifiers_changed(0), None);
        assert_eq!(normalize_meta(0x20 | 0x40000), 0x22 | 0x50000);
    }

    #[test]
    fn down_and_button_press_and_both_release_orders_are_deduplicated() {
        for release_up_first in [false, true] {
            let mut state = InputState::default();
            assert_eq!(state.buttons_changed(9, 1, true, false, false), [(1, true)]);
            assert!(state.buttons_changed(9, 1, false, false, false).is_empty());
            assert_eq!(state.buttons_changed(9, 0, false, release_up_first, false), [(1, false)]);
            assert!(state.buttons_changed(9, 0, false, !release_up_first, false).is_empty());
        }
    }

    #[test]
    fn synthetic_taps_and_independent_devices_keep_correct_button_state() {
        let mut state = InputState::default();
        assert_eq!(state.buttons_changed(1, 0, true, false, false), [(1, true)]);
        assert!(state.buttons_changed(1, 0, false, false, false).is_empty());
        assert_eq!(state.buttons_changed(2, 2, true, false, false), [(2, true)]);
        assert_eq!(state.buttons_changed(1, 0, false, true, false), [(1, false)]);
        assert_eq!(state.buttons_changed(2, 6, false, false, false), [(4, true)]);
        assert_eq!(state.buttons_changed(2, 4, false, false, false), [(2, false)]);
        assert_eq!(state.buttons_changed(2, 0, false, true, false), [(4, false)]);
        assert!(state.release_all().is_empty());
    }

    #[test]
    fn cancel_and_lifecycle_reset_allow_the_next_press() {
        let mut state = InputState::default();
        state.buttons_changed(7, 3, true, false, false);
        assert_eq!(state.buttons_changed(7, 3, false, false, true), [(1, false), (2, false)]);
        state.buttons_changed(7, 1, true, false, false);
        assert_eq!(state.release_all(), [(7, 1)]);
        assert!(state.release_all().is_empty());
        assert_eq!(state.buttons_changed(7, 1, true, false, false), [(1, true)]);
    }

    #[test]
    fn wheels_preserve_fractional_steps_and_translate_horizontal_direction() {
        assert_eq!(scroll_delta(0.25, -0.5), Some((-0.25, -0.5)));
        assert_eq!(scroll_delta(0.0, 1.0), Some((-0.0, 1.0)));
        for (horizontal, vertical) in [(0.0, 0.0), (f32::NAN, 1.0), (1.0, f32::INFINITY)] {
            assert_eq!(scroll_delta(horizontal, vertical), None);
        }
    }
}
