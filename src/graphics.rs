pub type ImageId = u32;

#[derive(Debug, Clone)]
pub struct ImagePlacement {
    pub image_id: ImageId,
    /// Non-zero renderer-local identity. Sixel reserves zero as its source marker.
    pub placement_id: u32,
    /// Protocol `p=` supplied by the Kitty client, distinct from local identity.
    pub client_placement_id: Option<u32>,
    pub mode: PlacementMode,
    pub z_index: i32,
}

#[derive(Debug, Clone)]
pub enum PlacementMode {
    Inline {
        row: usize,
        col: usize,
        /// Effective cell footprint used for cursor movement and cell sizing.
        cols: u32,
        rows: u32,
        x_offset: u32,
        y_offset: u32,
        /// Visual size, which may remain pixel-native inside a larger footprint.
        render_size: InlineRenderSize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineRenderSize {
    CellAnchored,
    NativePixels { width: u32, height: u32 },
}

impl InlineRenderSize {
    /// Kitty keeps the source pixel size only when both c and r are automatic.
    pub(crate) fn for_kitty_request(
        columns: Option<u32>,
        rows: Option<u32>,
        image_width: u32,
        image_height: u32,
    ) -> Self {
        if columns.unwrap_or(0) == 0 && rows.unwrap_or(0) == 0 {
            Self::NativePixels {
                width: image_width,
                height: image_height,
            }
        } else {
            Self::CellAnchored
        }
    }
}

pub(crate) fn cell_anchored_pixel_extent(
    cells: u32,
    cell_extent: f32,
    pixel_offset: u32,
) -> f32 {
    let pixel_offset = cell_anchored_pixel_offset(pixel_offset, cell_extent);
    (cells as f32 * cell_extent - pixel_offset).max(0.0)
}

fn cell_anchored_pixel_offset(pixel_offset: u32, cell_extent: f32) -> f32 {
    if !cell_extent.is_finite() || cell_extent <= 0.0 {
        return 0.0;
    }

    let max_offset = (cell_extent.ceil() - 1.0).max(0.0);
    f64::from(pixel_offset).min(f64::from(max_offset)) as f32
}

pub(crate) fn placement_cell_count(
    requested_cells: Option<u32>,
    image_extent: u32,
    pixel_offset: u32,
    cell_extent: f32,
) -> u32 {
    // Kitty treats an omitted or zero c/r as automatic. The effective cell
    // footprint includes X/Y even though auto/auto keeps its native pixel size.
    if let Some(cells) = requested_cells.filter(|cells| *cells != 0) {
        return cells;
    }

    let occupied_pixels = f64::from(image_extent) + f64::from(pixel_offset);
    (occupied_pixels / f64::from(cell_extent))
        .ceil()
        .clamp(1.0, f64::from(u32::MAX)) as u32
}

impl PlacementMode {
    pub(crate) fn pixel_rect(
        &self,
        cell_width: f32,
        cell_height: f32,
    ) -> (f32, f32, f32, f32) {
        match self {
            // X/Y move the origin within the first cell. Explicit c/r stop at
            // their cell boundary; when both are automatic, pixels stay native.
            Self::Inline {
                row,
                col,
                cols,
                rows,
                x_offset,
                y_offset,
                render_size,
            } => {
                let effective_x_offset = cell_anchored_pixel_offset(*x_offset, cell_width);
                let effective_y_offset = cell_anchored_pixel_offset(*y_offset, cell_height);
                let (width, height) = match render_size {
                    InlineRenderSize::CellAnchored => (
                        cell_anchored_pixel_extent(*cols, cell_width, *x_offset),
                        cell_anchored_pixel_extent(*rows, cell_height, *y_offset),
                    ),
                    InlineRenderSize::NativePixels { width, height } => {
                        (*width as f32, *height as f32)
                    }
                };
                (
                    *col as f32 * cell_width + effective_x_offset,
                    *row as f32 * cell_height + effective_y_offset,
                    width,
                    height,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{placement_cell_count, InlineRenderSize, PlacementMode};

    #[test]
    fn inline_pixel_rect_tracks_cell_size_and_preserves_offsets() {
        let placement = PlacementMode::Inline {
            row: 3,
            col: 7,
            cols: 4,
            rows: 2,
            x_offset: 3,
            y_offset: 5,
            render_size: InlineRenderSize::CellAnchored,
        };

        assert_eq!(placement.pixel_rect(10.0, 20.0), (73.0, 65.0, 37.0, 35.0));
        assert_eq!(placement.pixel_rect(20.0, 30.0), (143.0, 95.0, 77.0, 55.0));
    }

    #[test]
    fn inline_offsets_stay_inside_cells_after_cell_size_shrinks() {
        let placement = PlacementMode::Inline {
            row: 0,
            col: 0,
            cols: 1,
            rows: 1,
            x_offset: 9,
            y_offset: 19,
            render_size: InlineRenderSize::CellAnchored,
        };

        assert_eq!(placement.pixel_rect(10.0, 20.0), (9.0, 19.0, 1.0, 1.0));
        assert_eq!(placement.pixel_rect(8.0, 16.0), (7.0, 15.0, 1.0, 1.0));
    }

    #[test]
    fn native_pixel_size_is_independent_from_its_effective_cell_footprint() {
        let placement = PlacementMode::Inline {
            row: 3,
            col: 7,
            cols: 2,
            rows: 2,
            x_offset: 9,
            y_offset: 19,
            render_size: InlineRenderSize::NativePixels {
                width: 10,
                height: 20,
            },
        };

        assert_eq!(
            placement.pixel_rect(10.0, 20.0),
            (79.0, 79.0, 10.0, 20.0)
        );
        assert_eq!(
            placement.pixel_rect(8.0, 16.0),
            (63.0, 63.0, 10.0, 20.0)
        );

        let explicit = PlacementMode::Inline {
            row: 3,
            col: 7,
            cols: 2,
            rows: 2,
            x_offset: 9,
            y_offset: 19,
            render_size: InlineRenderSize::CellAnchored,
        };
        assert_eq!(
            explicit.pixel_rect(10.0, 20.0),
            (79.0, 79.0, 11.0, 21.0)
        );
    }

    #[test]
    fn kitty_auto_size_normalizes_missing_and_zero_cell_counts() {
        let native = InlineRenderSize::NativePixels {
            width: 10,
            height: 20,
        };
        for (columns, rows) in [
            (None, None),
            (Some(0), None),
            (None, Some(0)),
            (Some(0), Some(0)),
        ] {
            assert_eq!(
                InlineRenderSize::for_kitty_request(columns, rows, 10, 20),
                native
            );
        }

        for (columns, rows) in [
            (Some(1), None),
            (None, Some(1)),
            (Some(1), Some(0)),
            (Some(0), Some(1)),
            (Some(1), Some(1)),
        ] {
            assert_eq!(
                InlineRenderSize::for_kitty_request(columns, rows, 10, 20),
                InlineRenderSize::CellAnchored
            );
        }
    }

    #[test]
    fn automatic_cell_footprint_includes_offsets_without_expanding_explicit_sizes() {
        assert_eq!(placement_cell_count(None, 10, 9, 10.0), 2);
        assert_eq!(placement_cell_count(Some(0), 10, 9, 10.0), 2);
        assert_eq!(placement_cell_count(None, 20, 19, 20.0), 2);
        assert_eq!(placement_cell_count(Some(1), 10, 9, 10.0), 1);
        assert_eq!(placement_cell_count(None, 11, 0, 10.0), 2);
    }
}
