use crate::config::ImagesConfig;
use crate::graphics::{
    retain_unreferenced_image_ids, ImagePlacement, InlineRenderSize, PlacementMode,
};
use crate::grid::{Grid, TerminalEvent};
use crate::renderer::image_store::{ImageFormat, ImageStore};
use crate::renderer::kitty_handler::{KittyHandler, KittyHandlerOptions};
use std::sync::Arc;

const MAX_PLACEMENTS: usize = 4096;

pub(crate) struct SoftwareGraphics {
    store: ImageStore,
    kitty: KittyHandler,
}

#[cfg(test)]
mod tests {
    use super::{SoftwareGraphics, MAX_PLACEMENTS};
    use crate::config::ImagesConfig;
    use crate::grid::Grid;
    use crate::parser::ansi::GraphicsSupport;
    use crate::software_raster::draw_image_rgba;
    use crate::terminal_decoder::TerminalDecoder;
    use base64::Engine;

    fn fixture(config: ImagesConfig) -> (TerminalDecoder, SoftwareGraphics, Grid) {
        let decoder = TerminalDecoder::new(GraphicsSupport {
            kitty: config.kitty_graphics_enabled(),
            sixel: config.sixel_graphics_enabled(),
        });
        let graphics = SoftwareGraphics::new(&config);
        let mut grid = Grid::new(12, 6, 16);
        grid.cell_pixel_width = 4;
        grid.cell_pixel_height = 8;
        (decoder, graphics, grid)
    }

    fn feed(
        decoder: &mut TerminalDecoder,
        graphics: &mut SoftwareGraphics,
        grid: &mut Grid,
        mut input: &[u8],
    ) -> Vec<Vec<u8>> {
        let mut responses = Vec::new();
        while !input.is_empty() {
            let step = decoder.feed_until_event(input, grid);
            assert!(step.consumed > 0, "decoder must make progress");
            input = &input[step.consumed..];
            for event in step.events {
                if let Some(response) = graphics.process(event, grid) {
                    responses.push(response);
                }
            }
        }
        responses
    }

    fn upload(control: &str, pixels: &[u8]) -> Vec<u8> {
        format!(
            "\x1b_G{control};{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(pixels)
        )
        .into_bytes()
    }

    #[test]
    fn raw_kitty_image_is_acknowledged_placed_and_drawn_before_trailing_text() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        let input = [
            b"\x1b[2;3H".to_vec(),
            upload("a=T,f=32,s=2,v=1,i=7", &[255, 0, 0, 255, 0, 255, 0, 255]),
            b"X\x1b[6n".to_vec(),
        ]
        .concat();

        let responses = feed(&mut decoder, &mut graphics, &mut grid, &input);

        assert_eq!(
            responses,
            [b"\x1b_Gi=7;OK\x1b\\".to_vec(), b"\x1b[3;5R".to_vec()]
        );
        assert_eq!(grid.buffer.cell(2, 3).c, 'X');
        let images = graphics.snapshot(&grid, (4, 8));
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].rectangle, (8.0, 8.0, 2.0, 1.0));
        assert_eq!(images[0].size, (2, 1));
        let mut frame = vec![0; 48 * 48];
        draw_image_rgba(
            &mut frame,
            (48, 48),
            &images[0].pixels,
            images[0].size,
            images[0].rectangle,
        );
        assert_eq!(&frame[8 * 48 + 7..8 * 48 + 11], &[0, 0xff0000, 0x00ff00, 0]);
        assert_eq!(frame.iter().filter(|pixel| **pixel != 0).count(), 2);
    }

    #[test]
    fn fragmented_png_transmission_preserves_embedded_size_alpha_and_cursor_policy() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[255, 0, 0, 128, 0, 255, 0, 0])
                .unwrap();
        }
        let command = upload("a=T,f=100,i=8,C=1", &png);
        let responses: Vec<_> = command
            .chunks(3)
            .flat_map(|part| feed(&mut decoder, &mut graphics, &mut grid, part))
            .collect();

        assert_eq!(responses, [b"\x1b_Gi=8;OK\x1b\\".to_vec()]);
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 0));
        let images = graphics.snapshot(&grid, (4, 8));
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].size, (2, 1));
        assert_eq!(images[0].rectangle, (0.0, 0.0, 2.0, 1.0));
        let mut frame = [0x0000ff; 2];
        draw_image_rgba(
            &mut frame,
            (2, 1),
            &images[0].pixels,
            images[0].size,
            images[0].rectangle,
        );
        assert_eq!(frame, [0x80007f, 0x0000ff]);
    }

    #[test]
    fn consecutive_sixels_keep_native_pixels_and_move_text_in_event_order() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        let responses = feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b[2;3H\x1bPq#1;2;100;0;0~\x1b\\\x1bPq#1;2;0;100;0~\x1b\\X\x1b[6n",
        );

        assert_eq!(responses, [b"\x1b[4;4R".to_vec()]);
        assert_eq!(grid.buffer.cell(3, 2).c, 'X');
        let images = graphics.snapshot(&grid, (4, 8));
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].rectangle, (8.0, 8.0, 1.0, 6.0));
        assert_eq!(images[1].rectangle, (8.0, 16.0, 1.0, 6.0));
        let mut frame = vec![0; 48 * 48];
        for image in images {
            draw_image_rgba(
                &mut frame,
                (48, 48),
                &image.pixels,
                image.size,
                image.rectangle,
            );
        }
        for row in 8..14 {
            assert_eq!(frame[row * 48 + 8], 0xff0000);
        }
        for row in 16..22 {
            assert_eq!(frame[row * 48 + 8], 0x00ff00);
        }
        assert_eq!(frame.iter().filter(|pixel| **pixel != 0).count(), 12);
    }

    #[test]
    fn disabled_graphics_leave_no_images_responses_or_cursor_advance() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig {
            enabled: false,
            ..ImagesConfig::default()
        });
        let input = [
            upload("a=T,f=32,s=1,v=1,i=7", &[255; 4]),
            b"\x1bPq~\x1b\\X\x1b[c".to_vec(),
        ]
        .concat();

        let responses = feed(&mut decoder, &mut graphics, &mut grid, &input);

        assert_eq!(responses, [b"\x1b[?62;22c".to_vec()]);
        assert_eq!(grid.buffer.cell(0, 0).c, 'X');
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 1));
        assert!(grid.image_placements.is_empty());
        assert!(graphics.snapshot(&grid, (4, 8)).is_empty());
        assert_eq!(graphics.store.image_count(), 0);
    }

    #[test]
    fn deleting_alt_screen_placement_preserves_pixels_referenced_by_primary_screen() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=T,f=32,s=1,v=1,i=7,C=1", &[255; 4]),
        );
        let primary = graphics.snapshot(&grid, (4, 8));
        assert_eq!(primary.len(), 1);

        feed(&mut decoder, &mut graphics, &mut grid, b"\x1b[?1049h");
        assert!(graphics.snapshot(&grid, (4, 8)).is_empty());
        let response = feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=p,i=7,C=1\x1b\\",
        );
        assert_eq!(response, [b"\x1b_Gi=7;OK\x1b\\".to_vec()]);
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=d,d=I,i=7\x1b\\",
        );
        assert!(graphics.snapshot(&grid, (4, 8)).is_empty());
        assert!(graphics.store.get(primary[0].image_id).is_some());

        feed(&mut decoder, &mut graphics, &mut grid, b"\x1b[?1049l");
        let restored = graphics.snapshot(&grid, (4, 8));
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].image_id, primary[0].image_id);
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=d,d=I,i=7\x1b\\",
        );
        assert!(graphics.snapshot(&grid, (4, 8)).is_empty());
        assert_eq!(graphics.store.image_count(), 0);
        assert_eq!(
            primary[0].pixels.as_ref(),
            &[255; 4],
            "an in-flight frame keeps its immutable pixels"
        );
    }

    #[test]
    fn retransmission_on_alt_screen_invalidates_hidden_primary_placements() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=T,f=32,s=1,v=1,i=7,C=1", &[255; 4]),
        );
        feed(&mut decoder, &mut graphics, &mut grid, b"\x1b[?1049h");
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=T,f=32,s=1,v=1,i=7,C=1", &[255, 0, 0, 255]),
        );
        let replacement = graphics.snapshot(&grid, (4, 8));
        assert_eq!(replacement.len(), 1);
        assert_eq!(replacement[0].pixels.as_ref(), &[255, 0, 0, 255]);

        feed(&mut decoder, &mut graphics, &mut grid, b"\x1b[?1049l");
        assert!(graphics.snapshot(&grid, (4, 8)).is_empty());
        assert!(grid.image_placements.is_empty());
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=p,i=7,C=1\x1b\\",
        );
        assert_eq!(
            graphics.snapshot(&grid, (4, 8))[0].pixels.as_ref(),
            &[255, 0, 0, 255]
        );
    }

    #[test]
    fn cache_eviction_removes_dangling_placements_without_invalidating_a_frame() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig {
            max_memory_mb: 1,
            ..ImagesConfig::default()
        });
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=T,f=32,s=512,v=512,i=1,C=1", &vec![255; 1024 * 1024]),
        );
        let old_frame = graphics.snapshot(&grid, (4, 8));
        assert_eq!(old_frame.len(), 1);
        let responses = feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=T,f=32,s=1,v=1,i=2,C=1", &[255, 0, 0, 255]),
        );

        assert_eq!(responses, [b"\x1b_Gi=2;OK\x1b\\".to_vec()]);
        assert_eq!(graphics.store.image_count(), 1);
        assert!(graphics.store.get(old_frame[0].image_id).is_none());
        assert_eq!(grid.image_placements.len(), 1);
        assert_eq!(
            graphics.snapshot(&grid, (4, 8))[0].pixels.as_ref(),
            &[255, 0, 0, 255]
        );
        assert_eq!(old_frame[0].pixels.len(), 1024 * 1024);
        assert_eq!(&old_frame[0].pixels[..4], &[255; 4]);
    }

    #[test]
    fn repeated_tiny_placements_evict_oldest_metadata_at_the_limit() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=t,f=32,s=1,v=1,i=7,q=2", &[255; 4]),
        );
        for placement in 1..=MAX_PLACEMENTS + 1 {
            let command = format!("\x1b_Ga=p,i=7,p={placement},C=1,q=2\x1b\\");
            assert!(feed(&mut decoder, &mut graphics, &mut grid, command.as_bytes()).is_empty());
        }

        assert_eq!(grid.image_placements.len(), MAX_PLACEMENTS);
        assert_eq!(
            grid.image_placements.first().unwrap().client_placement_id,
            Some(2)
        );
        assert_eq!(
            grid.image_placements.last().unwrap().client_placement_id,
            Some((MAX_PLACEMENTS + 1) as u32)
        );
        assert_eq!(graphics.store.image_count(), 1);
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 0));
    }
}

pub(crate) struct ImageSnapshot {
    pub(crate) pixels: Arc<[u8]>,
    pub(crate) size: (u32, u32),
    pub(crate) rectangle: (f32, f32, f32, f32),
    pub(crate) z_index: i32,
    image_id: u64,
}

impl SoftwareGraphics {
    pub(crate) fn new(config: &ImagesConfig) -> Self {
        Self {
            store: ImageStore::new(config.max_memory_mb),
            kitty: KittyHandler::new(KittyHandlerOptions::from_megabytes(
                config.kitty.max_image_size_mb,
                config.kitty.allow_file_transfer,
            )),
        }
    }

    /// Called under the grid lock, before decoding any bytes after the image.
    pub(crate) fn process(&mut self, event: TerminalEvent, grid: &mut Grid) -> Option<Vec<u8>> {
        let cell_width = f32::from(grid.cell_pixel_width.max(1));
        let cell_height = f32::from(grid.cell_pixel_height.max(1));
        let response = match event {
            TerminalEvent::Response(response) => Some(response),
            TerminalEvent::KittyGraphics {
                command,
                cursor_row,
                cursor_col,
            } => {
                let outcome = self.kitty.process(
                    command,
                    &mut self.store,
                    cursor_row,
                    cursor_col,
                    cell_width,
                    cell_height,
                    grid.cols(),
                    grid.rows(),
                    &mut grid.image_placements,
                );
                if let Some(image_id) = outcome.retransmitted_image_id {
                    grid.remove_hidden_primary_kitty_placements(image_id);
                }
                if let Some(advance) = outcome.advance {
                    grid.advance_image_cursor(advance.cols, advance.rows);
                }
                let mut candidates = outcome.hard_delete_candidates;
                retain_unreferenced_image_ids(&mut candidates, grid.all_image_placements());
                for image_id in candidates {
                    self.store.remove(image_id);
                }
                outcome.response
            }
            TerminalEvent::SixelGraphics {
                image,
                cursor_row,
                cursor_col,
            } => {
                let columns = image
                    .width
                    .div_ceil(u32::from(grid.cell_pixel_width.max(1)));
                let rows = image
                    .height
                    .div_ceil(u32::from(grid.cell_pixel_height.max(1)));
                if let Some(image_id) = self.store.store(
                    &image.pixels,
                    image.width,
                    image.height,
                    ImageFormat::Rgba,
                    None,
                ) {
                    grid.image_placements.push(ImagePlacement {
                        image_id,
                        placement_id: 0,
                        client_placement_id: None,
                        mode: PlacementMode::Inline {
                            row: cursor_row as _,
                            col: cursor_col,
                            cols: columns,
                            rows,
                            x_offset: 0,
                            y_offset: 0,
                            render_size: InlineRenderSize::NativePixels {
                                width: image.width,
                                height: image.height,
                            },
                        },
                        z_index: 0,
                    });
                }
                for _ in 0..rows {
                    grid.newline();
                }
                None
            }
        };
        // Tiny repeated placements must not grow metadata without bound.
        grid.image_placements
            .retain(|placement| self.store.get(placement.image_id).is_some());
        let excess = grid.image_placements.len().saturating_sub(MAX_PLACEMENTS);
        grid.image_placements.drain(..excess);
        response
    }

    /// Shares immutable pixels; rendering never holds the PTY/grid/cache locks.
    pub(crate) fn snapshot(&self, grid: &Grid, cell_size: (u16, u16)) -> Vec<ImageSnapshot> {
        let (cell_width, cell_height) = (f32::from(cell_size.0), f32::from(cell_size.1));
        let viewport_width = grid.cols() as f32 * cell_width;
        let viewport_height = grid.rows() as f32 * cell_height;
        let mut images: Vec<_> = grid
            .image_placements
            .iter()
            .filter_map(|placement| {
                let stored = self.store.get(placement.image_id)?;
                let (x, y, width, height) = placement.mode.pixel_rect(cell_width, cell_height);
                let y = y + grid.scroll_offset as f32 * cell_height;
                if x >= viewport_width
                    || y >= viewport_height
                    || x + width <= 0.0
                    || y + height <= 0.0
                {
                    return None;
                }
                Some(ImageSnapshot {
                    pixels: stored.pixels.clone(),
                    size: (stored.width, stored.height),
                    rectangle: (x, y, width, height),
                    z_index: placement.z_index,
                    image_id: placement.image_id,
                })
            })
            .collect();
        images.sort_by_key(|image| (image.z_index, image.image_id));
        images
    }
}
