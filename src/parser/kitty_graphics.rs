use base64::Engine;

#[derive(Debug, Clone)]
pub struct KittyCommand {
    pub action: KittyAction,
    pub quiet: u8,
    pub image_id: Option<u32>,
    pub image_number: Option<u32>,
    pub placement_id: Option<u32>,
    pub format: KittyFormat,
    pub transmission: KittyTransmission,
    pub compression: KittyCompression,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub more_chunks: bool,
    pub payload: Vec<u8>,
    // Placement keys
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub x_offset: Option<u32>,
    pub y_offset: Option<u32>,
    pub z_index: Option<i32>,
    pub cursor_movement: Option<u8>,
    // Delete specifier
    pub delete_specifier: Option<KittyDeleteSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyAction {
    Transmit,
    TransmitAndPlace,
    Place,
    Delete,
    Query,
    Frame,
    Animate,
    Compose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyFormat {
    Rgb,
    Rgba,
    Png,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyTransmission {
    Direct,
    File,
    SharedMemory,
    TempFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyCompression {
    None,
    Zlib,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyDeleteSpec {
    All,
    ById(u32),
    AllImages,
    ByNumber(u32),
    AtCursor,
    ByColumn(u32),
    ByRow(u32),
    ByZIndex(i32),
}

impl Default for KittyCommand {
    fn default() -> Self {
        Self {
            action: KittyAction::Transmit,
            quiet: 0,
            image_id: None,
            image_number: None,
            placement_id: None,
            format: KittyFormat::Rgba,
            transmission: KittyTransmission::Direct,
            compression: KittyCompression::None,
            width: None,
            height: None,
            more_chunks: false,
            payload: Vec::new(),
            columns: None,
            rows: None,
            x_offset: None,
            y_offset: None,
            z_index: None,
            cursor_movement: None,
            delete_specifier: None,
        }
    }
}

/// Parse a Kitty graphics APC sequence. The `data` is everything between `\x1b_G` and `\x1b\\`.
pub fn parse_kitty_command(data: &[u8]) -> Option<KittyCommand> {
    // Split on ';' to separate control data from payload
    let (control_data, payload_b64) = match data.iter().position(|&b| b == b';') {
        Some(pos) => (&data[..pos], &data[pos + 1..]),
        None => (data, &[] as &[u8]),
    };

    let control_str = std::str::from_utf8(control_data).ok()?;
    let mut cmd = KittyCommand::default();

    // Parse comma-separated key=value pairs
    for pair in control_str.split(',') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.find('=') {
            Some(pos) => (&pair[..pos], &pair[pos + 1..]),
            None => continue,
        };

        match key {
            "a" => {
                cmd.action = match value {
                    "t" => KittyAction::Transmit,
                    "T" => KittyAction::TransmitAndPlace,
                    "p" => KittyAction::Place,
                    "d" => KittyAction::Delete,
                    "q" => KittyAction::Query,
                    "f" => KittyAction::Frame,
                    "a" => KittyAction::Animate,
                    "c" => KittyAction::Compose,
                    _ => KittyAction::Transmit,
                };
            }
            "q" => {
                cmd.quiet = value.parse().unwrap_or(0);
            }
            "i" => {
                cmd.image_id = value.parse().ok();
            }
            "I" => {
                cmd.image_number = value.parse().ok();
            }
            "p" => {
                cmd.placement_id = value.parse().ok();
            }
            "f" => {
                cmd.format = match value {
                    "24" => KittyFormat::Rgb,
                    "32" => KittyFormat::Rgba,
                    "100" => KittyFormat::Png,
                    _ => KittyFormat::Rgba,
                };
            }
            "t" => {
                cmd.transmission = match value {
                    "d" => KittyTransmission::Direct,
                    "f" => KittyTransmission::File,
                    "s" => KittyTransmission::SharedMemory,
                    "t" => KittyTransmission::TempFile,
                    _ => KittyTransmission::Direct,
                };
            }
            "o" => {
                cmd.compression = match value {
                    "z" => KittyCompression::Zlib,
                    _ => KittyCompression::None,
                };
            }
            "s" => {
                cmd.width = value.parse().ok();
            }
            "v" => {
                cmd.height = value.parse().ok();
            }
            "m" => {
                cmd.more_chunks = value == "1";
            }
            "c" => {
                cmd.columns = value.parse().ok();
            }
            "r" => {
                cmd.rows = value.parse().ok();
            }
            "X" => {
                cmd.x_offset = value.parse().ok();
            }
            "Y" => {
                cmd.y_offset = value.parse().ok();
            }
            "z" => {
                cmd.z_index = value.parse().ok();
            }
            "C" => {
                cmd.cursor_movement = value.parse().ok();
            }
            "d" => {
                cmd.delete_specifier = Some(parse_delete_spec(value, &cmd));
            }
            _ => {
                log::trace!("Unknown kitty graphics key: {key}={value}");
            }
        }
    }

    // Decode base64 payload
    if !payload_b64.is_empty() {
        match base64::engine::general_purpose::STANDARD.decode(payload_b64) {
            Ok(decoded) => cmd.payload = decoded,
            Err(e) => {
                log::warn!("Failed to decode kitty graphics payload: {e}");
                return None;
            }
        }
    }

    Some(cmd)
}

fn parse_delete_spec(value: &str, cmd: &KittyCommand) -> KittyDeleteSpec {
    match value {
        "a" => KittyDeleteSpec::All,
        "I" => KittyDeleteSpec::AllImages,
        "i" => {
            if let Some(id) = cmd.image_id {
                KittyDeleteSpec::ById(id)
            } else {
                KittyDeleteSpec::All
            }
        }
        "n" => {
            if let Some(num) = cmd.image_number {
                KittyDeleteSpec::ByNumber(num)
            } else {
                KittyDeleteSpec::All
            }
        }
        "c" => KittyDeleteSpec::AtCursor,
        "x" => {
            if let Some(col) = cmd.columns {
                KittyDeleteSpec::ByColumn(col)
            } else {
                KittyDeleteSpec::AtCursor
            }
        }
        "y" => {
            if let Some(row) = cmd.rows {
                KittyDeleteSpec::ByRow(row)
            } else {
                KittyDeleteSpec::AtCursor
            }
        }
        "z" => {
            KittyDeleteSpec::ByZIndex(cmd.z_index.unwrap_or(0))
        }
        _ => KittyDeleteSpec::All,
    }
}
