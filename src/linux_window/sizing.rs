//! Window, surface and terminal sizing: cell metrics from the glyph atlas,
//! terminal dimensions for a surface, and the PTY/grid resize transaction.

use super::draw::rounded_positive_i32;
use super::GridAccessError;
use crate::glyph_atlas::GlyphAtlas;
use crate::grid::Grid;
use std::num::NonZeroU32;
use thiserror::Error;
use winit::dpi::{LogicalSize, PhysicalSize};

pub(super) const INITIAL_CELL_WIDTH: u32 = 10;

pub(super) const INITIAL_CELL_HEIGHT: u32 = 20;

// Keep the pre-atlas placeholder modest; Winit interprets this size in logical pixels.
pub(super) const MAX_INITIAL_LOGICAL_WIDTH: u32 = 1920;

pub(super) const MAX_INITIAL_LOGICAL_HEIGHT: u32 = 1080;

// A full RGBA software frame at this bound is about 135 MiB.
pub(super) const MAX_REQUESTED_PHYSICAL_WIDTH: u32 = 8192;

pub(super) const MAX_REQUESTED_PHYSICAL_HEIGHT: u32 = 4320;

pub(super) const SCALE_CHANGE_EPSILON: f64 = 0.001;

// Limit the Grid/snapshot budget to 262,144 visible cells while still covering wide 8K layouts.
pub(super) const MAX_TERMINAL_COLUMNS: u16 = 1024;

pub(super) const MAX_TERMINAL_ROWS: u16 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TerminalDimensions {
    pub(super) columns: u16,
    pub(super) rows: u16,
}

#[derive(Debug, Error)]
pub(super) enum GlyphAtlasRebuildError {
    #[error("{0}")]
    Atlas(String),
    #[error(transparent)]
    Grid(#[from] GridAccessError),
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(super) enum SurfaceSizeError {
    #[error(
        "Linux surface {width}x{height} exceeds the renderer budget of \
         {MAX_REQUESTED_PHYSICAL_WIDTH}x{MAX_REQUESTED_PHYSICAL_HEIGHT} pixels"
    )]
    ExceedsRenderBudget { width: u32, height: u32 },
}

impl std::fmt::Display for TerminalDimensions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}x{}", self.columns, self.rows)
    }
}

pub(super) fn create_glyph_atlas(
    font_family: &str,
    font_size: f32,
    scale_factor: f64,
) -> Result<GlyphAtlas, String> {
    GlyphAtlas::new(font_family, font_size, scale_factor as f32).map_err(|error| {
        format!("could not initialize the Linux glyph atlas at scale {scale_factor}: {error}")
    })
}

pub(super) fn replace_glyph_atlas_for_scale<F>(
    glyph_atlas: &mut Option<GlyphAtlas>,
    atlas_scale_factor: &mut Option<f64>,
    font_family: &str,
    font_size: f32,
    scale_factor: f64,
    before_replace: F,
) -> Result<bool, GlyphAtlasRebuildError>
where
    F: FnOnce(&GlyphAtlas, (u16, u16)) -> Result<(), GridAccessError>,
{
    if atlas_scale_factor
        .is_some_and(|current| (current - scale_factor).abs() <= SCALE_CHANGE_EPSILON)
    {
        return Ok(false);
    }

    let replacement = create_glyph_atlas(font_family, font_size, scale_factor)
        .map_err(GlyphAtlasRebuildError::Atlas)?;
    let cell_dimensions =
        atlas_cell_dimensions(&replacement).map_err(GlyphAtlasRebuildError::Atlas)?;
    before_replace(&replacement, cell_dimensions)?;
    *glyph_atlas = Some(replacement);
    *atlas_scale_factor = Some(scale_factor);
    Ok(true)
}

pub(super) fn initial_window_dimensions(columns: u16, rows: u16) -> LogicalSize<u32> {
    LogicalSize::new(
        (u32::from(columns).max(1) * INITIAL_CELL_WIDTH).min(MAX_INITIAL_LOGICAL_WIDTH),
        (u32::from(rows).max(1) * INITIAL_CELL_HEIGHT).min(MAX_INITIAL_LOGICAL_HEIGHT),
    )
}

pub(super) fn configured_terminal_dimensions(columns: u16, rows: u16) -> TerminalDimensions {
    TerminalDimensions {
        columns: columns.clamp(1, MAX_TERMINAL_COLUMNS),
        rows: rows.clamp(1, MAX_TERMINAL_ROWS),
    }
}

pub(super) fn immediate_surface_size_to_reconcile(
    immediate: Option<PhysicalSize<u32>>,
    current: PhysicalSize<u32>,
) -> PhysicalSize<u32> {
    // Asynchronous backends may keep the existing size until a later event.
    // Publish that actual pixel area now, including on the first atlas setup.
    immediate.unwrap_or(current)
}

pub(super) fn is_current_surface_size(
    event_size: PhysicalSize<u32>,
    current_size: PhysicalSize<u32>,
) -> bool {
    event_size == current_size
}

pub(super) fn drawable_dimensions(
    size: PhysicalSize<u32>,
) -> Result<Option<(NonZeroU32, NonZeroU32)>, SurfaceSizeError> {
    let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
    else {
        return Ok(None);
    };
    if size.width > MAX_REQUESTED_PHYSICAL_WIDTH || size.height > MAX_REQUESTED_PHYSICAL_HEIGHT {
        return Err(SurfaceSizeError::ExceedsRenderBudget {
            width: size.width,
            height: size.height,
        });
    }
    Ok(Some((width, height)))
}

pub(super) fn atlas_cell_dimensions(atlas: &GlyphAtlas) -> Result<(u16, u16), String> {
    let cell_width = rounded_positive_i32(atlas.cell_width)
        .and_then(|width| u16::try_from(width).ok())
        .ok_or_else(|| {
            format!(
                "Linux glyph atlas produced an invalid cell width: {}",
                atlas.cell_width
            )
        })?;
    let cell_height = rounded_positive_i32(atlas.cell_height)
        .and_then(|height| u16::try_from(height).ok())
        .ok_or_else(|| {
            format!(
                "Linux glyph atlas produced an invalid cell height: {}",
                atlas.cell_height
            )
        })?;
    Ok((cell_width, cell_height))
}

pub(super) fn terminal_dimensions_from_locked_grid(
    grid: &Grid,
) -> Result<TerminalDimensions, GridAccessError> {
    let columns = grid.cols();
    let rows = grid.rows();
    Ok(TerminalDimensions {
        columns: u16::try_from(columns)
            .map_err(|_| GridAccessError::DimensionsOutOfRange { columns, rows })?,
        rows: u16::try_from(rows)
            .map_err(|_| GridAccessError::DimensionsOutOfRange { columns, rows })?,
    })
}

pub(super) fn physical_size_for_terminal(
    terminal_dimensions: TerminalDimensions,
    cell_dimensions: (u16, u16),
) -> Option<PhysicalSize<u32>> {
    if terminal_dimensions.columns == 0
        || terminal_dimensions.rows == 0
        || cell_dimensions.0 == 0
        || cell_dimensions.1 == 0
    {
        return None;
    }

    let width = u32::from(terminal_dimensions.columns)
        .checked_mul(u32::from(cell_dimensions.0))?
        .min(MAX_REQUESTED_PHYSICAL_WIDTH);
    let height = u32::from(terminal_dimensions.rows)
        .checked_mul(u32::from(cell_dimensions.1))?
        .min(MAX_REQUESTED_PHYSICAL_HEIGHT);
    Some(PhysicalSize::new(width, height))
}

#[cfg(test)]
mod tests {
    use super::{
        atlas_cell_dimensions, configured_terminal_dimensions, drawable_dimensions,
        immediate_surface_size_to_reconcile, initial_window_dimensions, is_current_surface_size,
        physical_size_for_terminal, replace_glyph_atlas_for_scale, SurfaceSizeError,
        TerminalDimensions, MAX_INITIAL_LOGICAL_HEIGHT, MAX_INITIAL_LOGICAL_WIDTH,
        MAX_REQUESTED_PHYSICAL_HEIGHT, MAX_REQUESTED_PHYSICAL_WIDTH, MAX_TERMINAL_COLUMNS,
        MAX_TERMINAL_ROWS,
    };
    use crate::glyph_atlas::GlyphAtlas;
    use crate::linux_window::test_support::test_atlas;
    use crate::linux_window::GridAccessError;
    use winit::dpi::PhysicalSize;

    #[test]
    fn initial_dimensions_follow_the_configured_grid() {
        let size = initial_window_dimensions(80, 24);
        assert_eq!(size.width, 800);
        assert_eq!(size.height, 480);
    }

    #[test]
    fn initial_dimensions_never_start_at_zero() {
        let size = initial_window_dimensions(0, 0);
        assert_eq!(size.width, 10);
        assert_eq!(size.height, 20);
    }

    #[test]
    fn initial_placeholder_dimensions_respect_the_logical_size_budget() {
        let size = initial_window_dimensions(u16::MAX, u16::MAX);
        assert_eq!(size.width, MAX_INITIAL_LOGICAL_WIDTH);
        assert_eq!(size.height, MAX_INITIAL_LOGICAL_HEIGHT);
    }

    #[test]
    fn configured_dimensions_are_clamped_before_allocating_grid_or_pty() {
        assert_eq!(
            configured_terminal_dimensions(0, 0),
            TerminalDimensions {
                columns: 1,
                rows: 1,
            }
        );
        assert_eq!(
            configured_terminal_dimensions(u16::MAX, u16::MAX),
            TerminalDimensions {
                columns: MAX_TERMINAL_COLUMNS,
                rows: MAX_TERMINAL_ROWS,
            }
        );
        assert_eq!(
            configured_terminal_dimensions(80, 24),
            TerminalDimensions {
                columns: 80,
                rows: 24,
            }
        );
    }

    #[test]
    fn immediate_surface_sizes_reconcile_and_stale_events_are_filtered() {
        let requested = PhysicalSize::new(800, 480);
        let constrained = PhysicalSize::new(790, 470);

        assert_eq!(
            immediate_surface_size_to_reconcile(None, constrained),
            constrained
        );
        assert_eq!(
            immediate_surface_size_to_reconcile(Some(requested), constrained),
            requested
        );
        assert_eq!(
            immediate_surface_size_to_reconcile(Some(constrained), requested),
            constrained
        );
        assert!(is_current_surface_size(constrained, constrained));
        assert!(!is_current_surface_size(requested, constrained));
    }

    #[test]
    fn unchanged_capped_surface_reconciles_after_cell_metrics_grow() {
        let configured = TerminalDimensions {
            columns: 512,
            rows: 135,
        };
        let current_surface = physical_size_for_terminal(configured, (16, 32))
            .expect("the original cell metrics should produce a valid surface");
        let grown_cell_dimensions = (32, 64);
        let requested = physical_size_for_terminal(configured, grown_cell_dimensions)
            .expect("the grown cell metrics should produce a capped request");
        assert_eq!(
            current_surface,
            PhysicalSize::new(MAX_REQUESTED_PHYSICAL_WIDTH, MAX_REQUESTED_PHYSICAL_HEIGHT)
        );
        assert_eq!(requested, current_surface);
        // The pane layout fits whole cells into the capped surface, so the
        // grid shrinks even though the window size did not change.
        assert_eq!(
            (
                requested.width / u32::from(grown_cell_dimensions.0),
                requested.height / u32::from(grown_cell_dimensions.1)
            ),
            (256, 67)
        );
    }

    #[test]
    fn physical_grid_size_is_checked_and_dpi_metrics_preserve_rows_and_columns() {
        let maximum = TerminalDimensions {
            columns: u16::MAX,
            rows: u16::MAX,
        };
        let maximum_size = physical_size_for_terminal(maximum, (u16::MAX, u16::MAX))
            .expect("u16 terminal and cell products should fit u32");
        assert_eq!(
            maximum_size,
            PhysicalSize::new(MAX_REQUESTED_PHYSICAL_WIDTH, MAX_REQUESTED_PHYSICAL_HEIGHT,)
        );
        assert_eq!(
            physical_size_for_terminal(
                TerminalDimensions {
                    columns: 0,
                    rows: 24,
                },
                (10, 20),
            ),
            None
        );

        let grid = TerminalDimensions {
            columns: 80,
            rows: 24,
        };
        for cell_dimensions in [(8, 16), (13, 29), (24, 48)] {
            let physical_size = physical_size_for_terminal(grid, cell_dimensions)
                .expect("normal DPI dimensions should fit");
            assert_eq!(
                physical_size,
                PhysicalSize::new(
                    80 * u32::from(cell_dimensions.0),
                    24 * u32::from(cell_dimensions.1)
                )
            );
        }

        let capped = physical_size_for_terminal(
            TerminalDimensions {
                columns: MAX_TERMINAL_COLUMNS,
                rows: MAX_TERMINAL_ROWS,
            },
            (20, 30),
        )
        .expect("large terminal requests should be capped, not overflow");
        assert_eq!(
            capped,
            PhysicalSize::new(MAX_REQUESTED_PHYSICAL_WIDTH, MAX_REQUESTED_PHYSICAL_HEIGHT,)
        );
        assert!(
            u32::from(MAX_TERMINAL_COLUMNS) * 20 > capped.width,
            "a capped window request must be reconciled by the following resize event"
        );
    }

    #[test]
    fn zero_sized_windows_do_not_create_a_surface_extent() {
        assert_eq!(drawable_dimensions(PhysicalSize::new(0, 480)), Ok(None));
        assert_eq!(drawable_dimensions(PhysicalSize::new(800, 0)), Ok(None));
        assert_eq!(drawable_dimensions(PhysicalSize::new(0, 0)), Ok(None));

        let (width, height) = drawable_dimensions(PhysicalSize::new(800, 480))
            .expect("normal dimensions should fit the renderer budget")
            .expect("non-zero dimensions should be drawable");
        assert_eq!(width.get(), 800);
        assert_eq!(height.get(), 480);
    }

    #[test]
    fn surface_extent_guard_rejects_frames_above_the_physical_budget() {
        assert!(drawable_dimensions(PhysicalSize::new(
            MAX_REQUESTED_PHYSICAL_WIDTH,
            MAX_REQUESTED_PHYSICAL_HEIGHT,
        ))
        .expect("the configured maximum must be drawable")
        .is_some());
        assert_eq!(
            drawable_dimensions(PhysicalSize::new(
                MAX_REQUESTED_PHYSICAL_WIDTH + 1,
                MAX_REQUESTED_PHYSICAL_HEIGHT,
            )),
            Err(SurfaceSizeError::ExceedsRenderBudget {
                width: MAX_REQUESTED_PHYSICAL_WIDTH + 1,
                height: MAX_REQUESTED_PHYSICAL_HEIGHT,
            })
        );
        assert_eq!(
            drawable_dimensions(PhysicalSize::new(
                MAX_REQUESTED_PHYSICAL_WIDTH,
                MAX_REQUESTED_PHYSICAL_HEIGHT + 1,
            )),
            Err(SurfaceSizeError::ExceedsRenderBudget {
                width: MAX_REQUESTED_PHYSICAL_WIDTH,
                height: MAX_REQUESTED_PHYSICAL_HEIGHT + 1,
            })
        );
    }

    #[test]
    fn atlas_metrics_are_positive_and_rounded() {
        let atlas = test_atlas();
        let dimensions =
            atlas_cell_dimensions(&atlas).expect("test atlas should have valid dimensions");

        assert_eq!(dimensions.0, atlas.cell_width.round() as u16);
        assert_eq!(dimensions.1, atlas.cell_height.round() as u16);
        assert!(dimensions.0 > 0);
        assert!(dimensions.1 > 0);
    }

    #[test]
    fn failed_scale_or_metric_rebuild_preserves_the_previous_atlas() {
        let font_family = "kokuban-test-font-that-does-not-exist";
        let font_size = 14.0;
        let atlas = GlyphAtlas::new(font_family, font_size, 1.0)
            .expect("system monospace fallback should be available");
        let original_cell_width = atlas.cell_width;
        let original_glyph_count = atlas.glyphs.len();
        let mut glyph_atlas = Some(atlas);
        let mut atlas_scale_factor = Some(1.0);

        assert!(replace_glyph_atlas_for_scale(
            &mut glyph_atlas,
            &mut atlas_scale_factor,
            font_family,
            font_size,
            f64::NAN,
            |_, _| Ok(()),
        )
        .is_err());

        let preserved = glyph_atlas
            .as_ref()
            .expect("failed replacement must keep the old atlas");
        assert_eq!(atlas_scale_factor, Some(1.0));
        assert_eq!(preserved.cell_width, original_cell_width);
        assert_eq!(preserved.glyphs.len(), original_glyph_count);
        assert!(replace_glyph_atlas_for_scale(
            &mut glyph_atlas,
            &mut atlas_scale_factor,
            font_family,
            font_size,
            2.0,
            |_, _| Err(GridAccessError::Poisoned),
        )
        .is_err());
        let preserved = glyph_atlas
            .as_ref()
            .expect("failed metric synchronization must keep the old atlas");
        assert_eq!(atlas_scale_factor, Some(1.0));
        assert_eq!(preserved.cell_width, original_cell_width);
        assert_eq!(preserved.glyphs.len(), original_glyph_count);
        assert!(!replace_glyph_atlas_for_scale(
            &mut glyph_atlas,
            &mut atlas_scale_factor,
            font_family,
            font_size,
            1.0,
            |_, _| Ok(()),
        )
        .expect("duplicate scale should be a no-op"));
    }
}
