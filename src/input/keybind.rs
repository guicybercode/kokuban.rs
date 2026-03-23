use crate::config::KeybindConfig;
use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KeyModifiers: u8 {
        const CMD   = 0x01;
        const SHIFT = 0x02;
        const CTRL  = 0x04;
        const ALT   = 0x08;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBind {
    pub key_code: u16,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneAction {
    SplitVertical,
    SplitHorizontal,
    ClosePane,
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    ResizeLeft,
    ResizeDown,
    ResizeUp,
    ResizeRight,
}

pub struct KeybindMap {
    bindings: Vec<(KeyBind, PaneAction)>,
}

impl KeybindMap {
    pub fn from_config(config: &KeybindConfig) -> Self {
        let entries = [
            (&config.split_vertical, PaneAction::SplitVertical),
            (&config.split_horizontal, PaneAction::SplitHorizontal),
            (&config.close_pane, PaneAction::ClosePane),
            (&config.focus_left, PaneAction::FocusLeft),
            (&config.focus_down, PaneAction::FocusDown),
            (&config.focus_up, PaneAction::FocusUp),
            (&config.focus_right, PaneAction::FocusRight),
            (&config.resize_left, PaneAction::ResizeLeft),
            (&config.resize_down, PaneAction::ResizeDown),
            (&config.resize_up, PaneAction::ResizeUp),
            (&config.resize_right, PaneAction::ResizeRight),
        ];

        let mut bindings = Vec::new();
        for (key_str, action) in entries {
            if let Some(kb) = parse_keybind(key_str) {
                bindings.push((kb, action));
            }
        }
        Self { bindings }
    }

    pub fn lookup(&self, key_code: u16, modifiers: KeyModifiers) -> Option<PaneAction> {
        for (kb, action) in &self.bindings {
            if kb.key_code == key_code && kb.modifiers == modifiers {
                return Some(*action);
            }
        }
        None
    }
}

fn parse_keybind(s: &str) -> Option<KeyBind> {
    let parts: Vec<&str> = s.split('+').collect();
    let mut modifiers = KeyModifiers::empty();
    let mut key_name = "";

    for part in &parts {
        match part.to_lowercase().as_str() {
            "cmd" | "command" | "super" => modifiers.insert(KeyModifiers::CMD),
            "shift" => modifiers.insert(KeyModifiers::SHIFT),
            "ctrl" | "control" => modifiers.insert(KeyModifiers::CTRL),
            "alt" | "option" => modifiers.insert(KeyModifiers::ALT),
            _ => key_name = part,
        }
    }

    let key_code = key_name_to_code(key_name)?;
    Some(KeyBind { key_code, modifiers })
}

fn key_name_to_code(name: &str) -> Option<u16> {
    match name.to_lowercase().as_str() {
        "a" => Some(0),
        "s" => Some(1),
        "d" => Some(2),
        "f" => Some(3),
        "h" => Some(4),
        "g" => Some(5),
        "z" => Some(6),
        "x" => Some(7),
        "c" => Some(8),
        "v" => Some(9),
        "b" => Some(11),
        "q" => Some(12),
        "w" => Some(13),
        "e" => Some(14),
        "r" => Some(15),
        "y" => Some(16),
        "t" => Some(17),
        "o" => Some(31),
        "u" => Some(32),
        "i" => Some(34),
        "p" => Some(35),
        "l" => Some(37),
        "j" => Some(38),
        "k" => Some(40),
        "n" => Some(45),
        "m" => Some(46),
        _ => None,
    }
}
