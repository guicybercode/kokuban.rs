use crate::config::ImagesConfig;
use crate::graphics::{
    retain_unreferenced_image_ids, ImagePlacement, InlineRenderSize, PlacementMode,
};
use crate::grid::{Grid, TerminalEvent};
use crate::renderer::image_animation::AnimationUpdate;
use crate::renderer::image_store::{ImageFormat, ImageStore};
use crate::renderer::kitty_handler::{KittyHandler, KittyHandlerOptions};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

const MAX_PLACEMENTS: usize = 4096;
// Snapshot culling uses bounded metadata and at most this many rectangle tests
// per placement, even for a screen containing thousands of unrelated images.
const MAX_OCCLUDERS: usize = 64;

pub(crate) struct SoftwareGraphics {
    store: ImageStore,
    kitty: KittyHandler,
}

pub(crate) struct ImageSnapshot {
    pub(crate) pixels: Arc<[u8]>,
    pub(crate) size: (u32, u32),
    pub(crate) rectangle: (f32, f32, f32, f32),
    pub(crate) z_index: i32,
    image_id: u64,
    opaque: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PixelCoverage {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

impl PixelCoverage {
    fn from_rectangle(rectangle: (f32, f32, f32, f32)) -> Option<Self> {
        let (x, y, width, height) = rectangle;
        if ![x, y, width, height].iter().all(|value| value.is_finite())
            || width <= 0.0
            || height <= 0.0
        {
            return None;
        }
        // Match software_raster's pixel-center coverage, computing far edges
        // in f64 so large f32 origins do not absorb a small positive extent.
        // Do not clip to grid dimensions: the surface can contain additional
        // partial-cell pixels. Containment here holds for every u32 surface.
        let edge = |value: f64| (value - 0.5).ceil().clamp(0.0, f64::from(u32::MAX)) as u32;
        let coverage = Self {
            left: edge(f64::from(x)),
            top: edge(f64::from(y)),
            right: edge(f64::from(x) + f64::from(width)),
            bottom: edge(f64::from(y) + f64::from(height)),
        };
        (coverage.left < coverage.right && coverage.top < coverage.bottom).then_some(coverage)
    }

    fn contains(self, other: Self) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && self.right >= other.right
            && self.bottom >= other.bottom
    }
}

#[derive(Default)]
struct Occluders {
    rectangles: Vec<PixelCoverage>,
}

impl Occluders {
    fn covers(&self, rectangle: PixelCoverage) -> bool {
        self.rectangles
            .iter()
            .any(|occluder| occluder.contains(rectangle))
    }

    fn add(&mut self, rectangle: PixelCoverage) {
        if self.rectangles.len() < MAX_OCCLUDERS {
            self.rectangles.push(rectangle);
        }
    }
}

fn cull_covered_images(images: &mut Vec<ImageSnapshot>) {
    let mut occluders = Occluders::default();
    images.reverse();
    images.retain(|image| {
        let Some(coverage) = PixelCoverage::from_rectangle(image.rectangle) else {
            return true;
        };
        if occluders.covers(coverage) {
            return false;
        }
        if image.opaque {
            occluders.add(coverage);
        }
        true
    });
    images.reverse();
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
        // Translate once per snapshot; scanning the client registry for each placement
        // would make rendering quadratic. Sixel images have no client ID and retain
        // their cache allocation order for equal-z overlaps.
        let client_ids: HashMap<_, _> = self.kitty.image_client_ids().collect();
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
                    opaque: stored.opaque,
                })
            })
            .collect();
        images.sort_by_key(|image| {
            let order_id = client_ids
                .get(&image.image_id)
                .map(|id| u64::from(*id))
                .unwrap_or(image.image_id);
            (image.z_index, order_id)
        });
        // The three image/text drawing groups preserve this image order. A
        // later fully opaque canvas replaces all earlier contributions under
        // its coverage, including across a text/background drawing boundary.
        // Remove only redundant draw work, retaining cache and placements so
        // deleting/editing the covering image reveals previous content again.
        cull_covered_images(&mut images);
        images
    }

    /// Only visible animations schedule work; static and hidden images stay idle.
    pub(crate) fn advance_animations(
        &mut self,
        grid: &Grid,
        cell_size: (u16, u16),
        now: Instant,
    ) -> AnimationUpdate {
        let (cell_width, cell_height) = (f32::from(cell_size.0), f32::from(cell_size.1));
        let viewport_width = grid.cols() as f32 * cell_width;
        let viewport_height = grid.rows() as f32 * cell_height;
        let visible: HashSet<_> = grid
            .image_placements
            .iter()
            .filter_map(|placement| {
                let (x, y, width, height) = placement.mode.pixel_rect(cell_width, cell_height);
                let y = y + grid.scroll_offset as f32 * cell_height;
                (x < viewport_width && y < viewport_height && x + width > 0.0 && y + height > 0.0)
                    .then_some(placement.image_id)
            })
            .collect();
        self.store.advance_animations(now, &visible)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cull_covered_images, ImageSnapshot, Occluders, PixelCoverage, SoftwareGraphics,
        MAX_OCCLUDERS, MAX_PLACEMENTS,
    };
    use crate::config::ImagesConfig;
    use crate::grid::Grid;
    use crate::parser::ansi::GraphicsSupport;
    use crate::software_raster::draw_image_rgba;
    use crate::terminal_decoder::TerminalDecoder;
    use base64::Engine;
    use std::sync::Arc;

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
        let encoded = base64::engine::general_purpose::STANDARD.encode(pixels);
        let mut command = Vec::new();
        let chunks = encoded.as_bytes().chunks(4096);
        let chunk_count = chunks.len();
        for (index, chunk) in chunks.enumerate() {
            let more = u8::from(index + 1 < chunk_count);
            let header = if index == 0 {
                format!("\x1b_G{control},m={more};")
            } else {
                format!("\x1b_Gm={more};")
            };
            command.extend_from_slice(header.as_bytes());
            command.extend_from_slice(chunk);
            command.extend_from_slice(b"\x1b\\");
        }
        command
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

    #[test]
    fn retransmitting_a_lower_client_id_does_not_raise_it_above_a_higher_id() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        for (id, pixel) in [
            (20, [255, 0, 0, 255]),
            (10, [0, 255, 0, 255]),
            (10, [0, 0, 255, 255]),
        ] {
            feed(
                &mut decoder,
                &mut graphics,
                &mut grid,
                &upload(&format!("a=T,f=32,s=1,v=1,i={id},C=1"), &pixel),
            );
        }
        let images = graphics.snapshot(&grid, (4, 8));

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].pixels.as_ref(), &[255, 0, 0, 255]);
        assert_eq!(graphics.store.image_count(), 2);
        // Culling cannot erase protocol-owned data. Removing the high client
        // ID must reveal the most recently transmitted lower-ID canvas.
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=d,d=I,i=20\x1b\\",
        );
        let revealed = graphics.snapshot(&grid, (4, 8));
        assert_eq!(revealed.len(), 1);
        assert_eq!(revealed[0].pixels.as_ref(), &[0, 0, 255, 255]);
    }

    fn image_snapshot(
        id: u64,
        pixel: [u8; 4],
        rectangle: (f32, f32, f32, f32),
        z: i32,
    ) -> ImageSnapshot {
        ImageSnapshot {
            pixels: Arc::from(pixel),
            size: (1, 1),
            rectangle,
            z_index: z,
            image_id: id,
            opaque: pixel[3] == 255,
        }
    }

    fn render_layers(images: &[ImageSnapshot], size: (u32, u32)) -> Vec<u32> {
        let mut frame = vec![0x102030; size.0 as usize * size.1 as usize];
        for image in images.iter().filter(|image| image.z_index < i32::MIN / 2) {
            draw_image_rgba(&mut frame, size, &image.pixels, image.size, image.rectangle);
        }
        // Non-default cell backgrounds sit between the lowest two image groups.
        for (index, pixel) in frame.iter_mut().enumerate() {
            if index % 3 == 0 {
                *pixel = 0x405060;
            }
        }
        for image in images
            .iter()
            .filter(|image| (i32::MIN / 2..0).contains(&image.z_index))
        {
            draw_image_rgba(&mut frame, size, &image.pixels, image.size, image.rectangle);
        }
        // Foreground glyph pixels sit between ordinary negative and positive z.
        for (index, pixel) in frame.iter_mut().enumerate() {
            if index % 5 == 0 {
                *pixel = 0xaabbcc;
            }
        }
        for image in images.iter().filter(|image| image.z_index >= 0) {
            draw_image_rgba(&mut frame, size, &image.pixels, image.size, image.rectangle);
        }
        frame
    }

    #[test]
    fn anonymous_opaque_video_frames_reduce_draws_without_deleting_images() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        let mut first_frame = None;
        for frame in 1..=100u8 {
            let pixels = [frame, 20, 30, 255, 40, frame, 60, 255];
            feed(
                &mut decoder,
                &mut graphics,
                &mut grid,
                &upload("a=T,f=32,s=2,v=1,C=1,q=2", &pixels),
            );
            let snapshot = graphics.snapshot(&grid, (4, 8));
            assert_eq!(
                snapshot.len(),
                1,
                "only the newest complete video frame needs drawing"
            );
            assert_eq!(snapshot[0].pixels.as_ref(), &pixels);
            if first_frame.is_none() {
                first_frame = Some(Arc::clone(&snapshot[0].pixels));
            }
        }
        assert_eq!(grid.image_placements.len(), 100);
        assert_eq!(graphics.store.image_count(), 100);
        assert_eq!(
            first_frame.unwrap().as_ref(),
            &[1, 20, 30, 255, 40, 1, 60, 255]
        );
        let last_id = grid.image_placements.last().unwrap().image_id;
        let client_id = graphics
            .kitty
            .image_client_ids()
            .find(|(id, _)| *id == last_id)
            .unwrap()
            .1;
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            format!("\x1b_Ga=d,d=i,i={client_id}\x1b\\").as_bytes(),
        );
        let revealed = graphics.snapshot(&grid, (4, 8));
        assert_eq!(revealed.len(), 1);
        assert_eq!(
            revealed[0].pixels.as_ref(),
            &[99, 20, 30, 255, 40, 99, 60, 255]
        );
        assert_eq!(
            graphics.store.image_count(),
            100,
            "lowercase deletion preserves cache data"
        );
    }

    #[test]
    fn pixel_center_coverage_culls_only_pixels_the_later_image_actually_draws() {
        for (rectangle, expected_images) in [
            ((0.25, 0.0, 1.3, 1.0), 1),
            ((0.501, 0.0, 1.499, 1.0), 2),
            ((0.0, 0.501, 2.0, 1.0), 2),
            ((-2.0, -1.0, 4.0, 2.0), 1),
        ] {
            let mut images = vec![
                image_snapshot(1, [0, 0, 255, 255], (0.0, 0.0, 2.0, 1.0), 0),
                image_snapshot(2, [255, 0, 0, 255], rectangle, 0),
            ];
            let before = render_layers(&images, (4, 3));
            cull_covered_images(&mut images);
            assert_eq!(images.len(), expected_images, "coverage {rectangle:?}");
            assert_eq!(render_layers(&images, (4, 3)), before);
        }
    }

    #[test]
    fn transparent_partial_and_nonoverlapping_images_never_hide_required_draws() {
        for (pixel, rectangle) in [
            ([255, 0, 0, 128], (0.0, 0.0, 2.0, 2.0)),
            ([255, 0, 0, 0], (0.0, 0.0, 2.0, 2.0)),
            ([255, 0, 0, 255], (0.0, 0.0, 1.0, 2.0)),
            ([255, 0, 0, 255], (2.0, 0.0, 2.0, 2.0)),
        ] {
            let mut images = vec![
                image_snapshot(1, [0, 0, 255, 255], (0.0, 0.0, 2.0, 2.0), 0),
                image_snapshot(2, pixel, rectangle, 0),
            ];
            let before = render_layers(&images, (4, 3));
            cull_covered_images(&mut images);
            assert_eq!(images.len(), 2);
            assert_eq!(render_layers(&images, (4, 3)), before);
        }
    }

    #[test]
    fn culling_keeps_scaled_source_sampling_unchanged_after_negative_origin_clipping() {
        let mut covering = image_snapshot(2, [255; 4], (-1.0, -1.0, 4.0, 4.0), 0);
        covering.size = (2, 2);
        covering.pixels = Arc::from([
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ]);
        let mut images = vec![
            image_snapshot(1, [10, 20, 30, 255], (0.0, 0.0, 3.0, 3.0), 0),
            covering,
        ];
        let before = render_layers(&images, (3, 3));
        cull_covered_images(&mut images);
        assert_eq!(images.len(), 1);
        let after = render_layers(&images, (3, 3));
        assert_eq!(after, before);
        assert_eq!(
            (after[0], after[2], after[6], after[8]),
            (0xff0000, 0x00ff00, 0x0000ff, 0xffffff)
        );
    }

    #[test]
    fn integer_coverage_preserves_small_extents_at_large_float_origins() {
        assert_eq!(
            PixelCoverage::from_rectangle((16_777_216.0, 0.0, 1.0, 1.0)),
            Some(PixelCoverage {
                left: 16_777_216,
                top: 0,
                right: 16_777_217,
                bottom: 1
            })
        );
        for rectangle in [
            (f32::NAN, 0.0, 1.0, 1.0),
            (0.0, 0.0, f32::INFINITY, 1.0),
            (0.0, 0.0, -1.0, 1.0),
            (0.0, 0.0, 0.0, 1.0),
        ] {
            assert_eq!(PixelCoverage::from_rectangle(rectangle), None);
        }
    }

    #[test]
    fn culling_respects_all_image_and_text_layers() {
        let layers = [i32::MIN, i32::MIN / 2 - 1, i32::MIN / 2, -1, 0, 1];
        for (index, lower) in layers.iter().copied().enumerate() {
            for upper in layers[index..].iter().copied() {
                let mut images = vec![
                    image_snapshot(1, [0, 0, 255, 180], (-1.0, -1.0, 4.0, 4.0), lower),
                    image_snapshot(2, [255, 0, 0, 255], (0.0, 0.0, 3.0, 3.0), upper),
                ];
                let before = render_layers(&images, (4, 4));
                cull_covered_images(&mut images);
                assert_eq!(images.len(), 1, "layers {lower}/{upper}");
                assert_eq!(render_layers(&images, (4, 4)), before);
            }
        }
    }

    #[test]
    fn culling_does_not_clip_away_pixels_in_a_partial_cell_surface_margin() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        grid.resize(2, 1);
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=T,f=32,s=10,v=1,i=1,C=1", &[0, 0, 255, 255].repeat(10)),
        );
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=T,f=32,s=8,v=1,i=2,C=1", &[255, 0, 0, 255].repeat(8)),
        );
        let images = graphics.snapshot(&grid, (4, 8));
        assert_eq!(images.len(), 2);
        let mut frame = vec![0; 10 * 8];
        for image in &images {
            draw_image_rgba(
                &mut frame,
                (10, 8),
                &image.pixels,
                image.size,
                image.rectangle,
            );
        }
        assert_eq!(
            &frame[..10],
            &[
                0xff0000, 0xff0000, 0xff0000, 0xff0000, 0xff0000, 0xff0000, 0xff0000, 0xff0000,
                0x0000ff, 0x0000ff
            ]
        );
    }

    #[test]
    fn bounded_occluders_keep_thousands_of_nonoverlapping_images() {
        let mut images: Vec<_> = (0..MAX_PLACEMENTS)
            .map(|index| image_snapshot(index as u64, [255; 4], (index as f32, 0.0, 1.0, 1.0), 0))
            .collect();
        cull_covered_images(&mut images);
        assert_eq!(images.len(), MAX_PLACEMENTS);
        let mut occluders = Occluders::default();
        for image in &images {
            occluders.add(PixelCoverage::from_rectangle(image.rectangle).unwrap());
        }
        assert_eq!(occluders.rectangles.len(), MAX_OCCLUDERS);
        assert!(occluders.rectangles.capacity() <= MAX_OCCLUDERS * 2);
    }

    #[test]
    fn animation_transparency_restores_the_covered_image_on_frame_selection() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        for (id, pixel) in [(1, [0, 0, 255, 255]), (2, [255, 0, 0, 255])] {
            feed(
                &mut decoder,
                &mut graphics,
                &mut grid,
                &upload(&format!("a=T,f=32,s=1,v=1,i={id},c=2,r=1,C=1"), &pixel),
            );
        }
        assert_eq!(graphics.snapshot(&grid, (4, 8)).len(), 1);
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=f,f=32,s=1,v=1,i=2", &[0, 255, 0, 128]),
        );
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=a,i=2,c=2\x1b\\",
        );
        let transparent = graphics.snapshot(&grid, (4, 8));
        assert_eq!(transparent.len(), 2);
        let rendered = render_layers(&transparent, (8, 8));
        assert_eq!(rendered[1], 0x00807f);
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=a,i=2,c=1\x1b\\",
        );
        assert_eq!(graphics.snapshot(&grid, (4, 8)).len(), 1);
    }

    #[test]
    fn numbered_animation_uploads_continue_without_selectors_and_control_is_silent() {
        use std::time::{Duration, Instant};
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload(
                "a=T,f=32,s=2,v=1,I=42,C=1",
                &[255, 0, 0, 255, 255, 0, 0, 255],
            ),
        );
        // The client repeats a=f but omits I and metadata in the continuation.
        let responses = feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=f,f=32,s=1,v=1,I=42,c=1,x=1,z=10000,C=1,m=1;AP8A\x1b\\\x1b_Ga=f,m=0;/w\x1b\\",
        );
        assert_eq!(responses, [b"\x1b_Gi=1,I=42,r=2;OK\x1b\\".to_vec()]);
        assert!(feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=a,I=42,r=1,z=10000,v=2,s=3\x1b\\"
        )
        .is_empty());
        let update = graphics.advance_animations(&grid, (4, 8), Instant::now());
        assert!(update.next_deadline.is_some());
        let next = update.next_deadline.unwrap();
        assert!(graphics.advance_animations(&grid, (4, 8), next).changed);
        assert_eq!(
            graphics.snapshot(&grid, (4, 8))[0].pixels.as_ref(),
            &[255, 0, 0, 255, 0, 255, 0, 255]
        );
        let stopped = graphics.advance_animations(&grid, (4, 8), next + Duration::from_secs(60));
        assert!(stopped.next_deadline.is_none());
        assert_eq!(grid.image_placements.len(), 1);
    }

    #[test]
    fn frame_composition_and_deletion_update_existing_placements() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload(
                "a=T,f=32,s=2,v=1,i=7,C=1",
                &[255, 0, 0, 255, 255, 0, 0, 255],
            ),
        );
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=f,f=32,s=2,v=1,i=7", &[0, 255, 0, 255, 0, 0, 255, 255]),
        );
        let responses = feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=c,i=7,r=2,c=1,X=1,x=0,w=1,h=1,C=1\x1b\\",
        );
        assert_eq!(responses, [b"\x1b_Gi=7;OK\x1b\\".to_vec()]);
        assert_eq!(
            graphics.snapshot(&grid, (4, 8))[0].pixels.as_ref(),
            &[0, 0, 255, 255, 255, 0, 0, 255]
        );
        // Root deletion promotes the second canvas without creating a placement.
        assert!(feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=d,d=f,i=7,r=1\x1b\\"
        )
        .is_empty());
        assert_eq!(
            graphics.snapshot(&grid, (4, 8))[0].pixels.as_ref(),
            &[0, 255, 0, 255, 0, 0, 255, 255]
        );
        assert!(feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=d,d=f,i=7\x1b\\"
        )
        .is_empty());
        assert_eq!(grid.image_placements.len(), 1);
        assert!(feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=d,d=F,i=7\x1b\\"
        )
        .is_empty());
        assert!(grid.image_placements.is_empty());
        assert_eq!(graphics.store.image_count(), 0);
    }

    #[test]
    fn hidden_animation_has_no_deadline_and_catches_up_when_visible() {
        use std::time::{Duration, Instant};
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=T,f=32,s=1,v=1,i=7,C=1", &[255, 0, 0, 255]),
        );
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=f,f=32,s=1,v=1,i=7,z=10000", &[0, 255, 0, 255]),
        );
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=a,i=7,r=1,z=10000,s=3,v=2\x1b\\",
        );
        grid.scroll_offset = 6;
        let now = Instant::now() + Duration::from_secs(60);
        let hidden = graphics.advance_animations(&grid, (4, 8), now);
        assert!(!hidden.changed);
        assert!(hidden.next_deadline.is_none());
        grid.scroll_offset = 0;
        let visible = graphics.advance_animations(&grid, (4, 8), now);
        assert!(visible.changed);
        assert!(visible.next_deadline.is_none());
        assert_eq!(
            graphics.snapshot(&grid, (4, 8))[0].pixels.as_ref(),
            &[0, 255, 0, 255]
        );
    }

    #[test]
    fn malformed_and_interleaved_frames_preserve_the_existing_image() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=T,f=32,s=1,v=1,i=7,C=1", &[255, 0, 0, 255]),
        );
        let invalid = feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=f,f=32,s=1,v=1,i=7,c=99,r=1", &[0, 255, 0, 255]),
        );
        // Editing ignores the append-only base selector and returns the edited frame.
        assert_eq!(invalid, [b"\x1b_Gi=7,r=1;OK\x1b\\".to_vec()]);
        let invalid = feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=f,f=32,s=1,v=1,i=7,r=99", &[0, 0, 255, 255]),
        );
        assert!(String::from_utf8_lossy(&invalid[0]).contains("i=7,r=99;ENOENT:"));
        assert!(feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=f,f=32,s=1,v=1,i=7,m=1;AAAA\x1b\\"
        )
        .is_empty());
        let interleaved = feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=t,i=7,m=0;/w==\x1b\\",
        );
        assert!(String::from_utf8_lossy(&interleaved[0]).contains("EINVAL:"));
        let invalid = feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=a,i=7,s=9\x1b\\",
        );
        assert!(String::from_utf8_lossy(&invalid[0]).contains("EINVAL:"));
        assert_eq!(
            graphics.snapshot(&grid, (4, 8))[0].pixels.as_ref(),
            &[0, 255, 0, 255]
        );
        assert_eq!(grid.image_placements.len(), 1);
    }

    #[test]
    fn frame_chunk_errors_keep_the_target_identity_and_pending_quiet_mode() {
        let (mut decoder, mut graphics, mut grid) = fixture(ImagesConfig::default());
        feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            &upload("a=T,f=32,s=1,v=1,I=42,C=1", &[255, 0, 0, 255]),
        );
        let errors = feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=f,I=42,f=32,s=1,v=1,m=1;AAAA\x1b\\\x1b_Ga=f,I=43,m=0;/w\x1b\\",
        );
        assert_eq!(
            errors,
            [b"\x1b_Gi=1,I=42;EINVAL:interleaved transmission I=43\x1b\\".to_vec()]
        );
        assert!(feed(
            &mut decoder,
            &mut graphics,
            &mut grid,
            b"\x1b_Ga=f,I=42,q=2,f=32,s=1,v=1,m=1;AAAA\x1b\\\x1b_Ga=f,r=bad,m=0;/w\x1b\\"
        )
        .is_empty());
        assert_eq!(
            graphics.snapshot(&grid, (4, 8))[0].pixels.as_ref(),
            &[255, 0, 0, 255]
        );
    }
}
