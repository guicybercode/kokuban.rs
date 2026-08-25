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
        cols: u32,
        rows: u32,
        x_offset: u32,
        y_offset: u32,
    },
    Overlay {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
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

impl PlacementMode {
    pub(crate) fn pixel_rect(
        &self,
        cell_width: f32,
        cell_height: f32,
    ) -> (f32, f32, f32, f32) {
        match self {
            // X/Y move the origin within the first cell without expanding the
            // right or bottom boundary selected by c/r.
            Self::Inline {
                row,
                col,
                cols,
                rows,
                x_offset,
                y_offset,
            } => {
                let effective_x_offset = cell_anchored_pixel_offset(*x_offset, cell_width);
                let effective_y_offset = cell_anchored_pixel_offset(*y_offset, cell_height);
                (
                    *col as f32 * cell_width + effective_x_offset,
                    *row as f32 * cell_height + effective_y_offset,
                    cell_anchored_pixel_extent(*cols, cell_width, *x_offset),
                    cell_anchored_pixel_extent(*rows, cell_height, *y_offset),
                )
            }
            Self::Overlay {
                x,
                y,
                width,
                height,
            } => (*x, *y, *width, *height),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{cell_anchored_pixel_extent, PlacementMode};

    #[test]
    fn inline_pixel_rect_tracks_cell_size_and_preserves_offsets() {
        let placement = PlacementMode::Inline {
            row: 3,
            col: 7,
            cols: 4,
            rows: 2,
            x_offset: 3,
            y_offset: 5,
        };

        assert_eq!(placement.pixel_rect(10.0, 20.0), (73.0, 65.0, 37.0, 35.0));
        assert_eq!(placement.pixel_rect(20.0, 30.0), (143.0, 95.0, 77.0, 55.0));
    }

    #[test]
    fn cursor_policy_modes_share_cell_anchored_geometry() {
        let inline = PlacementMode::Inline {
            row: 3,
            col: 7,
            cols: 1,
            rows: 1,
            x_offset: 9,
            y_offset: 19,
        };
        let overlay = PlacementMode::Overlay {
            x: 79.0,
            y: 79.0,
            width: cell_anchored_pixel_extent(1, 10.0, 9),
            height: cell_anchored_pixel_extent(1, 20.0, 19),
        };

        assert_eq!(inline.pixel_rect(10.0, 20.0), overlay.pixel_rect(10.0, 20.0));
        assert_eq!(inline.pixel_rect(10.0, 20.0), (79.0, 79.0, 1.0, 1.0));
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
        };

        assert_eq!(placement.pixel_rect(10.0, 20.0), (9.0, 19.0, 1.0, 1.0));
        assert_eq!(placement.pixel_rect(8.0, 16.0), (7.0, 15.0, 1.0, 1.0));
    }
}
