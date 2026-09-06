//! Android selects the CPU renderer and caps the shared graphics budgets.
//! Parser, placement, animation, cache and snapshots are shared with Linux.
#[path = "renderer/image_animation.rs"]
pub(crate) mod image_animation;
#[path = "renderer/image_decode.rs"]
pub(crate) mod image_decode;
#[path = "renderer/software_image_store.rs"]
pub(crate) mod image_store;
#[path = "renderer/kitty_handler.rs"]
pub(crate) mod kitty_handler;

use crate::config::{ImagesConfig, KittyImagesConfig};
use crate::grid::{Grid, TerminalEvent};
use crate::software_graphics::SoftwareGraphics;
use crate::terminal_reader::ReaderExit;
use std::sync::{Arc, Mutex};

pub(crate) struct AndroidImages {
    pub(crate) store: Arc<Mutex<SoftwareGraphics>>,
}

impl AndroidImages {
    pub(crate) fn new(config: &ImagesConfig) -> Self {
        let config = ImagesConfig {
            enabled: config.enabled,
            max_memory_mb: config.max_memory_mb.min(64),
            sixel_enabled: config.sixel_enabled,
            kitty_enabled: config.kitty_enabled,
            kitty: KittyImagesConfig {
                max_image_size_mb: config.kitty.max_image_size_mb.min(32),
                allow_file_transfer: config.kitty.allow_file_transfer,
            },
        };
        Self {
            store: Arc::new(Mutex::new(SoftwareGraphics::new(&config))),
        }
    }

    pub(crate) fn process(
        &mut self,
        event: TerminalEvent,
        grid: &mut Grid,
    ) -> Result<Option<Vec<u8>>, ReaderExit> {
        let mut graphics = self.store.lock().map_err(|_| ReaderExit::GridPoisoned)?;
        Ok(graphics.process(event, grid))
    }
}
