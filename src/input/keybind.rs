//! Atalhos de pane neutros de plataforma.
//!
//! Um `KeyBind` é um `KeyName` (tecla lógica) mais `KeyModifiers`. Cada janela
//! traduz o evento nativo para `KeyName` (`input::macos::key_name_from_key_code`,
//! `input::linux::key_name_from_winit`) e consulta o `KeybindMap`.

use crate::config::{Config, KeybindConfig, LinuxKeybindConfig};
use bitflags::bitflags;
use std::fmt;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KeyModifiers: u8 {
        /// Cmd no macOS, Super/Win no Linux.
        const SUPER = 0x01;
        const SHIFT = 0x02;
        const CTRL  = 0x04;
        const ALT   = 0x08;
    }
}

/// Tecla lógica, independente de keycode e de toolkit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyName {
    /// Letra ASCII minúscula, dígito ou pontuação, como a tecla produz sem
    /// modificadores (Shift não entra aqui; vai em `KeyModifiers`).
    Char(char),
    /// Tecla de função `F1`..`F12`.
    F(u8),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    Escape,
    Enter,
    Backspace,
    Space,
    Insert,
    Delete,
}

impl KeyName {
    /// Normaliza um caractere ASCII imprimível: letras ficam minúsculas, o
    /// espaço vira `Space`; outros caracteres não produzem tecla.
    pub fn from_char(c: char) -> Option<Self> {
        match c {
            ' ' => Some(Self::Space),
            c if c.is_ascii_alphabetic() => Some(Self::Char(c.to_ascii_lowercase())),
            c if c.is_ascii_graphic() => Some(Self::Char(c)),
            _ => None,
        }
    }

    /// Nome usado nos arquivos de configuração.
    fn config_name(self) -> String {
        match self {
            Self::Char('+') => "plus".to_string(),
            Self::Char('-') => "minus".to_string(),
            Self::Char(c) => c.to_string(),
            Self::F(n) => format!("f{n}"),
            Self::Up => "up".to_string(),
            Self::Down => "down".to_string(),
            Self::Left => "left".to_string(),
            Self::Right => "right".to_string(),
            Self::PageUp => "pageup".to_string(),
            Self::PageDown => "pagedown".to_string(),
            Self::Home => "home".to_string(),
            Self::End => "end".to_string(),
            Self::Tab => "tab".to_string(),
            Self::Escape => "escape".to_string(),
            Self::Enter => "enter".to_string(),
            Self::Backspace => "backspace".to_string(),
            Self::Space => "space".to_string(),
            Self::Insert => "insert".to_string(),
            Self::Delete => "delete".to_string(),
        }
    }

    fn from_config_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        let mut chars = lower.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Self::from_char(c);
        }
        Some(match lower.as_str() {
            "plus" => Self::Char('+'),
            "minus" => Self::Char('-'),
            "up" => Self::Up,
            "down" => Self::Down,
            "left" => Self::Left,
            "right" => Self::Right,
            "pageup" => Self::PageUp,
            "pagedown" => Self::PageDown,
            "home" => Self::Home,
            "end" => Self::End,
            "tab" => Self::Tab,
            "esc" | "escape" => Self::Escape,
            "enter" | "return" => Self::Enter,
            "backspace" => Self::Backspace,
            "space" => Self::Space,
            "insert" => Self::Insert,
            "delete" => Self::Delete,
            _ => {
                let number: u8 = lower.strip_prefix('f')?.parse().ok()?;
                if (1..=12).contains(&number) {
                    Self::F(number)
                } else {
                    return None;
                }
            }
        })
    }
}

/// Par (base, com Shift) da fileira de pontuação do teclado US. Com Shift
/// pressionado, `=` e `+` nomeiam a mesma tecla, assim como `-` e `_`: a
/// configuração pode usar qualquer grafia e cada SO reporta a que conhece.
const US_SHIFT_PAIRS: [(char, char); 21] = [
    ('`', '~'),
    ('1', '!'),
    ('2', '@'),
    ('3', '#'),
    ('4', '$'),
    ('5', '%'),
    ('6', '^'),
    ('7', '&'),
    ('8', '*'),
    ('9', '('),
    ('0', ')'),
    ('-', '_'),
    ('=', '+'),
    ('[', '{'),
    (']', '}'),
    ('\\', '|'),
    (';', ':'),
    ('\'', '"'),
    (',', '<'),
    ('.', '>'),
    ('/', '?'),
];

/// O outro nome de um caractere de pontuação quando Shift está pressionado.
fn shift_alias(c: char) -> Option<char> {
    US_SHIFT_PAIRS.iter().find_map(|&(base, shifted)| {
        if c == base {
            Some(shifted)
        } else if c == shifted {
            Some(base)
        } else {
            None
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBind {
    pub key: KeyName,
    pub modifiers: KeyModifiers,
}

impl KeyBind {
    /// Verdadeiro quando `key`+`modifiers` aciona este atalho. Com Shift, a
    /// pontuação casa também pela grafia alternativa (`=`/`+`, `-`/`_`).
    fn matches(&self, key: KeyName, modifiers: KeyModifiers) -> bool {
        if self.modifiers != modifiers {
            return false;
        }
        if self.key == key {
            return true;
        }
        if !modifiers.contains(KeyModifiers::SHIFT) {
            return false;
        }
        match (self.key, key) {
            (KeyName::Char(bound), KeyName::Char(pressed)) => shift_alias(pressed) == Some(bound),
            _ => false,
        }
    }
}

impl fmt::Display for KeyBind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (flag, name) in [
            (KeyModifiers::SUPER, "super"),
            (KeyModifiers::CTRL, "ctrl"),
            (KeyModifiers::ALT, "alt"),
            (KeyModifiers::SHIFT, "shift"),
        ] {
            if self.modifiers.contains(flag) {
                write!(f, "{name}+")?;
            }
        }
        write!(f, "{}", self.key.config_name())
    }
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
    ZoomIn,
    ZoomOut,
    ZoomReset,
    PrevPrompt,
    NextPrompt,
    Search,
    /// Jump to the next pane flagged with attention (BEL / notification).
    FocusAttention,
}

/// Number of configurable pane actions.
pub const PANE_ACTION_COUNT: usize = 18;

/// A configuration table of pane keybinds (`[keybind]` or `[keybind.linux]`).
pub trait PaneKeybindTable {
    /// Each action with its configured chord, unparsed.
    fn bindings(&self) -> [(&str, PaneAction); PANE_ACTION_COUNT];
    /// Pane resize step, in pixels.
    fn resize_step(&self) -> f32;
}

macro_rules! impl_pane_keybind_table {
    ($config:ty) => {
        impl PaneKeybindTable for $config {
            fn bindings(&self) -> [(&str, PaneAction); PANE_ACTION_COUNT] {
                [
                    (&self.split_vertical, PaneAction::SplitVertical),
                    (&self.split_horizontal, PaneAction::SplitHorizontal),
                    (&self.close_pane, PaneAction::ClosePane),
                    (&self.focus_left, PaneAction::FocusLeft),
                    (&self.focus_down, PaneAction::FocusDown),
                    (&self.focus_up, PaneAction::FocusUp),
                    (&self.focus_right, PaneAction::FocusRight),
                    (&self.resize_left, PaneAction::ResizeLeft),
                    (&self.resize_down, PaneAction::ResizeDown),
                    (&self.resize_up, PaneAction::ResizeUp),
                    (&self.resize_right, PaneAction::ResizeRight),
                    (&self.zoom_in, PaneAction::ZoomIn),
                    (&self.zoom_out, PaneAction::ZoomOut),
                    (&self.zoom_reset, PaneAction::ZoomReset),
                    (&self.prev_prompt, PaneAction::PrevPrompt),
                    (&self.next_prompt, PaneAction::NextPrompt),
                    (&self.search, PaneAction::Search),
                    (&self.focus_attention, PaneAction::FocusAttention),
                ]
            }

            fn resize_step(&self) -> f32 {
                self.resize.step as f32
            }
        }
    };
}

impl_pane_keybind_table!(KeybindConfig);
impl_pane_keybind_table!(LinuxKeybindConfig);

/// The keybind table of the running platform: `[keybind]` on macOS,
/// `[keybind.linux]` on Linux.
pub fn platform_table(config: &Config) -> &dyn PaneKeybindTable {
    if cfg!(target_os = "linux") {
        &config.keybind.linux
    } else {
        &config.keybind
    }
}

pub struct KeybindMap {
    bindings: Vec<(KeyBind, PaneAction)>,
}

impl KeybindMap {
    /// Bindings of the platform's table (see `platform_table`).
    pub fn from_platform_config(config: &Config) -> Self {
        Self::from_table(platform_table(config))
    }

    pub fn from_table(table: &dyn PaneKeybindTable) -> Self {
        let mut bindings = Vec::new();
        for (key_str, action) in table.bindings() {
            match parse_keybind(key_str) {
                Some(kb) => bindings.push((kb, action)),
                None => log::warn!("Ignoring unrecognized keybind {key_str:?} for {action:?}"),
            }
        }
        Self { bindings }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn lookup(&self, key: KeyName, modifiers: KeyModifiers) -> Option<PaneAction> {
        self.bindings
            .iter()
            .find(|(kb, _)| kb.matches(key, modifiers))
            .map(|(_, action)| *action)
    }
}

/// Interpreta `"ctrl+shift+plus"`, `"cmd+="`, `"super+f1"`, `"cmd++"`...
/// Modificadores: `cmd`/`command`/`super`, `ctrl`/`control`, `alt`/`option`,
/// `shift`. A tecla é um caractere, `plus`/`minus`, `f1`..`f12` ou um nome
/// (`up`, `pageup`, `esc`, `enter`, `space`, ...).
pub fn parse_keybind(s: &str) -> Option<KeyBind> {
    let s = s.trim();
    // `cmd++` nomeia a tecla `+` por si só.
    let (body, trailing_plus) = match s.strip_suffix("++") {
        Some(body) => (body, true),
        None => (s, false),
    };

    let mut modifiers = KeyModifiers::empty();
    let mut key_name = None;
    for part in body.split('+') {
        let part = part.trim();
        match part.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "super" => modifiers.insert(KeyModifiers::SUPER),
            "shift" => modifiers.insert(KeyModifiers::SHIFT),
            "ctrl" | "control" => modifiers.insert(KeyModifiers::CTRL),
            "alt" | "option" => modifiers.insert(KeyModifiers::ALT),
            "" if trailing_plus => {}
            _ => {
                if key_name.replace(part).is_some() {
                    return None;
                }
            }
        }
    }

    let key = match (key_name, trailing_plus) {
        (None, true) => KeyName::Char('+'),
        (Some(name), false) => KeyName::from_config_name(name)?,
        _ => return None,
    };
    Some(KeyBind { key, modifiers })
}

#[cfg(test)]
mod tests {
    use super::{
        parse_keybind, platform_table, KeyBind, KeyModifiers, KeyName, KeybindMap, PaneAction,
        PANE_ACTION_COUNT,
    };
    use crate::config::{Config, KeybindConfig, LinuxKeybindConfig};

    fn bind(key: KeyName, modifiers: KeyModifiers) -> Option<KeyBind> {
        Some(KeyBind { key, modifiers })
    }

    #[test]
    fn parses_macos_style_and_linux_style_names() {
        let sup = KeyModifiers::SUPER;
        let cs = KeyModifiers::CTRL | KeyModifiers::SHIFT;
        let cases = [
            (
                "cmd+shift+a",
                bind(KeyName::Char('a'), sup | KeyModifiers::SHIFT),
            ),
            ("Command+D", bind(KeyName::Char('d'), sup)),
            ("super+w", bind(KeyName::Char('w'), sup)),
            ("cmd+=", bind(KeyName::Char('='), sup)),
            ("cmd+-", bind(KeyName::Char('-'), sup)),
            ("cmd++", bind(KeyName::Char('+'), sup)),
            ("cmd+0", bind(KeyName::Char('0'), sup)),
            ("cmd+up", bind(KeyName::Up, sup)),
            ("cmd+down", bind(KeyName::Down, sup)),
            ("ctrl+shift+plus", bind(KeyName::Char('+'), cs)),
            ("ctrl+shift+minus", bind(KeyName::Char('-'), cs)),
            ("ctrl+shift+f", bind(KeyName::Char('f'), cs)),
            ("control+shift+pageup", bind(KeyName::PageUp, cs)),
            ("ctrl+shift+PageDown", bind(KeyName::PageDown, cs)),
            (
                "ctrl+alt+shift+h",
                bind(KeyName::Char('h'), cs | KeyModifiers::ALT),
            ),
            ("option+left", bind(KeyName::Left, KeyModifiers::ALT)),
            ("alt+right", bind(KeyName::Right, KeyModifiers::ALT)),
            ("ctrl+home", bind(KeyName::Home, KeyModifiers::CTRL)),
            ("ctrl+end", bind(KeyName::End, KeyModifiers::CTRL)),
            ("ctrl+tab", bind(KeyName::Tab, KeyModifiers::CTRL)),
            ("ctrl+esc", bind(KeyName::Escape, KeyModifiers::CTRL)),
            ("ctrl+escape", bind(KeyName::Escape, KeyModifiers::CTRL)),
            ("shift+enter", bind(KeyName::Enter, KeyModifiers::SHIFT)),
            ("shift+return", bind(KeyName::Enter, KeyModifiers::SHIFT)),
            (
                "ctrl+backspace",
                bind(KeyName::Backspace, KeyModifiers::CTRL),
            ),
            ("ctrl+space", bind(KeyName::Space, KeyModifiers::CTRL)),
            ("shift+insert", bind(KeyName::Insert, KeyModifiers::SHIFT)),
            ("ctrl+delete", bind(KeyName::Delete, KeyModifiers::CTRL)),
            ("f1", bind(KeyName::F(1), KeyModifiers::empty())),
            ("super+F12", bind(KeyName::F(12), sup)),
            (" ctrl + shift + d ", bind(KeyName::Char('d'), cs)),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_keybind(text), expected, "{text:?}");
        }
    }

    #[test]
    fn rejects_unknown_or_ambiguous_names() {
        for text in [
            "", "cmd", "cmd+", "cmd+f13", "cmd+f0", "cmd+a+b", "ctrl+ç", "ctrl+foo", "+",
        ] {
            assert_eq!(parse_keybind(text), None, "{text:?}");
        }
    }

    #[test]
    fn display_round_trips_through_parse() {
        for text in [
            "super+shift+a",
            "super+=",
            "ctrl+shift+plus",
            "ctrl+shift+minus",
            "super+ctrl+alt+shift+f5",
            "ctrl+pageup",
            "shift+enter",
            "alt+space",
            "super+0",
            "ctrl+shift+n",
        ] {
            let parsed = parse_keybind(text).expect(text);
            assert_eq!(parsed.to_string(), text);
            assert_eq!(parse_keybind(&parsed.to_string()), Some(parsed));
        }
    }

    #[test]
    fn default_focus_attention_binding_resolves() {
        let map = KeybindMap::from_table(&KeybindConfig::default());
        assert_eq!(
            map.lookup(
                KeyName::Char('a'),
                KeyModifiers::SUPER | KeyModifiers::SHIFT
            ),
            Some(PaneAction::FocusAttention)
        );
        assert_eq!(
            map.lookup(KeyName::Char('a'), KeyModifiers::SUPER),
            None,
            "cmd+a stays free"
        );
        assert_eq!(
            map.lookup(KeyName::Char('d'), KeyModifiers::SUPER),
            Some(PaneAction::SplitVertical)
        );
        assert_eq!(
            map.lookup(KeyName::Up, KeyModifiers::SUPER),
            Some(PaneAction::PrevPrompt)
        );
        assert_eq!(
            map.lookup(KeyName::Char('='), KeyModifiers::SUPER),
            Some(PaneAction::ZoomIn)
        );
    }

    #[test]
    fn shifted_punctuation_matches_either_spelling() {
        let config = KeybindConfig {
            zoom_in: "ctrl+shift+plus".to_string(),
            zoom_out: "ctrl+shift+-".to_string(),
            ..KeybindConfig::default()
        };
        let map = KeybindMap::from_table(&config);
        let cs = KeyModifiers::CTRL | KeyModifiers::SHIFT;
        // Linux reports the unshifted key, macOS the shifted character.
        assert_eq!(map.lookup(KeyName::Char('='), cs), Some(PaneAction::ZoomIn));
        assert_eq!(map.lookup(KeyName::Char('+'), cs), Some(PaneAction::ZoomIn));
        assert_eq!(
            map.lookup(KeyName::Char('-'), cs),
            Some(PaneAction::ZoomOut)
        );
        assert_eq!(
            map.lookup(KeyName::Char('_'), cs),
            Some(PaneAction::ZoomOut)
        );
        // Without Shift the spellings are distinct keys.
        assert_eq!(map.lookup(KeyName::Char('+'), KeyModifiers::CTRL), None);
        assert_eq!(map.lookup(KeyName::Char('='), KeyModifiers::CTRL), None);
    }

    #[test]
    fn from_char_lowercases_letters_and_maps_space() {
        assert_eq!(KeyName::from_char('A'), Some(KeyName::Char('a')));
        assert_eq!(KeyName::from_char('z'), Some(KeyName::Char('z')));
        assert_eq!(KeyName::from_char('7'), Some(KeyName::Char('7')));
        assert_eq!(KeyName::from_char('['), Some(KeyName::Char('[')));
        assert_eq!(KeyName::from_char(' '), Some(KeyName::Space));
        assert_eq!(KeyName::from_char('\r'), None);
        assert_eq!(KeyName::from_char('é'), None);
        assert_eq!(KeyName::from_char('\u{F700}'), None);
    }

    #[test]
    fn every_default_binding_parses_on_both_platforms() {
        assert_eq!(
            KeybindMap::from_table(&KeybindConfig::default()).len(),
            PANE_ACTION_COUNT
        );
        assert_eq!(
            KeybindMap::from_table(&LinuxKeybindConfig::default()).len(),
            PANE_ACTION_COUNT
        );
    }

    #[test]
    fn linux_defaults_resolve_and_avoid_the_fixed_clipboard_and_scrollback_chords() {
        let map = KeybindMap::from_table(&LinuxKeybindConfig::default());
        let cs = KeyModifiers::CTRL | KeyModifiers::SHIFT;
        let cas = cs | KeyModifiers::ALT;
        let expected = [
            (KeyName::Char('d'), cs, PaneAction::SplitVertical),
            (KeyName::Char('e'), cs, PaneAction::SplitHorizontal),
            (KeyName::Char('w'), cs, PaneAction::ClosePane),
            (KeyName::Char('h'), cs, PaneAction::FocusLeft),
            (KeyName::Char('j'), cs, PaneAction::FocusDown),
            (KeyName::Char('k'), cs, PaneAction::FocusUp),
            (KeyName::Char('l'), cs, PaneAction::FocusRight),
            (KeyName::Char('h'), cas, PaneAction::ResizeLeft),
            (KeyName::Char('j'), cas, PaneAction::ResizeDown),
            (KeyName::Char('k'), cas, PaneAction::ResizeUp),
            (KeyName::Char('l'), cas, PaneAction::ResizeRight),
            // winit reports the unshifted key for Ctrl+Shift+=.
            (KeyName::Char('='), cs, PaneAction::ZoomIn),
            (KeyName::Char('+'), cs, PaneAction::ZoomIn),
            (KeyName::Char('-'), cs, PaneAction::ZoomOut),
            (KeyName::Char('0'), cs, PaneAction::ZoomReset),
            (KeyName::Up, cs, PaneAction::PrevPrompt),
            (KeyName::Down, cs, PaneAction::NextPrompt),
            (KeyName::Char('f'), cs, PaneAction::Search),
            (KeyName::Char('n'), cs, PaneAction::FocusAttention),
        ];
        for (key, modifiers, action) in expected {
            assert_eq!(map.lookup(key, modifiers), Some(action), "{key:?}");
        }

        // Fixed Linux chords (`linux_window::clipboard`, `input::linux`) must
        // never be shadowed by a default pane binding.
        let fixed = [
            (KeyName::Char('c'), cs),
            (KeyName::Char('v'), cs),
            (KeyName::Char('a'), cs),
            (KeyName::Insert, KeyModifiers::CTRL),
            (KeyName::Insert, KeyModifiers::SHIFT),
            (KeyName::PageUp, KeyModifiers::SHIFT),
            (KeyName::PageDown, KeyModifiers::SHIFT),
            (KeyName::Home, KeyModifiers::SHIFT),
            (KeyName::End, KeyModifiers::SHIFT),
        ];
        for (key, modifiers) in fixed {
            assert_eq!(
                map.lookup(key, modifiers),
                None,
                "{key:?}+{modifiers:?} is a fixed chord"
            );
        }
    }

    #[test]
    fn platform_table_picks_the_running_platform_section() {
        let config = Config::default();
        let table = platform_table(&config);
        let (split, _) = table.bindings()[0];
        if cfg!(target_os = "linux") {
            assert_eq!(split, "ctrl+shift+d");
        } else {
            assert_eq!(split, "cmd+d");
        }
        assert_eq!(table.resize_step(), 20.0);
        assert_eq!(
            KeybindMap::from_platform_config(&config).len(),
            PANE_ACTION_COUNT
        );
    }
}
