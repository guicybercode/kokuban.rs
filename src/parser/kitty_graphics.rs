use base64::Engine;
use std::num::NonZeroU32;

// Released kitten clients emit up to 128 KiB of encoded data per APC. Keep a
// separate bounded allowance for control keys and the leading graphics marker.
pub(super) const MAX_PAYLOAD_BASE64_BYTES: usize = 128 * 1024;
pub(super) const MAX_KITTY_APC_BYTES: usize = MAX_PAYLOAD_BASE64_BYTES + 1024;
const PAYLOAD_ENGINE: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::general_purpose::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );

#[derive(Debug, Clone)]
pub struct KittyCommand {
    pub action: KittyAction,
    pub action_explicit: bool,
    pub animation: Option<KittyAnimationCommand>,
    pub invalid_animation_parameters: bool,
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
pub enum KittyAnimationCommand {
    Frame(KittyFrameUpload),
    Control(KittyAnimationControl),
    Compose(KittyFrameComposition),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KittyBlendMode {
    #[default]
    AlphaBlend,
    Overwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyAnimationState {
    Stopped,
    Loading,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyLoopCount {
    Infinite,
    Finite(NonZeroU32),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KittyFrameUpload {
    pub x: u32,
    pub y: u32,
    /// One-based existing frame to edit (`r`); otherwise append a frame.
    pub edit_frame: Option<NonZeroU32>,
    /// One-based frame to use as the canvas for an appended frame (`c`).
    pub base_frame: Option<NonZeroU32>,
    /// Zero is unspecified; negative values request a gapless frame.
    pub gap_ms: Option<i32>,
    pub background_rgba: u32,
    pub blend: KittyBlendMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KittyAnimationControl {
    pub state: Option<KittyAnimationState>,
    pub gap_frame: Option<NonZeroU32>,
    pub gap_ms: Option<i32>,
    pub current_frame: Option<NonZeroU32>,
    pub loops: Option<KittyLoopCount>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KittyFrameComposition {
    pub source_frame: u32,
    pub destination_frame: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub destination_x: u32,
    pub destination_y: u32,
    /// Unspecified dimensions use the full image canvas dimensions.
    pub width: Option<NonZeroU32>,
    pub height: Option<NonZeroU32>,
    pub blend: KittyBlendMode,
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
    Frame { frame: u32, delete_last_image: bool },
}

impl Default for KittyCommand {
    fn default() -> Self {
        Self {
            action: KittyAction::Transmit,
            action_explicit: false,
            animation: None,
            invalid_animation_parameters: false,
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
                cmd.action_explicit = true;
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
            // These rectangle dimensions are interpreted for composition below.
            "w" | "h" => {}
            _ => {
                log::trace!("Unknown kitty graphics key: {key}={value}");
            }
        }
    }

    cmd.invalid_image_selector =
        invalid_image_id || invalid_image_number || (image_id_key_seen && image_number_key_seen);

    if matches!(
        cmd.action,
        KittyAction::Frame | KittyAction::Animate | KittyAction::Compose
    ) {
        match parse_animation_parameters(control_str, cmd.action) {
            Ok(animation) => cmd.animation = Some(animation),
            Err(()) => cmd.invalid_animation_parameters = true,
        }
    } else if cmd.action == KittyAction::Delete && matches!(delete_selector, Some("f" | "F")) {
        // An invalid frame selector must never fall back to deleting frame one.
        cmd.invalid_animation_parameters = control_pairs(control_str)
            .any(|(key, value)| key == "r" && value.parse::<u32>().is_err());
    }

    let invalid_delete_command = cmd.action == KittyAction::Delete
        && (invalid_placement_id || cmd.invalid_image_selector || cmd.invalid_animation_parameters);

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
        match PAYLOAD_ENGINE.decode(payload_b64) {
            Ok(decoded) => cmd.payload = decoded,
            Err(e) => {
                log::warn!("Failed to decode kitty graphics payload: {e}");
                return None;
            }
        }
    }

    Some(cmd)
}

fn control_pairs(control_data: &str) -> impl Iterator<Item = (&str, &str)> {
    control_data
        .split(',')
        .filter_map(|pair| pair.split_once('='))
}

/// Resolve overloaded keys after the action is known, independent of key order.
fn parse_animation_parameters(
    control_data: &str,
    action: KittyAction,
) -> Result<KittyAnimationCommand, ()> {
    let mut frame = KittyFrameUpload::default();
    let mut control = KittyAnimationControl::default();
    let mut composition = KittyFrameComposition::default();
    let mut frame_blend_x = None;
    let mut frame_blend_c = None;

    for (key, value) in control_pairs(control_data) {
        match (action, key) {
            (_, "q") => match parse_animation_u32(value)? {
                0..=2 => {}
                _ => return Err(()),
            },
            (KittyAction::Frame, "s" | "v") => {
                parse_animation_u32(value)?;
            }
            (KittyAction::Frame, "f") if !matches!(value, "24" | "32" | "100") => {
                return Err(());
            }
            (KittyAction::Frame, "m") if !matches!(value, "0" | "1") => return Err(()),
            (KittyAction::Frame, "x") => frame.x = parse_animation_u32(value)?,
            (KittyAction::Frame, "y") => frame.y = parse_animation_u32(value)?,
            (KittyAction::Frame, "r") => {
                frame.edit_frame = NonZeroU32::new(parse_animation_u32(value)?);
            }
            (KittyAction::Frame, "c") => {
                frame.base_frame = NonZeroU32::new(parse_animation_u32(value)?);
            }
            (KittyAction::Frame, "z") => frame.gap_ms = parse_animation_gap(value)?,
            (KittyAction::Frame, "Y") => frame.background_rgba = parse_animation_u32(value)?,
            (KittyAction::Frame, "X") => frame_blend_x = Some(parse_animation_blend(value)?),
            (KittyAction::Frame, "C") => frame_blend_c = Some(parse_animation_blend(value)?),
            (KittyAction::Animate, "s") => {
                control.state = match parse_animation_u32(value)? {
                    0 => None,
                    1 => Some(KittyAnimationState::Stopped),
                    2 => Some(KittyAnimationState::Loading),
                    3 => Some(KittyAnimationState::Running),
                    _ => return Err(()),
                };
            }
            (KittyAction::Animate, "v") => {
                control.loops = match parse_animation_u32(value)? {
                    0 => None,
                    1 => Some(KittyLoopCount::Infinite),
                    loops => NonZeroU32::new(loops - 1).map(KittyLoopCount::Finite),
                };
            }
            (KittyAction::Animate, "r") => {
                control.gap_frame = NonZeroU32::new(parse_animation_u32(value)?);
            }
            (KittyAction::Animate, "c") => {
                control.current_frame = NonZeroU32::new(parse_animation_u32(value)?);
            }
            (KittyAction::Animate, "z") => control.gap_ms = parse_animation_gap(value)?,
            // The published table has contradictory frame/offset descriptions.
            // These mappings match Kitty's handle_compose_command implementation.
            (KittyAction::Compose, "r") => composition.source_frame = parse_animation_u32(value)?,
            (KittyAction::Compose, "c") => {
                composition.destination_frame = parse_animation_u32(value)?;
            }
            (KittyAction::Compose, "X") => composition.source_x = parse_animation_u32(value)?,
            (KittyAction::Compose, "Y") => composition.source_y = parse_animation_u32(value)?,
            (KittyAction::Compose, "x") => composition.destination_x = parse_animation_u32(value)?,
            (KittyAction::Compose, "y") => composition.destination_y = parse_animation_u32(value)?,
            (KittyAction::Compose, "w") => {
                composition.width = NonZeroU32::new(parse_animation_u32(value)?);
            }
            (KittyAction::Compose, "h") => {
                composition.height = NonZeroU32::new(parse_animation_u32(value)?);
            }
            (KittyAction::Compose, "C") => composition.blend = parse_animation_blend(value)?,
            _ => {}
        }
    }

    Ok(match action {
        KittyAction::Frame => {
            // The protocol documents X, while released icat clients use C.
            frame.blend = frame_blend_c.or(frame_blend_x).unwrap_or_default();
            KittyAnimationCommand::Frame(frame)
        }
        KittyAction::Animate => KittyAnimationCommand::Control(control),
        KittyAction::Compose => KittyAnimationCommand::Compose(composition),
        _ => return Err(()),
    })
}

fn parse_animation_u32(value: &str) -> Result<u32, ()> {
    value.parse().map_err(|_| ())
}

fn parse_animation_gap(value: &str) -> Result<Option<i32>, ()> {
    let gap = value.parse::<i32>().map_err(|_| ())?;
    Ok((gap != 0).then_some(gap))
}

fn parse_animation_blend(value: &str) -> Result<KittyBlendMode, ()> {
    match parse_animation_u32(value)? {
        0 => Ok(KittyBlendMode::AlphaBlend),
        1 => Ok(KittyBlendMode::Overwrite),
        _ => Err(()),
    }
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
        "f" | "F" => KittyDeleteSpec::Frame {
            frame: cmd.rows.unwrap_or(0),
            delete_last_image: value == "F",
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
    use super::{
        parse_kitty_command, KittyAction, KittyAnimationCommand, KittyAnimationControl,
        KittyAnimationState, KittyBlendMode, KittyDeleteSpec, KittyFrameComposition,
        KittyFrameUpload, KittyLoopCount, MAX_PAYLOAD_BASE64_BYTES,
    };
    use std::num::NonZeroU32;

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

        let command = parse_kitty_command(&exact).expect("128 KiB chunk should be accepted");
        assert_eq!(command.payload.len(), MAX_PAYLOAD_BASE64_BYTES / 4 * 3);

        let oversized_payload = vec![b'A'; MAX_PAYLOAD_BASE64_BYTES + 4];
        let mut oversized = b"f=100,m=1;".to_vec();
        oversized.extend_from_slice(&oversized_payload);
        assert!(parse_kitty_command(&oversized).is_none());
    }

    #[test]
    fn accepts_padded_and_unpadded_payloads_but_rejects_invalid_base64() {
        for payload in ["AQ==", "AQ", "AQI=", "AQI", "AQID"] {
            let command = parse_kitty_command(format!("i=9;{payload}").as_bytes()).unwrap();
            assert_eq!(command.payload, [1, 2, 3][..command.payload.len()]);
        }
        for payload in ["AR", "A", "AQ$", "AQ==extra", "-_8"] {
            assert!(parse_kitty_command(format!("i=9;{payload}").as_bytes()).is_none());
        }
    }

    #[test]
    fn distinguishes_omitted_action_chunks_from_explicit_transmission() {
        let omitted = parse_kitty_command(b"m=0;AQ").unwrap();
        let explicit = parse_kitty_command(b"a=t,m=0;AQ").unwrap();
        assert_eq!(omitted.action, KittyAction::Transmit);
        assert_eq!(explicit.action, KittyAction::Transmit);
        assert!(!omitted.action_explicit);
        assert!(explicit.action_explicit);
        let continuation = parse_kitty_command(b"a=f,q=2,m=1;AQID").unwrap();
        assert!(continuation.action_explicit);
        assert_eq!(continuation.image_id, None);
        assert_eq!(continuation.image_number, None);
        assert_eq!(continuation.payload, [1, 2, 3]);
        assert_eq!(
            continuation.animation,
            Some(KittyAnimationCommand::Frame(KittyFrameUpload::default()))
        );
    }

    #[test]
    fn parses_frame_rectangle_base_and_edit_after_final_action_is_known() {
        let keys = "I=83,s=100,v=200,x=10,y=5,c=1,r=2,z=-1,Y=4278190335,X=1";
        let expected = Some(KittyAnimationCommand::Frame(KittyFrameUpload {
            x: 10,
            y: 5,
            edit_frame: NonZeroU32::new(2),
            base_frame: NonZeroU32::new(1),
            gap_ms: Some(-1),
            background_rgba: 0xff0000ff,
            blend: KittyBlendMode::Overwrite,
        }));
        for input in [format!("a=f,{keys}"), format!("{keys},a=f")] {
            let command = parse_kitty_command(input.as_bytes()).unwrap();
            assert_eq!(command.animation, expected);
            assert_eq!((command.width, command.height), (Some(100), Some(200)));
            assert!(!command.invalid_animation_parameters);
        }
    }

    #[test]
    fn released_icat_composition_key_takes_precedence_for_frame_uploads() {
        for keys in ["X=1,C=0", "C=0,X=1"] {
            let input = format!("a=f,i=4,{keys}");
            let command = parse_kitty_command(input.as_bytes()).unwrap();
            assert_eq!(
                command.animation,
                Some(KittyAnimationCommand::Frame(KittyFrameUpload::default()))
            );
        }
        for keys in ["C=1", "X=1", "C=1,X=0", "X=0,C=1"] {
            let input = format!("a=f,i=4,{keys}");
            let command = parse_kitty_command(input.as_bytes()).unwrap();
            assert_eq!(
                command.animation,
                Some(KittyAnimationCommand::Frame(KittyFrameUpload {
                    blend: KittyBlendMode::Overwrite,
                    ..KittyFrameUpload::default()
                }))
            );
        }
    }

    #[test]
    fn normalizes_zero_animation_keys_without_assigning_runtime_defaults() {
        for input in ["a=f,i=1", "a=f,i=1,r=0,c=0,z=0,Y=0,X=0"] {
            let command = parse_kitty_command(input.as_bytes()).unwrap();
            assert_eq!(
                command.animation,
                Some(KittyAnimationCommand::Frame(KittyFrameUpload::default()))
            );
        }
        for input in ["a=a,i=1", "a=a,i=1,s=0,v=0,r=0,c=0,z=0"] {
            let command = parse_kitty_command(input.as_bytes()).unwrap();
            assert_eq!(
                command.animation,
                Some(KittyAnimationCommand::Control(
                    KittyAnimationControl::default()
                ))
            );
        }
        let command = parse_kitty_command(b"a=c,i=1,w=0,h=0").unwrap();
        assert_eq!(
            command.animation,
            Some(KittyAnimationCommand::Compose(
                KittyFrameComposition::default()
            ))
        );
    }

    #[test]
    fn parses_loading_running_stop_and_wire_loop_count() {
        for (wire, state) in [
            (1, KittyAnimationState::Stopped),
            (2, KittyAnimationState::Loading),
            (3, KittyAnimationState::Running),
        ] {
            let input = format!("I=91,r=3,z=48,c=7,s={wire},v=4,a=a");
            let command = parse_kitty_command(input.as_bytes()).unwrap();
            assert_eq!(
                command.animation,
                Some(KittyAnimationCommand::Control(KittyAnimationControl {
                    state: Some(state),
                    gap_frame: NonZeroU32::new(3),
                    gap_ms: Some(48),
                    current_frame: NonZeroU32::new(7),
                    loops: NonZeroU32::new(3).map(KittyLoopCount::Finite),
                }))
            );
        }
        for (wire, loops) in [
            (1, KittyLoopCount::Infinite),
            (2, KittyLoopCount::Finite(NonZeroU32::new(1).unwrap())),
            (
                u32::MAX,
                KittyLoopCount::Finite(NonZeroU32::new(u32::MAX - 1).unwrap()),
            ),
        ] {
            let input = format!("a=a,i=1,v={wire}");
            assert_eq!(
                parse_kitty_command(input.as_bytes()).unwrap().animation,
                Some(KittyAnimationCommand::Control(KittyAnimationControl {
                    loops: Some(loops),
                    ..KittyAnimationControl::default()
                }))
            );
        }
    }

    #[test]
    fn composition_maps_asymmetric_rectangles_to_released_kitty_behavior() {
        let keys = "i=1,r=7,c=9,w=23,h=27,X=4,Y=8,x=1,y=3,C=1";
        for input in [format!("a=c,{keys}"), format!("{keys},a=c")] {
            let command = parse_kitty_command(input.as_bytes()).unwrap();
            assert_eq!(
                command.animation,
                Some(KittyAnimationCommand::Compose(KittyFrameComposition {
                    source_frame: 7,
                    destination_frame: 9,
                    source_x: 4,
                    source_y: 8,
                    destination_x: 1,
                    destination_y: 3,
                    width: NonZeroU32::new(23),
                    height: NonZeroU32::new(27),
                    blend: KittyBlendMode::Overwrite,
                }))
            );
        }
    }

    #[test]
    fn malformed_animation_numbers_never_become_default_operations() {
        for input in [
            "a=f,i=1,s=bad",
            "a=f,i=1,v=-1",
            "a=f,i=1,x=4294967296",
            "a=f,i=1,y=bad",
            "a=f,i=1,c=-1",
            "a=f,i=1,r=bad",
            "a=f,i=1,Y=4294967296",
            "a=f,i=1,X=2",
            "a=f,i=1,C=2",
            "a=f,i=1,X=2,C=0",
            "a=f,i=1,C=0,X=2",
            "a=f,i=1,f=bad",
            "a=f,i=1,m=2",
            "a=f,i=1,q=bad",
            "a=f,i=1,z=2147483648",
            "a=f,i=1,z=-2147483649",
            "a=f,i=1,x=bad,x=1",
            "a=a,i=1,s=4",
            "a=a,i=1,s=bad",
            "a=a,i=1,v=4294967296",
            "a=a,i=1,r=bad",
            "a=a,i=1,c=-1",
            "a=a,i=1,z=bad",
            "a=c,i=1,r=bad",
            "a=c,i=1,c=-1",
            "a=c,i=1,w=bad",
            "a=c,i=1,h=-1",
            "a=c,i=1,x=bad",
            "a=c,i=1,y=-1",
            "a=c,i=1,X=4294967296",
            "a=c,i=1,Y=bad",
            "a=c,i=1,C=256",
        ] {
            let command = parse_kitty_command(input.as_bytes()).unwrap();
            assert!(command.invalid_animation_parameters, "accepted {input}");
            assert!(command.animation.is_none(), "defaulted {input}");
        }
    }

    #[test]
    fn overloaded_keys_are_validated_in_their_final_action_namespace() {
        let frame = parse_kitty_command(b"s=4,Y=4294967295,a=f,I=2").unwrap();
        assert!(!frame.invalid_animation_parameters);
        let composition = parse_kitty_command(b"X=400,Y=500,a=c,i=2").unwrap();
        assert!(!composition.invalid_animation_parameters);
        let placement = parse_kitty_command(b"a=p,i=2,X=400,s=4").unwrap();
        assert!(!placement.invalid_animation_parameters);
        assert!(placement.animation.is_none());
        for action in ["f", "a", "c"] {
            let input = format!("a={action},i=3,I=4,r=bad");
            let command = parse_kitty_command(input.as_bytes()).unwrap();
            assert!(command.invalid_image_selector);
            assert!(command.invalid_animation_parameters);
        }
    }

    #[test]
    fn frame_delete_selectors_preserve_root_default_and_hard_delete_policy() {
        for (input, frame, delete_last_image) in [
            ("a=d,d=f,i=4", 0, false),
            ("a=d,d=F,I=4,r=0", 0, true),
            ("a=d,d=f,i=4,r=2", 2, false),
            ("r=4294967295,I=4,d=F,a=d", u32::MAX, true),
        ] {
            assert_eq!(
                delete_spec(input.as_bytes()),
                KittyDeleteSpec::Frame {
                    frame,
                    delete_last_image
                }
            );
        }
        for input in [
            "a=d,d=f,i=4,r=bad",
            "a=d,d=F,I=4,r=-1",
            "a=d,d=f,i=4,r=4294967296",
            "a=d,d=F,I=4,r=bad,r=1",
            "a=d,d=f,i=4,I=5,r=1",
        ] {
            assert_eq!(delete_spec(input.as_bytes()), KittyDeleteSpec::NoOp);
        }
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
        for selector in ["unknown", "p", "P", "q", "Q", "r", "R"] {
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
