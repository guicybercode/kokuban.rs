use super::image_decode::{probe_image_data, ImageFormat};
use super::image_store::ImageStore;
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
use std::time::Instant;

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
            if will_place
                || (pending.metadata.action == KittyAction::Frame)
                    != (cmd.action == KittyAction::Frame)
            {
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

    /// The protocol orders equal-z images by client ID, independently of cache allocation.
    #[cfg(target_os = "linux")]
    pub(crate) fn image_client_ids(&self) -> impl Iterator<Item = (ImageId, KittyImageId)> + '_ {
        self.client_images.image_client_ids()
    }

    /// Process a parsed Kitty graphics command.
    /// Returns any PTY response, cursor movement, and deferred cache effects.
    pub(crate) fn process(
        &mut self,
        mut cmd: KittyCommand,
        store: &mut ImageStore,
        cursor_row: usize,
        cursor_col: usize,
        cell_width: f32,
        cell_height: f32,
        grid_cols: usize,
        grid_rows: usize,
        placements: &mut Vec<ImagePlacement>,
    ) -> KittyProcessOutcome {
        // Frame continuations may repeat a=f or omit a entirely. An explicit
        // a=t is a new transfer and must not be appended to a pending frame.
        if cmd.action == KittyAction::Transmit
            && !cmd.action_explicit
            && self.chunks.pending.as_ref().is_some_and(|pending| {
                pending.metadata.action == KittyAction::Frame
            })
        {
            cmd.action = KittyAction::Frame;
        }
        let selectors_conflict = cmd.image_id.is_some() && cmd.image_number.is_some();
        if cmd.invalid_animation_parameters || cmd.invalid_image_selector || selectors_conflict {
            let reason = if cmd.invalid_animation_parameters {
                "EINVAL:invalid animation parameters"
            } else if selectors_conflict {
                "EINVAL:image ID and image number are mutually exclusive"
            } else {
                "EINVAL:invalid image selector"
            };
            let mut identity = (cmd.image_id, cmd.image_number, cmd.quiet);
            if let Some(pending) = self.chunks.pending.take() {
                if cmd.action == KittyAction::Frame && pending.metadata.action == KittyAction::Frame {
                    identity = (Some(pending.image_id), pending.metadata.image_number, pending.metadata.quiet);
                }
            }
            return KittyProcessOutcome::new(
                kitty_response(
                    identity.0,
                    identity.1,
                    identity.2,
                    true,
                    reason,
                ),
                None,
            );
        }

        if !matches!(
            cmd.action,
            KittyAction::Transmit | KittyAction::TransmitAndPlace | KittyAction::Frame
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
                if let Some(KittyDeleteSpec::Frame { frame, delete_last_image }) = cmd.delete_specifier {
                    return self.handle_frame_delete(&cmd, store, placements, frame, delete_last_image);
                }
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
            KittyAction::Frame => {
                let response = self.handle_frame(cmd, store);
                self.prune_image_registries(store, placements);
                KittyProcessOutcome::new(response, None)
            }
            KittyAction::Animate | KittyAction::Compose =>
                KittyProcessOutcome::new(self.handle_animation_control(&cmd, store), None),
        }
    }

    fn animation_target(
        &self,
        cmd: &KittyCommand,
        store: &ImageStore,
    ) -> Option<(KittyImageId, ImageId)> {
        if let Some(client_id) = cmd.image_id.filter(|id| *id != 0) {
            self.resolve_client_image(client_id, store)
                .map(|id| (client_id, id))
        } else {
            cmd.image_number
                .filter(|number| *number != 0)
                .and_then(|number| self.resolve_image_number(number, store))
        }
    }

    fn handle_frame(&mut self, cmd: KittyCommand, store: &mut ImageStore) -> Option<Vec<u8>> {
        // Frames reuse an image identity; they must never allocate a client ID.
        let client_id = self
            .animation_target(&cmd, store)
            .map(|(client_id, _)| client_id)
            .unwrap_or(0);
        let completed = match self.chunks.push(cmd, false, || client_id) {
            Ok(Some(completed)) => completed,
            Ok(None) => return None,
            Err(error) => return chunk_error_response(error),
        };
        let cmd = completed.metadata;
        let Some((client_id, image_id)) = self.animation_target(&cmd, store) else {
            return kitty_response(
                cmd.image_id,
                cmd.image_number,
                cmd.quiet,
                true,
                "ENOENT:animation image not found",
            );
        };
        let Some(KittyAnimationCommand::Frame(params)) = cmd.animation else {
            return kitty_response(
                Some(client_id),
                cmd.image_number,
                cmd.quiet,
                true,
                "EINVAL:missing frame parameters",
            );
        };
        let error_response = |reason| {
            frame_response(
                Some(client_id),
                cmd.image_number,
                params.edit_frame.map(|frame| frame.get()),
                cmd.quiet,
                true,
                reason,
            )
        };
        let image_data = match cmd.transmission {
            KittyTransmission::Direct => completed.data,
            KittyTransmission::File | KittyTransmission::TempFile => {
                match load_file_data(
                    &completed.data,
                    cmd.transmission == KittyTransmission::TempFile,
                    self.options,
                ) {
                    Ok(data) => data,
                    Err(error) => {
                        return file_load_error_response_with_number(
                            client_id,
                            cmd.image_number,
                            cmd.quiet,
                            error,
                        )
                    }
                }
            }
            KittyTransmission::SharedMemory => {
                return error_response("ENOSYS:shared memory not supported")
            }
        };
        let image_data = match maybe_decompress_with_limit(
            &image_data,
            cmd.compression,
            self.options.max_image_bytes,
        ) {
            Ok(data) => data,
            Err(error) => {
                return decompression_error_response_with_number(
                    client_id,
                    cmd.image_number,
                    cmd.quiet,
                    error,
                )
            }
        };
        let (width, height, format) = match cmd.format {
            KittyFormat::Png => (0, 0, ImageFormat::Png),
            KittyFormat::Rgb | KittyFormat::Rgba => {
                let (width, height) = (cmd.width.unwrap_or(0), cmd.height.unwrap_or(0));
                let channels = if cmd.format == KittyFormat::Rgb { 3 } else { 4 };
                let expected = u64::from(width)
                    .checked_mul(u64::from(height))
                    .and_then(|pixels| pixels.checked_mul(channels))
                    .and_then(|bytes| usize::try_from(bytes).ok());
                if width == 0 || height == 0 || expected != Some(image_data.len()) {
                    return error_response("EINVAL:frame data does not match dimensions");
                }
                (
                    width,
                    height,
                    if channels == 3 {
                        ImageFormat::Rgb
                    } else {
                        ImageFormat::Rgba
                    },
                )
            }
        };
        match store.store_animation_frame(
            image_id,
            &image_data,
            width,
            height,
            format,
            &params,
            Instant::now(),
        ) {
            Ok(frame) => frame_response(
                Some(client_id),
                cmd.image_number,
                Some(frame),
                cmd.quiet,
                false,
                "OK",
            ),
            Err(error) => error_response(error.response_message()),
        }
    }

    fn handle_animation_control(
        &self,
        cmd: &KittyCommand,
        store: &mut ImageStore,
    ) -> Option<Vec<u8>> {
        let Some((client_id, image_id)) = self.animation_target(cmd, store) else {
            return kitty_response(
                cmd.image_id,
                cmd.image_number,
                cmd.quiet,
                true,
                "ENOENT:animation image not found",
            );
        };
        let result = match (cmd.action, cmd.animation) {
            (KittyAction::Animate, Some(KittyAnimationCommand::Control(control))) => {
                store.control_animation(image_id, &control, Instant::now())
            }
            (KittyAction::Compose, Some(KittyAnimationCommand::Compose(composition))) => {
                store.compose_animation_frame(image_id, &composition, Instant::now())
            }
            _ => {
                return kitty_response(
                    Some(client_id),
                    cmd.image_number,
                    cmd.quiet,
                    true,
                    "EINVAL:missing animation parameters",
                )
            }
        };
        match result {
            // Successful animation control must be silent: icat sends it after
            // its response-reading loop, so an OK would leak into the shell.
            Ok(()) if cmd.action == KittyAction::Animate => None,
            Ok(()) => kitty_response(Some(client_id), cmd.image_number, cmd.quiet, false, "OK"),
            Err(error) => kitty_response(
                Some(client_id),
                cmd.image_number,
                cmd.quiet,
                true,
                error.response_message(),
            ),
        }
    }

    fn handle_frame_delete(
        &self,
        cmd: &KittyCommand,
        store: &mut ImageStore,
        placements: &mut Vec<ImagePlacement>,
        frame: u32,
        delete_last_image: bool,
    ) -> KittyProcessOutcome {
        let Some((client_id, image_id)) = self.animation_target(cmd, store) else {
            return KittyProcessOutcome::new(None, None);
        };
        match store.delete_animation_frame(image_id, frame, delete_last_image, Instant::now()) {
            Ok(()) => {
                let mut outcome = KittyProcessOutcome::new(None, None);
                if store.get(image_id).is_none() {
                    placements.retain(|placement| placement.image_id != image_id);
                    outcome.retransmitted_image_id = Some(image_id);
                }
                outcome
            }
            Err(error) => KittyProcessOutcome::new(
                kitty_response(
                    Some(client_id),
                    cmd.image_number,
                    cmd.quiet,
                    true,
                    error.response_message(),
                ),
                None,
            ),
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
                let response = chunk_error_response(error);
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
        KittyDeleteSpec::NoOp | KittyDeleteSpec::Frame { .. } => HashSet::new(),
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
            row: row as i64,
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

fn chunk_error_response(error: ChunkAssemblyError) -> Option<Vec<u8>> {
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
    kitty_response(
        Some(error.image_id),
        error.image_number,
        error.quiet,
        true,
        &reason,
    )
}

fn frame_response(
    image_id: Option<KittyImageId>,
    image_number: Option<u32>,
    frame: Option<u32>,
    quiet: u8,
    is_error: bool,
    result: &str,
) -> Option<Vec<u8>> {
    let mut response = kitty_response(image_id, image_number, quiet, is_error, result)?;
    if let Some(frame) = frame {
        let position = response.iter().position(|&byte| byte == b';')?;
        response.splice(position..position, format!(",r={frame}").bytes());
    }
    Some(response)
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
#[path = "kitty_handler_tests.rs"]
mod tests;
