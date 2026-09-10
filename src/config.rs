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
    pub prompt_marks: PromptMarksConfig,
    pub images: ImagesConfig,
    pub confirm: ConfirmConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    pub zoom_step: f32,
    pub min_size: f32,
    pub max_size: f32,
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
    pub zoom_in: String,
    pub zoom_out: String,
    pub zoom_reset: String,
    pub prev_prompt: String,
    pub next_prompt: String,
    pub resize: ResizeConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct PromptMarksConfig {
    pub enabled: bool,
    pub show_indicator: bool,
    pub indicator_color: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ResizeConfig {
    pub step: u32,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ImagesConfig {
    pub enabled: bool,
    pub max_memory_mb: usize,
    pub sixel_enabled: bool,
    pub kitty_enabled: bool,
    pub kitty: KittyImagesConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct KittyImagesConfig {
    pub max_image_size_mb: usize,
    pub allow_file_transfer: bool,
}

impl ImagesConfig {
    pub(crate) fn kitty_graphics_enabled(&self) -> bool {
        self.enabled && self.kitty_enabled
    }

    pub(crate) fn sixel_graphics_enabled(&self) -> bool {
        self.enabled && self.sixel_enabled
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ConfirmConfig {
    pub on_close_pane: bool,
    pub on_quit: bool,
}

impl Default for ConfirmConfig {
    fn default() -> Self {
        Self {
            on_close_pane: true,
            on_quit: true,
        }
    }
}

impl Default for ImagesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_memory_mb: 256,
            sixel_enabled: true,
            kitty_enabled: true,
            kitty: KittyImagesConfig::default(),
        }
    }
}

impl Default for KittyImagesConfig {
    fn default() -> Self {
        Self {
            max_image_size_mb: 50,
            allow_file_transfer: true,
        }
    }
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
            prompt_marks: PromptMarksConfig::default(),
            images: ImagesConfig::default(),
            confirm: ConfirmConfig::default(),
        }
    }
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "Menlo".to_string(),
            size: 14.0,
            zoom_step: 1.0,
            min_size: 6.0,
            max_size: 72.0,
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
            zoom_in: "cmd+=".to_string(),
            zoom_out: "cmd+-".to_string(),
            zoom_reset: "cmd+0".to_string(),
            prev_prompt: "cmd+up".to_string(),
            next_prompt: "cmd+down".to_string(),
            resize: ResizeConfig::default(),
        }
    }
}

impl Default for PromptMarksConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_indicator: true,
            indicator_color: "#b5312c".to_string(),
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
        Self::load_paths(config_paths(
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        ))
    }

    fn load_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        for path in paths {
            let contents = match std::fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    log::warn!("Failed to read {}: {error}", path.display());
                    return Self::default();
                }
            };
            match toml::from_str(&contents) {
                Ok(config) => {
                    log::info!("Loaded config from {}", path.display());
                    return config;
                }
                Err(error) => {
                    log::warn!("Failed to parse {}: {error}", path.display());
                    return Self::default();
                }
            }
        }
        log::info!("No kokuban.toml found, using defaults");
        Self::default()
    }
}

fn config_paths(xdg_config_home: Option<PathBuf>, home: Option<PathBuf>) -> Vec<PathBuf> {
    // Keep project-local configuration first for existing development setups.
    let mut paths = vec![PathBuf::from("kokuban.toml")];
    let directory = xdg_config_home
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        });
    if let Some(directory) = directory {
        paths.push(directory.join("kokuban/kokuban.toml"));
    }
    paths
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

#[cfg(test)]
mod tests {
    use super::{config_paths, Config, ImagesConfig};
    use std::path::PathBuf;

    #[test]
    fn user_configuration_paths_follow_xdg_and_preserve_local_priority() {
        assert_eq!(
            config_paths(Some("/custom/config".into()), Some("/home/test".into())),
            [
                PathBuf::from("kokuban.toml"),
                "/custom/config/kokuban/kokuban.toml".into()
            ]
        );
        for invalid_xdg in [None, Some(PathBuf::new()), Some("relative".into())] {
            assert_eq!(
                config_paths(invalid_xdg, Some("/home/test".into())),
                [
                    PathBuf::from("kokuban.toml"),
                    "/home/test/.config/kokuban/kokuban.toml".into()
                ]
            );
        }
        assert_eq!(config_paths(None, None), [PathBuf::from("kokuban.toml")]);
    }

    #[test]
    fn loads_first_existing_configuration_without_merging_or_hiding_parse_errors() {
        let directory = std::env::temp_dir().join(format!(
            "kokuban-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let local = directory.join("local.toml");
        let user = directory.join("user.toml");
        std::fs::write(&user, "[window]\ncolumns = 93\n").unwrap();
        assert_eq!(
            Config::load_paths([local.clone(), user.clone()])
                .window
                .columns,
            93
        );
        std::fs::write(&local, "[window]\ncolumns = 71\n").unwrap();
        assert_eq!(
            Config::load_paths([local.clone(), user.clone()])
                .window
                .columns,
            71
        );
        std::fs::write(&local, "invalid toml [").unwrap();
        assert_eq!(
            Config::load_paths([local, user]).window.columns,
            Config::default().window.columns
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn graphics_protocols_honor_master_and_specific_switches() {
        let cases = [
            (false, false, false, false, false),
            (false, false, true, false, false),
            (false, true, false, false, false),
            (false, true, true, false, false),
            (true, false, false, false, false),
            (true, false, true, false, true),
            (true, true, false, true, false),
            (true, true, true, true, true),
        ];

        for (enabled, kitty_enabled, sixel_enabled, expected_kitty, expected_sixel) in cases {
            let config = ImagesConfig {
                enabled,
                kitty_enabled,
                sixel_enabled,
                ..ImagesConfig::default()
            };

            assert_eq!(config.kitty_graphics_enabled(), expected_kitty);
            assert_eq!(config.sixel_graphics_enabled(), expected_sixel);
        }
    }
}
