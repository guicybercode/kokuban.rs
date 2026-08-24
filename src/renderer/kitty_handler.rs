use crate::parser::kitty_graphics::*;
use super::image_store::{ImageFormat, ImageId, ImageStore};
use std::collections::HashMap;

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

struct PendingImage {
    data: Vec<u8>,
    format: KittyFormat,
    width: Option<u32>,
    height: Option<u32>,
    compression: KittyCompression,
    transmission: KittyTransmission,
    // Placement info to apply after transmit (for a=T)
    #[allow(dead_code)]
    place_after: Option<KittyCommand>,
}

pub struct KittyHandler {
    pending: HashMap<u32, PendingImage>,
    next_placement_id: u32,
}

impl KittyHandler {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
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
        match cmd.action {
            KittyAction::Query => {
                return self.handle_query(&cmd, store);
            }
            KittyAction::Transmit => {
                return self.handle_transmit(cmd, store, false);
            }
            KittyAction::TransmitAndPlace => {
                let result = self.handle_transmit(cmd.clone(), store, true);
                // If transmit succeeded (response contains "OK"), create placement
                let ok = result.0.as_ref().map_or(true, |r| {
                    std::str::from_utf8(r).map_or(false, |s| s.contains("OK"))
                });
                if ok {
                    let image_id = cmd.image_id.unwrap_or(0);
                    if store.contains(image_id) {
                        let advance = self.create_placement(
                            &cmd, image_id, store, cursor_row, cursor_col,
                            cell_width, cell_height, grid_cols, grid_rows, placements,
                        );
                        return (result.0, advance);
                    }
                }
                return result;
            }
            KittyAction::Place => {
                let image_id = cmd.image_id.unwrap_or(0);
                if store.contains(image_id) {
                    let advance = self.create_placement(
                        &cmd, image_id, store, cursor_row, cursor_col,
                        cell_width, cell_height, grid_cols, grid_rows, placements,
                    );
                    let resp = if cmd.quiet < 1 {
                        Some(format!("\x1b_Gi={image_id};OK\x1b\\").into_bytes())
                    } else {
                        None
                    };
                    return (resp, advance);
                } else {
                    let resp = if cmd.quiet < 2 {
                        Some(format!("\x1b_Gi={image_id};ENOENT:image not found\x1b\\").into_bytes())
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
            // Store the test image briefly
            let data = maybe_decompress(&cmd.payload, cmd.compression);
            if let Some(id) = store.store(&data, w, h, format, Some(image_id)) {
                // Remove the test image immediately
                store.remove(id);
            }
        }
        let resp = format!("\x1b_Gi={image_id};OK\x1b\\").into_bytes();
        (Some(resp), None)
    }

    fn handle_transmit(
        &mut self,
        cmd: KittyCommand,
        store: &mut ImageStore,
        will_place: bool,
    ) -> (Option<Vec<u8>>, Option<CursorAdvance>) {
        let image_id = cmd.image_id.unwrap_or_else(|| store.next_id());

        if cmd.more_chunks {
            // Accumulate chunk
            let pending = self.pending.entry(image_id).or_insert_with(|| PendingImage {
                data: Vec::new(),
                format: cmd.format,
                width: cmd.width,
                height: cmd.height,
                compression: cmd.compression,
                transmission: cmd.transmission,
                place_after: if will_place { Some(cmd.clone()) } else { None },
            });
            pending.data.extend_from_slice(&cmd.payload);
            log::trace!(
                "Kitty chunk for image {image_id}, total bytes: {}",
                pending.data.len()
            );
            // No response for intermediate chunks
            return (None, None);
        }

        // Final chunk or single-chunk transmission
        let (full_data, format, width, height, compression, transmission) =
            if let Some(mut pending) = self.pending.remove(&image_id) {
                pending.data.extend_from_slice(&cmd.payload);
                (
                    pending.data,
                    pending.format,
                    pending.width.or(cmd.width),
                    pending.height.or(cmd.height),
                    pending.compression,
                    pending.transmission,
                )
            } else {
                (
                    cmd.payload.clone(),
                    cmd.format,
                    cmd.width,
                    cmd.height,
                    cmd.compression,
                    cmd.transmission,
                )
            };

        // Handle file transmission
        let image_data = match transmission {
            KittyTransmission::File | KittyTransmission::TempFile => {
                match load_file_data(&full_data, transmission == KittyTransmission::TempFile) {
                    Some(data) => data,
                    None => {
                        let resp = if cmd.quiet < 2 {
                            Some(format!("\x1b_Gi={image_id};ENOENT:file not found\x1b\\").into_bytes())
                        } else {
                            None
                        };
                        return (resp, None);
                    }
                }
            }
            KittyTransmission::SharedMemory => {
                let resp = if cmd.quiet < 2 {
                    Some(format!("\x1b_Gi={image_id};ENOSYS:shared memory not supported\x1b\\").into_bytes())
                } else {
                    None
                };
                return (resp, None);
            }
            KittyTransmission::Direct => full_data,
        };

        // Decompress if needed
        let image_data = maybe_decompress(&image_data, compression);

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
                    let resp = if cmd.quiet < 2 {
                        Some(format!("\x1b_Gi={image_id};EINVAL:missing dimensions\x1b\\").into_bytes())
                    } else {
                        None
                    };
                    return (resp, None);
                }
                (w, h, ImageFormat::Rgb)
            }
            KittyFormat::Rgba => {
                let w = width.unwrap_or(0);
                let h = height.unwrap_or(0);
                if w == 0 || h == 0 {
                    let resp = if cmd.quiet < 2 {
                        Some(format!("\x1b_Gi={image_id};EINVAL:missing dimensions\x1b\\").into_bytes())
                    } else {
                        None
                    };
                    return (resp, None);
                }
                (w, h, ImageFormat::Rgba)
            }
        };

        match store.store(&image_data, w, h, img_format, Some(image_id)) {
            Some(_) => {
                let resp = if cmd.quiet < 1 {
                    Some(format!("\x1b_Gi={image_id};OK\x1b\\").into_bytes())
                } else {
                    None
                };
                (resp, None)
            }
            None => {
                let resp = if cmd.quiet < 2 {
                    Some(format!("\x1b_Gi={image_id};ENOMEM:failed to store image\x1b\\").into_bytes())
                } else {
                    None
                };
                (resp, None)
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
        let display_cols = cmd.columns.unwrap_or_else(|| {
            ((img.width as f32) / cell_width).ceil() as u32
        });
        let display_rows = cmd.rows.unwrap_or_else(|| {
            ((img.height as f32) / cell_height).ceil() as u32
        });

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
    let (cols, rows) =
        bounded_inline_dimensions(display_cols, display_rows, grid_cols, grid_rows);
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

fn maybe_decompress(data: &[u8], compression: KittyCompression) -> Vec<u8> {
    match compression {
        KittyCompression::None => data.to_vec(),
        KittyCompression::Zlib => {
            use std::io::Read;
            let mut decoder = flate2::read::ZlibDecoder::new(data);
            let mut decompressed = Vec::new();
            match decoder.read_to_end(&mut decompressed) {
                Ok(_) => decompressed,
                Err(e) => {
                    log::warn!("Failed to decompress zlib data: {e}");
                    data.to_vec()
                }
            }
        }
    }
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
    use super::{inline_placement_and_advance, PlacementMode};

    #[test]
    fn clamps_inline_dimensions_and_cursor_advance_to_the_grid() {
        let (mode, advance) =
            inline_placement_and_advance(3, 7, u32::MAX, u32::MAX, 80, 24);

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
}
