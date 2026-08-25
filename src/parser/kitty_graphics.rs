use base64::Engine;

const MAX_PAYLOAD_BASE64_BYTES: usize = 4096;

#[derive(Debug, Clone)]
pub struct KittyCommand {
    pub action: KittyAction,
    pub quiet: u8,
    pub image_id: Option<u32>,
    pub image_number: Option<u32>,
    // True when i/I cannot be parsed or both selector namespaces are present.
    pub invalid_image_selector: bool,
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
    NoOp,
    All,
    AllImages,
    ById { id: u32, delete_data: bool },
    ByNumber { number: u32, delete_data: bool },
    AtCursor { delete_data: bool },
    ByColumn { column: u32, delete_data: bool },
    ByRow { row: u32, delete_data: bool },
    ByZIndex { z_index: i32, delete_data: bool },
}

impl Default for KittyCommand {
    fn default() -> Self {
        Self {
            action: KittyAction::Transmit,
            quiet: 0,
            image_id: None,
            image_number: None,
            invalid_image_selector: false,
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
    let mut delete_selector = None;
    let mut delete_x = None;
    let mut delete_y = None;
    let mut invalid_placement_id = false;
    let mut image_id_key_seen = false;
    let mut image_number_key_seen = false;
    let mut invalid_image_id = false;
    let mut invalid_image_number = false;

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
                image_id_key_seen = true;
                cmd.image_id = match value.parse() {
                    Ok(image_id) => Some(image_id),
                    Err(_) => {
                        invalid_image_id = true;
                        None
                    }
                };
            }
            "I" => {
                image_number_key_seen = true;
                cmd.image_number = match value.parse() {
                    Ok(image_number) => Some(image_number),
                    Err(_) => {
                        invalid_image_number = true;
                        None
                    }
                };
            }
            "p" => match value.parse::<u32>() {
                Ok(0) => {
                    cmd.placement_id = None;
                    invalid_placement_id = false;
                }
                Ok(placement_id) => {
                    cmd.placement_id = Some(placement_id);
                    invalid_placement_id = false;
                }
                Err(_) => {
                    cmd.placement_id = None;
                    invalid_placement_id = true;
                }
            },
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
            "x" => {
                delete_x = value.parse().ok();
            }
            "y" => {
                delete_y = value.parse().ok();
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
                delete_selector = Some(value);
            }
            _ => {
                log::trace!("Unknown kitty graphics key: {key}={value}");
            }
        }
    }

    cmd.invalid_image_selector = invalid_image_id
        || invalid_image_number
        || (image_id_key_seen && image_number_key_seen);

    let invalid_delete_command = cmd.action == KittyAction::Delete
        && (invalid_placement_id || cmd.invalid_image_selector);

    if let Some(selector) = delete_selector {
        let specifier = if invalid_delete_command {
            KittyDeleteSpec::NoOp
        } else {
            parse_delete_spec(selector, &cmd, delete_x, delete_y)
        };
        if specifier == KittyDeleteSpec::NoOp {
            log::warn!("Ignoring invalid or unsupported Kitty delete selector: {selector}");
        }
        cmd.delete_specifier = Some(specifier);
    } else if invalid_delete_command {
        log::warn!("Ignoring invalid Kitty delete command");
        cmd.delete_specifier = Some(KittyDeleteSpec::NoOp);
    }

    // Decode base64 payload
    if !payload_b64.is_empty() {
        if payload_b64.len() > MAX_PAYLOAD_BASE64_BYTES {
            log::warn!("Kitty graphics chunk exceeds {MAX_PAYLOAD_BASE64_BYTES} encoded bytes");
            return None;
        }
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

fn parse_delete_spec(
    value: &str,
    cmd: &KittyCommand,
    delete_x: Option<u32>,
    delete_y: Option<u32>,
) -> KittyDeleteSpec {
    match value {
        "a" => KittyDeleteSpec::All,
        "A" => KittyDeleteSpec::AllImages,
        "i" | "I" => KittyDeleteSpec::ById {
            id: cmd.image_id.unwrap_or(0),
            delete_data: value == "I",
        },
        "n" | "N" => KittyDeleteSpec::ByNumber {
            number: cmd.image_number.unwrap_or(0),
            delete_data: value == "N",
        },
        "c" | "C" => KittyDeleteSpec::AtCursor {
            delete_data: value == "C",
        },
        "x" | "X" => delete_x
            .filter(|column| *column != 0)
            .map(|column| KittyDeleteSpec::ByColumn {
                column,
                delete_data: value == "X",
            })
            .unwrap_or(KittyDeleteSpec::NoOp),
        "y" | "Y" => delete_y
            .filter(|row| *row != 0)
            .map(|row| KittyDeleteSpec::ByRow {
                row,
                delete_data: value == "Y",
            })
            .unwrap_or(KittyDeleteSpec::NoOp),
        "z" | "Z" => cmd
            .z_index
            .map(|z_index| KittyDeleteSpec::ByZIndex {
                z_index,
                delete_data: value == "Z",
            })
            .unwrap_or(KittyDeleteSpec::NoOp),
        _ => KittyDeleteSpec::NoOp,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_kitty_command, KittyDeleteSpec, MAX_PAYLOAD_BASE64_BYTES};

    fn delete_spec(input: &[u8]) -> KittyDeleteSpec {
        parse_kitty_command(input)
            .expect("delete command should parse")
            .delete_specifier
            .expect("delete selector should resolve")
    }

    #[test]
    fn enforces_the_kitty_encoded_chunk_limit() {
        let exact_payload = vec![b'A'; MAX_PAYLOAD_BASE64_BYTES];
        let mut exact = b"f=100,m=1;".to_vec();
        exact.extend_from_slice(&exact_payload);

        let command = parse_kitty_command(&exact).expect("4096-byte chunk should be accepted");
        assert_eq!(command.payload.len(), MAX_PAYLOAD_BASE64_BYTES / 4 * 3);

        let oversized_payload = vec![b'A'; MAX_PAYLOAD_BASE64_BYTES + 4];
        let mut oversized = b"f=100,m=1;".to_vec();
        oversized.extend_from_slice(&oversized_payload);
        assert!(parse_kitty_command(&oversized).is_none());
    }

    #[test]
    fn delete_id_selectors_are_order_independent() {
        for input in [b"a=d,d=i,i=10".as_slice(), b"a=d,i=10,d=i".as_slice()] {
            assert_eq!(
                delete_spec(input),
                KittyDeleteSpec::ById {
                    id: 10,
                    delete_data: false,
                }
            );
        }

        for input in [b"a=d,d=I,i=10".as_slice(), b"a=d,i=10,d=I".as_slice()] {
            assert_eq!(
                delete_spec(input),
                KittyDeleteSpec::ById {
                    id: 10,
                    delete_data: true,
                }
            );
        }
    }

    #[test]
    fn delete_number_selectors_are_order_independent() {
        for input in [b"a=d,d=n,I=20".as_slice(), b"a=d,I=20,d=n".as_slice()] {
            assert_eq!(
                delete_spec(input),
                KittyDeleteSpec::ByNumber {
                    number: 20,
                    delete_data: false,
                }
            );
        }

        for input in [b"a=d,d=N,I=20".as_slice(), b"a=d,I=20,d=N".as_slice()] {
            assert_eq!(
                delete_spec(input),
                KittyDeleteSpec::ByNumber {
                    number: 20,
                    delete_data: true,
                }
            );
        }
    }

    #[test]
    fn incomplete_or_unknown_delete_selectors_never_expand_to_all() {
        assert_eq!(
            delete_spec(b"a=d,d=i"),
            KittyDeleteSpec::ById {
                id: 0,
                delete_data: false,
            }
        );
        assert_eq!(
            delete_spec(b"a=d,d=n"),
            KittyDeleteSpec::ByNumber {
                number: 0,
                delete_data: false,
            }
        );
        for selector in ["unknown", "p", "P", "q", "Q", "r", "R", "f", "F"] {
            let command = format!("a=d,d={selector}");
            assert_eq!(delete_spec(command.as_bytes()), KittyDeleteSpec::NoOp);
        }
        for command in [b"a=d,d=x".as_slice(), b"a=d,d=y,y=0".as_slice()] {
            assert_eq!(delete_spec(command), KittyDeleteSpec::NoOp);
        }
    }

    #[test]
    fn invalid_delete_identifiers_become_safe_noop_events() {
        for command in [
            b"a=d,d=i,i=7,p=bad".as_slice(),
            b"a=d,p=bad".as_slice(),
            b"a=d,d=i,i=7,I=8".as_slice(),
            b"a=d,d=n,I=8,i=7".as_slice(),
            b"a=d,d=z".as_slice(),
            b"a=d,d=Z,z=bad".as_slice(),
        ] {
            assert_eq!(delete_spec(command), KittyDeleteSpec::NoOp);
        }

        let p_zero = parse_kitty_command(b"a=d,d=i,i=7,p=0")
            .expect("p=0 should parse as an unspecified placement ID");
        assert!(p_zero.placement_id.is_none());
        assert_eq!(
            p_zero.delete_specifier,
            Some(KittyDeleteSpec::ById {
                id: 7,
                delete_data: false,
            })
        );
    }

    #[test]
    fn invalid_image_selectors_are_preserved_for_every_action() {
        let actions = ["t", "T", "p", "q", "d", "f", "a", "c"];
        let invalid_selectors = [
            "i=7,I=8",
            "i=invalid",
            "I=invalid",
            "i=4294967296",
            "I=4294967296",
        ];

        for action in actions {
            for selector in invalid_selectors {
                let input = format!("a={action},{selector}");
                let command = parse_kitty_command(input.as_bytes())
                    .expect("invalid image selector should remain available to the handler");

                assert!(
                    command.invalid_image_selector,
                    "a={action} did not preserve invalid selector {selector}"
                );
                if action == "d" {
                    assert_eq!(command.delete_specifier, Some(KittyDeleteSpec::NoOp));
                }
            }
        }
    }

    #[test]
    fn valid_isolated_image_selectors_are_not_marked_invalid() {
        let actions = ["t", "T", "p", "q", "d", "f", "a", "c"];
        let valid_selectors = [
            ("i=0", Some(0), None),
            ("i=4294967295", Some(u32::MAX), None),
            ("I=0", None, Some(0)),
            ("I=4294967295", None, Some(u32::MAX)),
        ];

        for action in actions {
            for (selector, image_id, image_number) in valid_selectors {
                let input = format!("a={action},{selector}");
                let command = parse_kitty_command(input.as_bytes())
                    .expect("valid image selector should parse");

                assert!(
                    !command.invalid_image_selector,
                    "a={action} marked valid selector {selector} invalid"
                );
                assert_eq!(command.image_id, image_id);
                assert_eq!(command.image_number, image_number);
            }
        }
    }

    #[test]
    fn delete_capitalization_and_coordinates_are_preserved() {
        assert_eq!(delete_spec(b"a=d,d=a"), KittyDeleteSpec::All);
        assert_eq!(delete_spec(b"a=d,d=A"), KittyDeleteSpec::AllImages);
        assert_eq!(
            delete_spec(b"a=d,d=X,x=4,c=99"),
            KittyDeleteSpec::ByColumn {
                column: 4,
                delete_data: true,
            }
        );
        assert_eq!(
            delete_spec(b"a=d,y=5,d=y,r=88"),
            KittyDeleteSpec::ByRow {
                row: 5,
                delete_data: false,
            }
        );
        assert_eq!(
            delete_spec(b"a=d,d=Z,z=-3"),
            KittyDeleteSpec::ByZIndex {
                z_index: -3,
                delete_data: true,
            }
        );
    }
}
