use super::image_store::{probe_image_data, ImageFormat, ImageId, ImageStore};
use crate::parser::kitty_graphics::*;
use nix::libc;
use std::borrow::Cow;
use std::ffi::{CString, OsStr};
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

const MAX_PENDING_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const DECOMPRESSION_CHUNK_BYTES: usize = 32 * 1024;
const FILE_READ_CHUNK_BYTES: usize = 32 * 1024;
const BYTES_PER_MEBIBYTE: usize = 1024 * 1024;
const TEMP_FILE_MARKER: &str = "tty-graphics-protocol";

#[derive(Debug, Clone, Copy)]
pub struct KittyHandlerOptions {
    max_image_bytes: usize,
    allow_file_transfer: bool,
}

impl KittyHandlerOptions {
    pub fn from_megabytes(max_image_size_mb: usize, allow_file_transfer: bool) -> Self {
        let configured_bytes = max_image_size_mb.saturating_mul(BYTES_PER_MEBIBYTE);
        Self {
            max_image_bytes: configured_bytes.min(MAX_PENDING_IMAGE_BYTES),
            allow_file_transfer,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImagePlacement {
    pub image_id: ImageId,
    pub placement_id: u32,
    pub mode: PlacementMode,
    pub z_index: i32,
}

#[derive(Debug, Clone)]
pub enum PlacementMode {
    Inline {
        row: usize,
        col: usize,
        cols: u32,
        rows: u32,
    },
    Overlay {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}

#[derive(Debug)]
struct PendingImage {
    image_id: ImageId,
    data: Vec<u8>,
    metadata: KittyCommand,
    place_after: Option<KittyCommand>,
}

#[derive(Debug)]
struct CompletedImage {
    image_id: ImageId,
    data: Vec<u8>,
    metadata: KittyCommand,
    place_after: Option<KittyCommand>,
}

#[derive(Debug, PartialEq, Eq)]
enum ChunkAssemblyErrorKind {
    Interleaved { received_id: ImageId },
    UnexpectedStart,
    TooLarge,
    AllocationFailed,
}

#[derive(Debug, PartialEq, Eq)]
struct ChunkAssemblyError {
    image_id: ImageId,
    quiet: u8,
    kind: ChunkAssemblyErrorKind,
}

struct ChunkAssembler {
    pending: Option<PendingImage>,
    max_bytes: usize,
}

impl ChunkAssembler {
    fn new(max_bytes: usize) -> Self {
        Self {
            pending: None,
            max_bytes,
        }
    }

    fn push<F>(
        &mut self,
        mut cmd: KittyCommand,
        will_place: bool,
        mut next_image_id: F,
    ) -> Result<Option<CompletedImage>, ChunkAssemblyError>
    where
        F: FnMut() -> ImageId,
    {
        if let Some(pending) = self.pending.as_mut() {
            if cmd.quiet != 0 {
                pending.metadata.quiet = cmd.quiet;
            }
        }

        if let Some(pending) = self.pending.as_ref() {
            if will_place {
                let error = ChunkAssemblyError {
                    image_id: pending.image_id,
                    quiet: pending.metadata.quiet,
                    kind: ChunkAssemblyErrorKind::UnexpectedStart,
                };
                self.pending = None;
                return Err(error);
            }
            if let Some(received_id) = cmd.image_id {
                if received_id != pending.image_id {
                    let error = ChunkAssemblyError {
                        image_id: pending.image_id,
                        quiet: pending.metadata.quiet,
                        kind: ChunkAssemblyErrorKind::Interleaved { received_id },
                    };
                    self.pending = None;
                    return Err(error);
                }
            }

            let payload = std::mem::take(&mut cmd.payload);
            let pending = self.pending.as_mut().expect("pending image disappeared");
            if let Err(kind) = append_bounded(&mut pending.data, &payload, self.max_bytes) {
                let error = ChunkAssemblyError {
                    image_id: pending.image_id,
                    quiet: pending.metadata.quiet,
                    kind,
                };
                self.pending = None;
                return Err(error);
            }

            if cmd.more_chunks {
                return Ok(None);
            }

            let mut pending = self.pending.take().expect("pending image disappeared");
            // Some clients repeat dimensions only on the final chunk. Retain every
            // value supplied by the first chunk, filling only missing dimensions.
            if pending.metadata.width.is_none() {
                pending.metadata.width = cmd.width;
            }
            if pending.metadata.height.is_none() {
                pending.metadata.height = cmd.height;
            }
            return Ok(Some(CompletedImage {
                image_id: pending.image_id,
                data: pending.data,
                metadata: pending.metadata,
                place_after: pending.place_after,
            }));
        }

        let image_id = cmd.image_id.unwrap_or_else(&mut next_image_id);
        let quiet = cmd.quiet;
        let payload = std::mem::take(&mut cmd.payload);
        let mut data = Vec::new();
        if let Err(kind) = append_bounded(&mut data, &payload, self.max_bytes) {
            return Err(ChunkAssemblyError {
                image_id,
                quiet,
                kind,
            });
        }
        let place_after = if will_place { Some(cmd.clone()) } else { None };

        if cmd.more_chunks {
            self.pending = Some(PendingImage {
                image_id,
                data,
                metadata: cmd,
                place_after,
            });
            Ok(None)
        } else {
            Ok(Some(CompletedImage {
                image_id,
                data,
                metadata: cmd,
                place_after,
            }))
        }
    }

    fn abort(&mut self) -> bool {
        self.pending.take().is_some()
    }
}

fn append_bounded(
    destination: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
) -> Result<(), ChunkAssemblyErrorKind> {
    if destination.capacity() > max_bytes {
        return Err(ChunkAssemblyErrorKind::TooLarge);
    }

    let new_len = destination
        .len()
        .checked_add(chunk.len())
        .ok_or(ChunkAssemblyErrorKind::TooLarge)?;
    if new_len > max_bytes {
        return Err(ChunkAssemblyErrorKind::TooLarge);
    }

    if new_len > destination.capacity() {
        let doubled_capacity = destination.capacity().checked_mul(2).unwrap_or(max_bytes);
        let target_capacity = new_len.max(doubled_capacity).min(max_bytes);
        let additional = target_capacity
            .checked_sub(destination.len())
            .ok_or(ChunkAssemblyErrorKind::TooLarge)?;
        destination
            .try_reserve_exact(additional)
            .map_err(|_| ChunkAssemblyErrorKind::AllocationFailed)?;
        if destination.capacity() > max_bytes {
            return Err(ChunkAssemblyErrorKind::TooLarge);
        }
    }

    destination.extend_from_slice(chunk);
    Ok(())
}

struct StoredTransmission {
    image_id: ImageId,
    place_after: Option<KittyCommand>,
}

struct TransmitOutcome {
    response: Option<Vec<u8>>,
    stored: Option<StoredTransmission>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecompressionError {
    TooLarge,
    InvalidData,
    AllocationFailed,
}

pub struct KittyHandler {
    chunks: ChunkAssembler,
    next_placement_id: u32,
    options: KittyHandlerOptions,
}

impl KittyHandler {
    pub fn new(options: KittyHandlerOptions) -> Self {
        Self {
            chunks: ChunkAssembler::new(options.max_image_bytes),
            next_placement_id: 1,
            options,
        }
    }

    /// Process a parsed Kitty graphics command.
    /// Returns an optional response to send back to the PTY,
    /// and optional placement(s) to add.
    pub fn process(
        &mut self,
        cmd: KittyCommand,
        store: &mut ImageStore,
        cursor_row: usize,
        cursor_col: usize,
        cell_width: f32,
        cell_height: f32,
        grid_cols: usize,
        grid_rows: usize,
        placements: &mut Vec<ImagePlacement>,
    ) -> (Option<Vec<u8>>, Option<CursorAdvance>) {
        if !matches!(
            cmd.action,
            KittyAction::Transmit | KittyAction::TransmitAndPlace
        ) && self.chunks.abort()
        {
            log::warn!("Aborted pending Kitty transmission on non-transmit command");
        }

        match cmd.action {
            KittyAction::Query => {
                return self.handle_query(&cmd);
            }
            KittyAction::Transmit | KittyAction::TransmitAndPlace => {
                let will_place = cmd.action == KittyAction::TransmitAndPlace;
                let outcome = self.handle_transmit(cmd, store, will_place);
                let advance = outcome.stored.and_then(|stored| {
                    stored.place_after.and_then(|place_cmd| {
                        self.create_placement(
                            &place_cmd,
                            stored.image_id,
                            store,
                            cursor_row,
                            cursor_col,
                            cell_width,
                            cell_height,
                            grid_cols,
                            grid_rows,
                            placements,
                        )
                    })
                });
                return (outcome.response, advance);
            }
            KittyAction::Place => {
                let image_id = cmd.image_id.unwrap_or(0);
                if store.contains(image_id) {
                    let advance = self.create_placement(
                        &cmd,
                        image_id,
                        store,
                        cursor_row,
                        cursor_col,
                        cell_width,
                        cell_height,
                        grid_cols,
                        grid_rows,
                        placements,
                    );
                    let resp = if cmd.quiet < 1 {
                        Some(format!("\x1b_Gi={image_id};OK\x1b\\").into_bytes())
                    } else {
                        None
                    };
                    return (resp, advance);
                } else {
                    let resp = if cmd.quiet < 2 {
                        Some(
                            format!("\x1b_Gi={image_id};ENOENT:image not found\x1b\\").into_bytes(),
                        )
                    } else {
                        None
                    };
                    return (resp, None);
                }
            }
            KittyAction::Delete => {
                self.handle_delete(&cmd, store, placements);
                return (None, None);
            }
            KittyAction::Frame | KittyAction::Animate | KittyAction::Compose => {
                // Out of scope
                return (None, None);
            }
        }
    }

    fn handle_query(&self, cmd: &KittyCommand) -> (Option<Vec<u8>>, Option<CursorAdvance>) {
        let image_id = cmd.image_id.unwrap_or(0);
        // For queries, we try to process the tiny test image and respond OK
        if !cmd.payload.is_empty() || cmd.transmission != KittyTransmission::Direct {
            let format = match cmd.format {
                KittyFormat::Rgb => ImageFormat::Rgb,
                KittyFormat::Rgba => ImageFormat::Rgba,
                KittyFormat::Png => ImageFormat::Png,
            };
            let w = cmd.width.unwrap_or(1);
            let h = cmd.height.unwrap_or(1);
            let transmission_data = match cmd.transmission {
                KittyTransmission::Direct => Cow::Borrowed(cmd.payload.as_slice()),
                KittyTransmission::File | KittyTransmission::TempFile => {
                    match load_file_data(
                        &cmd.payload,
                        cmd.transmission == KittyTransmission::TempFile,
                        self.options,
                    ) {
                        Ok(data) => Cow::Owned(data),
                        Err(error) => {
                            return (file_load_error_response(image_id, cmd.quiet, error), None);
                        }
                    }
                }
                KittyTransmission::SharedMemory => {
                    let response = (cmd.quiet < 2).then(|| {
                        format!("\x1b_Gi={image_id};ENOSYS:shared memory not supported\x1b\\")
                            .into_bytes()
                    });
                    return (response, None);
                }
            };
            // Store the test image briefly. A failed decompression must not be
            // interpreted as raw pixels: doing so would make corrupt streams
            // appear supported and could retain attacker-controlled data.
            let data = match maybe_decompress_with_limit(
                transmission_data.as_ref(),
                cmd.compression,
                self.options.max_image_bytes,
            ) {
                Ok(data) => data,
                Err(error) => {
                    return (
                        decompression_error_response(image_id, cmd.quiet, error),
                        None,
                    );
                }
            };
            if !probe_image_data(data.as_ref(), w, h, format, self.options.max_image_bytes) {
                let response = if cmd.quiet < 2 {
                    Some(
                        format!("\x1b_Gi={image_id};ENOMEM:failed to load query image\x1b\\")
                            .into_bytes(),
                    )
                } else {
                    None
                };
                return (response, None);
            }
        }
        let resp = if cmd.quiet < 1 {
            Some(format!("\x1b_Gi={image_id};OK\x1b\\").into_bytes())
        } else {
            None
        };
        (resp, None)
    }

    fn handle_transmit(
        &mut self,
        cmd: KittyCommand,
        store: &mut ImageStore,
        will_place: bool,
    ) -> TransmitOutcome {
        let completed = match self.chunks.push(cmd, will_place, || store.next_id()) {
            Ok(None) => {
                return TransmitOutcome {
                    response: None,
                    stored: None,
                };
            }
            Ok(Some(completed)) => completed,
            Err(error) => {
                let reason = match error.kind {
                    ChunkAssemblyErrorKind::Interleaved { received_id } => {
                        format!("EINVAL:interleaved transmission i={received_id}")
                    }
                    ChunkAssemblyErrorKind::UnexpectedStart => {
                        "EINVAL:new transmission before previous completed".to_owned()
                    }
                    ChunkAssemblyErrorKind::TooLarge => {
                        "E2BIG:transmission exceeds pending image limit".to_owned()
                    }
                    ChunkAssemblyErrorKind::AllocationFailed => {
                        "ENOMEM:failed to buffer transmission".to_owned()
                    }
                };
                log::warn!(
                    "Rejected Kitty transmission for image {}: {reason}",
                    error.image_id
                );
                let response = if error.quiet < 2 {
                    Some(format!("\x1b_Gi={};{reason}\x1b\\", error.image_id).into_bytes())
                } else {
                    None
                };
                return TransmitOutcome {
                    response,
                    stored: None,
                };
            }
        };

        let CompletedImage {
            image_id,
            data: full_data,
            metadata,
            place_after,
        } = completed;
        let KittyCommand {
            quiet,
            format,
            width,
            height,
            compression,
            transmission,
            ..
        } = metadata;

        // Handle file transmission
        let image_data = match transmission {
            KittyTransmission::File | KittyTransmission::TempFile => {
                match load_file_data(
                    &full_data,
                    transmission == KittyTransmission::TempFile,
                    self.options,
                ) {
                    Ok(data) => data,
                    Err(error) => {
                        return TransmitOutcome {
                            response: file_load_error_response(image_id, quiet, error),
                            stored: None,
                        };
                    }
                }
            }
            KittyTransmission::SharedMemory => {
                let resp = if quiet < 2 {
                    Some(
                        format!("\x1b_Gi={image_id};ENOSYS:shared memory not supported\x1b\\")
                            .into_bytes(),
                    )
                } else {
                    None
                };
                return TransmitOutcome {
                    response: resp,
                    stored: None,
                };
            }
            KittyTransmission::Direct => full_data,
        };

        // Decompress if needed. Invalid or oversized streams fail closed and
        // never reach image storage or deferred placement.
        let image_data = match maybe_decompress_with_limit(
            &image_data,
            compression,
            self.options.max_image_bytes,
        ) {
            Ok(data) => data,
            Err(error) => {
                return TransmitOutcome {
                    response: decompression_error_response(image_id, quiet, error),
                    stored: None,
                };
            }
        };

        // Determine dimensions for PNG
        let (w, h, img_format) = match format {
            KittyFormat::Png => {
                // For PNG, width/height come from the PNG header
                (0, 0, ImageFormat::Png)
            }
            KittyFormat::Rgb => {
                let w = width.unwrap_or(0);
                let h = height.unwrap_or(0);
                if w == 0 || h == 0 {
                    let resp = if quiet < 2 {
                        Some(
                            format!("\x1b_Gi={image_id};EINVAL:missing dimensions\x1b\\")
                                .into_bytes(),
                        )
                    } else {
                        None
                    };
                    return TransmitOutcome {
                        response: resp,
                        stored: None,
                    };
                }
                (w, h, ImageFormat::Rgb)
            }
            KittyFormat::Rgba => {
                let w = width.unwrap_or(0);
                let h = height.unwrap_or(0);
                if w == 0 || h == 0 {
                    let resp = if quiet < 2 {
                        Some(
                            format!("\x1b_Gi={image_id};EINVAL:missing dimensions\x1b\\")
                                .into_bytes(),
                        )
                    } else {
                        None
                    };
                    return TransmitOutcome {
                        response: resp,
                        stored: None,
                    };
                }
                (w, h, ImageFormat::Rgba)
            }
        };

        match store.store(image_data.as_ref(), w, h, img_format, Some(image_id)) {
            Some(_) => {
                let resp = if quiet < 1 {
                    Some(format!("\x1b_Gi={image_id};OK\x1b\\").into_bytes())
                } else {
                    None
                };
                TransmitOutcome {
                    response: resp,
                    stored: Some(StoredTransmission {
                        image_id,
                        place_after,
                    }),
                }
            }
            None => {
                let resp = if quiet < 2 {
                    Some(
                        format!("\x1b_Gi={image_id};ENOMEM:failed to store image\x1b\\")
                            .into_bytes(),
                    )
                } else {
                    None
                };
                TransmitOutcome {
                    response: resp,
                    stored: None,
                }
            }
        }
    }

    fn create_placement(
        &mut self,
        cmd: &KittyCommand,
        image_id: ImageId,
        store: &ImageStore,
        cursor_row: usize,
        cursor_col: usize,
        cell_width: f32,
        cell_height: f32,
        grid_cols: usize,
        grid_rows: usize,
        placements: &mut Vec<ImagePlacement>,
    ) -> Option<CursorAdvance> {
        let img = store.get(image_id)?;
        let placement_id = cmd.placement_id.unwrap_or_else(|| {
            let id = self.next_placement_id;
            self.next_placement_id = self.next_placement_id.wrapping_add(1).max(1);
            id
        });

        // Calculate display dimensions in cells
        let display_cols = cmd
            .columns
            .unwrap_or_else(|| ((img.width as f32) / cell_width).ceil() as u32);
        let display_rows = cmd
            .rows
            .unwrap_or_else(|| ((img.height as f32) / cell_height).ceil() as u32);

        let z_index = cmd.z_index.unwrap_or(0);
        let cursor_movement = cmd.cursor_movement.unwrap_or(0);

        let (mode, cursor_advance) = if cursor_movement == 1 {
            // Don't move cursor → overlay mode
            let x = cursor_col as f32 * cell_width + cmd.x_offset.unwrap_or(0) as f32;
            let y = cursor_row as f32 * cell_height + cmd.y_offset.unwrap_or(0) as f32;
            (
                PlacementMode::Overlay {
                    x,
                    y,
                    width: display_cols as f32 * cell_width,
                    height: display_rows as f32 * cell_height,
                },
                None,
            )
        } else {
            let (mode, advance) = inline_placement_and_advance(
                cursor_row,
                cursor_col,
                display_cols,
                display_rows,
                grid_cols,
                grid_rows,
            );
            (mode, Some(advance))
        };

        placements.push(ImagePlacement {
            image_id,
            placement_id,
            mode: mode.clone(),
            z_index,
        });

        cursor_advance
    }

    fn handle_delete(
        &self,
        cmd: &KittyCommand,
        _store: &mut ImageStore,
        placements: &mut Vec<ImagePlacement>,
    ) {
        let spec = cmd.delete_specifier.unwrap_or(KittyDeleteSpec::All);

        match spec {
            KittyDeleteSpec::All => {
                placements.clear();
            }
            KittyDeleteSpec::ById(id) => {
                placements.retain(|p| p.image_id != id);
                // Optionally also remove the placement_id if specified
                if let Some(pid) = cmd.placement_id {
                    placements.retain(|p| !(p.image_id == id && p.placement_id == pid));
                }
            }
            KittyDeleteSpec::AllImages => {
                // Delete all images AND placements
                placements.clear();
                // Note: we can't easily iterate and remove all from store here
                // since we don't have the full list. The store IDs are tracked internally.
                // For now, just clear placements. Images will be evicted by LRU.
            }
            KittyDeleteSpec::ByNumber(num) => {
                // Delete by image number (I=) — treat as image ID
                placements.retain(|p| p.image_id != num);
            }
            KittyDeleteSpec::AtCursor => {
                // Delete placements at cursor position - caller would need to provide cursor pos
                // For simplicity, delete all placements (Yazi sends specific d=i commands)
            }
            KittyDeleteSpec::ByColumn(col) => {
                placements.retain(|p| match &p.mode {
                    PlacementMode::Inline { col: c, .. } => *c as u32 != col,
                    _ => true,
                });
            }
            KittyDeleteSpec::ByRow(row) => {
                placements.retain(|p| match &p.mode {
                    PlacementMode::Inline { row: r, .. } => *r as u32 != row,
                    _ => true,
                });
            }
            KittyDeleteSpec::ByZIndex(z) => {
                placements.retain(|p| p.z_index != z);
            }
        }
    }
}

/// How much to advance the cursor after placing an inline image.
pub struct CursorAdvance {
    pub rows: usize,
    pub cols: usize,
}

fn bounded_inline_dimensions(
    display_cols: u32,
    display_rows: u32,
    grid_cols: usize,
    grid_rows: usize,
) -> (u32, u32) {
    let max_cols = u32::try_from(grid_cols).unwrap_or(u32::MAX);
    let max_rows = u32::try_from(grid_rows).unwrap_or(u32::MAX);
    (display_cols.min(max_cols), display_rows.min(max_rows))
}

fn inline_placement_and_advance(
    row: usize,
    col: usize,
    display_cols: u32,
    display_rows: u32,
    grid_cols: usize,
    grid_rows: usize,
) -> (PlacementMode, CursorAdvance) {
    let (cols, rows) = bounded_inline_dimensions(display_cols, display_rows, grid_cols, grid_rows);
    (
        PlacementMode::Inline {
            row,
            col,
            cols,
            rows,
        },
        CursorAdvance {
            rows: rows as usize,
            cols: cols as usize,
        },
    )
}

fn maybe_decompress_with_limit(
    data: &[u8],
    compression: KittyCompression,
    max_bytes: usize,
) -> Result<Cow<'_, [u8]>, DecompressionError> {
    match compression {
        KittyCompression::None => {
            if data.len() > max_bytes {
                Err(DecompressionError::TooLarge)
            } else {
                Ok(Cow::Borrowed(data))
            }
        }
        KittyCompression::Zlib => {
            use flate2::{Decompress, FlushDecompress, Status};

            let mut decoder = Decompress::new(true);
            let mut decompressed = Vec::new();
            let mut input_offset = 0usize;
            let mut output_chunk = [0u8; DECOMPRESSION_CHUNK_BYTES];

            loop {
                let remaining = max_bytes
                    .checked_sub(decompressed.len())
                    .ok_or(DecompressionError::TooLarge)?;
                // Always leave room to observe one byte beyond the budget.
                // That byte is rejected before it can be appended.
                let output_limit = remaining.saturating_add(1).min(DECOMPRESSION_CHUNK_BYTES);
                let before_in = decoder.total_in();
                let before_out = decoder.total_out();
                let status = decoder
                    .decompress(
                        data.get(input_offset..)
                            .ok_or(DecompressionError::InvalidData)?,
                        &mut output_chunk[..output_limit],
                        FlushDecompress::None,
                    )
                    .map_err(|error| {
                        log::warn!("Failed to decompress zlib data: {error}");
                        DecompressionError::InvalidData
                    })?;
                let consumed = usize::try_from(decoder.total_in() - before_in)
                    .map_err(|_| DecompressionError::InvalidData)?;
                let produced = usize::try_from(decoder.total_out() - before_out)
                    .map_err(|_| DecompressionError::TooLarge)?;
                input_offset = input_offset
                    .checked_add(consumed)
                    .ok_or(DecompressionError::InvalidData)?;

                append_decompressed(&mut decompressed, &output_chunk[..produced], max_bytes)?;

                match status {
                    Status::StreamEnd => {
                        if input_offset != data.len() {
                            return Err(DecompressionError::InvalidData);
                        }
                        return Ok(Cow::Owned(decompressed));
                    }
                    Status::Ok | Status::BufError => {
                        if consumed == 0 && produced == 0 {
                            // No StreamEnd and no possible progress means the
                            // zlib wrapper or checksum was truncated.
                            return Err(DecompressionError::InvalidData);
                        }
                    }
                }
            }
        }
    }
}

fn append_decompressed(
    destination: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
) -> Result<(), DecompressionError> {
    if destination.capacity() > max_bytes {
        return Err(DecompressionError::TooLarge);
    }

    let new_len = destination
        .len()
        .checked_add(chunk.len())
        .ok_or(DecompressionError::TooLarge)?;
    if new_len > max_bytes {
        return Err(DecompressionError::TooLarge);
    }

    if new_len > destination.capacity() {
        let doubled_capacity = destination.capacity().checked_mul(2).unwrap_or(max_bytes);
        let target_capacity = new_len.max(doubled_capacity).min(max_bytes);
        let additional = target_capacity
            .checked_sub(destination.len())
            .ok_or(DecompressionError::TooLarge)?;
        destination
            .try_reserve_exact(additional)
            .map_err(|_| DecompressionError::AllocationFailed)?;
        if destination.capacity() > max_bytes {
            return Err(DecompressionError::TooLarge);
        }
    }
    destination.extend_from_slice(chunk);
    Ok(())
}

fn decompression_error_response(
    image_id: ImageId,
    quiet: u8,
    error: DecompressionError,
) -> Option<Vec<u8>> {
    let reason = match error {
        DecompressionError::TooLarge => "E2BIG:decompressed image exceeds limit",
        DecompressionError::InvalidData => "EINVAL:invalid zlib stream",
        DecompressionError::AllocationFailed => "ENOMEM:failed to buffer decompressed image",
    };
    log::warn!("Rejected Kitty transmission for image {image_id}: {reason}");
    (quiet < 2).then(|| format!("\x1b_Gi={image_id};{reason}\x1b\\").into_bytes())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileLoadError {
    Disabled,
    InvalidPath,
    UnsafePath,
    NotRegular,
    TooLarge,
    Io,
    DeleteFailed,
    AllocationFailed,
}

struct TempFileDeletion {
    path: PathBuf,
    parent_fd: OwnedFd,
    file_name: CString,
    entry_device: u64,
    entry_inode: u64,
}

impl TempFileDeletion {
    fn for_request(path: &Path, opened_metadata: &Metadata) -> Option<Self> {
        let path = normalized_path_entry(path)?;
        if !is_safe_temp_delete_path(&path) {
            return None;
        }

        let (parent_fd, file_name) = open_canonical_parent(&path).ok()?;
        let entry_metadata = stat_at(&parent_fd, &file_name, libc::AT_SYMLINK_NOFOLLOW).ok()?;
        let followed_metadata = stat_at(&parent_fd, &file_name, 0).ok()?;
        if followed_metadata.st_dev as u64 != opened_metadata.dev()
            || followed_metadata.st_ino as u64 != opened_metadata.ino()
        {
            return None;
        }

        Some(Self {
            path,
            parent_fd,
            file_name,
            entry_device: entry_metadata.st_dev as u64,
            entry_inode: entry_metadata.st_ino as u64,
        })
    }

    fn delete_if_unchanged(self) -> Result<(), FileLoadError> {
        let current = match stat_at(&self.parent_fd, &self.file_name, libc::AT_SYMLINK_NOFOLLOW) {
            Ok(metadata) => metadata,
            Err(_) => {
                log::warn!(
                    "Could not revalidate Kitty temporary file {}",
                    self.path.display()
                );
                return Err(FileLoadError::DeleteFailed);
            }
        };

        if current.st_dev as u64 != self.entry_device || current.st_ino as u64 != self.entry_inode {
            log::warn!(
                "Refused to delete changed Kitty temporary file: {}",
                self.path.display()
            );
            return Err(FileLoadError::DeleteFailed);
        }

        let result =
            unsafe { libc::unlinkat(self.parent_fd.as_raw_fd(), self.file_name.as_ptr(), 0) };
        if result < 0 {
            let error = io::Error::last_os_error();
            log::warn!(
                "Could not delete Kitty temporary file {}: {error}",
                self.path.display()
            );
            return Err(FileLoadError::DeleteFailed);
        }
        Ok(())
    }
}

fn load_file_data(
    path_data: &[u8],
    delete_after: bool,
    options: KittyHandlerOptions,
) -> Result<Vec<u8>, FileLoadError> {
    if !options.allow_file_transfer {
        return Err(FileLoadError::Disabled);
    }

    if path_data.is_empty() {
        return Err(FileLoadError::InvalidPath);
    }

    let requested_path = Path::new(OsStr::from_bytes(path_data));
    let canonical_path = requested_path.canonicalize().map_err(classify_path_error)?;
    if !is_safe_read_path(&canonical_path) {
        log::warn!(
            "Kitty file transfer blocked for path: {}",
            canonical_path.display()
        );
        return Err(FileLoadError::UnsafePath);
    }

    let (data, metadata) = read_regular_file(&canonical_path, options.max_image_bytes)?;
    if delete_after {
        let deletion = TempFileDeletion::for_request(requested_path, &metadata);
        if let Some(deletion) = deletion {
            deletion.delete_if_unchanged()?;
        } else {
            log::warn!(
                "Refused unsafe Kitty temporary-file deletion: {}",
                requested_path.display()
            );
        }
    }

    Ok(data)
}

fn classify_path_error(error: io::Error) -> FileLoadError {
    if error.kind() == io::ErrorKind::InvalidInput {
        FileLoadError::InvalidPath
    } else {
        FileLoadError::Io
    }
}

fn read_regular_file(path: &Path, max_bytes: usize) -> Result<(Vec<u8>, Metadata), FileLoadError> {
    let (mut file, opened_metadata) = open_canonical_regular_file(path, max_bytes)?;

    let initial_len =
        usize::try_from(opened_metadata.len()).map_err(|_| FileLoadError::TooLarge)?;
    let mut data = Vec::new();
    reserve_file_buffer(&mut data, initial_len, max_bytes)?;
    let mut chunk = [0u8; FILE_READ_CHUNK_BYTES];

    loop {
        let remaining = max_bytes
            .checked_sub(data.len())
            .ok_or(FileLoadError::TooLarge)?;
        let read_limit = remaining.saturating_add(1).min(FILE_READ_CHUNK_BYTES);
        let read = match file.read(&mut chunk[..read_limit]) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(FileLoadError::Io),
        };
        if read == 0 {
            break;
        }
        append_file_bytes(&mut data, &chunk[..read], max_bytes)?;
    }

    Ok((data, opened_metadata))
}

fn open_canonical_regular_file(
    path: &Path,
    max_bytes: usize,
) -> Result<(File, Metadata), FileLoadError> {
    let (parent_fd, final_name) = open_canonical_parent(path)?;
    let before_open = stat_at(&parent_fd, &final_name, libc::AT_SYMLINK_NOFOLLOW)?;
    if (before_open.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return Err(FileLoadError::NotRegular);
    }
    let stat_len = u64::try_from(before_open.st_size).map_err(|_| FileLoadError::Io)?;
    if stat_len > u64::try_from(max_bytes).unwrap_or(u64::MAX) {
        return Err(FileLoadError::TooLarge);
    }

    let file_fd = unsafe {
        libc::openat(
            parent_fd.as_raw_fd(),
            final_name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if file_fd < 0 {
        return Err(classify_path_error(io::Error::last_os_error()));
    }
    let file_fd = unsafe { OwnedFd::from_raw_fd(file_fd) };
    let file = File::from(file_fd);
    let opened_metadata = file.metadata().map_err(|_| FileLoadError::Io)?;
    validate_regular_file(&opened_metadata, max_bytes)?;
    if opened_metadata.dev() != before_open.st_dev as u64
        || opened_metadata.ino() != before_open.st_ino as u64
    {
        return Err(FileLoadError::Io);
    }

    Ok((file, opened_metadata))
}

fn open_canonical_parent(path: &Path) -> Result<(OwnedFd, CString), FileLoadError> {
    if !path.is_absolute() {
        return Err(FileLoadError::InvalidPath);
    }

    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => names.push(name),
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(FileLoadError::InvalidPath);
            }
        }
    }
    let final_name = names.pop().ok_or(FileLoadError::InvalidPath)?;

    let root_fd = unsafe {
        libc::open(
            b"/\0".as_ptr().cast(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(classify_path_error(io::Error::last_os_error()));
    }
    let mut parent_fd = unsafe { OwnedFd::from_raw_fd(root_fd) };

    for name in names {
        let name = CString::new(name.as_bytes()).map_err(|_| FileLoadError::InvalidPath)?;
        let directory_fd = unsafe {
            libc::openat(
                parent_fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if directory_fd < 0 {
            return Err(classify_path_error(io::Error::last_os_error()));
        }
        parent_fd = unsafe { OwnedFd::from_raw_fd(directory_fd) };
    }

    let final_name = CString::new(final_name.as_bytes()).map_err(|_| FileLoadError::InvalidPath)?;
    Ok((parent_fd, final_name))
}

fn stat_at(
    parent_fd: &OwnedFd,
    file_name: &CString,
    flags: i32,
) -> Result<libc::stat, FileLoadError> {
    let mut before_open = std::mem::MaybeUninit::<libc::stat>::uninit();
    let stat_result = unsafe {
        libc::fstatat(
            parent_fd.as_raw_fd(),
            file_name.as_ptr(),
            before_open.as_mut_ptr(),
            flags,
        )
    };
    if stat_result < 0 {
        return Err(classify_path_error(io::Error::last_os_error()));
    }
    Ok(unsafe { before_open.assume_init() })
}

fn validate_regular_file(metadata: &Metadata, max_bytes: usize) -> Result<(), FileLoadError> {
    if !metadata.file_type().is_file() {
        return Err(FileLoadError::NotRegular);
    }
    let max_bytes = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if metadata.len() > max_bytes {
        return Err(FileLoadError::TooLarge);
    }
    Ok(())
}

fn append_file_bytes(
    destination: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
) -> Result<(), FileLoadError> {
    let new_len = destination
        .len()
        .checked_add(chunk.len())
        .ok_or(FileLoadError::TooLarge)?;
    if new_len > max_bytes {
        return Err(FileLoadError::TooLarge);
    }
    reserve_file_buffer(destination, new_len, max_bytes)?;
    destination.extend_from_slice(chunk);
    Ok(())
}

fn reserve_file_buffer(
    destination: &mut Vec<u8>,
    required_len: usize,
    max_bytes: usize,
) -> Result<(), FileLoadError> {
    if required_len > max_bytes || destination.capacity() > max_bytes {
        return Err(FileLoadError::TooLarge);
    }
    if required_len <= destination.capacity() {
        return Ok(());
    }

    let doubled_capacity = destination.capacity().checked_mul(2).unwrap_or(max_bytes);
    let target_capacity = required_len.max(doubled_capacity).min(max_bytes);
    let additional = target_capacity
        .checked_sub(destination.len())
        .ok_or(FileLoadError::TooLarge)?;
    destination
        .try_reserve_exact(additional)
        .map_err(|_| FileLoadError::AllocationFailed)?;
    if destination.capacity() > max_bytes {
        return Err(FileLoadError::TooLarge);
    }
    Ok(())
}

fn is_safe_read_path(path: &Path) -> bool {
    let temporary_roots = known_temporary_roots();
    if path_is_within_roots(path, &temporary_roots) {
        return true;
    }
    !is_sensitive_path(path)
}

fn is_safe_temp_delete_path(path: &Path) -> bool {
    path.to_string_lossy().contains(TEMP_FILE_MARKER)
        && path_is_within_roots(path, &known_temporary_roots())
}

fn normalized_path_entry(path: &Path) -> Option<PathBuf> {
    let file_name = path.file_name()?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize().ok()?;
    Some(parent.join(file_name))
}

fn known_temporary_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    add_canonical_directory(&mut roots, &std::env::temp_dir());
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        add_canonical_directory(&mut roots, Path::new(&tmpdir));
    }
    for path in ["/tmp", "/var/tmp", "/dev/shm"] {
        add_canonical_directory(&mut roots, Path::new(path));
    }
    roots
}

fn add_canonical_directory(roots: &mut Vec<PathBuf>, path: &Path) {
    let Ok(path) = path.canonicalize() else {
        return;
    };
    if path.parent().is_some() && path.is_dir() && !roots.iter().any(|existing| existing == &path) {
        roots.push(path);
    }
}

fn path_is_within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn is_sensitive_path(path: &Path) -> bool {
    let mut roots = Vec::new();
    for path in ["/proc", "/sys", "/dev"] {
        add_canonical_directory(&mut roots, Path::new(path));
    }
    path_is_within_roots(path, &roots)
}

fn file_load_error_response(image_id: ImageId, quiet: u8, error: FileLoadError) -> Option<Vec<u8>> {
    let reason = match error {
        FileLoadError::Disabled => "EPERM:file transmission disabled",
        FileLoadError::InvalidPath => "EINVAL:invalid file path",
        FileLoadError::UnsafePath => "EPERM:file path is not allowed",
        FileLoadError::NotRegular => "EINVAL:not a regular file",
        FileLoadError::TooLarge => "E2BIG:file exceeds image limit",
        FileLoadError::Io => "EIO:failed to read file",
        FileLoadError::DeleteFailed => "EIO:failed to delete temporary file",
        FileLoadError::AllocationFailed => "ENOMEM:failed to buffer file",
    };
    log::warn!("Rejected Kitty file transmission for image {image_id}: {reason}");
    (quiet < 2).then(|| format!("\x1b_Gi={image_id};{reason}\x1b\\").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::{
        append_bounded, decompression_error_response, file_load_error_response,
        inline_placement_and_advance, is_safe_read_path, is_safe_temp_delete_path, load_file_data,
        maybe_decompress_with_limit, normalized_path_entry, path_is_within_roots,
        read_regular_file, ChunkAssembler, ChunkAssemblyErrorKind, CompletedImage,
        DecompressionError, FileLoadError, KittyHandler, KittyHandlerOptions, PlacementMode,
        TempFileDeletion, DECOMPRESSION_CHUNK_BYTES, MAX_PENDING_IMAGE_BYTES,
    };
    use crate::parser::kitty_graphics::{
        KittyAction, KittyCommand, KittyCompression, KittyFormat, KittyTransmission,
    };
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::borrow::Cow;
    #[cfg(target_os = "linux")]
    use std::ffi::OsString;
    use std::fs;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    #[cfg(target_os = "linux")]
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            loop {
                let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("kokuban-{label}-{}-{sequence}", std::process::id()));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("failed to create test directory: {error}"),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn file_options(max_image_bytes: usize, allow_file_transfer: bool) -> KittyHandlerOptions {
        KittyHandlerOptions {
            max_image_bytes,
            allow_file_transfer,
        }
    }

    #[test]
    fn direct_query_validates_without_an_image_store() {
        let handler = KittyHandler::new(file_options(64, true));
        let cmd = KittyCommand {
            action: KittyAction::Query,
            image_id: Some(17),
            width: Some(1),
            height: Some(1),
            payload: vec![255, 0, 0, 255],
            ..KittyCommand::default()
        };

        let (response, advance) = handler.handle_query(&cmd);

        assert_eq!(response, Some(b"\x1b_Gi=17;OK\x1b\\".to_vec()));
        assert!(advance.is_none());
    }

    fn chunk(data: &[u8], more_chunks: bool, image_id: Option<u32>) -> KittyCommand {
        KittyCommand {
            image_id,
            more_chunks,
            payload: data.to_vec(),
            ..KittyCommand::default()
        }
    }

    fn complete(assembly: Option<CompletedImage>) -> CompletedImage {
        assembly.expect("expected a completed image")
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).expect("zlib input should encode");
        encoder.finish().expect("zlib stream should finish")
    }

    #[test]
    fn applies_configured_image_limit_and_disables_file_access_early() {
        let configured = KittyHandlerOptions::from_megabytes(50, true);
        assert_eq!(configured.max_image_bytes, 50 * 1024 * 1024);

        let clamped = KittyHandlerOptions::from_megabytes(usize::MAX, true);
        assert_eq!(clamped.max_image_bytes, MAX_PENDING_IMAGE_BYTES);
        let handler = KittyHandler::new(file_options(123, true));
        assert_eq!(handler.chunks.max_bytes, 123);

        let disabled = load_file_data(&[0xff], false, file_options(8, false));
        assert!(matches!(disabled, Err(FileLoadError::Disabled)));
    }

    #[test]
    fn reads_regular_files_at_the_exact_limit_and_rejects_larger_files() {
        let directory = TestDirectory::new("file-limit");
        let exact_path = directory.path().join("exact.bin");
        fs::write(&exact_path, b"12345678").expect("exact test file should be written");

        let loaded = load_file_data(
            exact_path.as_os_str().as_bytes(),
            false,
            file_options(8, true),
        )
        .expect("a regular file at the exact limit should load");
        assert_eq!(loaded, b"12345678");
        assert!(loaded.capacity() <= 8);

        assert!(matches!(
            load_file_data(
                exact_path.as_os_str().as_bytes(),
                false,
                file_options(7, true)
            ),
            Err(FileLoadError::TooLarge)
        ));

        let sparse_path = directory.path().join("sparse.bin");
        let sparse = fs::File::create(&sparse_path).expect("sparse test file should be created");
        sparse
            .set_len(1024 * 1024 * 1024)
            .expect("sparse test file should be extended");
        assert!(matches!(
            load_file_data(
                sparse_path.as_os_str().as_bytes(),
                false,
                file_options(8, true)
            ),
            Err(FileLoadError::TooLarge)
        ));
    }

    #[test]
    fn rejects_directories_and_fifos_without_blocking() {
        let directory = TestDirectory::new("special-files");
        assert!(matches!(
            load_file_data(
                directory.path().as_os_str().as_bytes(),
                false,
                file_options(64, true)
            ),
            Err(FileLoadError::NotRegular)
        ));

        let fifo_path = directory.path().join("image.fifo");
        nix::unistd::mkfifo(
            &fifo_path,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .expect("test FIFO should be created");
        let fifo_bytes = fifo_path.as_os_str().as_bytes().to_vec();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = load_file_data(&fifo_bytes, false, file_options(64, true));
            let _ = sender.send(matches!(result, Err(FileLoadError::NotRegular)));
        });
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(true),
            "FIFO validation must not wait for a writer"
        );
    }

    #[test]
    fn follows_regular_file_symlinks_and_rejects_symlink_loops() {
        let directory = TestDirectory::new("symlinks");
        let target = directory.path().join("target.bin");
        let link = directory.path().join("link.bin");
        fs::write(&target, b"image").expect("symlink target should be written");
        symlink(&target, &link).expect("regular-file symlink should be created");

        let loaded = load_file_data(link.as_os_str().as_bytes(), false, file_options(16, true))
            .expect("regular-file symlinks are required by the protocol");
        assert_eq!(loaded, b"image");

        let first = directory.path().join("loop-a");
        let second = directory.path().join("loop-b");
        symlink(&second, &first).expect("first loop link should be created");
        symlink(&first, &second).expect("second loop link should be created");
        assert!(matches!(
            load_file_data(first.as_os_str().as_bytes(), false, file_options(16, true)),
            Err(FileLoadError::Io)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn supports_non_utf8_unix_file_names() {
        let directory = TestDirectory::new("non-utf8");
        let mut file_name = b"image-".to_vec();
        file_name.push(0xff);
        let path = directory.path().join(OsString::from_vec(file_name));
        fs::write(&path, b"pixels").expect("non-UTF8 test file should be written");

        let loaded = load_file_data(path.as_os_str().as_bytes(), false, file_options(16, true))
            .expect("Unix file names must not require UTF-8");
        assert_eq!(loaded, b"pixels");
    }

    #[test]
    fn deletes_only_safe_temporary_entries_after_reading() {
        let directory = TestDirectory::new("temp-delete");
        let marked = directory.path().join("image-tty-graphics-protocol.bin");
        fs::write(&marked, b"pixels").expect("marked temporary file should be written");
        let loaded = load_file_data(marked.as_os_str().as_bytes(), true, file_options(16, true))
            .expect("marked temporary file should load");
        assert_eq!(loaded, b"pixels");
        assert!(!marked.exists());

        let unmarked = directory.path().join("ordinary-image.bin");
        fs::write(&unmarked, b"pixels").expect("unmarked file should be written");
        let loaded = load_file_data(
            unmarked.as_os_str().as_bytes(),
            true,
            file_options(16, true),
        )
        .expect("unmarked file should still be readable");
        assert_eq!(loaded, b"pixels");
        assert!(unmarked.exists());
    }

    #[test]
    fn temporary_symlink_deletion_removes_the_link_not_its_target() {
        let directory = TestDirectory::new("temp-symlink");
        let target = directory.path().join("preserved-target.bin");
        let link = directory.path().join("link-tty-graphics-protocol.bin");
        fs::write(&target, b"pixels").expect("temporary target should be written");
        symlink(&target, &link).expect("temporary symlink should be created");

        let loaded = load_file_data(link.as_os_str().as_bytes(), true, file_options(16, true))
            .expect("temporary symlink should load its regular target");
        assert_eq!(loaded, b"pixels");
        assert!(!link.exists());
        assert_eq!(fs::read(&target).expect("target must remain"), b"pixels");
    }

    #[test]
    fn refuses_to_delete_a_replaced_temporary_entry() {
        let directory = TestDirectory::new("temp-replaced");
        let path = directory.path().join("replace-tty-graphics-protocol.bin");
        let old_path = directory.path().join("old.bin");
        fs::write(&path, b"old").expect("original file should be written");
        let canonical_path = path.canonicalize().expect("path should canonicalize");
        let (_, metadata) =
            read_regular_file(&canonical_path, 16).expect("original file should load");
        let deletion = TempFileDeletion::for_request(&path, &metadata)
            .expect("original entry should have a deletion candidate");
        fs::rename(&path, &old_path).expect("original file should move");
        fs::write(&path, b"new").expect("replacement file should be written");

        assert_eq!(
            deletion.delete_if_unchanged(),
            Err(FileLoadError::DeleteFailed)
        );
        assert_eq!(
            fs::read(&path).expect("replacement must not be deleted"),
            b"new"
        );
    }

    #[test]
    fn temporary_deletion_stays_bound_to_the_opened_parent_directory() {
        let directory = TestDirectory::new("temp-parent-race");
        let original_parent = directory.path().join("original");
        let moved_parent = directory.path().join("moved");
        fs::create_dir(&original_parent).expect("original parent should be created");
        let file_name = "image-tty-graphics-protocol.bin";
        let original_path = original_parent.join(file_name);
        fs::write(&original_path, b"old").expect("original file should be written");

        let canonical_path = original_path
            .canonicalize()
            .expect("original path should canonicalize");
        let (_, metadata) =
            read_regular_file(&canonical_path, 16).expect("original file should load");
        let deletion = TempFileDeletion::for_request(&original_path, &metadata)
            .expect("original entry should have a deletion candidate");

        fs::rename(&original_parent, &moved_parent).expect("parent should move");
        fs::create_dir(&original_parent).expect("replacement parent should be created");
        let replacement = original_parent.join(file_name);
        fs::write(&replacement, b"new").expect("replacement file should be written");

        deletion
            .delete_if_unchanged()
            .expect("pinned original entry should be deleted");
        assert!(!moved_parent.join(file_name).exists());
        assert_eq!(
            fs::read(&replacement).expect("replacement must remain"),
            b"new"
        );
    }

    #[test]
    fn path_roots_are_component_aware_and_temp_errors_honor_quiet() {
        let roots = vec![PathBuf::from("/tmp/kokuban-safe")];
        assert!(path_is_within_roots(
            Path::new("/tmp/kokuban-safe/image"),
            &roots
        ));
        assert!(!path_is_within_roots(
            Path::new("/tmp/kokuban-safe-evil/image"),
            &roots
        ));

        let directory = TestDirectory::new("temp-root");
        let marked = directory.path().join("tty-graphics-protocol-image.bin");
        fs::write(&marked, b"pixels").expect("temporary path should exist");
        let normalized = normalized_path_entry(&marked).expect("path should normalize");
        assert!(is_safe_temp_delete_path(&normalized));
        assert!(is_safe_read_path(Path::new("/usr/share/kokuban-image")));
        assert!(!is_safe_read_path(Path::new("/dev/null")));

        let expected = b"\x1b_Gi=9;E2BIG:file exceeds image limit\x1b\\".to_vec();
        assert_eq!(
            file_load_error_response(9, 0, FileLoadError::TooLarge),
            Some(expected.clone())
        );
        assert_eq!(
            file_load_error_response(9, 1, FileLoadError::TooLarge),
            Some(expected)
        );
        assert_eq!(
            file_load_error_response(9, 2, FileLoadError::TooLarge),
            None
        );
    }

    #[test]
    fn assembles_three_standard_chunks_without_reallocating_an_id() {
        let mut assembler = ChunkAssembler::new(64);
        let mut allocations = 0;

        let first = assembler
            .push(chunk(b"one", true, None), false, || {
                allocations += 1;
                41
            })
            .expect("first chunk should be accepted");
        assert!(first.is_none());

        let second = assembler
            .push(chunk(b"-two", true, None), false, || {
                allocations += 1;
                42
            })
            .expect("continuation should reuse the active upload");
        assert!(second.is_none());

        let image = complete(
            assembler
                .push(chunk(b"-three", false, None), false, || {
                    allocations += 1;
                    43
                })
                .expect("final chunk should complete the active upload"),
        );
        assert_eq!(allocations, 1);
        assert_eq!(image.image_id, 41);
        assert_eq!(image.data, b"one-two-three");
    }

    #[test]
    fn empty_final_chunk_completes_the_active_upload() {
        let mut assembler = ChunkAssembler::new(64);
        assembler
            .push(chunk(b"complete", true, Some(44)), false, || 99)
            .expect("first chunk should start an upload");

        let image = complete(
            assembler
                .push(chunk(b"", false, None), false, || 99)
                .expect("empty final chunk should complete the upload"),
        );
        assert_eq!(
            (image.image_id, image.data.as_slice()),
            (44, b"complete".as_slice())
        );
        assert!(assembler.pending.is_none());
    }

    #[test]
    fn enforces_exact_aggregate_limit_and_clears_rejected_upload() {
        let mut assembler = ChunkAssembler::new(4);

        assert!(assembler
            .push(chunk(b"12", true, Some(7)), false, || 99)
            .expect("first exact-limit chunk should fit")
            .is_none());
        let image = complete(
            assembler
                .push(chunk(b"34", false, None), false, || 99)
                .expect("the exact byte limit should fit"),
        );
        assert_eq!(image.data, b"1234");

        assert!(assembler
            .push(chunk(b"123", true, Some(8)), false, || 99)
            .expect("first chunk should fit")
            .is_none());
        let error = assembler
            .push(chunk(b"45", false, None), false, || 99)
            .expect_err("one byte over the limit must be rejected");
        assert_eq!(error.image_id, 8);
        assert_eq!(error.kind, ChunkAssemblyErrorKind::TooLarge);
        assert!(assembler.pending.is_none());

        let image = complete(
            assembler
                .push(chunk(b"ok", false, None), false, || 9)
                .expect("a new upload should work after rejection"),
        );
        assert_eq!(
            (image.image_id, image.data.as_slice()),
            (9, b"ok".as_slice())
        );
    }

    #[test]
    fn grows_pending_storage_logarithmically_for_small_chunks() {
        const LIMIT: usize = 4096;
        let mut data = Vec::new();
        let mut capacity_changes = 0;
        let mut previous_capacity = data.capacity();

        for _ in 0..LIMIT {
            append_bounded(&mut data, b"x", LIMIT).expect("chunk should fit");
            if data.capacity() != previous_capacity {
                capacity_changes += 1;
                previous_capacity = data.capacity();
            }
        }

        assert_eq!(data.len(), LIMIT);
        assert!(data.capacity() <= LIMIT);
        assert!(capacity_changes <= LIMIT.ilog2() as usize + 1);
        assert_eq!(
            append_bounded(&mut data, b"x", LIMIT),
            Err(ChunkAssemblyErrorKind::TooLarge)
        );
        assert_eq!(data.len(), LIMIT);
    }

    #[test]
    fn continuation_quiet_value_applies_to_completion_and_errors() {
        let mut assembler = ChunkAssembler::new(8);
        let mut first = chunk(b"a", true, Some(30));
        first.quiet = 1;
        assembler
            .push(first, false, || 99)
            .expect("first chunk should start an upload");

        let mut final_chunk = chunk(b"b", false, None);
        final_chunk.quiet = 2;
        let image = complete(
            assembler
                .push(final_chunk, false, || 99)
                .expect("continuation should complete the upload"),
        );
        assert_eq!(image.metadata.quiet, 2);

        let mut assembler = ChunkAssembler::new(2);
        assembler
            .push(chunk(b"12", true, Some(31)), false, || 99)
            .expect("first chunk should fill the budget");
        let mut overflowing = chunk(b"3", false, None);
        overflowing.quiet = 2;
        let error = assembler
            .push(overflowing, false, || 99)
            .expect_err("overflowing continuation should be rejected");
        assert_eq!(error.kind, ChunkAssemblyErrorKind::TooLarge);
        assert_eq!(error.quiet, 2);
        assert!(assembler.pending.is_none());
    }

    #[test]
    fn accepts_matching_explicit_id_and_rejects_interleaving() {
        let mut assembler = ChunkAssembler::new(64);

        assembler
            .push(chunk(b"a", true, Some(7)), false, || 99)
            .expect("first explicit chunk should start an upload");
        let image = complete(
            assembler
                .push(chunk(b"b", false, Some(7)), false, || 99)
                .expect("the matching explicit id should continue the upload"),
        );
        assert_eq!(
            (image.image_id, image.data.as_slice()),
            (7, b"ab".as_slice())
        );

        assembler
            .push(chunk(b"old", true, Some(10)), false, || 99)
            .expect("first upload should start");
        let error = assembler
            .push(chunk(b"new", true, Some(11)), false, || 99)
            .expect_err("a second explicit id must not interleave");
        assert_eq!(error.image_id, 10);
        assert_eq!(
            error.kind,
            ChunkAssemblyErrorKind::Interleaved { received_id: 11 }
        );
        assert!(assembler.pending.is_none());

        assembler
            .push(chunk(b"old", true, Some(12)), false, || 99)
            .expect("first upload should start");
        let mut new_start = chunk(b"new", true, None);
        new_start.action = KittyAction::TransmitAndPlace;
        let error = assembler
            .push(new_start, true, || 13)
            .expect_err("a new a=T command must abort the active upload");
        assert_eq!(error.image_id, 12);
        assert_eq!(error.kind, ChunkAssemblyErrorKind::UnexpectedStart);
        assert!(assembler.pending.is_none());
    }

    #[test]
    fn abort_clears_the_only_active_upload() {
        let mut assembler = ChunkAssembler::new(64);
        assembler
            .push(chunk(b"partial", true, Some(21)), false, || 99)
            .expect("upload should start");

        assert!(assembler.abort());
        assert!(assembler.pending.is_none());
        assert!(!assembler.abort());

        let image = complete(
            assembler
                .push(chunk(b"new", false, Some(22)), false, || 99)
                .expect("a new upload should work after abort"),
        );
        assert_eq!(
            (image.image_id, image.data.as_slice()),
            (22, b"new".as_slice())
        );
    }

    #[test]
    fn preserves_first_chunk_metadata_and_deferred_placement() {
        let mut assembler = ChunkAssembler::new(64);
        let first = KittyCommand {
            action: KittyAction::TransmitAndPlace,
            quiet: 1,
            format: KittyFormat::Png,
            transmission: KittyTransmission::Direct,
            compression: KittyCompression::Zlib,
            width: Some(20),
            height: Some(10),
            more_chunks: true,
            payload: b"png-".to_vec(),
            columns: Some(5),
            rows: Some(3),
            z_index: Some(-2),
            cursor_movement: Some(1),
            ..KittyCommand::default()
        };

        assembler
            .push(first, true, || 51)
            .expect("transmit-and-place should start an upload");
        let image = complete(
            assembler
                .push(chunk(b"data", false, None), false, || 52)
                .expect("a default final chunk should complete the upload"),
        );

        assert_eq!(image.image_id, 51);
        assert_eq!(image.data, b"png-data");
        assert_eq!(image.metadata.action, KittyAction::TransmitAndPlace);
        assert_eq!(image.metadata.quiet, 1);
        assert_eq!(image.metadata.format, KittyFormat::Png);
        assert_eq!(image.metadata.compression, KittyCompression::Zlib);
        assert_eq!(
            (image.metadata.width, image.metadata.height),
            (Some(20), Some(10))
        );

        let placement = image.place_after.expect("first a=T must be retained");
        assert_eq!(placement.action, KittyAction::TransmitAndPlace);
        assert_eq!((placement.columns, placement.rows), (Some(5), Some(3)));
        assert_eq!(placement.z_index, Some(-2));
        assert_eq!(placement.cursor_movement, Some(1));
    }

    #[test]
    fn transmit_and_place_without_explicit_id_uses_allocated_id() {
        let mut assembler = ChunkAssembler::new(64);
        let cmd = KittyCommand {
            action: KittyAction::TransmitAndPlace,
            payload: b"rgba".to_vec(),
            ..KittyCommand::default()
        };

        let image = complete(
            assembler
                .push(cmd, true, || 73)
                .expect("single transmit-and-place should complete"),
        );
        assert_eq!(image.image_id, 73);
        assert!(image.place_after.is_some());
    }

    #[test]
    fn clamps_inline_dimensions_and_cursor_advance_to_the_grid() {
        let (mode, advance) = inline_placement_and_advance(3, 7, u32::MAX, u32::MAX, 80, 24);

        match mode {
            PlacementMode::Inline {
                row,
                col,
                cols,
                rows,
            } => assert_eq!((row, col, cols, rows), (3, 7, 80, 24)),
            PlacementMode::Overlay { .. } => panic!("expected inline placement"),
        }
        assert_eq!((advance.cols, advance.rows), (80, 24));

        let (mode, advance) = inline_placement_and_advance(0, 0, 10, 5, 80, 24);
        assert!(matches!(
            mode,
            PlacementMode::Inline {
                cols: 10,
                rows: 5,
                ..
            }
        ));
        assert_eq!((advance.cols, advance.rows), (10, 5));
    }

    #[test]
    fn accepts_exact_decompressed_limit_and_rejects_one_byte_over() {
        let exact = zlib(b"1234");
        let decoded = maybe_decompress_with_limit(&exact, KittyCompression::Zlib, 4)
            .expect("exact decompressed limit should fit");
        assert_eq!(decoded.as_ref(), b"1234");

        let oversized = zlib(b"12345");
        assert_eq!(
            maybe_decompress_with_limit(&oversized, KittyCompression::Zlib, 4),
            Err(DecompressionError::TooLarge)
        );

        let chunk_sized = vec![b'x'; DECOMPRESSION_CHUNK_BYTES];
        let encoded = zlib(&chunk_sized);
        let decoded = maybe_decompress_with_limit(
            &encoded,
            KittyCompression::Zlib,
            DECOMPRESSION_CHUNK_BYTES,
        )
        .expect("an exact full output chunk should still reach StreamEnd");
        assert_eq!(decoded.as_ref(), chunk_sized);
    }

    #[test]
    fn decompresses_valid_streams_across_multiple_output_chunks() {
        for size in [32_769usize, 65_536, 1_000_000] {
            let original = vec![b'x'; size];
            let encoded = zlib(&original);
            let decoded = maybe_decompress_with_limit(&encoded, KittyCompression::Zlib, size)
                .unwrap_or_else(|error| panic!("valid {size}-byte stream failed: {error:?}"));
            assert_eq!(decoded.as_ref(), original);
        }
    }

    #[test]
    fn rejects_invalid_truncated_and_trailing_zlib_data() {
        assert_eq!(
            maybe_decompress_with_limit(b"not zlib", KittyCompression::Zlib, 64),
            Err(DecompressionError::InvalidData)
        );

        let mut truncated = zlib(b"complete stream");
        truncated.pop();
        assert_eq!(
            maybe_decompress_with_limit(&truncated, KittyCompression::Zlib, 64),
            Err(DecompressionError::InvalidData)
        );

        let mut trailing = zlib(b"complete stream");
        trailing.push(0);
        assert_eq!(
            maybe_decompress_with_limit(&trailing, KittyCompression::Zlib, 64),
            Err(DecompressionError::InvalidData)
        );
    }

    #[test]
    fn uncompressed_data_is_borrowed_and_still_obeys_the_limit() {
        let data = b"borrowed";
        let decoded = maybe_decompress_with_limit(data, KittyCompression::None, data.len())
            .expect("uncompressed data at the exact limit should fit");
        assert!(matches!(decoded, Cow::Borrowed(bytes) if std::ptr::eq(bytes, data)));

        assert_eq!(
            maybe_decompress_with_limit(data, KittyCompression::None, data.len() - 1),
            Err(DecompressionError::TooLarge)
        );
    }

    #[test]
    fn decompression_errors_honor_quiet_level() {
        let expected = b"\x1b_Gi=17;EINVAL:invalid zlib stream\x1b\\".to_vec();
        assert_eq!(
            decompression_error_response(17, 0, DecompressionError::InvalidData),
            Some(expected.clone())
        );
        assert_eq!(
            decompression_error_response(17, 1, DecompressionError::InvalidData),
            Some(expected)
        );
        assert_eq!(
            decompression_error_response(17, 2, DecompressionError::InvalidData),
            None
        );
    }
}
