use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub colors: ColorConfig,
    pub selection: SelectionConfig,
    pub status_bar: StatusBarConfig,
    pub theme: ThemeConfig,
    pub keybind: KeybindConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub columns: u16,
    pub rows: u16,
    pub scrollback_lines: usize,
    pub opacity: f32,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct SelectionConfig {
    pub foreground: String,
    pub background: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ColorConfig {
    pub foreground: String,
    pub background: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct StatusBarConfig {
    pub enabled: bool,
    pub show_shell: bool,
    pub show_cwd: bool,
    pub show_pane_index: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub chrome: ChromeConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ChromeConfig {
    pub sumi_black: String,
    pub sumi_dark: String,
    pub sumi_medium: String,
    pub sumi_light: String,
    pub sumi_ghost: String,
    pub hanko_red: String,
    pub hanko_dim: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct KeybindConfig {
    pub split_vertical: String,
    pub split_horizontal: String,
    pub close_pane: String,
    pub focus_left: String,
    pub focus_down: String,
    pub focus_up: String,
    pub focus_right: String,
    pub resize_left: String,
    pub resize_down: String,
    pub resize_up: String,
    pub resize_right: String,
    pub resize: ResizeConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ResizeConfig {
    pub step: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: FontConfig::default(),
            window: WindowConfig::default(),
            colors: ColorConfig::default(),
            selection: SelectionConfig::default(),
            status_bar: StatusBarConfig::default(),
            theme: ThemeConfig::default(),
            keybind: KeybindConfig::default(),
        }
    }
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "Menlo".to_string(),
            size: 14.0,
        }
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            columns: 80,
            rows: 24,
            scrollback_lines: 10000,
            opacity: 1.0,
        }
    }
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            foreground: "#000000".to_string(),
            background: "#b4d5fe".to_string(),
        }
    }
}

impl Default for ColorConfig {
    fn default() -> Self {
        Self {
            foreground: "#c0c0c0".to_string(),
            background: "#1a1a2e".to_string(),
        }
    }
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_shell: true,
            show_cwd: true,
            show_pane_index: true,
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            chrome: ChromeConfig::default(),
        }
    }
}

impl Default for ChromeConfig {
    fn default() -> Self {
        Self {
            sumi_black: "#1a1a1a".to_string(),
            sumi_dark: "#2d2d2d".to_string(),
            sumi_medium: "#4a4a4a".to_string(),
            sumi_light: "#7a7a7a".to_string(),
            sumi_ghost: "#3a3a3a".to_string(),
            hanko_red: "#b5312c".to_string(),
            hanko_dim: "#6b2320".to_string(),
        }
    }
}

impl Default for KeybindConfig {
    fn default() -> Self {
        Self {
            split_vertical: "cmd+d".to_string(),
            split_horizontal: "cmd+shift+d".to_string(),
            close_pane: "cmd+w".to_string(),
            focus_left: "cmd+h".to_string(),
            focus_down: "cmd+j".to_string(),
            focus_up: "cmd+k".to_string(),
            focus_right: "cmd+l".to_string(),
            resize_left: "cmd+shift+h".to_string(),
            resize_down: "cmd+shift+j".to_string(),
            resize_up: "cmd+shift+k".to_string(),
            resize_right: "cmd+shift+l".to_string(),
            resize: ResizeConfig::default(),
        }
    }
}

impl Default for ResizeConfig {
    fn default() -> Self {
        Self { step: 20 }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = PathBuf::from("kokuban.toml");
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match toml::from_str(&contents) {
                    Ok(config) => {
                        log::info!("Loaded config from kokuban.toml");
                        return config;
                    }
                    Err(e) => log::warn!("Failed to parse kokuban.toml: {e}"),
                },
                Err(e) => log::warn!("Failed to read kokuban.toml: {e}"),
            }
        } else {
            log::info!("No kokuban.toml found, using defaults");
        }
        Self::default()
    }
}

impl ColorConfig {
    pub fn parse_hex(hex: &str) -> (u8, u8, u8) {
        let hex = hex.trim_start_matches('#');
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(192);
            let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(192);
            let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(192);
            (r, g, b)
        } else {
            (192, 192, 192)
        }
    }
}
