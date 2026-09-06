//! Android adapter over the shared graphics parser, handler and image cache.
#[path = "renderer/image_decode.rs"]
mod image_decode;
#[path = "renderer/software_image_store.rs"]
pub(crate) mod image_store;
#[path = "renderer/kitty_handler.rs"]
mod kitty_handler;

use crate::config::ImagesConfig;
use crate::graphics::{
    retain_unreferenced_image_ids, ImagePlacement, InlineRenderSize, PlacementMode,
};
use crate::grid::{Grid, TerminalEvent};
use crate::terminal_reader::ReaderExit;
use image_store::{ImageFormat, ImageStore};
use kitty_handler::{KittyHandler, KittyHandlerOptions};
use std::sync::{Arc, Mutex};

pub(crate) struct AndroidImages {
    pub(crate) store: Arc<Mutex<ImageStore>>,
    handler: KittyHandler,
}

impl AndroidImages {
    pub(crate) fn new(config: &ImagesConfig) -> Self {
        Self {
            store: Arc::new(Mutex::new(ImageStore::new(config.max_memory_mb.min(64)))),
            handler: KittyHandler::new(KittyHandlerOptions::from_megabytes(
                config.kitty.max_image_size_mb.min(32),
                config.kitty.allow_file_transfer,
            )),
        }
    }

    pub(crate) fn process(
        &mut self,
        event: TerminalEvent,
        grid: &mut Grid,
    ) -> Result<Option<Vec<u8>>, ReaderExit> {
        let mut store = self.store.lock().map_err(|_| ReaderExit::GridPoisoned)?;
        let cw = f32::from(grid.cell_pixel_width.max(1));
        let ch = f32::from(grid.cell_pixel_height.max(1));
        match event {
            TerminalEvent::KittyGraphics {
                command,
                cursor_row,
                cursor_col,
            } => {
                let mut outcome = self.handler.process(
                    command,
                    &mut store,
                    cursor_row,
                    cursor_col,
                    cw,
                    ch,
                    grid.cols(),
                    grid.rows(),
                    &mut grid.image_placements,
                );
                if let Some(id) = outcome.retransmitted_image_id {
                    grid.remove_hidden_primary_kitty_placements(id);
                }
                if let Some(advance) = outcome.advance {
                    grid.advance_image_cursor(advance.cols, advance.rows);
                }
                retain_unreferenced_image_ids(
                    &mut outcome.hard_delete_candidates,
                    grid.all_image_placements(),
                );
                for id in outcome.hard_delete_candidates {
                    store.remove(id);
                }
                Ok(outcome.response)
            }
            TerminalEvent::SixelGraphics {
                image,
                cursor_row,
                cursor_col,
            } => {
                let columns = (image.width as f32 / cw).ceil() as u32;
                let rows = (image.height as f32 / ch).ceil() as u32;
                if let Some(id) = store.store(
                    &image.pixels,
                    image.width,
                    image.height,
                    ImageFormat::Rgba,
                    None,
                ) {
                    grid.image_placements.push(ImagePlacement {
                        image_id: id,
                        placement_id: 0,
                        client_placement_id: None,
                        z_index: 0,
                        mode: PlacementMode::Inline {
                            row: cursor_row as i64,
                            col: cursor_col,
                            cols: columns,
                            rows,
                            x_offset: 0,
                            y_offset: 0,
                            render_size: InlineRenderSize::CellAnchored,
                        },
                    });
                }
                // Cache rejection does not change protocol cursor movement.
                for _ in 0..rows {
                    grid.newline();
                }
                Ok(None)
            }
            TerminalEvent::Response(response) => Ok(Some(response)),
        }
    }
}

pub(crate) struct ImageSnapshot {
    pub(crate) pixels: Arc<[u8]>,
    pub(crate) size: (u32, u32),
    pub(crate) rect: (f32, f32, f32, f32),
    pub(crate) z: i32,
}

/// Lock order matches the reader: grid then cache. One frame retains shared
/// pixels until presentation, including if the reader replaces the image.
pub(crate) fn snapshot(
    grid: &Grid,
    store: &Mutex<ImageStore>,
) -> Result<Vec<ImageSnapshot>, String> {
    let store = store.lock().map_err(|_| "Image store lock poisoned")?;
    let mut images: Vec<_> = grid
        .image_placements
        .iter()
        .filter_map(|placement| {
            let image = store.get(placement.image_id)?;
            let mut rect = placement.mode.pixel_rect(
                f32::from(grid.cell_pixel_width),
                f32::from(grid.cell_pixel_height),
            );
            rect.1 += grid.scroll_offset as f32 * f32::from(grid.cell_pixel_height);
            Some(ImageSnapshot {
                pixels: image.pixels.clone(),
                size: (image.width, image.height),
                rect,
                z: placement.z_index,
            })
        })
        .collect();
    images.sort_by_key(|image| image.z);
    Ok(images)
}
