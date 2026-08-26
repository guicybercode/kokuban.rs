use super::image_store::{probe_image_data, ImageFormat, ImageStore};
use crate::graphics::{
    resolve_kitty_placement_layout, ClientImageRegistry, ImageId,
    ImageNumberRegistry, ImagePlacement, InlineRenderSize, KittyImageId,
    PlacementMode,
};
use crate::parser::kitty_graphics::*;
use nix::libc;
use std::borrow::Cow;
use std::collections::HashSet;
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
// Batch cache-existence scans while bounding registry growth relative to the shared cache.
const MAX_STALE_IMAGE_REGISTRY_ENTRIES: usize = 32;

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

#[derive(Debug)]
struct PendingImage {
    image_id: KittyImageId,
    data: Vec<u8>,
    metadata: KittyCommand,
    place_after: Option<KittyCommand>,
}

#[derive(Debug)]
struct CompletedImage {
    image_id: KittyImageId,
    data: Vec<u8>,
    metadata: KittyCommand,
    place_after: Option<KittyCommand>,
}

#[derive(Debug, PartialEq, Eq)]
enum ChunkAssemblyErrorKind {
    Interleaved { received_id: KittyImageId },
    InterleavedNumber { received_number: u32 },
    UnexpectedStart,
    TooLarge,
    AllocationFailed,
}

#[derive(Debug, PartialEq, Eq)]
struct ChunkAssemblyError {
    image_id: KittyImageId,
    image_number: Option<u32>,
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
        F: FnMut() -> KittyImageId,
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
                    image_number: pending
                        .metadata
                        .image_number
                        .filter(|number| *number != 0),
                    quiet: pending.metadata.quiet,
                    kind: ChunkAssemblyErrorKind::UnexpectedStart,
                };
                self.pending = None;
                return Err(error);
            }
            if let Some(received_id) = cmd.image_id.filter(|id| *id != 0) {
                if pending.metadata.image_id.filter(|id| *id != 0) != Some(received_id) {
                    let error = ChunkAssemblyError {
                        image_id: pending.image_id,
                        image_number: pending
                            .metadata
                            .image_number
                            .filter(|number| *number != 0),
                        quiet: pending.metadata.quiet,
                        kind: ChunkAssemblyErrorKind::Interleaved { received_id },
                    };
                    self.pending = None;
                    return Err(error);
                }
            }
            if let Some(received_number) = cmd.image_number.filter(|number| *number != 0) {
                if pending
                    .metadata
                    .image_number
                    .filter(|number| *number != 0)
                    != Some(received_number)
                {
                    let error = ChunkAssemblyError {
                        image_id: pending.image_id,
                        image_number: pending
                            .metadata
                            .image_number
                            .filter(|number| *number != 0),
                        quiet: pending.metadata.quiet,
                        kind: ChunkAssemblyErrorKind::InterleavedNumber {
                            received_number,
                        },
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
                    image_number: pending
                        .metadata
                        .image_number
                        .filter(|number| *number != 0),
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

        let image_id = cmd
            .image_id
            .filter(|id| *id != 0)
            .unwrap_or_else(&mut next_image_id);
        let quiet = cmd.quiet;
        let payload = std::mem::take(&mut cmd.payload);
        let mut data = Vec::new();
        if let Err(kind) = append_bounded(&mut data, &payload, self.max_bytes) {
            return Err(ChunkAssemblyError {
                image_id,
                image_number: cmd.image_number.filter(|number| *number != 0),
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
    client_image_id: KittyImageId,
    stored_image_id: ImageId,
    replaced_image_id: Option<ImageId>,
    image_number: Option<u32>,
    quiet: u8,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlacementError {
    ImageNotFound,
    InvalidCellOffset,
}

pub struct KittyHandler {
    chunks: ChunkAssembler,
    client_images: ClientImageRegistry,
    image_numbers: ImageNumberRegistry,
    next_placement_id: u32,
    options: KittyHandlerOptions,
}

pub(crate) struct KittyProcessOutcome {
    pub(crate) response: Option<Vec<u8>>,
    pub(crate) advance: Option<CursorAdvance>,
    pub(crate) hard_delete_candidates: HashSet<ImageId>,
    pub(crate) retransmitted_image_id: Option<ImageId>,
}

impl KittyProcessOutcome {
    fn new(response: Option<Vec<u8>>, advance: Option<CursorAdvance>) -> Self {
        Self {
            response,
            advance,
            hard_delete_candidates: HashSet::new(),
            retransmitted_image_id: None,
        }
    }
}

impl KittyHandler {
    pub fn new(options: KittyHandlerOptions) -> Self {
        Self {
            chunks: ChunkAssembler::new(options.max_image_bytes),
            client_images: ClientImageRegistry::default(),
            image_numbers: ImageNumberRegistry::default(),
            next_placement_id: 1,
            options,
        }
    }

    /// Process a parsed Kitty graphics command.
    /// Returns any PTY response, cursor movement, and deferred cache effects.
    pub(crate) fn process(
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
    ) -> KittyProcessOutcome {
        let selectors_conflict = cmd.image_id.is_some() && cmd.image_number.is_some();
        if cmd.invalid_image_selector || selectors_conflict {
            self.chunks.abort();
            let reason = if selectors_conflict {
                "EINVAL:image ID and image number are mutually exclusive"
            } else {
                "EINVAL:invalid image selector"
            };
            return KittyProcessOutcome::new(
                kitty_response(
                    cmd.image_id,
                    cmd.image_number,
                    cmd.quiet,
                    true,
                    reason,
                ),
                None,
            );
        }

        if !matches!(
            cmd.action,
            KittyAction::Transmit | KittyAction::TransmitAndPlace
        ) && self.chunks.abort()
        {
            log::warn!("Aborted pending Kitty transmission on non-transmit command");
        }

        match cmd.action {
            KittyAction::Query => {
                let (response, advance) = self.handle_query(&cmd);
                KittyProcessOutcome::new(response, advance)
            }
            KittyAction::Transmit | KittyAction::TransmitAndPlace => {
                let will_place = cmd.action == KittyAction::TransmitAndPlace;
                let outcome = self.handle_transmit(cmd, store, will_place);
                let should_prune = outcome.stored.is_some();
                let mut response = outcome.response;
                let mut advance = None;
                let mut retransmitted_image_id = None;
                if should_prune {
                    self.prune_image_registries(store, placements);
                }
                if let Some(stored) = outcome.stored {
                    if let Some(replaced_image_id) = stored.replaced_image_id {
                        remove_retransmitted_placements(placements, replaced_image_id);
                        retransmitted_image_id = Some(replaced_image_id);
                    }
                    if let Some(place_cmd) = stored.place_after {
                        match self.create_placement(
                            &place_cmd,
                            stored.stored_image_id,
                            store,
                            cursor_row,
                            cursor_col,
                            cell_width,
                            cell_height,
                            grid_cols,
                            grid_rows,
                            placements,
                        ) {
                            Ok(cursor_advance) => advance = cursor_advance,
                            Err(error) => {
                                response = placement_error_response_with_number(
                                    stored.client_image_id,
                                    stored.image_number,
                                    stored.quiet,
                                    error,
                                );
                            }
                        }
                    }
                }
                let mut outcome = KittyProcessOutcome::new(response, advance);
                outcome.retransmitted_image_id = retransmitted_image_id;
                outcome
            }
            KittyAction::Place => {
                let image_number = cmd.image_number.filter(|number| *number != 0);
                let explicit_client_id = cmd.image_id.filter(|id| *id != 0);
                let resolved_image = if let Some(client_id) = explicit_client_id {
                    self.resolve_client_image(client_id, store)
                        .map(|image_id| (client_id, image_id))
                } else {
                    image_number
                        .and_then(|number| self.resolve_image_number(number, store))
                };
                let (client_image_id, stored_image_id) = resolved_image
                    .unwrap_or((explicit_client_id.unwrap_or(0), 0));
                let response_image_id = if image_number.is_some() {
                    (client_image_id != 0).then_some(client_image_id)
                } else {
                    Some(client_image_id)
                };
                let result = match self.create_placement(
                    &cmd,
                    stored_image_id,
                    store,
                    cursor_row,
                    cursor_col,
                    cell_width,
                    cell_height,
                    grid_cols,
                    grid_rows,
                    placements,
                ) {
                    Ok(advance) => {
                        let response = kitty_response(
                            response_image_id,
                            image_number,
                            cmd.quiet,
                            false,
                            "OK",
                        );
                        (response, advance)
                    }
                    Err(error) => (
                        placement_error_response_with_number(
                            response_image_id.unwrap_or(0),
                            image_number,
                            cmd.quiet,
                            error,
                        ),
                        None,
                    ),
                };
                self.prune_image_registries(store, placements);
                KittyProcessOutcome::new(result.0, result.1)
            }
            KittyAction::Delete => {
                let hard_delete_candidates = self.handle_delete(
                    &cmd,
                    store,
                    (cursor_row, cursor_col),
                    (cell_width, cell_height),
                    (grid_cols, grid_rows),
                    placements,
                );
                self.prune_image_registries(store, placements);
                KittyProcessOutcome {
                    response: None,
                    advance: None,
                    hard_delete_candidates,
                    retransmitted_image_id: None,
                }
            }
            KittyAction::Frame | KittyAction::Animate | KittyAction::Compose => {
                // Out of scope
                KittyProcessOutcome::new(None, None)
            }
        }
    }

    fn handle_query(&self, cmd: &KittyCommand) -> (Option<Vec<u8>>, Option<CursorAdvance>) {
        let selectors_conflict = cmd.image_id.is_some() && cmd.image_number.is_some();
        if cmd.invalid_image_selector || selectors_conflict {
            let reason = if selectors_conflict {
                "EINVAL:image ID and image number are mutually exclusive"
            } else {
                "EINVAL:invalid image selector"
            };
            return (
                kitty_response(
                    cmd.image_id,
                    cmd.image_number,
                    cmd.quiet,
                    true,
                    reason,
                ),
                None,
            );
        }
        let Some(image_id) = cmd.image_id.filter(|id| *id != 0) else {
            log::warn!("Ignoring Kitty query without a non-zero image ID");
            return (None, None);
        };

        let (width, height, format, expected_raw_bytes) = match cmd.format {
            KittyFormat::Png => (0, 0, ImageFormat::Png, None),
            KittyFormat::Rgb | KittyFormat::Rgba => {
                let width = cmd.width.unwrap_or(0);
                let height = cmd.height.unwrap_or(0);
                if width == 0 || height == 0 {
                    return (
                        query_error_response(
                            image_id,
                            cmd.quiet,
                            "EINVAL:missing image dimensions",
                        ),
                        None,
                    );
                }
                let (format, bytes_per_pixel) = match cmd.format {
                    KittyFormat::Rgb => (ImageFormat::Rgb, 3u64),
                    KittyFormat::Rgba => (ImageFormat::Rgba, 4u64),
                    KittyFormat::Png => unreachable!(),
                };
                let expected = u64::from(width)
                    .checked_mul(u64::from(height))
                    .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
                    .and_then(|bytes| usize::try_from(bytes).ok());
                let Some(expected) = expected else {
                    return (
                        query_error_response(
                            image_id,
                            cmd.quiet,
                            "E2BIG:image dimensions exceed size limit",
                        ),
                        None,
                    );
                };
                if expected > self.options.max_image_bytes {
                    return (
                        query_error_response(
                            image_id,
                            cmd.quiet,
                            "E2BIG:image dimensions exceed size limit",
                        ),
                        None,
                    );
                }
                (width, height, format, Some(expected))
            }
        };

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
                return (
                    query_error_response(image_id, cmd.quiet, "ENOSYS:shared memory not supported"),
                    None,
                );
            }
        };
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
        if data.is_empty() {
            return (
                query_error_response(image_id, cmd.quiet, "ENODATA:missing image data"),
                None,
            );
        }
        if let Some(expected) = expected_raw_bytes {
            if data.len() < expected {
                return (
                    query_error_response(image_id, cmd.quiet, "ENODATA:insufficient image data"),
                    None,
                );
            }
            if data.len() > expected {
                return (
                    query_error_response(
                        image_id,
                        cmd.quiet,
                        "EINVAL:image data exceeds declared dimensions",
                    ),
                    None,
                );
            }
        }
        if !probe_image_data(
            data.as_ref(),
            width,
            height,
            format,
            self.options.max_image_bytes,
        ) {
            return (
                query_error_response(
                    image_id,
                    cmd.quiet,
                    "EINVAL:invalid or oversized image data",
                ),
                None,
            );
        }

        let resp = kitty_response(Some(image_id), None, cmd.quiet, false, "OK");
        (resp, None)
    }

    fn handle_transmit(
        &mut self,
        cmd: KittyCommand,
        store: &mut ImageStore,
        will_place: bool,
    ) -> TransmitOutcome {
        let chunks = &mut self.chunks;
        let client_images = &mut self.client_images;
        let completed = match chunks.push(cmd, will_place, || client_images.next_id()) {
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
                    ChunkAssemblyErrorKind::InterleavedNumber { received_number } => {
                        format!("EINVAL:interleaved transmission I={received_number}")
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
                let response = kitty_response(
                    Some(error.image_id),
                    error.image_number,
                    error.quiet,
                    true,
                    &reason,
                );
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
            image_number,
            format,
            width,
            height,
            compression,
            transmission,
            ..
        } = metadata;
        let image_number = image_number.filter(|number| *number != 0);

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
                            response: file_load_error_response_with_number(
                                image_id,
                                image_number,
                                quiet,
                                error,
                            ),
                            stored: None,
                        };
                    }
                }
            }
            KittyTransmission::SharedMemory => {
                let resp = kitty_response(
                    Some(image_id),
                    image_number,
                    quiet,
                    true,
                    "ENOSYS:shared memory not supported",
                );
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
                    response: decompression_error_response_with_number(
                        image_id,
                        image_number,
                        quiet,
                        error,
                    ),
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
                    let resp = kitty_response(
                        Some(image_id),
                        image_number,
                        quiet,
                        true,
                        "EINVAL:missing dimensions",
                    );
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
                    let resp = kitty_response(
                        Some(image_id),
                        image_number,
                        quiet,
                        true,
                        "EINVAL:missing dimensions",
                    );
                    return TransmitOutcome {
                        response: resp,
                        stored: None,
                    };
                }
                (w, h, ImageFormat::Rgba)
            }
        };

        let replaced_image_id = self.client_images.get(image_id);
        let stored_image_id = replaced_image_id.unwrap_or_else(|| store.next_id());
        let replacing_existing = store.get(stored_image_id).is_some();
        match store.store(
            image_data.as_ref(),
            w,
            h,
            img_format,
            Some(stored_image_id),
        ) {
            Some(stored_image_id) => {
                self.client_images.record(image_id, stored_image_id);
                update_image_number_association(
                    &mut self.image_numbers,
                    image_id,
                    image_number,
                    replacing_existing,
                );
                let resp = kitty_response(
                    Some(image_id),
                    image_number,
                    quiet,
                    false,
                    "OK",
                );
                TransmitOutcome {
                    response: resp,
                    stored: Some(StoredTransmission {
                        client_image_id: image_id,
                        stored_image_id,
                        replaced_image_id,
                        image_number,
                        quiet,
                        place_after,
                    }),
                }
            }
            None => {
                let resp = kitty_response(
                    Some(image_id),
                    image_number,
                    quiet,
                    true,
                    "ENOMEM:failed to store image",
                );
                TransmitOutcome {
                    response: resp,
                    stored: None,
                }
            }
        }
    }

    fn resolve_image_number(
        &self,
        image_number: u32,
        store: &ImageStore,
    ) -> Option<(KittyImageId, ImageId)> {
        let client_images = &self.client_images;
        let client_id = self.image_numbers.newest_matching(image_number, |client_id| {
            client_images
                .resolve_live(client_id, |image_id| store.get(image_id).is_some())
                .is_some()
        })?;
        self.resolve_client_image(client_id, store)
            .map(|image_id| (client_id, image_id))
    }

    fn resolve_client_image(
        &self,
        client_id: KittyImageId,
        store: &ImageStore,
    ) -> Option<ImageId> {
        self.client_images
            .resolve_live(client_id, |image_id| store.get(image_id).is_some())
    }

    fn resolve_image_number_for_delete(
        &self,
        image_number: u32,
        store: &ImageStore,
        placements: &[ImagePlacement],
    ) -> Option<(KittyImageId, ImageId)> {
        let placed_images: HashSet<ImageId> = placements
            .iter()
            .filter(|placement| placement.placement_id != 0)
            .map(|placement| placement.image_id)
            .collect();
        let client_images = &self.client_images;
        let client_id = self.image_numbers.newest_matching(image_number, |client_id| {
            let Some(image_id) = client_images.get(client_id) else {
                return false;
            };
            store.get(image_id).is_some()
                || placed_images.contains(&image_id)
        })?;
        client_images
            .get(client_id)
            .map(|image_id| (client_id, image_id))
    }

    fn prune_image_registries(
        &mut self,
        store: &ImageStore,
        placements: &[ImagePlacement],
    ) {
        let kitty_placement_count = placements
            .iter()
            .filter(|placement| placement.placement_id != 0)
            .count();
        let prune_client_images = client_image_registry_needs_pruning(
            self.client_images.len(),
            store.image_count(),
            kitty_placement_count,
        );
        let prune_image_numbers = image_number_registry_needs_pruning(
            self.image_numbers.len(),
            store.image_count().saturating_add(kitty_placement_count),
        );
        if !prune_client_images && !prune_image_numbers {
            return;
        }

        let placed_images: HashSet<ImageId> = placements
            .iter()
            .filter(|placement| placement.placement_id != 0)
            .map(|placement| placement.image_id)
            .collect();
        if prune_client_images {
            self.client_images.retain_existing(|image_id| {
                store.get(image_id).is_some() || placed_images.contains(&image_id)
            });
        }

        if prune_image_numbers {
            let client_images = &self.client_images;
            self.image_numbers.retain_existing(|client_id| {
                let Some(image_id) = client_images.get(client_id) else {
                    return false;
                };
                store.get(image_id).is_some() || placed_images.contains(&image_id)
            });
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
    ) -> Result<Option<CursorAdvance>, PlacementError> {
        let img = store
            .get(image_id)
            .ok_or(PlacementError::ImageNotFound)?;
        let (x_offset, y_offset) = placement_offsets(cmd, cell_width, cell_height)?;

        let placement_id = self.next_placement_id;
        self.next_placement_id = self.next_placement_id.wrapping_add(1).max(1);

        let layout = resolve_kitty_placement_layout(
            cmd.columns,
            cmd.rows,
            (img.width, img.height),
            (x_offset, y_offset),
            (cell_width, cell_height),
        );

        let z_index = cmd.z_index.unwrap_or(0);
        let cursor_movement = cmd.cursor_movement.unwrap_or(0);

        let (mode, advance) = inline_placement_and_advance(
            cursor_row,
            cursor_col,
            layout.display_cols,
            layout.display_rows,
            (x_offset, y_offset),
            layout.render_size,
            (grid_cols, grid_rows),
        );
        let cursor_advance = cursor_advance_for_policy(cursor_movement, advance);

        placements.push(ImagePlacement {
            image_id,
            placement_id,
            client_placement_id: cmd.placement_id,
            mode: mode.clone(),
            z_index,
        });

        Ok(cursor_advance)
    }

    fn handle_delete(
        &mut self,
        cmd: &KittyCommand,
        store: &ImageStore,
        cursor: (usize, usize),
        cell_size: (f32, f32),
        grid_size: (usize, usize),
        placements: &mut Vec<ImagePlacement>,
    ) -> HashSet<ImageId> {
        let resolved_image_id = match cmd.delete_specifier {
            Some(KittyDeleteSpec::ByNumber { number, .. }) => {
                let Some((_, image_id)) =
                    self.resolve_image_number_for_delete(number, store, placements)
                else {
                    return HashSet::new();
                };
                Some(image_id)
            }
            Some(KittyDeleteSpec::ById { id, .. }) => {
                let Some(image_id) = self.client_images.get(id) else {
                    return HashSet::new();
                };
                Some(image_id)
            }
            _ => None,
        };
        apply_resolved_delete_to_placements(
            cmd,
            resolved_image_id,
            placements,
            cursor,
            cell_size,
            grid_size,
        )
    }
}

fn update_image_number_association(
    registry: &mut ImageNumberRegistry,
    image_id: KittyImageId,
    image_number: Option<u32>,
    replacing_existing: bool,
) {
    if let Some(image_number) = image_number.filter(|number| *number != 0) {
        registry.record_new(image_number, image_id);
    } else if !replacing_existing {
        registry.forget(image_id);
    }
}

fn image_number_registry_needs_pruning(
    registry_entries: usize,
    stored_images: usize,
) -> bool {
    registry_entries
        > stored_images.saturating_add(MAX_STALE_IMAGE_REGISTRY_ENTRIES)
}

fn client_image_registry_needs_pruning(
    registry_entries: usize,
    stored_images: usize,
    kitty_placements: usize,
) -> bool {
    registry_entries
        > stored_images
            .saturating_add(kitty_placements)
            .saturating_add(MAX_STALE_IMAGE_REGISTRY_ENTRIES)
}

#[cfg(test)]
fn apply_delete_to_placements(
    cmd: &KittyCommand,
    placements: &mut Vec<ImagePlacement>,
    cursor_row: usize,
    cursor_col: usize,
    cell_width: f32,
    cell_height: f32,
) -> HashSet<ImageId> {
    let resolved_image_id = match cmd.delete_specifier {
        Some(KittyDeleteSpec::ById { id, .. }) if id != 0 => Some(u64::from(id)),
        _ => None,
    };
    apply_resolved_delete_to_placements(
        cmd,
        resolved_image_id,
        placements,
        (cursor_row, cursor_col),
        (cell_width, cell_height),
        (usize::MAX, usize::MAX),
    )
}

fn apply_resolved_delete_to_placements(
    cmd: &KittyCommand,
    resolved_image_id: Option<ImageId>,
    placements: &mut Vec<ImagePlacement>,
    cursor: (usize, usize),
    cell_size: (f32, f32),
    grid_size: (usize, usize),
) -> HashSet<ImageId> {
    let spec = cmd.delete_specifier.unwrap_or(KittyDeleteSpec::All);

    let removed_image_ids = match spec {
        KittyDeleteSpec::NoOp => HashSet::new(),
        KittyDeleteSpec::All | KittyDeleteSpec::AllImages => {
            remove_matching_kitty_placements(placements, |placement| {
                placement_intersects_grid(
                    placement,
                    grid_size.0,
                    grid_size.1,
                    cell_size.0,
                    cell_size.1,
                )
            })
        }
        KittyDeleteSpec::ById { .. } | KittyDeleteSpec::ByNumber { .. } => {
            let Some(image_id) = resolved_image_id else {
                return HashSet::new();
            };
            let client_placement_id = cmd.placement_id;
            remove_matching_kitty_placements(placements, |placement| {
                placement.image_id == image_id
                    && client_placement_id
                        .map(|expected| placement.client_placement_id == Some(expected))
                        .unwrap_or(true)
            })
        }
        KittyDeleteSpec::AtCursor { .. } => {
            remove_matching_kitty_placements(placements, |placement| {
                placement_intersects_cell(
                    placement,
                    cursor.0,
                    cursor.1,
                    cell_size.0,
                    cell_size.1,
                )
            })
        }
        KittyDeleteSpec::ByColumn { column, .. } => {
            let Some(column) = column
                .checked_sub(1)
                .and_then(|column| usize::try_from(column).ok())
            else {
                return HashSet::new();
            };
            remove_matching_kitty_placements(placements, |placement| {
                placement_intersects_column(placement, column, cell_size.0, cell_size.1)
            })
        }
        KittyDeleteSpec::ByRow { row, .. } => {
            let Some(row) = row.checked_sub(1).and_then(|row| usize::try_from(row).ok()) else {
                return HashSet::new();
            };
            remove_matching_kitty_placements(placements, |placement| {
                placement_intersects_row(placement, row, cell_size.0, cell_size.1)
            })
        }
        KittyDeleteSpec::ByZIndex { z_index, .. } => {
            remove_matching_kitty_placements(placements, |placement| placement.z_index == z_index)
        }
    };

    if !requests_image_data_deletion(spec) {
        return HashSet::new();
    }

    match spec {
        KittyDeleteSpec::ById { .. } | KittyDeleteSpec::ByNumber { .. }
            if cmd.placement_id.is_none() =>
        {
            // Uppercase I/N without p= also target transmitted images that
            // currently have no placement. With p=, an actual removal above
            // is required before the image becomes a deletion candidate.
            resolved_image_id.into_iter().collect()
        }
        _ => removed_image_ids,
    }
}

fn requests_image_data_deletion(spec: KittyDeleteSpec) -> bool {
    matches!(
        spec,
        KittyDeleteSpec::AllImages
            | KittyDeleteSpec::ById {
                delete_data: true,
                ..
            }
            | KittyDeleteSpec::ByNumber {
                delete_data: true,
                ..
            }
            | KittyDeleteSpec::AtCursor { delete_data: true }
            | KittyDeleteSpec::ByColumn {
                delete_data: true,
                ..
            }
            | KittyDeleteSpec::ByRow {
                delete_data: true,
                ..
            }
            | KittyDeleteSpec::ByZIndex {
                delete_data: true,
                ..
            }
    )
}

fn remove_matching_kitty_placements(
    placements: &mut Vec<ImagePlacement>,
    mut matches: impl FnMut(&ImagePlacement) -> bool,
) -> HashSet<ImageId> {
    // Sixel placements use the reserved placement ID zero. Kitty placements
    // are always assigned non-zero IDs, including when the client sends p=0.
    let mut removed_image_ids = HashSet::new();
    placements.retain(|placement| {
        let should_remove = placement.placement_id != 0 && matches(placement);
        if should_remove {
            removed_image_ids.insert(placement.image_id);
        }
        !should_remove
    });
    removed_image_ids
}

fn remove_retransmitted_placements(
    placements: &mut Vec<ImagePlacement>,
    image_id: ImageId,
) {
    let _ = remove_matching_kitty_placements(placements, |placement| {
        placement.image_id == image_id
    });
}

fn placement_intersects_cell(
    placement: &ImagePlacement,
    row: usize,
    column: usize,
    cell_width: f32,
    cell_height: f32,
) -> bool {
    placement_intersects_column(placement, column, cell_width, cell_height)
        && placement_intersects_row(placement, row, cell_width, cell_height)
}

fn placement_intersects_grid(
    placement: &ImagePlacement,
    grid_cols: usize,
    grid_rows: usize,
    cell_width: f32,
    cell_height: f32,
) -> bool {
    let (row, column, columns, rows) =
        placement.mode.effective_cell_rect(cell_width, cell_height);
    columns != 0 && rows != 0 && column < grid_cols && row < grid_rows
}

fn placement_intersects_column(
    placement: &ImagePlacement,
    column: usize,
    cell_width: f32,
    cell_height: f32,
) -> bool {
    let (_, start_column, columns, _) =
        placement.mode.effective_cell_rect(cell_width, cell_height);
    cell_span_intersects(start_column, columns, column)
}

fn placement_intersects_row(
    placement: &ImagePlacement,
    row: usize,
    cell_width: f32,
    cell_height: f32,
) -> bool {
    let (start_row, _, _, rows) =
        placement.mode.effective_cell_rect(cell_width, cell_height);
    cell_span_intersects(start_row, rows, row)
}

fn cell_span_intersects(start: usize, length: u32, target: usize) -> bool {
    let length = usize::try_from(length).unwrap_or(usize::MAX);
    target
        .checked_sub(start)
        .is_some_and(|offset| offset < length)
}

/// How much to advance the cursor after placing an inline image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorAdvance {
    pub rows: usize,
    pub cols: usize,
}

fn cursor_advance_for_policy(
    cursor_movement: u8,
    advance: CursorAdvance,
) -> Option<CursorAdvance> {
    // Kitty C=1 suppresses cursor movement without changing placement geometry.
    (cursor_movement != 1).then_some(advance)
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
    pixel_offsets: (u32, u32),
    render_size: InlineRenderSize,
    grid_size: (usize, usize),
) -> (PlacementMode, CursorAdvance) {
    let (grid_cols, grid_rows) = grid_size;
    let (cols, rows) = bounded_inline_dimensions(display_cols, display_rows, grid_cols, grid_rows);
    (
        PlacementMode::Inline {
            row,
            col,
            cols,
            rows,
            x_offset: pixel_offsets.0,
            y_offset: pixel_offsets.1,
            render_size,
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

fn kitty_response(
    image_id: Option<KittyImageId>,
    image_number: Option<u32>,
    quiet: u8,
    is_error: bool,
    result: &str,
) -> Option<Vec<u8>> {
    if (is_error && quiet >= 2) || (!is_error && quiet >= 1) {
        return None;
    }

    let mut response = String::from("\x1b_G");
    let mut has_identity = false;
    if let Some(image_id) = image_id {
        response.push_str(&format!("i={image_id}"));
        has_identity = true;
    }
    if let Some(image_number) = image_number.filter(|number| *number != 0) {
        if has_identity {
            response.push(',');
        }
        response.push_str(&format!("I={image_number}"));
    }
    response.push(';');
    response.push_str(result);
    response.push_str("\x1b\\");
    Some(response.into_bytes())
}

fn decompression_error_response(
    image_id: KittyImageId,
    quiet: u8,
    error: DecompressionError,
) -> Option<Vec<u8>> {
    decompression_error_response_with_number(image_id, None, quiet, error)
}

fn decompression_error_response_with_number(
    image_id: KittyImageId,
    image_number: Option<u32>,
    quiet: u8,
    error: DecompressionError,
) -> Option<Vec<u8>> {
    let reason = match error {
        DecompressionError::TooLarge => "E2BIG:decompressed image exceeds limit",
        DecompressionError::InvalidData => "EINVAL:invalid zlib stream",
        DecompressionError::AllocationFailed => "ENOMEM:failed to buffer decompressed image",
    };
    log::warn!("Rejected Kitty transmission for image {image_id}: {reason}");
    kitty_response(Some(image_id), image_number, quiet, true, reason)
}

fn query_error_response(image_id: KittyImageId, quiet: u8, reason: &str) -> Option<Vec<u8>> {
    log::warn!("Rejected Kitty query for image {image_id}: {reason}");
    kitty_response(Some(image_id), None, quiet, true, reason)
}

fn valid_cell_offset(offset: u32, cell_extent: f32) -> bool {
    cell_extent.is_finite()
        && cell_extent > 0.0
        && f64::from(offset) < f64::from(cell_extent)
}

fn placement_offsets(
    cmd: &KittyCommand,
    cell_width: f32,
    cell_height: f32,
) -> Result<(u32, u32), PlacementError> {
    let x_offset = cmd.x_offset.unwrap_or(0);
    let y_offset = cmd.y_offset.unwrap_or(0);
    if valid_cell_offset(x_offset, cell_width)
        && valid_cell_offset(y_offset, cell_height)
    {
        Ok((x_offset, y_offset))
    } else {
        Err(PlacementError::InvalidCellOffset)
    }
}

#[cfg(test)]
fn placement_error_response(
    image_id: KittyImageId,
    quiet: u8,
    error: PlacementError,
) -> Option<Vec<u8>> {
    placement_error_response_with_number(image_id, None, quiet, error)
}

fn placement_error_response_with_number(
    image_id: KittyImageId,
    image_number: Option<u32>,
    quiet: u8,
    error: PlacementError,
) -> Option<Vec<u8>> {
    let reason = match error {
        PlacementError::ImageNotFound => "ENOENT:image not found",
        PlacementError::InvalidCellOffset => "EINVAL:placement offset exceeds cell bounds",
    };
    log::warn!("Rejected Kitty placement for image {image_id}: {reason}");
    let response_image_id = if image_number.is_some() && image_id == 0 {
        None
    } else {
        Some(image_id)
    };
    kitty_response(response_image_id, image_number, quiet, true, reason)
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

fn file_load_error_response(
    image_id: KittyImageId,
    quiet: u8,
    error: FileLoadError,
) -> Option<Vec<u8>> {
    file_load_error_response_with_number(image_id, None, quiet, error)
}

fn file_load_error_response_with_number(
    image_id: KittyImageId,
    image_number: Option<u32>,
    quiet: u8,
    error: FileLoadError,
) -> Option<Vec<u8>> {
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
    kitty_response(Some(image_id), image_number, quiet, true, reason)
}

#[cfg(test)]
mod tests {
    use super::{
        append_bounded, apply_delete_to_placements,
        apply_resolved_delete_to_placements, decompression_error_response,
        client_image_registry_needs_pruning,
        cursor_advance_for_policy, file_load_error_response, inline_placement_and_advance,
        image_number_registry_needs_pruning,
        is_safe_read_path, is_safe_temp_delete_path, load_file_data,
        kitty_response, maybe_decompress_with_limit, normalized_path_entry,
        path_is_within_roots, placement_error_response,
        placement_error_response_with_number, placement_offsets, read_regular_file,
        remove_retransmitted_placements,
        ChunkAssembler, ChunkAssemblyErrorKind, CompletedImage,
        DecompressionError, FileLoadError, ImageFormat, ImageStore, KittyHandler,
        KittyHandlerOptions, PlacementError, TempFileDeletion,
        update_image_number_association, DECOMPRESSION_CHUNK_BYTES,
        MAX_PENDING_IMAGE_BYTES,
    };
    use crate::graphics::{
        resolve_kitty_placement_layout, retain_unreferenced_image_ids, ImageId,
        ImagePlacement, InlineRenderSize, PlacementMode,
    };
    #[cfg(target_os = "macos")]
    use crate::grid::Grid;
    use crate::parser::kitty_graphics::{
        KittyAction, KittyCommand, KittyCompression, KittyDeleteSpec, KittyFormat,
        KittyTransmission,
    };
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    #[cfg(target_os = "macos")]
    use objc2_metal::MTLCreateSystemDefaultDevice;
    use std::borrow::Cow;
    use std::collections::HashSet;
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

    #[cfg(target_os = "macos")]
    fn process_graphics_outcome(
        handler: &mut KittyHandler,
        command: KittyCommand,
        store: &mut ImageStore,
        placements: &mut Vec<ImagePlacement>,
    ) -> super::KittyProcessOutcome {
        handler.process(
            command,
            store,
            0,
            0,
            10.0,
            20.0,
            80,
            24,
            placements,
        )
    }

    #[cfg(target_os = "macos")]
    fn process_graphics(
        handler: &mut KittyHandler,
        command: KittyCommand,
        store: &mut ImageStore,
        placements: &mut Vec<ImagePlacement>,
    ) -> (Option<Vec<u8>>, Option<super::CursorAdvance>) {
        let outcome = process_graphics_outcome(handler, command, store, placements);
        (outcome.response, outcome.advance)
    }

    fn direct_query(image_id: Option<u32>, payload: &[u8]) -> KittyCommand {
        KittyCommand {
            action: KittyAction::Query,
            image_id,
            width: Some(1),
            height: Some(1),
            payload: payload.to_vec(),
            ..KittyCommand::default()
        }
    }

    fn inline_image(
        image_id: ImageId,
        placement_id: u32,
        row: usize,
        col: usize,
        rows: u32,
        cols: u32,
    ) -> ImagePlacement {
        ImagePlacement {
            image_id,
            placement_id,
            client_placement_id: (placement_id != 0).then_some(placement_id),
            mode: PlacementMode::Inline {
                row,
                col,
                cols,
                rows,
                x_offset: 0,
                y_offset: 0,
                render_size: InlineRenderSize::CellAnchored,
            },
            z_index: 0,
        }
    }

    fn offset_inline_image(
        image_id: ImageId,
        x_offset: u32,
        y_offset: u32,
    ) -> ImagePlacement {
        let mut placement = inline_image(image_id, 1, 0, 0, 1, 1);
        let PlacementMode::Inline {
            x_offset: placement_x_offset,
            y_offset: placement_y_offset,
            ..
        } = &mut placement.mode;
        *placement_x_offset = x_offset;
        *placement_y_offset = y_offset;
        placement
    }

    fn delete_command(specifier: KittyDeleteSpec) -> KittyCommand {
        KittyCommand {
            action: KittyAction::Delete,
            delete_specifier: Some(specifier),
            ..KittyCommand::default()
        }
    }

    #[test]
    fn direct_query_validates_without_an_image_store() {
        let handler = KittyHandler::new(file_options(64, true));
        let cmd = direct_query(Some(17), &[255, 0, 0, 255]);

        let (response, advance) = handler.handle_query(&cmd);

        assert_eq!(response, Some(b"\x1b_Gi=17;OK\x1b\\".to_vec()));
        assert!(advance.is_none());
    }

    #[test]
    fn query_requires_a_nonzero_image_id() {
        let handler = KittyHandler::new(file_options(64, true));
        let mut image_number_only = direct_query(None, &[255, 0, 0, 255]);
        image_number_only.image_number = Some(7);

        for cmd in [
            direct_query(None, &[255, 0, 0, 255]),
            direct_query(Some(0), &[255, 0, 0, 255]),
            image_number_only,
        ] {
            let (response, advance) = handler.handle_query(&cmd);
            assert!(response.is_none());
            assert!(advance.is_none());
        }
    }

    #[test]
    fn query_rejects_an_image_id_combined_with_an_image_number() {
        let handler = KittyHandler::new(file_options(64, true));
        for image_number in [0, 9] {
            let mut cmd = direct_query(Some(17), &[255, 0, 0, 255]);
            cmd.image_number = Some(image_number);

            let (response, advance) = handler.handle_query(&cmd);

            assert_eq!(
                response,
                Some(
                    format!(
                        "\x1b_Gi=17{};EINVAL:image ID and image number are mutually exclusive\x1b\\",
                        if image_number == 0 {
                            String::new()
                        } else {
                            format!(",I={image_number}")
                        }
                    )
                    .into_bytes()
                )
            );
            assert!(advance.is_none());

            cmd.quiet = 2;
            assert!(handler.handle_query(&cmd).0.is_none());
        }
    }

    #[test]
    fn query_rejects_empty_data_and_missing_raw_dimensions() {
        let handler = KittyHandler::new(file_options(64, true));
        let empty = direct_query(Some(21), &[]);
        assert_eq!(
            handler.handle_query(&empty).0,
            Some(b"\x1b_Gi=21;ENODATA:missing image data\x1b\\".to_vec())
        );

        for format in [KittyFormat::Rgb, KittyFormat::Rgba] {
            for (width, height) in [
                (None, Some(1)),
                (Some(1), None),
                (Some(0), Some(1)),
                (Some(1), Some(0)),
            ] {
                let mut missing_dimensions = direct_query(Some(22), &[255, 0, 0, 255]);
                missing_dimensions.format = format;
                missing_dimensions.width = width;
                missing_dimensions.height = height;
                assert_eq!(
                    handler.handle_query(&missing_dimensions).0,
                    Some(b"\x1b_Gi=22;EINVAL:missing image dimensions\x1b\\".to_vec())
                );
            }
        }
    }

    #[test]
    fn query_requires_exact_raw_payload_sizes() {
        let handler = KittyHandler::new(file_options(64, true));
        let valid_rgb = KittyCommand {
            format: KittyFormat::Rgb,
            ..direct_query(Some(23), &[255, 0, 0])
        };
        assert_eq!(
            handler.handle_query(&valid_rgb).0,
            Some(b"\x1b_Gi=23;OK\x1b\\".to_vec())
        );

        let trailing_rgba = direct_query(Some(24), &[255, 0, 0, 255, 1]);
        assert_eq!(
            handler.handle_query(&trailing_rgba).0,
            Some(b"\x1b_Gi=24;EINVAL:image data exceeds declared dimensions\x1b\\".to_vec())
        );

        let short_rgb = KittyCommand {
            format: KittyFormat::Rgb,
            ..direct_query(Some(25), &[255, 0])
        };
        assert_eq!(
            handler.handle_query(&short_rgb).0,
            Some(b"\x1b_Gi=25;ENODATA:insufficient image data\x1b\\".to_vec())
        );
    }

    #[test]
    fn query_quiet_levels_suppress_only_the_expected_responses() {
        let handler = KittyHandler::new(file_options(64, true));
        let mut valid = direct_query(Some(31), &[255, 0, 0, 255]);
        valid.quiet = 1;
        assert!(handler.handle_query(&valid).0.is_none());

        let mut invalid = direct_query(Some(32), &[]);
        invalid.quiet = 1;
        assert_eq!(
            handler.handle_query(&invalid).0,
            Some(b"\x1b_Gi=32;ENODATA:missing image data\x1b\\".to_vec())
        );

        invalid.quiet = 2;
        assert!(handler.handle_query(&invalid).0.is_none());
    }

    #[test]
    fn numbered_responses_include_both_identities_and_honor_quiet_levels() {
        let success = b"\x1b_Gi=41,I=7;OK\x1b\\".to_vec();
        assert_eq!(
            kitty_response(Some(41), Some(7), 0, false, "OK"),
            Some(success)
        );
        assert!(kitty_response(Some(41), Some(7), 1, false, "OK").is_none());

        let error = b"\x1b_Gi=41,I=7;EINVAL:rejected\x1b\\".to_vec();
        for quiet in [0, 1] {
            assert_eq!(
                kitty_response(Some(41), Some(7), quiet, true, "EINVAL:rejected"),
                Some(error.clone())
            );
        }
        assert!(
            kitty_response(Some(41), Some(7), 2, true, "EINVAL:rejected").is_none()
        );
        assert_eq!(
            kitty_response(None, Some(7), 0, true, "ENOENT:image not found"),
            Some(b"\x1b_GI=7;ENOENT:image not found\x1b\\".to_vec())
        );
        assert_eq!(
            kitty_response(None, None, 0, true, "EINVAL:invalid image selector"),
            Some(b"\x1b_G;EINVAL:invalid image selector\x1b\\".to_vec())
        );
        assert_eq!(
            kitty_response(Some(41), Some(0), 0, true, "EINVAL:rejected"),
            Some(b"\x1b_Gi=41;EINVAL:rejected\x1b\\".to_vec())
        );
        assert_eq!(
            placement_error_response_with_number(
                0,
                Some(7),
                0,
                PlacementError::ImageNotFound,
            ),
            Some(b"\x1b_GI=7;ENOENT:image not found\x1b\\".to_vec())
        );
    }

    #[test]
    fn image_number_namespaces_are_isolated_per_handler() {
        let mut first = KittyHandler::new(file_options(64, true));
        let mut second = KittyHandler::new(file_options(64, true));
        first.client_images.record(7, 41);
        second.client_images.record(7, 42);
        first.image_numbers.record_new(7, 7);

        assert_eq!(first.client_images.get(7), Some(41));
        assert_eq!(second.client_images.get(7), Some(42));
        assert_eq!(
            first.image_numbers.newest_existing(7, |_| true),
            Some(7)
        );
        assert_eq!(second.image_numbers.newest_existing(7, |_| true), None);
    }

    #[test]
    fn retransmission_preserves_a_live_alias_but_id_reuse_clears_it() {
        let mut registry = crate::graphics::ImageNumberRegistry::default();
        registry.record_new(7, 41);

        update_image_number_association(&mut registry, 41, None, true);
        assert_eq!(registry.newest_existing(7, |_| true), Some(41));

        update_image_number_association(&mut registry, 41, None, false);
        assert_eq!(registry.newest_existing(7, |_| true), None);

        update_image_number_association(&mut registry, 41, Some(8), false);
        assert_eq!(registry.newest_existing(8, |_| true), Some(41));
    }

    #[test]
    fn image_number_pruning_threshold_includes_store_size_and_stale_slack() {
        assert!(!image_number_registry_needs_pruning(32, 0));
        assert!(image_number_registry_needs_pruning(33, 0));
        assert!(!image_number_registry_needs_pruning(42, 10));
        assert!(image_number_registry_needs_pruning(43, 10));
    }

    #[test]
    fn client_image_pruning_threshold_preserves_placement_bindings() {
        assert!(!client_image_registry_needs_pruning(42, 10, 0));
        assert!(client_image_registry_needs_pruning(43, 10, 0));
        assert!(!client_image_registry_needs_pruning(44, 10, 2));
        assert!(client_image_registry_needs_pruning(45, 10, 2));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn kitty_retransmission_in_alt_cleans_both_screens_but_preserves_sixel() {
        let Some(device) = MTLCreateSystemDefaultDevice() else {
            eprintln!("skipping Metal integration test: no device is available");
            return;
        };
        let mut store = ImageStore::new(device, 1);
        let mut handler = KittyHandler::new(file_options(64, true));
        let mut grid = Grid::new(80, 24, 0);

        let initial = KittyCommand {
            action: KittyAction::TransmitAndPlace,
            image_id: Some(7),
            width: Some(1),
            height: Some(1),
            columns: Some(1),
            rows: Some(1),
            payload: vec![255, 0, 0, 255],
            ..KittyCommand::default()
        };
        let outcome = process_graphics_outcome(
            &mut handler,
            initial,
            &mut store,
            &mut grid.image_placements,
        );
        assert_eq!(outcome.retransmitted_image_id, None);
        let stored_image_id = handler.client_images.get(7).unwrap();
        grid.image_placements
            .push(inline_image(stored_image_id, 0, 0, 0, 1, 1));

        grid.enter_alt_screen();
        let place_on_alt = KittyCommand {
            action: KittyAction::Place,
            image_id: Some(7),
            columns: Some(1),
            rows: Some(1),
            ..KittyCommand::default()
        };
        process_graphics_outcome(
            &mut handler,
            place_on_alt,
            &mut store,
            &mut grid.image_placements,
        );
        grid.image_placements
            .push(inline_image(stored_image_id, 0, 0, 0, 1, 1));

        let failed = KittyCommand {
            image_id: Some(7),
            payload: vec![0, 255, 0, 255],
            ..KittyCommand::default()
        };
        let failed_outcome = process_graphics_outcome(
            &mut handler,
            failed,
            &mut store,
            &mut grid.image_placements,
        );
        assert_eq!(failed_outcome.retransmitted_image_id, None);
        assert_eq!(grid.all_image_placements().count(), 4);

        let replacement = KittyCommand {
            action: KittyAction::TransmitAndPlace,
            image_id: Some(7),
            width: Some(1),
            height: Some(1),
            columns: Some(1),
            rows: Some(1),
            payload: vec![0, 0, 255, 255],
            ..KittyCommand::default()
        };
        let replacement_outcome = process_graphics_outcome(
            &mut handler,
            replacement,
            &mut store,
            &mut grid.image_placements,
        );
        assert_eq!(
            replacement_outcome.retransmitted_image_id,
            Some(stored_image_id)
        );
        grid.remove_hidden_primary_kitty_placements(stored_image_id);

        assert_eq!(
            grid.image_placements
                .iter()
                .filter(|placement| placement.placement_id != 0)
                .count(),
            1
        );
        assert_eq!(
            grid.all_image_placements()
                .filter(|placement| placement.placement_id == 0)
                .count(),
            2
        );
        assert_eq!(
            grid.all_image_placements()
                .filter(|placement| placement.placement_id != 0)
                .count(),
            1
        );

        grid.leave_alt_screen();
        assert_eq!(grid.image_placements.len(), 1);
        assert_eq!(grid.image_placements[0].placement_id, 0);
        assert_eq!(grid.image_placements[0].image_id, stored_image_id);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn explicit_image_ids_are_isolated_between_handlers_sharing_a_store() {
        let Some(device) = MTLCreateSystemDefaultDevice() else {
            eprintln!("skipping Metal integration test: no device is available");
            return;
        };
        let mut store = ImageStore::new(device, 1);
        let sixel_id = store
            .store(
                &[255, 255, 0, 255, 255, 0, 255, 255, 255, 0, 255, 255],
                3,
                1,
                ImageFormat::Rgba,
                Some(7),
            )
            .unwrap();
        assert_eq!(sixel_id, 7);
        let mut first = KittyHandler::new(file_options(64, true));
        let mut second = KittyHandler::new(file_options(64, true));
        let mut first_placements = Vec::new();
        let mut second_placements = Vec::new();

        let foreign_placement = KittyCommand {
            action: KittyAction::Place,
            image_id: Some(7),
            columns: Some(1),
            rows: Some(1),
            ..KittyCommand::default()
        };
        assert_eq!(
            process_graphics(
                &mut first,
                foreign_placement,
                &mut store,
                &mut first_placements,
            )
            .0,
            Some(b"\x1b_Gi=7;ENOENT:image not found\x1b\\".to_vec())
        );
        assert!(first_placements.is_empty());

        let first_transmission = KittyCommand {
            image_id: Some(7),
            width: Some(1),
            height: Some(1),
            payload: vec![255, 0, 0, 255],
            ..KittyCommand::default()
        };
        assert_eq!(
            process_graphics(
                &mut first,
                first_transmission,
                &mut store,
                &mut first_placements,
            )
            .0,
            Some(b"\x1b_Gi=7;OK\x1b\\".to_vec())
        );

        let second_transmission = KittyCommand {
            image_id: Some(7),
            width: Some(2),
            height: Some(1),
            payload: vec![0, 255, 0, 255, 0, 0, 255, 255],
            ..KittyCommand::default()
        };
        assert_eq!(
            process_graphics(
                &mut second,
                second_transmission,
                &mut store,
                &mut second_placements,
            )
            .0,
            Some(b"\x1b_Gi=7;OK\x1b\\".to_vec())
        );

        let first_stored_id = first.client_images.get(7).unwrap();
        let second_stored_id = second.client_images.get(7).unwrap();
        assert_ne!(first_stored_id, second_stored_id);
        assert_eq!(store.image_count(), 3);
        assert_eq!(
            store.get(sixel_id).map(|image| (image.width, image.height)),
            Some((3, 1))
        );
        assert_eq!(
            store.get(first_stored_id).map(|image| (image.width, image.height)),
            Some((1, 1))
        );
        assert_eq!(
            store.get(second_stored_id).map(|image| (image.width, image.height)),
            Some((2, 1))
        );

        let placement = KittyCommand {
            action: KittyAction::Place,
            image_id: Some(7),
            columns: Some(1),
            rows: Some(1),
            ..KittyCommand::default()
        };
        process_graphics(
            &mut first,
            placement.clone(),
            &mut store,
            &mut first_placements,
        );
        process_graphics(
            &mut second,
            placement,
            &mut store,
            &mut second_placements,
        );
        assert_eq!(
            first_placements.last().map(|placement| placement.image_id),
            Some(first_stored_id)
        );
        assert_eq!(
            second_placements.last().map(|placement| placement.image_id),
            Some(second_stored_id)
        );

        let failed_retransmission = KittyCommand {
            image_id: Some(7),
            payload: vec![255, 0, 0, 255],
            ..KittyCommand::default()
        };
        let failed_outcome = process_graphics_outcome(
            &mut first,
            failed_retransmission,
            &mut store,
            &mut first_placements,
        );
        assert_eq!(
            failed_outcome.response,
            Some(b"\x1b_Gi=7;EINVAL:missing dimensions\x1b\\".to_vec())
        );
        assert_eq!(failed_outcome.retransmitted_image_id, None);
        assert_eq!(first_placements.len(), 1);
        assert_eq!(
            store.get(first_stored_id).map(|image| (image.width, image.height)),
            Some((1, 1))
        );

        let successful_retransmission = KittyCommand {
            action: KittyAction::TransmitAndPlace,
            image_id: Some(7),
            placement_id: Some(91),
            width: Some(1),
            height: Some(2),
            columns: Some(2),
            rows: Some(3),
            x_offset: Some(4),
            y_offset: Some(5),
            z_index: Some(6),
            payload: vec![255, 0, 0, 255, 0, 0, 255, 255],
            ..KittyCommand::default()
        };
        let successful_outcome = process_graphics_outcome(
            &mut first,
            successful_retransmission,
            &mut store,
            &mut first_placements,
        );
        assert_eq!(
            successful_outcome.response,
            Some(b"\x1b_Gi=7;OK\x1b\\".to_vec())
        );
        assert_eq!(
            successful_outcome.retransmitted_image_id,
            Some(first_stored_id)
        );
        assert_eq!(first_placements.len(), 1);
        assert_eq!(second_placements.len(), 1);
        let replacement = &first_placements[0];
        assert_eq!(replacement.image_id, first_stored_id);
        assert_eq!(replacement.client_placement_id, Some(91));
        assert_eq!(replacement.z_index, 6);
        assert!(matches!(
            replacement.mode,
            PlacementMode::Inline {
                row: 0,
                col: 0,
                cols: 2,
                rows: 3,
                x_offset: 4,
                y_offset: 5,
                render_size: InlineRenderSize::CellAnchored,
            }
        ));
        assert_eq!(
            store.get(first_stored_id).map(|image| (image.width, image.height)),
            Some((1, 2))
        );
        assert_eq!(
            store.get(second_stored_id).map(|image| (image.width, image.height)),
            Some((2, 1))
        );

        let delete_first = KittyCommand {
            action: KittyAction::Delete,
            image_id: Some(7),
            delete_specifier: Some(KittyDeleteSpec::ById {
                id: 7,
                delete_data: false,
            }),
            ..KittyCommand::default()
        };
        store.remove(first_stored_id);
        process_graphics(
            &mut first,
            delete_first,
            &mut store,
            &mut first_placements,
        );
        assert!(first_placements.is_empty());
        assert_eq!(second_placements.len(), 1);
        assert_eq!(store.image_count(), 2);
        assert_eq!(
            store.get(sixel_id).map(|image| (image.width, image.height)),
            Some((3, 1))
        );
        assert!(store.get(second_stored_id).is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn image_numbers_translate_client_ids_and_retain_evicted_placements_for_delete() {
        let Some(device) = MTLCreateSystemDefaultDevice() else {
            eprintln!("skipping Metal integration test: no device is available");
            return;
        };
        let mut store = ImageStore::new(device, 1);
        let foreign_id = store
            .store(
                &[255, 255, 0, 255],
                1,
                1,
                ImageFormat::Rgba,
                Some(7),
            )
            .unwrap();
        let mut handler = KittyHandler::new(file_options(64, true));
        let mut placements = Vec::new();

        let transmission = KittyCommand {
            image_number: Some(9),
            width: Some(1),
            height: Some(1),
            payload: vec![255, 0, 0, 255],
            ..KittyCommand::default()
        };
        assert_eq!(
            process_graphics(
                &mut handler,
                transmission,
                &mut store,
                &mut placements,
            )
            .0,
            Some(b"\x1b_Gi=1,I=9;OK\x1b\\".to_vec())
        );

        let place = KittyCommand {
            action: KittyAction::Place,
            image_number: Some(9),
            columns: Some(1),
            rows: Some(1),
            ..KittyCommand::default()
        };
        assert_eq!(
            process_graphics(
                &mut handler,
                place.clone(),
                &mut store,
                &mut placements,
            )
            .0,
            Some(b"\x1b_Gi=1,I=9;OK\x1b\\".to_vec())
        );
        let stored_image_id = placements.last().unwrap().image_id;
        assert_ne!(stored_image_id, 1);
        assert_ne!(stored_image_id, foreign_id);

        store.remove(stored_image_id);
        assert_eq!(
            process_graphics(
                &mut handler,
                place,
                &mut store,
                &mut placements,
            )
            .0,
            Some(b"\x1b_GI=9;ENOENT:image not found\x1b\\".to_vec())
        );
        assert_eq!(placements.len(), 1);

        let delete = KittyCommand {
            action: KittyAction::Delete,
            image_number: Some(9),
            delete_specifier: Some(KittyDeleteSpec::ByNumber {
                number: 9,
                delete_data: false,
            }),
            ..KittyCommand::default()
        };
        process_graphics(
            &mut handler,
            delete,
            &mut store,
            &mut placements,
        );
        assert!(placements.is_empty());
        assert!(store.get(foreign_id).is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn uppercase_by_number_deletes_unreferenced_newest_generation_and_falls_back() {
        let Some(device) = MTLCreateSystemDefaultDevice() else {
            eprintln!("skipping Metal integration test: no device is available");
            return;
        };
        let mut store = ImageStore::new(device, 1);
        let mut handler = KittyHandler::new(file_options(64, true));
        let mut placements = Vec::new();

        for (expected_id, pixel) in [(1, [255, 0, 0, 255]), (2, [0, 255, 0, 255])] {
            let command = KittyCommand {
                image_number: Some(7),
                width: Some(1),
                height: Some(1),
                payload: pixel.to_vec(),
                ..KittyCommand::default()
            };
            assert_eq!(
                process_graphics(&mut handler, command, &mut store, &mut placements).0,
                Some(format!("\x1b_Gi={expected_id},I=7;OK\x1b\\").into_bytes())
            );
        }
        assert!(store.get(1).is_some());
        assert!(store.get(2).is_some());

        let place_newest = KittyCommand {
            action: KittyAction::Place,
            image_number: Some(7),
            columns: Some(1),
            rows: Some(1),
            ..KittyCommand::default()
        };
        assert_eq!(
            process_graphics(
                &mut handler,
                place_newest.clone(),
                &mut store,
                &mut placements,
            )
            .0,
            Some(b"\x1b_Gi=2,I=7;OK\x1b\\".to_vec())
        );
        assert_eq!(placements.last().map(|placement| placement.image_id), Some(2));

        let place_oldest = KittyCommand {
            action: KittyAction::Place,
            image_id: Some(1),
            columns: Some(1),
            rows: Some(1),
            ..KittyCommand::default()
        };
        process_graphics(
            &mut handler,
            place_oldest,
            &mut store,
            &mut placements,
        );
        assert_eq!(placements.last().map(|placement| placement.image_id), Some(1));

        let mut other_handler = KittyHandler::new(file_options(64, true));
        let mut other_placements = Vec::new();
        assert_eq!(
            process_graphics(
                &mut other_handler,
                place_newest.clone(),
                &mut store,
                &mut other_placements,
            )
            .0,
            Some(b"\x1b_GI=7;ENOENT:image not found\x1b\\".to_vec())
        );
        assert!(other_placements.is_empty());

        let delete_newest = KittyCommand {
            action: KittyAction::Delete,
            image_number: Some(7),
            delete_specifier: Some(KittyDeleteSpec::ByNumber {
                number: 7,
                delete_data: true,
            }),
            ..KittyCommand::default()
        };
        let outcome = process_graphics_outcome(
            &mut handler,
            delete_newest,
            &mut store,
            &mut placements,
        );
        assert_eq!(outcome.hard_delete_candidates, HashSet::from([2]));
        assert_eq!(
            placements
                .iter()
                .map(|placement| placement.image_id)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert!(store.get(1).is_some());

        let mut candidates = outcome.hard_delete_candidates;
        retain_unreferenced_image_ids(
            &mut candidates,
            placements.iter().chain(other_placements.iter()),
        );
        assert_eq!(candidates, HashSet::from([2]));
        for image_id in candidates {
            store.remove(image_id);
        }
        assert!(store.get(2).is_none());

        assert_eq!(
            process_graphics(
                &mut handler,
                place_newest,
                &mut store,
                &mut placements,
            )
            .0,
            Some(b"\x1b_Gi=1,I=7;OK\x1b\\".to_vec())
        );
        assert_eq!(placements.last().map(|placement| placement.image_id), Some(1));

        let failed_transmission = KittyCommand {
            image_number: Some(8),
            payload: vec![255, 0, 0, 255],
            ..KittyCommand::default()
        };
        assert_eq!(
            process_graphics(
                &mut handler,
                failed_transmission,
                &mut store,
                &mut placements,
            )
            .0,
            Some(b"\x1b_Gi=3,I=8;EINVAL:missing dimensions\x1b\\".to_vec())
        );
        let missing_number = KittyCommand {
            action: KittyAction::Place,
            image_number: Some(8),
            ..KittyCommand::default()
        };
        assert_eq!(
            process_graphics(
                &mut handler,
                missing_number,
                &mut store,
                &mut placements,
            )
            .0,
            Some(b"\x1b_GI=8;ENOENT:image not found\x1b\\".to_vec())
        );
    }

    #[test]
    fn placement_offsets_must_stay_inside_positive_finite_cells() {
        for cursor_movement in [0, 1] {
            let mut command = KittyCommand {
                cursor_movement: Some(cursor_movement),
                x_offset: Some(9),
                y_offset: Some(19),
                ..KittyCommand::default()
            };
            assert_eq!(placement_offsets(&command, 10.0, 20.0), Ok((9, 19)));

            command.x_offset = Some(10);
            assert_eq!(
                placement_offsets(&command, 10.0, 20.0),
                Err(PlacementError::InvalidCellOffset)
            );

            command.x_offset = Some(9);
            command.y_offset = Some(20);
            assert_eq!(
                placement_offsets(&command, 10.0, 20.0),
                Err(PlacementError::InvalidCellOffset)
            );
        }

        let command = KittyCommand::default();
        for invalid_width in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                placement_offsets(&command, invalid_width, 20.0),
                Err(PlacementError::InvalidCellOffset)
            );
        }
        assert_eq!(placement_offsets(&command, 0.5, 0.5), Ok((0, 0)));
    }

    #[test]
    fn placement_errors_honor_quiet_level_and_preserve_image_id() {
        let invalid_offset =
            b"\x1b_Gi=41;EINVAL:placement offset exceeds cell bounds\x1b\\".to_vec();
        assert_eq!(
            placement_error_response(41, 0, PlacementError::InvalidCellOffset),
            Some(invalid_offset.clone())
        );
        assert_eq!(
            placement_error_response(41, 1, PlacementError::InvalidCellOffset),
            Some(invalid_offset)
        );
        assert_eq!(
            placement_error_response(41, 2, PlacementError::InvalidCellOffset),
            None
        );
        assert_eq!(
            placement_error_response(42, 0, PlacementError::ImageNotFound),
            Some(b"\x1b_Gi=42;ENOENT:image not found\x1b\\".to_vec())
        );
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

    fn rgba_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header should encode");
            writer
                .write_image_data(pixels)
                .expect("PNG pixels should encode");
        }
        encoded
    }

    #[test]
    fn png_query_uses_embedded_dimensions() {
        let handler = KittyHandler::new(file_options(1024, true));
        let cmd = KittyCommand {
            format: KittyFormat::Png,
            width: None,
            height: None,
            ..direct_query(Some(41), &rgba_png(1, 1, &[255, 0, 0, 255]))
        };

        assert_eq!(
            handler.handle_query(&cmd).0,
            Some(b"\x1b_Gi=41;OK\x1b\\".to_vec())
        );
    }

    #[test]
    fn png_query_rejects_empty_and_corrupt_data() {
        let handler = KittyHandler::new(file_options(1024, true));
        for (payload, expected) in [
            (
                Vec::new(),
                b"\x1b_Gi=42;ENODATA:missing image data\x1b\\".as_slice(),
            ),
            (
                b"not a PNG".to_vec(),
                b"\x1b_Gi=42;EINVAL:invalid or oversized image data\x1b\\".as_slice(),
            ),
        ] {
            let cmd = KittyCommand {
                format: KittyFormat::Png,
                ..direct_query(Some(42), &payload)
            };
            assert_eq!(handler.handle_query(&cmd).0, Some(expected.to_vec()));
        }
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
    fn numbered_chunks_allocate_once_and_preserve_the_first_identity() {
        let mut assembler = ChunkAssembler::new(64);
        let mut allocations = 0;
        let mut first = chunk(b"one", true, None);
        first.image_number = Some(7);
        assembler
            .push(first, false, || {
                allocations += 1;
                41
            })
            .expect("numbered transmission should start");

        let mut second = chunk(b"-two", true, None);
        second.image_number = Some(7);
        assembler
            .push(second, false, || {
                allocations += 1;
                42
            })
            .expect("matching image number should continue");

        let image = complete(
            assembler
                .push(chunk(b"-three", false, None), false, || {
                    allocations += 1;
                    43
                })
                .expect("selector-free final chunk should complete"),
        );
        assert_eq!(allocations, 1);
        assert_eq!(image.image_id, 41);
        assert_eq!(image.metadata.image_number, Some(7));
        assert_eq!(image.data, b"one-two-three");

        let mut next_generation = chunk(b"new", false, None);
        next_generation.image_number = Some(7);
        let image = complete(
            assembler
                .push(next_generation, false, || {
                    allocations += 1;
                    42
                })
                .expect("the same number should create a fresh generation"),
        );
        assert_eq!(allocations, 2);
        assert_eq!(image.image_id, 42);
    }

    #[test]
    fn numbered_chunks_reject_divergent_numbers_and_selector_switches() {
        let mut assembler = ChunkAssembler::new(64);
        let mut first = chunk(b"old", true, None);
        first.image_number = Some(7);
        assembler
            .push(first, false, || 41)
            .expect("numbered transmission should start");

        let mut divergent = chunk(b"new", false, None);
        divergent.image_number = Some(8);
        let error = assembler
            .push(divergent, false, || 42)
            .expect_err("a divergent image number must abort the transmission");
        assert_eq!(error.image_id, 41);
        assert_eq!(error.image_number, Some(7));
        assert_eq!(
            error.kind,
            ChunkAssemblyErrorKind::InterleavedNumber {
                received_number: 8,
            }
        );
        assert!(assembler.pending.is_none());

        let mut first = chunk(b"old", true, None);
        first.image_number = Some(7);
        assembler
            .push(first, false, || 43)
            .expect("second numbered transmission should start");
        let error = assembler
            .push(chunk(b"new", false, Some(43)), false, || 44)
            .expect_err("switching from image number to image ID must abort");
        assert_eq!(error.image_id, 43);
        assert_eq!(error.image_number, Some(7));
        assert_eq!(
            error.kind,
            ChunkAssemblyErrorKind::Interleaved { received_id: 43 }
        );
        assert!(assembler.pending.is_none());
    }

    #[test]
    fn zero_image_number_is_absent_from_chunk_errors() {
        let mut assembler = ChunkAssembler::new(1);
        let mut command = chunk(b"too large", false, None);
        command.image_number = Some(0);

        let error = assembler
            .push(command, false, || 41)
            .expect_err("oversized chunk should be rejected");

        assert_eq!(error.image_id, 41);
        assert_eq!(error.image_number, None);
        assert_eq!(error.kind, ChunkAssemblyErrorKind::TooLarge);
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
    fn zero_image_id_uses_a_fresh_nonzero_allocation() {
        let mut assembler = ChunkAssembler::new(64);
        let image = complete(
            assembler
                .push(chunk(b"rgba", false, Some(0)), false, || 73)
                .expect("zero must be treated as an unspecified image ID"),
        );

        assert_eq!(image.image_id, 73);
    }

    #[test]
    fn delete_by_id_honors_the_optional_placement_id() {
        let mut placements = vec![
            inline_image(7, 1, 0, 0, 1, 1),
            inline_image(7, 2, 1, 1, 1, 1),
            inline_image(8, 1, 2, 2, 1, 1),
        ];
        let mut cmd = delete_command(KittyDeleteSpec::ById {
            id: 7,
            delete_data: false,
        });
        cmd.placement_id = Some(1);

        let candidates =
            apply_delete_to_placements(&cmd, &mut placements, 0, 0, 10.0, 20.0);

        assert!(candidates.is_empty());
        assert_eq!(
            placements
                .iter()
                .map(|placement| (placement.image_id, placement.placement_id))
                .collect::<Vec<_>>(),
            vec![(7, 2), (8, 1)]
        );
    }

    #[test]
    fn client_placement_ids_never_match_synthetic_local_ids() {
        let mut implicit = inline_image(19, 1, 0, 0, 1, 1);
        implicit.client_placement_id = None;
        let mut explicit = inline_image(19, 2, 1, 1, 1, 1);
        explicit.client_placement_id = Some(1);
        let mut placements = vec![implicit, explicit];
        let mut cmd = delete_command(KittyDeleteSpec::ById {
            id: 19,
            delete_data: false,
        });
        cmd.placement_id = Some(1);

        apply_delete_to_placements(&cmd, &mut placements, 0, 0, 10.0, 20.0);

        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].placement_id, 1);
        assert!(placements[0].client_placement_id.is_none());
    }

    #[test]
    fn uppercase_id_delete_uses_the_same_safe_placement_filter() {
        let mut placements = vec![
            inline_image(7, 1, 0, 0, 1, 1),
            inline_image(7, 2, 1, 1, 1, 1),
        ];
        let mut cmd = delete_command(KittyDeleteSpec::ById {
            id: 7,
            delete_data: true,
        });
        cmd.placement_id = Some(1);

        let mut candidates =
            apply_delete_to_placements(&cmd, &mut placements, 0, 0, 10.0, 20.0);
        assert_eq!(candidates, HashSet::from([7]));
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].placement_id, 2);
        retain_unreferenced_image_ids(&mut candidates, &placements);
        assert!(candidates.is_empty());

        cmd.placement_id = Some(99);
        let candidates =
            apply_delete_to_placements(&cmd, &mut placements, 0, 0, 10.0, 20.0);
        assert!(candidates.is_empty());
        assert_eq!(placements.len(), 1);

        cmd.placement_id = Some(2);
        let candidates =
            apply_delete_to_placements(&cmd, &mut placements, 0, 0, 10.0, 20.0);
        assert_eq!(candidates, HashSet::from([7]));
        assert!(placements.is_empty());
    }

    #[test]
    fn uppercase_explicit_delete_can_candidate_an_unplaced_image() {
        for specifier in [
            KittyDeleteSpec::ById {
                id: 7,
                delete_data: true,
            },
            KittyDeleteSpec::ByNumber {
                number: 7,
                delete_data: true,
            },
        ] {
            let cmd = delete_command(specifier);
            let candidates = apply_resolved_delete_to_placements(
                &cmd,
                Some(71),
                &mut Vec::new(),
                (0, 0),
                (10.0, 20.0),
                (usize::MAX, usize::MAX),
            );

            assert_eq!(candidates, HashSet::from([71]));
        }

        let candidates = apply_delete_to_placements(
            &delete_command(KittyDeleteSpec::AllImages),
            &mut Vec::new(),
            0,
            0,
            10.0,
            20.0,
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn missing_ids_and_image_numbers_never_alias_stored_image_ids() {
        let original = vec![inline_image(7, 1, 0, 0, 1, 1)];

        for specifier in [
            KittyDeleteSpec::ById {
                id: 0,
                delete_data: true,
            },
            KittyDeleteSpec::ByNumber {
                number: 7,
                delete_data: true,
            },
        ] {
            let mut placements = original.clone();
            let cmd = delete_command(specifier);
            let candidates =
                apply_delete_to_placements(&cmd, &mut placements, 0, 0, 10.0, 20.0);
            assert!(candidates.is_empty());
            assert_eq!(placements.len(), 1);
            assert_eq!(placements[0].image_id, 7);
        }
    }

    #[test]
    fn column_and_row_deletes_use_one_based_intersection_coordinates() {
        let mut columns = vec![
            inline_image(1, 1, 0, 0, 2, 2),
            inline_image(2, 1, 0, 2, 1, 1),
        ];
        let cmd = delete_command(KittyDeleteSpec::ByColumn {
            column: 2,
            delete_data: true,
        });
        let candidates =
            apply_delete_to_placements(&cmd, &mut columns, 0, 0, 10.0, 20.0);
        assert_eq!(candidates, HashSet::from([1]));
        assert_eq!(
            columns
                .iter()
                .map(|placement| placement.image_id)
                .collect::<Vec<_>>(),
            vec![2]
        );

        let mut rows = vec![
            inline_image(5, 1, 0, 0, 2, 1),
            inline_image(6, 1, 2, 0, 1, 1),
        ];
        let cmd = delete_command(KittyDeleteSpec::ByRow {
            row: 2,
            delete_data: true,
        });
        let candidates =
            apply_delete_to_placements(&cmd, &mut rows, 0, 0, 10.0, 20.0);
        assert_eq!(candidates, HashSet::from([5]));
        assert_eq!(
            rows.iter()
                .map(|placement| placement.image_id)
                .collect::<Vec<_>>(),
            vec![6]
        );
    }

    #[test]
    fn uppercase_z_index_delete_reports_only_removed_kitty_images() {
        let mut first = inline_image(30, 1, 0, 0, 1, 1);
        first.z_index = -3;
        let mut duplicate = inline_image(30, 2, 1, 1, 1, 1);
        duplicate.z_index = -3;
        let mut sixel = inline_image(32, 0, 0, 0, 1, 1);
        sixel.z_index = -3;
        let mut placements = vec![
            first,
            duplicate,
            inline_image(31, 3, 0, 0, 1, 1),
            sixel,
        ];
        let command = delete_command(KittyDeleteSpec::ByZIndex {
            z_index: -3,
            delete_data: true,
        });

        let candidates =
            apply_delete_to_placements(&command, &mut placements, 0, 0, 10.0, 20.0);

        assert_eq!(candidates, HashSet::from([30]));
        assert_eq!(
            placements
                .iter()
                .map(|placement| (placement.image_id, placement.placement_id))
                .collect::<Vec<_>>(),
            vec![(31, 3), (32, 0)]
        );
    }

    #[test]
    fn inline_offsets_do_not_expand_delete_bounds_beyond_explicit_cells() {
        let mut columns = vec![offset_inline_image(20, 5, 0)];
        let adjacent_column_delete = delete_command(KittyDeleteSpec::ByColumn {
            column: 2,
            delete_data: false,
        });

        apply_delete_to_placements(
            &adjacent_column_delete,
            &mut columns,
            0,
            0,
            10.0,
            20.0,
        );

        assert_eq!(columns.len(), 1);

        let anchor_column_delete = delete_command(KittyDeleteSpec::ByColumn {
            column: 1,
            delete_data: false,
        });
        apply_delete_to_placements(
            &anchor_column_delete,
            &mut columns,
            0,
            0,
            10.0,
            20.0,
        );

        assert!(columns.is_empty());

        let mut rows = vec![offset_inline_image(21, 0, 5)];
        let adjacent_row_delete = delete_command(KittyDeleteSpec::ByRow {
            row: 2,
            delete_data: false,
        });

        apply_delete_to_placements(
            &adjacent_row_delete,
            &mut rows,
            0,
            0,
            10.0,
            20.0,
        );

        assert_eq!(rows.len(), 1);

        let anchor_row_delete = delete_command(KittyDeleteSpec::ByRow {
            row: 1,
            delete_data: false,
        });
        apply_delete_to_placements(
            &anchor_row_delete,
            &mut rows,
            0,
            0,
            10.0,
            20.0,
        );

        assert!(rows.is_empty());
    }

    #[test]
    fn native_delete_hit_tests_recompute_effective_cells_after_cell_shrink() {
        let mut native = inline_image(22, 1, 0, 0, 2, 2);
        let PlacementMode::Inline {
            x_offset,
            render_size,
            ..
        } = &mut native.mode;
        *x_offset = 9;
        *render_size = InlineRenderSize::NativePixels {
            width: 10,
            height: 20,
        };

        let explicit = {
            let mut placement = inline_image(23, 1, 0, 0, 2, 2);
            let PlacementMode::Inline { x_offset, .. } = &mut placement.mode;
            *x_offset = 9;
            placement
        };
        let mut placements = vec![native, explicit];
        let third_column_delete = delete_command(KittyDeleteSpec::ByColumn {
            column: 3,
            delete_data: false,
        });

        apply_delete_to_placements(
            &third_column_delete,
            &mut placements,
            0,
            0,
            8.0,
            16.0,
        );

        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].image_id, 23);
    }

    #[test]
    fn aspect_ratio_delete_hit_tests_follow_effective_cells_after_cell_shrink() {
        let mut from_columns = inline_image(24, 1, 0, 0, 2, 3);
        let PlacementMode::Inline {
            x_offset,
            y_offset,
            render_size,
            ..
        } = &mut from_columns.mode;
        *x_offset = 2;
        *y_offset = 3;
        *render_size = InlineRenderSize::AspectFromColumns {
            columns: 3,
            source_width: 20,
            source_height: 10,
        };
        let mut columns = vec![from_columns];

        apply_delete_to_placements(
            &delete_command(KittyDeleteSpec::ByRow {
                row: 4,
                delete_data: false,
            }),
            &mut columns,
            0,
            0,
            8.0,
            6.0,
        );
        assert_eq!(columns.len(), 1);
        apply_delete_to_placements(
            &delete_command(KittyDeleteSpec::ByRow {
                row: 3,
                delete_data: false,
            }),
            &mut columns,
            0,
            0,
            8.0,
            6.0,
        );
        assert!(columns.is_empty());

        let mut from_rows = inline_image(25, 1, 0, 0, 3, 7);
        let PlacementMode::Inline {
            x_offset,
            y_offset,
            render_size,
            ..
        } = &mut from_rows.mode;
        *x_offset = 2;
        *y_offset = 3;
        *render_size = InlineRenderSize::AspectFromRows {
            rows: 3,
            source_width: 20,
            source_height: 10,
        };
        let mut rows = vec![from_rows];

        apply_delete_to_placements(
            &delete_command(KittyDeleteSpec::ByColumn {
                column: 7,
                delete_data: false,
            }),
            &mut rows,
            0,
            0,
            8.0,
            6.0,
        );
        assert_eq!(rows.len(), 1);
        apply_delete_to_placements(
            &delete_command(KittyDeleteSpec::ByColumn {
                column: 6,
                delete_data: false,
            }),
            &mut rows,
            0,
            0,
            8.0,
            6.0,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn cursor_delete_uses_command_time_coordinates() {
        let mut placements = vec![
            inline_image(9, 1, 0, 0, 2, 2),
            inline_image(11, 1, 2, 2, 1, 1),
        ];
        let cmd = delete_command(KittyDeleteSpec::AtCursor { delete_data: true });

        let candidates =
            apply_delete_to_placements(&cmd, &mut placements, 1, 1, 10.0, 20.0);
        assert_eq!(candidates, HashSet::from([9]));
        assert_eq!(
            placements
                .iter()
                .map(|placement| placement.image_id)
                .collect::<Vec<_>>(),
            vec![11]
        );
    }

    #[test]
    fn all_delete_variants_preserve_sixel_placements() {
        for specifier in [KittyDeleteSpec::All, KittyDeleteSpec::AllImages] {
            let mut placements = vec![
                inline_image(12, 1, 0, 0, 1, 1),
                inline_image(12, 0, 0, 0, 1, 1),
            ];
            let cmd = delete_command(specifier);

            let mut candidates =
                apply_delete_to_placements(&cmd, &mut placements, 0, 0, 10.0, 20.0);

            assert_eq!(placements.len(), 1);
            assert_eq!(placements[0].placement_id, 0);
            if specifier == KittyDeleteSpec::AllImages {
                assert_eq!(candidates, HashSet::from([12]));
                retain_unreferenced_image_ids(&mut candidates, &placements);
                assert!(candidates.is_empty());
            } else {
                assert!(candidates.is_empty());
            }
        }
    }

    #[test]
    fn all_delete_variants_only_remove_visible_kitty_placements() {
        let original = vec![
            inline_image(12, 1, 0, 0, 1, 1),
            inline_image(12, 2, 2, 0, 1, 1),
            inline_image(13, 3, 1, 1, 2, 2),
            inline_image(14, 0, 0, 0, 1, 1),
        ];

        for specifier in [KittyDeleteSpec::All, KittyDeleteSpec::AllImages] {
            let mut placements = original.clone();
            let command = delete_command(specifier);
            let mut candidates = apply_resolved_delete_to_placements(
                &command,
                None,
                &mut placements,
                (0, 0),
                (10.0, 20.0),
                (2, 2),
            );

            assert_eq!(
                placements
                    .iter()
                    .map(|placement| (placement.image_id, placement.placement_id))
                    .collect::<Vec<_>>(),
                vec![(12, 2), (14, 0)]
            );
            if specifier == KittyDeleteSpec::AllImages {
                assert_eq!(candidates, HashSet::from([12, 13]));
                retain_unreferenced_image_ids(&mut candidates, &placements);
                assert_eq!(candidates, HashSet::from([13]));
            } else {
                assert!(candidates.is_empty());
            }
        }
    }

    #[test]
    fn selective_kitty_deletes_preserve_sixel_placements_with_the_same_id() {
        let mut placements = vec![
            inline_image(15, 1, 0, 0, 1, 1),
            inline_image(15, 0, 0, 0, 1, 1),
        ];
        let cmd = delete_command(KittyDeleteSpec::ById {
            id: 15,
            delete_data: false,
        });

        let candidates =
            apply_delete_to_placements(&cmd, &mut placements, 0, 0, 10.0, 20.0);

        assert!(candidates.is_empty());
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].placement_id, 0);
    }

    #[test]
    fn retransmission_cleanup_removes_only_matching_kitty_placements() {
        let mut placements = vec![
            inline_image(15, 1, 0, 0, 1, 1),
            inline_image(15, 0, 0, 0, 1, 1),
            inline_image(16, 2, 0, 0, 1, 1),
        ];

        remove_retransmitted_placements(&mut placements, 15);

        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].placement_id, 0);
        assert_eq!(placements[1].image_id, 16);
    }

    #[test]
    fn clamps_inline_dimensions_and_cursor_advance_to_the_grid() {
        let (mode, advance) = inline_placement_and_advance(
            3,
            7,
            u32::MAX,
            u32::MAX,
            (3, 5),
            InlineRenderSize::CellAnchored,
            (80, 24),
        );

        let PlacementMode::Inline {
            row,
            col,
            cols,
            rows,
            x_offset,
            y_offset,
            render_size,
        } = mode;
        assert_eq!(
            (row, col, cols, rows, x_offset, y_offset, render_size),
            (3, 7, 80, 24, 3, 5, InlineRenderSize::CellAnchored)
        );
        assert_eq!((advance.cols, advance.rows), (80, 24));

        let (mode, advance) = inline_placement_and_advance(
            0,
            0,
            10,
            5,
            (0, 0),
            InlineRenderSize::CellAnchored,
            (80, 24),
        );
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
    fn single_axis_render_size_keeps_the_requested_cells_after_grid_clamp() {
        let layout = resolve_kitty_placement_layout(
            Some(100),
            None,
            (10, 10),
            (0, 0),
            (10.0, 10.0),
        );
        let (mode, advance) = inline_placement_and_advance(
            0,
            0,
            layout.display_cols,
            layout.display_rows,
            (0, 0),
            layout.render_size,
            (80, 24),
        );

        assert!(matches!(
            &mode,
            PlacementMode::Inline {
                cols: 80,
                rows: 24,
                render_size: InlineRenderSize::AspectFromColumns {
                    columns: 100,
                    ..
                },
                ..
            }
        ));
        assert_eq!((advance.cols, advance.rows), (80, 24));
        assert_eq!(mode.pixel_rect(10.0, 10.0), (0.0, 0.0, 1000.0, 1000.0));
    }

    #[test]
    fn c1_keeps_inline_geometry_and_only_suppresses_cursor_advance() {
        let (mode, advance) = inline_placement_and_advance(
            3,
            7,
            4,
            2,
            (9, 19),
            InlineRenderSize::CellAnchored,
            (80, 24),
        );

        assert!(matches!(&mode, PlacementMode::Inline { .. }));
        assert_eq!(mode.pixel_rect(10.0, 20.0), (79.0, 79.0, 31.0, 21.0));
        for cursor_movement in [0, 2, u8::MAX] {
            assert_eq!(
                cursor_advance_for_policy(cursor_movement, advance),
                Some(super::CursorAdvance { rows: 2, cols: 4 })
            );
        }
        assert_eq!(cursor_advance_for_policy(1, advance), None);
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
