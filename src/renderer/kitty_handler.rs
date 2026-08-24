use super::image_store::{ImageFormat, ImageId, ImageStore};
use crate::parser::kitty_graphics::*;
use std::borrow::Cow;

const MAX_PENDING_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_DECOMPRESSED_IMAGE_BYTES: usize = MAX_PENDING_IMAGE_BYTES;
const DECOMPRESSION_CHUNK_BYTES: usize = 32 * 1024;

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
}

impl KittyHandler {
    pub fn new() -> Self {
        Self {
            chunks: ChunkAssembler::new(MAX_PENDING_IMAGE_BYTES),
            next_placement_id: 1,
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
                return self.handle_query(&cmd, store);
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

    fn handle_query(
        &self,
        cmd: &KittyCommand,
        store: &mut ImageStore,
    ) -> (Option<Vec<u8>>, Option<CursorAdvance>) {
        let image_id = cmd.image_id.unwrap_or(0);
        // For queries, we try to process the tiny test image and respond OK
        if !cmd.payload.is_empty() {
            let format = match cmd.format {
                KittyFormat::Rgb => ImageFormat::Rgb,
                KittyFormat::Rgba => ImageFormat::Rgba,
                KittyFormat::Png => ImageFormat::Png,
            };
            let w = cmd.width.unwrap_or(1);
            let h = cmd.height.unwrap_or(1);
            // Store the test image briefly. A failed decompression must not be
            // interpreted as raw pixels: doing so would make corrupt streams
            // appear supported and could retain attacker-controlled data.
            let data = match maybe_decompress(&cmd.payload, cmd.compression) {
                Ok(data) => data,
                Err(error) => {
                    return (
                        decompression_error_response(image_id, cmd.quiet, error),
                        None,
                    );
                }
            };
            match store.store(data.as_ref(), w, h, format, Some(image_id)) {
                Some(id) => {
                    // Remove the test image immediately
                    store.remove(id);
                }
                None => {
                    let response = if cmd.quiet < 2 {
                        Some(
                            format!("\x1b_Gi={image_id};ENOMEM:failed to store query image\x1b\\")
                                .into_bytes(),
                        )
                    } else {
                        None
                    };
                    return (response, None);
                }
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
                match load_file_data(&full_data, transmission == KittyTransmission::TempFile) {
                    Some(data) => data,
                    None => {
                        let resp = if quiet < 2 {
                            Some(
                                format!("\x1b_Gi={image_id};ENOENT:file not found\x1b\\")
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
        let image_data = match maybe_decompress(&image_data, compression) {
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

fn maybe_decompress(
    data: &[u8],
    compression: KittyCompression,
) -> Result<Cow<'_, [u8]>, DecompressionError> {
    maybe_decompress_with_limit(data, compression, MAX_DECOMPRESSED_IMAGE_BYTES)
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

fn load_file_data(path_data: &[u8], delete_after: bool) -> Option<Vec<u8>> {
    let path_str = std::str::from_utf8(path_data).ok()?;
    let path = std::path::Path::new(path_str);

    // Security: only allow reads from safe directories
    if !is_safe_path(path) {
        log::warn!("Kitty file transfer blocked for path: {path_str}");
        return None;
    }

    let data = std::fs::read(path).ok()?;

    if delete_after {
        let _ = std::fs::remove_file(path);
    }

    Some(data)
}

fn is_safe_path(path: &std::path::Path) -> bool {
    let path = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let path_str = path.to_string_lossy();

    // Allow: $TMPDIR, /tmp, $HOME, current working directory
    if let Ok(tmp) = std::env::var("TMPDIR") {
        if path_str.starts_with(&tmp) {
            return true;
        }
    }
    if path_str.starts_with("/tmp/") || path_str.starts_with("/var/folders/") {
        return true;
    }
    if let Ok(home) = std::env::var("HOME") {
        if path_str.starts_with(&home) {
            return true;
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if path.starts_with(&cwd) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::{
        append_bounded, decompression_error_response, inline_placement_and_advance,
        maybe_decompress_with_limit, ChunkAssembler, ChunkAssemblyErrorKind, CompletedImage,
        DecompressionError, PlacementMode, DECOMPRESSION_CHUNK_BYTES,
    };
    use crate::parser::kitty_graphics::{
        KittyAction, KittyCommand, KittyCompression, KittyFormat, KittyTransmission,
    };
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::borrow::Cow;
    use std::io::Write;

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
