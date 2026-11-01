//! Window, surface and terminal sizing: cell metrics from the glyph atlas,
//! terminal dimensions for a surface, and the PTY/grid resize transaction.

use super::draw::rounded_positive_i32;
use super::GridAccessError;
use crate::glyph_atlas::GlyphAtlas;
use crate::grid::Grid;
use std::num::NonZeroU32;
use std::sync::Mutex;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TerminalSize {
    pub(super) cells: TerminalDimensions,
    pub(super) pixels: (u16, u16),
}

#[derive(Debug, Error)]
pub(super) enum TerminalResizeError {
    #[error("could not resize Linux terminal to {target}: {source}")]
    Grid {
        target: TerminalDimensions,
        #[source]
        source: GridAccessError,
    },
    #[error("could not resize Linux PTY to {target}: {source}")]
    Pty {
        target: TerminalDimensions,
        #[source]
        source: std::io::Error,
    },
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

pub(super) fn set_grid_cell_dimensions(
    grid: &Mutex<Grid>,
    cell_dimensions: (u16, u16),
) -> Result<(), GridAccessError> {
    let mut grid = grid.lock().map_err(|_| GridAccessError::Poisoned)?;
    grid.cell_pixel_width = cell_dimensions.0;
    grid.cell_pixel_height = cell_dimensions.1;
    Ok(())
}

pub(super) fn terminal_dimensions_from_grid(
    grid: &Mutex<Grid>,
) -> Result<TerminalDimensions, GridAccessError> {
    let grid = grid.lock().map_err(|_| GridAccessError::Poisoned)?;
    terminal_dimensions_from_locked_grid(&grid)
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

pub(super) fn terminal_dimensions_for_surface(
    surface_size: PhysicalSize<u32>,
    cell_dimensions: (u16, u16),
) -> Option<TerminalDimensions> {
    if surface_size.width == 0
        || surface_size.height == 0
        || cell_dimensions.0 == 0
        || cell_dimensions.1 == 0
    {
        return None;
    }

    let columns = (surface_size.width / u32::from(cell_dimensions.0))
        .clamp(1, u32::from(MAX_TERMINAL_COLUMNS));
    let rows =
        (surface_size.height / u32::from(cell_dimensions.1)).clamp(1, u32::from(MAX_TERMINAL_ROWS));
    Some(TerminalDimensions {
        columns: u16::try_from(columns).ok()?,
        rows: u16::try_from(rows).ok()?,
    })
}

pub(super) fn terminal_size_for_surface(
    surface_size: PhysicalSize<u32>,
    cell_dimensions: (u16, u16),
) -> Option<TerminalSize> {
    Some(TerminalSize {
        cells: terminal_dimensions_for_surface(surface_size, cell_dimensions)?,
        // winsize uses unsigned shorts. Keep residual partial-cell pixels: the
        // graphics viewport is the full drawable area, not only complete cells.
        pixels: (
            surface_size.width.min(u32::from(u16::MAX)) as u16,
            surface_size.height.min(u32::from(u16::MAX)) as u16,
        ),
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

pub(super) fn apply_terminal_resize<P, G>(
    current: TerminalDimensions,
    applied: &mut Option<TerminalSize>,
    target: TerminalSize,
    resize_pty: P,
    resize_grid: G,
) -> std::io::Result<bool>
where
    P: FnOnce(TerminalSize) -> std::io::Result<()>,
    G: FnOnce(u16, u16),
{
    let grid_changed = current != target.cells;
    if !grid_changed && *applied == Some(target) {
        return Ok(false);
    }

    if *applied != Some(target) {
        resize_pty(target)?;
    }
    if grid_changed {
        resize_grid(target.cells.columns, target.cells.rows);
    }
    // Failed ioctls leave the remembered size untouched so the same target can
    // be retried. Pixel-only updates do not disturb cursor, scroll or wrap state.
    *applied = Some(target);
    Ok(true)
}

pub(super) fn resize_terminal_with<F>(
    grid: &Mutex<Grid>,
    applied: &mut Option<TerminalSize>,
    target: TerminalSize,
    resize_pty: F,
) -> Result<bool, TerminalResizeError>
where
    F: FnOnce(TerminalSize) -> std::io::Result<()>,
{
    let mut grid = grid.lock().map_err(|_| TerminalResizeError::Grid {
        target: target.cells,
        source: GridAccessError::Poisoned,
    })?;
    let current = terminal_dimensions_from_locked_grid(&grid).map_err(|source| {
        TerminalResizeError::Grid {
            target: target.cells,
            source,
        }
    })?;

    apply_terminal_resize(current, applied, target, resize_pty, |columns, rows| {
        grid.resize(usize::from(columns), usize::from(rows));
    })
    .map_err(|source| TerminalResizeError::Pty {
        target: target.cells,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        apply_terminal_resize, atlas_cell_dimensions, configured_terminal_dimensions,
        drawable_dimensions, immediate_surface_size_to_reconcile, initial_window_dimensions,
        is_current_surface_size, physical_size_for_terminal, replace_glyph_atlas_for_scale,
        resize_terminal_with, set_grid_cell_dimensions, terminal_dimensions_for_surface,
        terminal_size_for_surface, SurfaceSizeError, TerminalDimensions, TerminalResizeError,
        MAX_INITIAL_LOGICAL_HEIGHT, MAX_INITIAL_LOGICAL_WIDTH, MAX_REQUESTED_PHYSICAL_HEIGHT,
        MAX_REQUESTED_PHYSICAL_WIDTH, MAX_TERMINAL_COLUMNS, MAX_TERMINAL_ROWS,
    };
    use crate::glyph_atlas::GlyphAtlas;
    use crate::grid::Grid;
    use crate::linux_window::test_support::test_atlas;
    use crate::linux_window::GridAccessError;
    use std::cell::RefCell;
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, TryLockError};
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
        let target = terminal_size_for_surface(requested, grown_cell_dimensions)
            .expect("the capped physical size should map to a terminal grid");
        let grid = Mutex::new(Grid::new(
            usize::from(configured.columns),
            usize::from(configured.rows),
            0,
        ));
        let pty_size = RefCell::new(None);

        assert!(resize_terminal_with(&grid, &mut None, target, |size| {
            pty_size.replace(Some((size.cells.columns, size.cells.rows)));
            Ok(())
        })
        .expect("the unchanged capped size should reconcile successfully"));
        assert_eq!(pty_size.into_inner(), Some((256, 67)));
        let grid = grid
            .lock()
            .expect("reconciled grid should remain available");
        assert_eq!((grid.cols(), grid.rows()), (256, 67));
    }

    #[test]
    fn surface_dimensions_use_floor_minimum_and_memory_budget_limits() {
        let cells = |width, height| {
            terminal_dimensions_for_surface(PhysicalSize::new(width, height), (10, 20))
        };

        assert_eq!(cells(0, 480), None);
        assert_eq!(cells(800, 0), None);
        assert_eq!(
            terminal_dimensions_for_surface(PhysicalSize::new(800, 480), (0, 20)),
            None
        );
        assert_eq!(
            cells(9, 19),
            Some(TerminalDimensions {
                columns: 1,
                rows: 1,
            })
        );
        assert_eq!(
            cells(29, 59),
            Some(TerminalDimensions {
                columns: 2,
                rows: 2,
            })
        );
        assert_eq!(
            cells(u32::MAX, u32::MAX),
            Some(TerminalDimensions {
                columns: MAX_TERMINAL_COLUMNS,
                rows: MAX_TERMINAL_ROWS,
            })
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
                terminal_dimensions_for_surface(physical_size, cell_dimensions),
                Some(grid)
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
        assert_ne!(
            terminal_dimensions_for_surface(capped, (20, 30)),
            Some(TerminalDimensions {
                columns: MAX_TERMINAL_COLUMNS,
                rows: MAX_TERMINAL_ROWS,
            }),
            "a capped window request must be reconciled by the following resize event"
        );
    }

    #[test]
    fn terminal_pixel_size_uses_the_drawable_area_and_saturates_without_wrapping() {
        let size = terminal_size_for_surface(PhysicalSize::new(809, 499), (10, 20)).unwrap();
        assert_eq!(
            size.cells,
            TerminalDimensions {
                columns: 80,
                rows: 24
            }
        );
        assert_eq!(size.pixels, (809, 499));
        assert_eq!(
            terminal_size_for_surface(PhysicalSize::new(u32::MAX, u32::MAX), (10, 20))
                .unwrap()
                .pixels,
            (u16::MAX, u16::MAX),
        );
        assert_eq!(
            terminal_size_for_surface(PhysicalSize::new(0, 480), (10, 20)),
            None
        );
    }

    #[test]
    fn initial_metrics_and_pixel_only_resizes_update_pty_without_resizing_grid() {
        let cells = TerminalDimensions {
            columns: 80,
            rows: 24,
        };
        let mut applied = None;
        let mut calls = Vec::new();
        // Initial atlas, partial-cell window growth, then a DPI change that
        // preserves the configured grid all need distinct pixel dimensions.
        for (surface, metrics) in [
            (PhysicalSize::new(800, 480), (10, 20)),
            (PhysicalSize::new(809, 499), (10, 20)),
            (PhysicalSize::new(1600, 960), (20, 40)),
        ] {
            let target = terminal_size_for_surface(surface, metrics).unwrap();
            assert_eq!(target.cells, cells);
            assert!(apply_terminal_resize(
                cells,
                &mut applied,
                target,
                |size| {
                    calls.push(size.pixels);
                    Ok(())
                },
                |_, _| panic!("pixel-only changes must preserve the grid state"),
            )
            .unwrap());
            assert_eq!(applied, Some(target));
            assert!(!apply_terminal_resize(
                cells,
                &mut applied,
                target,
                |_| panic!("duplicate window events must not repeat the ioctl"),
                |_, _| panic!("duplicate window events must not resize the grid"),
            )
            .unwrap());
        }
        assert_eq!(calls, [(800, 480), (809, 499), (1600, 960)]);
    }

    #[test]
    fn failed_pixel_or_grid_resize_retains_last_success_and_can_retry_same_target() {
        let initial = terminal_size_for_surface(PhysicalSize::new(800, 480), (10, 20)).unwrap();
        for surface in [PhysicalSize::new(809, 499), PhysicalSize::new(1000, 600)] {
            let grid = Mutex::new(Grid::new(80, 24, 0));
            {
                let mut grid = grid.lock().unwrap();
                grid.cursor_row = 7;
                grid.cursor_col = 80;
                grid.scroll_top = 3;
                grid.scroll_bottom = 20;
                grid.buffer.cell_mut(0, 0).c = 'x';
            }
            let mut applied = Some(initial);
            let target = terminal_size_for_surface(surface, (10, 20)).unwrap();
            let mut calls = 0;
            let result = resize_terminal_with(&grid, &mut applied, target, |_| {
                calls += 1;
                assert!(matches!(grid.try_lock(), Err(TryLockError::WouldBlock)));
                Err(io::Error::from_raw_os_error(5))
            });
            assert!(matches!(result, Err(TerminalResizeError::Pty { .. })));
            assert_eq!(applied, Some(initial));
            assert_eq!(grid.lock().unwrap().cols(), 80);

            assert!(resize_terminal_with(&grid, &mut applied, target, |size| {
                calls += 1;
                assert_eq!(size, target);
                assert!(matches!(grid.try_lock(), Err(TryLockError::WouldBlock)));
                Ok(())
            })
            .unwrap());
            assert_eq!(calls, 2);
            assert_eq!(applied, Some(target));
            let grid = grid.lock().unwrap();
            assert_eq!(
                (grid.cols(), grid.rows()),
                (
                    usize::from(target.cells.columns),
                    usize::from(target.cells.rows)
                )
            );
            if target.cells == initial.cells {
                assert_eq!((grid.cursor_row, grid.cursor_col), (7, 80));
                assert_eq!((grid.scroll_top, grid.scroll_bottom), (3, 20));
                assert_eq!(grid.buffer.cell(0, 0).c, 'x');
            }
        }
    }

    #[test]
    fn resize_transaction_calls_pty_before_grid_commit() {
        let order = RefCell::new(Vec::new());
        let current = TerminalDimensions {
            columns: 80,
            rows: 24,
        };
        let target = terminal_size_for_surface(PhysicalSize::new(1000, 600), (10, 20)).unwrap();
        let mut applied = None;

        assert!(apply_terminal_resize(
            current,
            &mut applied,
            target,
            |size| {
                assert_eq!(size, target);
                order.borrow_mut().push("pty");
                Ok(())
            },
            |columns, rows| {
                assert_eq!((columns, rows), (100, 30));
                order.borrow_mut().push("grid");
            },
        )
        .expect("fake PTY resize should succeed"));
        assert_eq!(order.into_inner(), ["pty", "grid"]);
        assert_eq!(applied, Some(target));
    }

    #[test]
    fn unchanged_terminal_cells_and_pixels_do_not_call_the_pty() {
        let grid = Mutex::new(Grid::new(80, 24, 0));
        let target = terminal_size_for_surface(PhysicalSize::new(800, 480), (10, 20)).unwrap();
        let mut applied = Some(target);
        let pty_calls = AtomicUsize::new(0);
        {
            let mut grid = grid.lock().expect("test grid should be available");
            grid.cursor_row = 7;
            grid.cursor_col = 80;
            grid.scroll_top = 3;
            grid.scroll_bottom = 20;
            grid.buffer.cell_mut(0, 0).c = 'x';
        }

        assert!(!resize_terminal_with(&grid, &mut applied, target, |_| {
            pty_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },)
        .expect("unchanged dimensions should be a no-op"));
        assert_eq!(pty_calls.load(Ordering::Relaxed), 0);
        let grid = grid.lock().expect("no-op should keep grid available");
        assert_eq!((grid.cols(), grid.rows()), (80, 24));
        assert_eq!((grid.cursor_row, grid.cursor_col), (7, 80));
        assert_eq!((grid.scroll_top, grid.scroll_bottom), (3, 20));
        assert_eq!(grid.buffer.cell(0, 0).c, 'x');
    }

    #[test]
    fn failed_pty_resize_preserves_grid_dimensions() {
        let grid = Mutex::new(Grid::new(80, 24, 0));
        {
            let mut grid = grid.lock().expect("test grid should be available");
            grid.cursor_row = 7;
            grid.cursor_col = 80;
            grid.scroll_top = 3;
            grid.scroll_bottom = 20;
            grid.buffer.cell_mut(0, 0).c = 'x';
        }
        let error = resize_terminal_with(
            &grid,
            &mut None,
            terminal_size_for_surface(PhysicalSize::new(1000, 600), (10, 20)).unwrap(),
            |_| Err(io::Error::from_raw_os_error(5)),
        )
        .expect_err("fake PTY failure should abort the transaction");

        assert!(matches!(error, TerminalResizeError::Pty { .. }));
        assert!(error.to_string().contains("100x30"));
        let TerminalResizeError::Pty { source, .. } = &error else {
            panic!("expected a PTY resize error");
        };
        assert_eq!(source.raw_os_error(), Some(5));
        let grid = grid
            .lock()
            .expect("failed PTY resize should not poison grid");
        assert_eq!((grid.cols(), grid.rows()), (80, 24));
        assert_eq!((grid.cursor_row, grid.cursor_col), (7, 80));
        assert_eq!((grid.scroll_top, grid.scroll_bottom), (3, 20));
        assert_eq!(grid.buffer.cell(0, 0).c, 'x');
    }

    #[test]
    fn successful_terminal_resize_updates_pty_and_grid() {
        let grid = Mutex::new(Grid::new(80, 24, 0));
        let pty_size = RefCell::new(None);

        assert!(resize_terminal_with(
            &grid,
            &mut None,
            terminal_size_for_surface(PhysicalSize::new(1000, 600), (10, 20)).unwrap(),
            |size| {
                assert!(matches!(grid.try_lock(), Err(TryLockError::WouldBlock)));
                pty_size.replace(Some((size.cells.columns, size.cells.rows)));
                Ok(())
            },
        )
        .expect("fake PTY resize should commit"));

        assert_eq!(pty_size.into_inner(), Some((100, 30)));
        let grid = grid
            .lock()
            .expect("successful resize should keep grid available");
        assert_eq!((grid.cols(), grid.rows()), (100, 30));
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
    fn atlas_metrics_are_positive_rounded_and_synchronized_with_the_grid() {
        let atlas = test_atlas();
        let dimensions =
            atlas_cell_dimensions(&atlas).expect("test atlas should have valid dimensions");
        let grid = Mutex::new(Grid::new(1, 1, 0));

        set_grid_cell_dimensions(&grid, dimensions)
            .expect("fresh test grid dimensions should synchronize");

        assert_eq!(dimensions.0, atlas.cell_width.round() as u16);
        assert_eq!(dimensions.1, atlas.cell_height.round() as u16);
        assert!(dimensions.0 > 0);
        assert!(dimensions.1 > 0);
        let grid = grid.lock().expect("test grid should remain available");
        assert_eq!(grid.cell_pixel_width, dimensions.0);
        assert_eq!(grid.cell_pixel_height, dimensions.1);
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
