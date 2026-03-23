use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub colors: ColorConfig,
    pub selection: SelectionConfig,
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

impl Default for Config {
    fn default() -> Self {
        Self {
            font: FontConfig::default(),
            window: WindowConfig::default(),
            colors: ColorConfig::default(),
            selection: SelectionConfig::default(),
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
