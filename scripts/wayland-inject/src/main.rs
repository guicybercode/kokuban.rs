//! Test-only Wayland input injector for wlroots compositors (sway headless),
//! used by `scripts/linux-wayland-smoke.py`. Not part of Kokuban itself.
//!
//! It binds zwlr_virtual_pointer_v1, zwp_virtual_keyboard_v1 and
//! zwp_input_method_v2, reads one command per line from stdin and keeps its
//! virtual devices alive until stdin closes:
//!   move X Y              absolute pointer motion in output logical pixels
//!   press|release|click BTN   BTN = left | middle | right
//!   wait-active MS        wait until a text-input-v3 client enables the IM
//!   preedit TEXT B E      input-method-v2 preedit (B/E: byte cursor, -1 = hidden)
//!   commit TEXT           input-method-v2 commit_string
//!   chord KEYS            e.g. ctrl+shift+v (press in order, release reversed)
//!   extent W H            output logical size after a scale change
//!   sleep MS
//! Text arguments use `\s` for spaces. Events are printed to stdout.

use std::io::{BufRead, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

/// US layout; the compositor resolves the includes with the system xkb data.
const KEYMAP: &str = r#"xkb_keymap {
    xkb_keycodes { include "evdev+aliases(qwerty)" };
    xkb_types { include "complete" };
    xkb_compat { include "complete" };
    xkb_symbols { include "pc+us+inet(evdev)" };
};
"#;

#[derive(Default)]
struct State {
    pending_active: bool,
    active: bool,
    done_count: u32,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrVirtualPointerManagerV1,
        _: <ZwlrVirtualPointerManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrVirtualPointerV1,
        _: <ZwlrVirtualPointerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: <ZwpVirtualKeyboardManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: <ZwpVirtualKeyboardV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Evdev key code and modifier mask (Shift=1, Control=4, Mod1=8).
fn key_code(name: &str) -> (u32, u32) {
    match name {
        "shift" => (42, 1),
        "ctrl" => (29, 4),
        "alt" => (56, 8),
        "a" => (30, 0),
        "c" => (46, 0),
        "v" => (47, 0),
        "x" => (45, 0),
        "insert" => (110, 0),
        "enter" => (28, 0),
        "escape" => (1, 0),
        other => panic!("unknown key {other}"),
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpInputMethodManagerV2,
        _: <ZwpInputMethodManagerV2 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.pending_active = true,
            zwp_input_method_v2::Event::Deactivate => state.pending_active = false,
            zwp_input_method_v2::Event::Done => {
                state.done_count += 1;
                if state.active != state.pending_active {
                    state.active = state.pending_active;
                    println!(
                        "im: {}",
                        if state.active {
                            "activate"
                        } else {
                            "deactivate"
                        }
                    );
                }
                println!("im: done #{}", state.done_count);
            }
            zwp_input_method_v2::Event::ContentType { hint, purpose } => {
                println!("im: content_type hint={hint:?} purpose={purpose:?}");
            }
            zwp_input_method_v2::Event::SurroundingText {
                text,
                cursor,
                anchor,
            } => {
                println!("im: surrounding {text:?} {cursor} {anchor}");
            }
            zwp_input_method_v2::Event::Unavailable => println!("im: unavailable"),
            other => println!("im: {other:?}"),
        }
        let _ = std::io::stdout().flush();
    }
}

fn now_ms(start: Instant) -> u32 {
    start.elapsed().as_millis() as u32
}

fn text_arg(value: &str) -> String {
    value.replace("\\s", " ")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut extent_w: u32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(1280);
    let mut extent_h: u32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(800);
    let conn = Connection::connect_to_env().expect("connect to WAYLAND_DISPLAY");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).expect("wl_seat");
    let pointer_manager: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .expect("zwlr_virtual_pointer_manager_v1");
    let pointer = pointer_manager.create_virtual_pointer(Some(&seat), &qh, ());
    let im_manager: Option<ZwpInputMethodManagerV2> = globals.bind(&qh, 1..=1, ()).ok();
    let input_method = im_manager
        .as_ref()
        .map(|m| m.get_input_method(&seat, &qh, ()));
    let mut keymap = KEYMAP.as_bytes().to_vec();
    keymap.push(0);
    let keymap_file = {
        let mut file = tempfile_in_runtime();
        file.write_all(&keymap).unwrap();
        file
    };
    let keyboard_manager: ZwpVirtualKeyboardManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("zwp_virtual_keyboard_manager_v1");
    let keyboard = keyboard_manager.create_virtual_keyboard(&seat, &qh, ());
    keyboard.keymap(1, keymap_file.as_fd(), keymap.len() as u32);
    let mut state = State::default();
    queue.roundtrip(&mut state).expect("roundtrip");
    println!(
        "ready extent={extent_w}x{extent_h} input_method={}",
        input_method.is_some()
    );
    let _ = std::io::stdout().flush();
    let start = Instant::now();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let button = |name: &str| match name {
            "left" => 0x110,
            "right" => 0x111,
            "middle" => 0x112,
            other => panic!("unknown button {other}"),
        };
        match parts[0] {
            "move" => {
                let x: f64 = parts[1].parse().unwrap();
                let y: f64 = parts[2].parse().unwrap();
                pointer.motion_absolute(
                    now_ms(start),
                    x.round() as u32,
                    y.round() as u32,
                    extent_w,
                    extent_h,
                );
                pointer.frame();
            }
            "press" | "release" | "click" => {
                let code = button(parts[1]);
                if parts[0] != "release" {
                    pointer.button(now_ms(start), code, wl_pointer::ButtonState::Pressed);
                    pointer.frame();
                }
                if parts[0] == "click" {
                    conn.flush().unwrap();
                    std::thread::sleep(Duration::from_millis(40));
                }
                if parts[0] != "press" {
                    pointer.button(now_ms(start), code, wl_pointer::ButtonState::Released);
                    pointer.frame();
                }
            }
            "wait-active" => {
                let limit = Duration::from_millis(parts[1].parse().unwrap());
                let begin = Instant::now();
                while !state.active && begin.elapsed() < limit {
                    queue.roundtrip(&mut state).unwrap();
                    std::thread::sleep(Duration::from_millis(20));
                }
                println!("wait-active: {}", state.active);
            }
            "preedit" => {
                // input-method-v2 commits carry the number of `done` events
                // seen; read pending ones so sway does not drop a stale commit.
                queue.roundtrip(&mut state).unwrap();
                let im = input_method.as_ref().expect("input method");
                let begin: i32 = parts[2].parse().unwrap();
                let end: i32 = parts[3].parse().unwrap();
                im.set_preedit_string(text_arg(parts[1]), begin, end);
                im.commit(state.done_count);
            }
            "commit" => {
                queue.roundtrip(&mut state).unwrap();
                let im = input_method.as_ref().expect("input method");
                im.set_preedit_string(String::new(), 0, 0);
                im.commit_string(text_arg(parts[1]));
                im.commit(state.done_count);
            }
            "chord" => {
                // chord ctrl+shift+v: press in order, release in reverse.
                let keys: Vec<(u32, u32)> = parts[1].split('+').map(key_code).collect();
                let mut mask = 0;
                for (code, modifier) in &keys {
                    keyboard.key(now_ms(start), *code, 1);
                    if *modifier != 0 {
                        mask |= modifier;
                        keyboard.modifiers(mask, 0, 0, 0);
                    }
                }
                conn.flush().unwrap();
                std::thread::sleep(Duration::from_millis(30));
                for (code, modifier) in keys.iter().rev() {
                    keyboard.key(now_ms(start), *code, 0);
                    if *modifier != 0 {
                        mask &= !modifier;
                        keyboard.modifiers(mask, 0, 0, 0);
                    }
                }
            }
            "extent" => {
                extent_w = parts[1].parse().unwrap();
                extent_h = parts[2].parse().unwrap();
            }
            "sleep" => {
                std::thread::sleep(Duration::from_millis(parts[1].parse().unwrap()));
            }
            other => panic!("unknown command {other}"),
        }
        queue.roundtrip(&mut state).unwrap();
        println!("ok {line}");
        let _ = std::io::stdout().flush();
    }
}

fn tempfile_in_runtime() -> std::fs::File {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path = format!("{dir}/wayland-inject-keymap-{}", std::process::id());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    file
}
