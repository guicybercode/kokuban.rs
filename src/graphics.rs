use std::collections::{HashMap, HashSet};

pub type ImageId = u64;
pub type KittyImageId = u32;

/// Maps pane-local Kitty protocol IDs to IDs in the shared renderer cache.
#[derive(Debug)]
pub(crate) struct ClientImageRegistry {
    by_client_id: HashMap<KittyImageId, ImageId>,
    next_client_id: KittyImageId,
}

impl Default for ClientImageRegistry {
    fn default() -> Self {
        Self {
            by_client_id: HashMap::new(),
            next_client_id: 1,
        }
    }
}

impl ClientImageRegistry {
    pub(crate) fn len(&self) -> usize {
        self.by_client_id.len()
    }

    pub(crate) fn next_id(&mut self) -> KittyImageId {
        loop {
            let candidate = self.next_client_id.max(1);
            self.next_client_id = candidate.wrapping_add(1).max(1);
            if !self.by_client_id.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    pub(crate) fn record(&mut self, client_id: KittyImageId, image_id: ImageId) {
        if client_id != 0 && image_id != 0 {
            self.by_client_id.insert(client_id, image_id);
        }
    }

    pub(crate) fn get(&self, client_id: KittyImageId) -> Option<ImageId> {
        self.by_client_id.get(&client_id).copied()
    }

    pub(crate) fn resolve_live(
        &self,
        client_id: KittyImageId,
        mut exists: impl FnMut(ImageId) -> bool,
    ) -> Option<ImageId> {
        let image_id = self.get(client_id)?;
        exists(image_id).then_some(image_id)
    }

    pub(crate) fn retain_existing(&mut self, mut exists: impl FnMut(ImageId) -> bool) {
        self.by_client_id
            .retain(|_, image_id| exists(*image_id));
    }
}

#[derive(Debug, Default)]
pub(crate) struct ImageNumberRegistry {
    by_number: HashMap<u32, Vec<KittyImageId>>,
    by_id: HashMap<KittyImageId, u32>,
}

impl ImageNumberRegistry {
    pub(crate) fn len(&self) -> usize {
        self.by_id.len()
    }

    pub(crate) fn record_new(&mut self, number: u32, image_id: KittyImageId) {
        if number == 0 || image_id == 0 {
            return;
        }

        self.forget(image_id);
        self.by_number.entry(number).or_default().push(image_id);
        self.by_id.insert(image_id, number);
    }

    pub(crate) fn forget(&mut self, image_id: KittyImageId) {
        let Some(number) = self.by_id.remove(&image_id) else {
            return;
        };
        let should_remove = if let Some(image_ids) = self.by_number.get_mut(&number) {
            image_ids.retain(|candidate| *candidate != image_id);
            image_ids.is_empty()
        } else {
            false
        };
        if should_remove {
            self.by_number.remove(&number);
        }
    }

    pub(crate) fn retain_existing(
        &mut self,
        mut exists: impl FnMut(KittyImageId) -> bool,
    ) {
        let by_id = &mut self.by_id;
        self.by_number.retain(|_, image_ids| {
            image_ids.retain(|image_id| {
                let keep = exists(*image_id);
                if !keep {
                    by_id.remove(image_id);
                }
                keep
            });
            !image_ids.is_empty()
        });
    }

    #[cfg(test)]
    pub(crate) fn newest_existing(
        &mut self,
        number: u32,
        mut exists: impl FnMut(KittyImageId) -> bool,
    ) -> Option<KittyImageId> {
        if number == 0 {
            return None;
        }

        let image_ids = self.by_number.remove(&number)?;
        let mut existing_ids = Vec::with_capacity(image_ids.len());
        for image_id in image_ids {
            if exists(image_id) {
                existing_ids.push(image_id);
            } else {
                self.by_id.remove(&image_id);
            }
        }
        let newest = existing_ids.last().copied();
        if !existing_ids.is_empty() {
            self.by_number.insert(number, existing_ids);
        }
        newest
    }

    pub(crate) fn newest_matching(
        &self,
        number: u32,
        mut matches: impl FnMut(KittyImageId) -> bool,
    ) -> Option<KittyImageId> {
        if number == 0 {
            return None;
        }

        self.by_number
            .get(&number)?
            .iter()
            .rev()
            .copied()
            .find(|image_id| matches(*image_id))
    }
}

pub(crate) fn next_available_image_id(
    next_id: &mut ImageId,
    mut is_occupied: impl FnMut(ImageId) -> bool,
) -> ImageId {
    loop {
        let candidate = (*next_id).max(1);
        *next_id = candidate.wrapping_add(1).max(1);
        if !is_occupied(candidate) {
            return candidate;
        }
    }
}

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

/// Keep only cache-deletion candidates with no remaining placement reference.
pub(crate) fn retain_unreferenced_image_ids<'a>(
    candidates: &mut HashSet<ImageId>,
    placements: impl IntoIterator<Item = &'a ImagePlacement>,
) {
    for placement in placements {
        candidates.remove(&placement.image_id);
        if candidates.is_empty() {
            return;
        }
    }
}

#[derive(Debug, Clone)]
pub enum PlacementMode {
    Inline {
        row: usize,
        col: usize,
        /// Grid-bounded cell footprint used for cursor movement and cell sizing.
        cols: u32,
        rows: u32,
        x_offset: u32,
        y_offset: u32,
        /// Visual sizing policy, kept separate from the cursor footprint.
        render_size: InlineRenderSize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineRenderSize {
    CellAnchored,
    NativePixels { width: u32, height: u32 },
    AspectFromColumns {
        columns: u32,
        source_width: u32,
        source_height: u32,
    },
    AspectFromRows {
        rows: u32,
        source_width: u32,
        source_height: u32,
    },
}

impl InlineRenderSize {
    /// Resolve Kitty's native, aspect-preserving, or fully explicit sizing mode.
    fn for_kitty_request(
        columns: Option<u32>,
        rows: Option<u32>,
        image_width: u32,
        image_height: u32,
    ) -> Self {
        let columns = columns.filter(|columns| *columns != 0);
        let rows = rows.filter(|rows| *rows != 0);
        match (columns, rows) {
            (None, None) => Self::NativePixels {
                width: image_width,
                height: image_height,
            },
            (Some(columns), None) => Self::AspectFromColumns {
                columns,
                source_width: image_width,
                source_height: image_height,
            },
            (None, Some(rows)) => Self::AspectFromRows {
                rows,
                source_width: image_width,
                source_height: image_height,
            },
            (Some(_), Some(_)) => Self::CellAnchored,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KittyPlacementLayout {
    pub display_cols: u32,
    pub display_rows: u32,
    pub render_size: InlineRenderSize,
}

fn cell_anchored_pixel_extent(
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

fn placement_cell_count(
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

fn aspect_ratio_cell_count(
    explicit_cells: u32,
    explicit_cell_extent: f32,
    explicit_pixel_offset: u32,
    explicit_source_extent: u32,
    automatic_source_extent: u32,
    automatic_cell_extent: f32,
) -> u32 {
    if explicit_source_extent == 0 {
        return 1;
    }

    // Match Kitty's effective_num_* calculation. Its footprint uses +X/+Y,
    // while the visual rectangle stops at the explicit cell boundary.
    let explicit_pixels = f64::from(explicit_cells) * f64::from(explicit_cell_extent)
        + f64::from(explicit_pixel_offset);
    let automatic_pixels = explicit_pixels * f64::from(automatic_source_extent)
        / f64::from(explicit_source_extent);
    (automatic_pixels / f64::from(automatic_cell_extent))
        .ceil()
        .clamp(1.0, f64::from(u32::MAX)) as u32
}

pub(crate) fn resolve_kitty_placement_layout(
    columns: Option<u32>,
    rows: Option<u32>,
    image_size: (u32, u32),
    pixel_offsets: (u32, u32),
    cell_size: (f32, f32),
) -> KittyPlacementLayout {
    let (image_width, image_height) = image_size;
    let (x_offset, y_offset) = pixel_offsets;
    let (cell_width, cell_height) = cell_size;
    let columns = columns.filter(|columns| *columns != 0);
    let rows = rows.filter(|rows| *rows != 0);
    let render_size = InlineRenderSize::for_kitty_request(
        columns,
        rows,
        image_width,
        image_height,
    );
    let (display_cols, display_rows) = match (columns, rows) {
        (None, None) => (
            placement_cell_count(None, image_width, x_offset, cell_width),
            placement_cell_count(None, image_height, y_offset, cell_height),
        ),
        (Some(columns), None) => (
            columns,
            aspect_ratio_cell_count(
                columns,
                cell_width,
                x_offset,
                image_width,
                image_height,
                cell_height,
            ),
        ),
        (None, Some(rows)) => (
            aspect_ratio_cell_count(
                rows,
                cell_height,
                y_offset,
                image_height,
                image_width,
                cell_width,
            ),
            rows,
        ),
        (Some(columns), Some(rows)) => (columns, rows),
    };

    KittyPlacementLayout {
        display_cols,
        display_rows,
        render_size,
    }
}

fn aspect_scaled_pixel_extent(
    explicit_pixel_extent: f32,
    explicit_source_extent: u32,
    automatic_source_extent: u32,
) -> f32 {
    if explicit_source_extent == 0 {
        return 0.0;
    }

    (f64::from(explicit_pixel_extent) * f64::from(automatic_source_extent)
        / f64::from(explicit_source_extent)) as f32
}

impl PlacementMode {
    /// Cell footprint used by Kitty's cursor and intersection-based deletes.
    /// Automatic axes are recalculated when the renderer's cell size changes.
    pub(crate) fn effective_cell_rect(
        &self,
        cell_width: f32,
        cell_height: f32,
    ) -> (usize, usize, u32, u32) {
        match self {
            Self::Inline {
                row,
                col,
                cols,
                rows,
                x_offset,
                y_offset,
                render_size,
            } => {
                let effective_x_offset =
                    cell_anchored_pixel_offset(*x_offset, cell_width) as u32;
                let effective_y_offset =
                    cell_anchored_pixel_offset(*y_offset, cell_height) as u32;
                let (effective_cols, effective_rows) = match render_size {
                    InlineRenderSize::CellAnchored => (*cols, *rows),
                    InlineRenderSize::NativePixels { width, height } => (
                        placement_cell_count(
                            None,
                            *width,
                            effective_x_offset,
                            cell_width,
                        ),
                        placement_cell_count(
                            None,
                            *height,
                            effective_y_offset,
                            cell_height,
                        ),
                    ),
                    InlineRenderSize::AspectFromColumns {
                        columns,
                        source_width,
                        source_height,
                    } => (
                        *columns,
                        aspect_ratio_cell_count(
                            *columns,
                            cell_width,
                            effective_x_offset,
                            *source_width,
                            *source_height,
                            cell_height,
                        ),
                    ),
                    InlineRenderSize::AspectFromRows {
                        rows,
                        source_width,
                        source_height,
                    } => (
                        aspect_ratio_cell_count(
                            *rows,
                            cell_height,
                            effective_y_offset,
                            *source_height,
                            *source_width,
                            cell_width,
                        ),
                        *rows,
                    ),
                };
                (*row, *col, effective_cols, effective_rows)
            }
        }
    }

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
                    InlineRenderSize::AspectFromColumns {
                        columns,
                        source_width,
                        source_height,
                    } => {
                        let width = cell_anchored_pixel_extent(
                            *columns,
                            cell_width,
                            *x_offset,
                        );
                        let height = aspect_scaled_pixel_extent(
                            width,
                            *source_width,
                            *source_height,
                        );
                        (width, height)
                    }
                    InlineRenderSize::AspectFromRows {
                        rows,
                        source_width,
                        source_height,
                    } => {
                        let height = cell_anchored_pixel_extent(
                            *rows,
                            cell_height,
                            *y_offset,
                        );
                        let width = aspect_scaled_pixel_extent(
                            height,
                            *source_height,
                            *source_width,
                        );
                        (width, height)
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
    use super::{
        aspect_ratio_cell_count, placement_cell_count,
        next_available_image_id, resolve_kitty_placement_layout,
        retain_unreferenced_image_ids, ClientImageRegistry, ImageNumberRegistry,
        ImagePlacement, InlineRenderSize, KittyPlacementLayout, PlacementMode,
    };
    use std::collections::HashSet;

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

        for (columns, rows) in [(Some(1), None), (Some(1), Some(0))] {
            assert_eq!(
                InlineRenderSize::for_kitty_request(columns, rows, 10, 20),
                InlineRenderSize::AspectFromColumns {
                    columns: 1,
                    source_width: 10,
                    source_height: 20,
                }
            );
        }

        for (columns, rows) in [(None, Some(1)), (Some(0), Some(1))] {
            assert_eq!(
                InlineRenderSize::for_kitty_request(columns, rows, 10, 20),
                InlineRenderSize::AspectFromRows {
                    rows: 1,
                    source_width: 10,
                    source_height: 20,
                }
            );
        }

        assert_eq!(
            InlineRenderSize::for_kitty_request(Some(1), Some(1), 10, 20),
            InlineRenderSize::CellAnchored
        );
    }

    #[test]
    fn automatic_cell_footprint_includes_offsets_without_expanding_explicit_sizes() {
        assert_eq!(placement_cell_count(None, 10, 9, 10.0), 2);
        assert_eq!(placement_cell_count(Some(0), 10, 9, 10.0), 2);
        assert_eq!(placement_cell_count(None, 20, 19, 20.0), 2);
        assert_eq!(placement_cell_count(Some(1), 10, 9, 10.0), 1);
        assert_eq!(placement_cell_count(None, 11, 0, 10.0), 2);
    }

    #[test]
    fn kitty_layout_resolves_auto_and_single_axis_requests_together() {
        let native = KittyPlacementLayout {
            display_cols: 3,
            display_rows: 2,
            render_size: InlineRenderSize::NativePixels {
                width: 20,
                height: 10,
            },
        };
        for (columns, rows) in [
            (None, None),
            (Some(0), None),
            (None, Some(0)),
            (Some(0), Some(0)),
        ] {
            assert_eq!(
                resolve_kitty_placement_layout(
                    columns,
                    rows,
                    (20, 10),
                    (2, 3),
                    (10.0, 10.0),
                ),
                native
            );
        }

        let from_columns = KittyPlacementLayout {
            display_cols: 3,
            display_rows: 2,
            render_size: InlineRenderSize::AspectFromColumns {
                columns: 3,
                source_width: 20,
                source_height: 10,
            },
        };
        for rows in [None, Some(0)] {
            assert_eq!(
                resolve_kitty_placement_layout(
                    Some(3),
                    rows,
                    (20, 10),
                    (2, 3),
                    (10.0, 10.0),
                ),
                from_columns
            );
        }

        let from_rows = KittyPlacementLayout {
            display_cols: 7,
            display_rows: 3,
            render_size: InlineRenderSize::AspectFromRows {
                rows: 3,
                source_width: 20,
                source_height: 10,
            },
        };
        for columns in [None, Some(0)] {
            assert_eq!(
                resolve_kitty_placement_layout(
                    columns,
                    Some(3),
                    (20, 10),
                    (2, 3),
                    (10.0, 10.0),
                ),
                from_rows
            );
        }

        assert_eq!(
            resolve_kitty_placement_layout(
                Some(3),
                Some(2),
                (20, 10),
                (2, 3),
                (10.0, 10.0),
            ),
            KittyPlacementLayout {
                display_cols: 3,
                display_rows: 2,
                render_size: InlineRenderSize::CellAnchored,
            }
        );
    }

    #[test]
    fn single_axis_pixel_rect_preserves_aspect_ratio_after_cell_resize() {
        let from_columns = PlacementMode::Inline {
            row: 0,
            col: 0,
            cols: 3,
            rows: 2,
            x_offset: 2,
            y_offset: 3,
            render_size: InlineRenderSize::AspectFromColumns {
                columns: 3,
                source_width: 20,
                source_height: 10,
            },
        };
        assert_eq!(
            from_columns.pixel_rect(10.0, 10.0),
            (2.0, 3.0, 28.0, 14.0)
        );
        assert_eq!(
            from_columns.pixel_rect(8.0, 6.0),
            (2.0, 3.0, 22.0, 11.0)
        );
        assert_eq!(
            from_columns.effective_cell_rect(8.0, 6.0),
            (0, 0, 3, 3)
        );

        let from_rows = PlacementMode::Inline {
            row: 0,
            col: 0,
            cols: 7,
            rows: 3,
            x_offset: 2,
            y_offset: 3,
            render_size: InlineRenderSize::AspectFromRows {
                rows: 3,
                source_width: 20,
                source_height: 10,
            },
        };
        assert_eq!(
            from_rows.pixel_rect(10.0, 10.0),
            (2.0, 3.0, 54.0, 27.0)
        );
        assert_eq!(
            from_rows.pixel_rect(8.0, 6.0),
            (2.0, 3.0, 30.0, 15.0)
        );
        assert_eq!(
            from_rows.effective_cell_rect(8.0, 6.0),
            (0, 0, 6, 3)
        );
    }

    #[test]
    fn single_axis_visual_rect_and_effective_footprint_are_distinct() {
        let from_columns_layout = resolve_kitty_placement_layout(
            Some(1),
            None,
            (10, 10),
            (9, 0),
            (10.0, 10.0),
        );
        let from_columns = PlacementMode::Inline {
            row: 0,
            col: 0,
            cols: from_columns_layout.display_cols,
            rows: from_columns_layout.display_rows,
            x_offset: 9,
            y_offset: 0,
            render_size: from_columns_layout.render_size,
        };
        assert_eq!(from_columns.pixel_rect(10.0, 10.0), (9.0, 0.0, 1.0, 1.0));
        assert_eq!(
            from_columns.effective_cell_rect(10.0, 10.0),
            (0, 0, 1, 2)
        );

        let from_rows_layout = resolve_kitty_placement_layout(
            None,
            Some(1),
            (10, 10),
            (0, 9),
            (10.0, 10.0),
        );
        let from_rows = PlacementMode::Inline {
            row: 0,
            col: 0,
            cols: from_rows_layout.display_cols,
            rows: from_rows_layout.display_rows,
            x_offset: 0,
            y_offset: 9,
            render_size: from_rows_layout.render_size,
        };
        assert_eq!(from_rows.pixel_rect(10.0, 10.0), (0.0, 9.0, 1.0, 1.0));
        assert_eq!(
            from_rows.effective_cell_rect(10.0, 10.0),
            (0, 0, 2, 1)
        );
    }

    #[test]
    fn aspect_ratio_footprint_rounds_up_and_saturates() {
        assert_eq!(aspect_ratio_cell_count(1, 10.0, 0, 100, 99, 10.0), 1);
        assert_eq!(aspect_ratio_cell_count(1, 10.0, 0, 100, 100, 10.0), 1);
        assert_eq!(aspect_ratio_cell_count(1, 10.0, 0, 100, 101, 10.0), 2);
        assert_eq!(aspect_ratio_cell_count(1, 10.0, 1, 100, 100, 10.0), 2);
        assert_eq!(aspect_ratio_cell_count(1, 10.0, 0, 0, 100, 10.0), 1);
        assert_eq!(
            aspect_ratio_cell_count(
                u32::MAX,
                10.0,
                9,
                1,
                u32::MAX,
                1.0,
            ),
            u32::MAX
        );
    }

    #[test]
    fn client_image_ids_are_namespaced_from_shared_store_ids() {
        let mut first = ClientImageRegistry::default();
        let mut second = ClientImageRegistry::default();

        first.record(7, 41);
        second.record(7, 42);

        assert_eq!(first.resolve_live(7, |_| true), Some(41));
        assert_eq!(second.resolve_live(7, |_| true), Some(42));
    }

    #[test]
    fn client_image_registry_retains_bindings_until_explicit_pruning() {
        let mut registry = ClientImageRegistry::default();
        registry.record(1, 41);
        registry.record(2, 42);

        assert_eq!(registry.next_id(), 3);
        assert_eq!(registry.resolve_live(1, |image_id| image_id == 42), None);
        assert_eq!(registry.get(1), Some(41));

        registry.retain_existing(|image_id| image_id == 42);
        assert_eq!(registry.len(), 1);

        let mut fresh = ClientImageRegistry::default();
        fresh.record(1, 0);
        fresh.record(0, 99);
        assert_eq!(fresh.next_id(), 1);
    }

    #[test]
    fn image_numbers_resolve_the_newest_live_generation_and_fall_back() {
        let mut registry = ImageNumberRegistry::default();
        registry.record_new(7, 11);
        registry.record_new(7, 12);

        assert_eq!(
            registry.newest_matching(7, |image_id| image_id != 12),
            Some(11)
        );
        assert_eq!(registry.newest_matching(7, |_| true), Some(12));
        assert_eq!(registry.newest_existing(7, |_| true), Some(12));
        assert_eq!(
            registry.newest_existing(7, |image_id| image_id != 12),
            Some(11)
        );

        registry.forget(11);
        assert_eq!(registry.newest_existing(7, |_| true), None);
        registry.record_new(0, 13);
        registry.record_new(8, 0);
        assert_eq!(registry.newest_existing(0, |_| true), None);
        assert_eq!(registry.newest_existing(8, |_| true), None);
    }

    #[test]
    fn image_number_reassignment_never_leaves_an_old_alias() {
        let mut registry = ImageNumberRegistry::default();
        registry.record_new(7, 11);
        registry.record_new(8, 11);

        assert_eq!(registry.newest_existing(7, |_| true), None);
        assert_eq!(registry.newest_existing(8, |_| true), Some(11));
    }

    #[test]
    fn image_number_registry_prunes_evicted_generations_across_all_numbers() {
        let mut registry = ImageNumberRegistry::default();
        registry.record_new(7, 11);
        registry.record_new(7, 12);
        registry.record_new(8, 13);

        registry.retain_existing(|image_id| image_id == 11);

        assert_eq!(registry.newest_existing(7, |_| true), Some(11));
        assert_eq!(registry.newest_existing(8, |_| true), None);
        registry.record_new(9, 13);
        assert_eq!(registry.newest_existing(9, |_| true), Some(13));
    }

    #[test]
    fn generated_image_ids_skip_live_entries_and_zero_after_wrap() {
        let mut next_id = 1;
        assert_eq!(
            next_available_image_id(&mut next_id, |image_id| [1, 2].contains(&image_id)),
            3
        );
        assert_eq!(next_id, 4);

        next_id = u64::from(u32::MAX) + 1;
        assert_eq!(next_available_image_id(&mut next_id, |_| false), 1 << 32);
        assert_eq!(next_id, (1 << 32) + 1);

        next_id = u64::MAX;
        assert_eq!(
            next_available_image_id(&mut next_id, |image_id| {
                [u64::MAX, 1].contains(&image_id)
            }),
            2
        );
        assert_eq!(next_id, 3);
    }

    #[test]
    fn hard_delete_candidates_require_zero_references_across_kitty_sixel_and_panes() {
        let placements = [
            ImagePlacement {
                image_id: 7,
                placement_id: 1,
                client_placement_id: None,
                mode: PlacementMode::Inline {
                    row: 0,
                    col: 0,
                    cols: 1,
                    rows: 1,
                    x_offset: 0,
                    y_offset: 0,
                    render_size: InlineRenderSize::CellAnchored,
                },
                z_index: 0,
            },
            ImagePlacement {
                image_id: 8,
                placement_id: 0,
                client_placement_id: None,
                mode: PlacementMode::Inline {
                    row: 1,
                    col: 0,
                    cols: 1,
                    rows: 1,
                    x_offset: 0,
                    y_offset: 0,
                    render_size: InlineRenderSize::CellAnchored,
                },
                z_index: 0,
            },
        ];
        let other_pane_placements = [ImagePlacement {
            image_id: 9,
            placement_id: 2,
            client_placement_id: None,
            mode: PlacementMode::Inline {
                row: 0,
                col: 1,
                cols: 1,
                rows: 1,
                x_offset: 0,
                y_offset: 0,
                render_size: InlineRenderSize::CellAnchored,
            },
            z_index: 0,
        }];
        let mut candidates = HashSet::from([7, 8, 9, 10]);

        retain_unreferenced_image_ids(
            &mut candidates,
            placements.iter().chain(other_pane_placements.iter()),
        );

        assert_eq!(candidates, HashSet::from([10]));
    }
}
