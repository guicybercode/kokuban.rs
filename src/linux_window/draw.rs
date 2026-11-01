//! Software rendering of a grid snapshot into an XRGB frame: cell colors,
//! glyphs, underlines, built-in box shapes, the cursor, images and the IME
//! preedit overlay. Everything here writes into a `&mut [u32]` band.

use super::ime::{ImePreeditGlyph, ImePreeditLayout};
#[cfg(test)]
use super::GridAccessError;
use crate::glyph_atlas::{GlyphAtlas, GlyphKey};
use crate::grid::cell::{Cell, CellFlags, Color, UnderlineStyle};
use crate::grid::{CursorShape, Grid};
use crate::renderer::box_drawing::{self, CellRect};
use crate::software_graphics::ImageSnapshot;
use crate::software_raster::{draw_glyph_a8, draw_glyph_rgba, draw_image_rgba, fill_rect};
use crate::terminal_colors::TerminalColors;
#[cfg(test)]
use std::sync::Mutex;

pub(super) const CURSOR_THICKNESS: u32 = 2;

pub(super) const CURSOR_ALPHA: u8 = 180;

#[derive(Clone, Copy)]
pub(super) struct GlyphSource<'a> {
    pub(super) pixels: &'a [u8],
    pub(super) rgba_pixels: Option<&'a [u8]>,
    pub(super) size: (u32, u32),
    pub(super) ascent: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CursorSnapshot {
    pub(super) row: usize,
    pub(super) column: usize,
    pub(super) shape: CursorShape,
}

#[derive(Debug)]
pub(super) struct GridSnapshot {
    pub(super) columns: usize,
    pub(super) rows: usize,
    pub(super) cells: Vec<Cell>,
    pub(super) cursor: Option<CursorSnapshot>,
    pub(super) input_cursor: (usize, usize),
    pub(super) wrap_pending: bool,
    pub(super) auto_wrap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResolvedCellColors {
    pub(super) foreground: u32,
    pub(super) background: u32,
}

impl GridSnapshot {
    pub(super) fn cell(&self, row: usize, column: usize) -> Option<&Cell> {
        let index = row.checked_mul(self.columns)?.checked_add(column)?;
        self.cells.get(index)
    }

    pub(super) fn rendered_cell(&self, row: usize, column: usize) -> Option<&Cell> {
        let cell = self.cell(row, column)?;
        if cell.flags.contains(CellFlags::WIDE_CONT) && column > 0 {
            let leader = self.cell(row, column - 1)?;
            if leader.flags.contains(CellFlags::WIDE) {
                return Some(leader);
            }
        }
        Some(cell)
    }
}

pub(super) fn rgb_to_xrgb(red: u8, green: u8, blue: u8) -> u32 {
    (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)
}

#[cfg(test)]
pub(super) fn snapshot_grid(grid: &Mutex<Grid>) -> Result<GridSnapshot, GridAccessError> {
    let grid = grid.lock().map_err(|_| GridAccessError::Poisoned)?;
    Ok(snapshot_locked_grid(&grid))
}

pub(super) fn snapshot_locked_grid(grid: &Grid) -> GridSnapshot {
    let columns = grid.cols();
    let rows = grid.rows();
    let mut cells = Vec::with_capacity(columns.saturating_mul(rows));

    for row in 0..rows {
        for column in 0..columns {
            cells.push(grid.visible_cell(row, column).clone());
        }
    }

    let cursor = if grid.cursor_visible && grid.scroll_offset == 0 && grid.cursor_row < rows {
        grid.screen_cursor_col().map(|column| CursorSnapshot {
            row: grid.cursor_row,
            column,
            shape: grid.cursor_style.shape,
        })
    } else {
        None
    };

    GridSnapshot {
        columns,
        rows,
        cells,
        cursor,
        input_cursor: (grid.cursor_row, grid.cursor_col),
        wrap_pending: grid.is_wrap_pending(),
        auto_wrap: grid.auto_wrap,
    }
}

pub(super) fn cell_content_is_visible(flags: CellFlags) -> bool {
    !flags.contains(CellFlags::HIDDEN)
}

pub(super) fn resolve_cell_colors(colors: &TerminalColors, cell: &Cell) -> ResolvedCellColors {
    let resolved = colors.resolve_cell_colors(cell.fg, cell.bg, cell.flags);

    ResolvedCellColors {
        foreground: rgb_to_xrgb(
            resolved.foreground.0,
            resolved.foreground.1,
            resolved.foreground.2,
        ),
        background: rgb_to_xrgb(
            resolved.background.0,
            resolved.background.1,
            resolved.background.2,
        ),
    }
}

pub(super) fn resolve_cell_underline_color(
    colors: &TerminalColors,
    cell: &Cell,
    resolved_foreground: u32,
) -> u32 {
    match cell.underline_color {
        Color::Default => resolved_foreground,
        color => {
            let color = colors.resolve_foreground(color, false);
            rgb_to_xrgb(color.0, color.1, color.2)
        }
    }
}

pub(super) fn underline_anchor_y(
    cell_origin_y: i32,
    cell_height: u32,
    ascent: f32,
    style: UnderlineStyle,
) -> Option<i32> {
    if style == UnderlineStyle::None || cell_height == 0 {
        return None;
    }

    let cell_height = i32::try_from(cell_height).ok()?;
    let last_row = cell_origin_y.checked_add(cell_height.checked_sub(1)?)?;
    let desired = rounded_f64_i32(f64::from(cell_origin_y) + f64::from(ascent))?.checked_add(2)?;
    let (leading_inset, trailing_inset) = match style {
        UnderlineStyle::Double => (0, 2),
        UnderlineStyle::Curly => (1, 1),
        UnderlineStyle::None
        | UnderlineStyle::Single
        | UnderlineStyle::Dotted
        | UnderlineStyle::Dashed => (0, 0),
    };
    let preferred_min = cell_origin_y.checked_add(leading_inset)?;
    let preferred_max = last_row.checked_sub(trailing_inset)?;

    if preferred_min <= preferred_max {
        Some(desired.clamp(preferred_min, preferred_max))
    } else {
        Some(desired.clamp(cell_origin_y, last_row))
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_cell_underline(
    frame: &mut [u32],
    frame_size: (u32, u32),
    origin: (i32, i32),
    width: u32,
    cell_height: u32,
    ascent: f32,
    style: UnderlineStyle,
    color: u32,
) {
    draw_cell_underline_in_band(
        frame,
        frame_size,
        origin,
        width,
        cell_height,
        ascent,
        style,
        color,
        0,
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_cell_underline_in_band(
    frame: &mut [u32],
    frame_size: (u32, u32),
    origin: (i32, i32),
    width: u32,
    cell_height: u32,
    ascent: f32,
    style: UnderlineStyle,
    color: u32,
    vertical_offset: i32,
) {
    let Some(anchor_y) = underline_anchor_y(origin.1, cell_height, ascent, style) else {
        return;
    };
    let Some(last_y) = i32::try_from(cell_height)
        .ok()
        .and_then(|height| height.checked_sub(1))
        .and_then(|height| origin.1.checked_add(height))
    else {
        return;
    };

    let draw_segment = |frame: &mut [u32], x_offset: u32, y: i32, segment_width: u32| {
        let Some(x_offset) = i32::try_from(x_offset).ok() else {
            return;
        };
        let Some(x) = origin.0.checked_add(x_offset) else {
            return;
        };
        let Some(y) = y.checked_sub(vertical_offset) else {
            return;
        };
        fill_rect(
            frame,
            frame_size,
            (x, y),
            (segment_width, 1),
            color,
            u8::MAX,
        );
    };

    match style {
        UnderlineStyle::None => {}
        UnderlineStyle::Single => draw_segment(frame, 0, anchor_y, width),
        UnderlineStyle::Double => {
            draw_segment(frame, 0, anchor_y, width);
            if let Some(second_y) = anchor_y.checked_add(2).filter(|y| *y <= last_y) {
                draw_segment(frame, 0, second_y, width);
            }
        }
        UnderlineStyle::Curly => {
            const OFFSETS: [i32; 6] = [0, 1, 1, 0, -1, -1];
            for x_offset in 0..width {
                let offset = OFFSETS[x_offset as usize % OFFSETS.len()];
                if let Some(y) = anchor_y.checked_add(offset) {
                    draw_segment(frame, x_offset, y.clamp(origin.1, last_y), 1);
                }
            }
        }
        UnderlineStyle::Dotted => {
            for x_offset in (0..width).step_by(3) {
                draw_segment(frame, x_offset, anchor_y, 1);
            }
        }
        UnderlineStyle::Dashed => {
            for x_offset in (0..width).step_by(6) {
                draw_segment(frame, x_offset, anchor_y, (width - x_offset).min(4));
            }
        }
    }
}

#[cfg(test)]
pub(super) fn draw_grid_snapshot(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: TerminalColors,
    cell_dimensions: (u16, u16),
    snapshot: &GridSnapshot,
    draw_terminal_cursor: bool,
) {
    draw_grid_snapshot_with_images(
        frame,
        frame_size,
        atlas,
        colors,
        cell_dimensions,
        snapshot,
        draw_terminal_cursor,
        &[],
    );
}

pub(super) fn draw_images(
    frame: &mut [u32],
    frame_size: (u32, u32),
    images: &[ImageSnapshot],
    include: impl Fn(i32) -> bool,
) {
    for image in images.iter().filter(|image| include(image.z_index)) {
        draw_image_rgba(
            frame,
            frame_size,
            &image.pixels,
            image.size,
            image.rectangle,
        );
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_grid_snapshot_with_images(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: TerminalColors,
    cell_dimensions: (u16, u16),
    snapshot: &GridSnapshot,
    draw_terminal_cursor: bool,
    images: &[ImageSnapshot],
) {
    draw_grid_snapshot_in_band(
        frame,
        frame_size,
        atlas,
        &colors,
        cell_dimensions,
        snapshot,
        draw_terminal_cursor,
        images,
        0..frame_size.1,
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_grid_snapshot_in_band(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: &TerminalColors,
    cell_dimensions: (u16, u16),
    snapshot: &GridSnapshot,
    draw_terminal_cursor: bool,
    images: &[ImageSnapshot],
    band: std::ops::Range<u32>,
) {
    if band.is_empty() || band.end > frame_size.1 {
        return;
    }
    let partial = band != (0..frame_size.1);
    debug_assert!(images.is_empty() || band == (0..frame_size.1));
    let start = u64::from(band.start) * u64::from(frame_size.0);
    let end = u64::from(band.end) * u64::from(frame_size.0);
    let (Ok(start), Ok(end), Ok(offset)) = (
        usize::try_from(start),
        usize::try_from(end),
        i32::try_from(band.start),
    ) else {
        return;
    };
    let Some(frame) = frame.get_mut(start..end) else {
        return;
    };
    let frame_size = (frame_size.0, band.end - band.start);
    let band_origin = |row, column| {
        let (x, y) = cell_origin(row, column, cell_dimensions)?;
        Some((x, y.checked_sub(offset)?))
    };
    let cell_size = (u32::from(cell_dimensions.0), u32::from(cell_dimensions.1));
    let background_rows = if partial && cell_size.1 != 0 {
        (band.start / cell_size.1) as usize..band.end.div_ceil(cell_size.1) as usize
    } else {
        0..snapshot.rows
    };
    let background_rows =
        background_rows.start.min(snapshot.rows)..background_rows.end.min(snapshot.rows);

    // Kitty's lowest layer is behind non-default cell backgrounds. Ordinary
    // negative z values are above backgrounds but below glyphs.
    draw_images(frame, frame_size, images, |z| z < i32::MIN / 2);
    for row in background_rows.clone() {
        for column in 0..snapshot.columns {
            let Some(cell) = snapshot.cell(row, column) else {
                continue;
            };
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                continue;
            }
            let Some(origin) = band_origin(row, column) else {
                continue;
            };
            // The caller initializes the whole frame or damaged band first.
            if cell.bg == Color::Default && !cell.flags.contains(CellFlags::REVERSE) {
                continue;
            }
            let width = if cell.flags.contains(CellFlags::WIDE) {
                cell_size.0.saturating_mul(2)
            } else {
                cell_size.0
            };
            fill_rect(
                frame,
                frame_size,
                origin,
                (width, cell_size.1),
                resolve_cell_colors(colors, cell).background,
                u8::MAX,
            );
        }
    }
    draw_images(frame, frame_size, images, |z| {
        (i32::MIN / 2..0).contains(&z)
    });

    for row in 0..snapshot.rows {
        // Ink can overhang any number of rows, so keep looking up its actual
        // bounds. Full repaints do not need these extra intersection checks.
        let baseline = if partial {
            cell_origin(row, 0, cell_dimensions)
                .and_then(|(_, y)| rounded_f64_i32(f64::from(y) + f64::from(atlas.ascent)))
        } else {
            None
        };
        let underline_row = !partial || background_rows.contains(&row);
        for column in 0..snapshot.columns {
            let Some(cell) = snapshot.cell(row, column) else {
                continue;
            };
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                continue;
            }
            let Some(origin) = cell_origin(row, column, cell_dimensions) else {
                continue;
            };
            if !cell_content_is_visible(cell.flags) {
                continue;
            }

            let has_content = cell.grapheme.is_some() || (cell.c != ' ' && cell.c != '\0');
            let builtin = if has_content {
                builtin_cell_rects(cell, cell_size, atlas.scale_factor())
            } else {
                None
            };
            let glyph = if has_content && builtin.is_none() {
                let glyph = atlas.get_or_insert_cell(cell);
                let intersects = !partial
                    || baseline
                        .and_then(|y| y.checked_add(glyph.bearing_y))
                        .is_some_and(|top| {
                            i64::from(top) < i64::from(band.end)
                                && i64::from(top) + i64::from(glyph.pixel_h) > i64::from(band.start)
                        });
                intersects.then_some(glyph)
            } else {
                None
            };
            // Underlines stay inside the cell box even when the glyph's ink
            // misses the band; they must not be skipped along with that glyph.
            let underline = underline_row && cell.underline_style != UnderlineStyle::None;
            // Built-in shapes stay inside the cell box, like underlines.
            let builtin = builtin.filter(|_| underline_row);
            if glyph.is_none() && builtin.is_none() && !underline {
                continue;
            }
            let resolved = resolve_cell_colors(colors, cell);
            if let Some(rects) = builtin {
                if let Some(origin) = band_origin(row, column) {
                    draw_builtin_rects(
                        frame,
                        frame_size,
                        &rects,
                        origin,
                        resolved.foreground,
                        None,
                    );
                }
            }
            if let Some(glyph) = glyph {
                let source = GlyphSource {
                    pixels: &atlas.pixels,
                    rgba_pixels: atlas.is_color(glyph).then_some(&atlas.rgba_pixels),
                    size: (atlas.width, atlas.height),
                    ascent: atlas.ascent,
                };
                draw_cell_glyph_in_band(
                    frame,
                    frame_size,
                    source,
                    glyph,
                    origin,
                    resolved.foreground,
                    offset,
                );
            }

            if underline {
                let background_width = if cell.flags.contains(CellFlags::WIDE) {
                    cell_size.0.saturating_mul(2)
                } else {
                    cell_size.0
                };
                let underline_color =
                    resolve_cell_underline_color(colors, cell, resolved.foreground);
                draw_cell_underline_in_band(
                    frame,
                    frame_size,
                    origin,
                    background_width,
                    cell_size.1,
                    atlas.ascent,
                    cell.underline_style,
                    underline_color,
                    offset,
                );
            }
        }
    }

    draw_images(frame, frame_size, images, |z| z >= 0);
    if !draw_terminal_cursor {
        return;
    }
    let Some(cursor) = snapshot.cursor else {
        return;
    };
    let Some(origin) = band_origin(cursor.row, cursor.column) else {
        return;
    };
    let Some((cursor_origin, cursor_size)) = cursor_rectangle(cursor.shape, origin, cell_size)
    else {
        return;
    };
    let cursor_cell = snapshot.rendered_cell(cursor.row, cursor.column);
    let cursor_background = cursor_cell
        .map(|cell| resolve_cell_colors(colors, cell).background)
        .unwrap_or_else(|| {
            let background = colors.default_background();
            rgb_to_xrgb(background.0, background.1, background.2)
        });
    fill_rect(
        frame,
        frame_size,
        cursor_origin,
        cursor_size,
        colors
            .cursor()
            .map(|color| rgb_to_xrgb(color.0, color.1, color.2))
            .unwrap_or_else(|| contrasting_cursor_color(cursor_background)),
        if colors.cursor().is_some() {
            u8::MAX
        } else {
            CURSOR_ALPHA
        },
    );
    let Some(cursor_color) = colors
        .cursor()
        .filter(|_| cursor.shape == CursorShape::Block)
    else {
        return;
    };
    let Some(cell) = cursor_cell.filter(|cell| {
        cell_content_is_visible(cell.flags)
            && !cell.flags.contains(CellFlags::WIDE_CONT)
            && (cell.grapheme.is_some() || (cell.c != ' ' && cell.c != '\0'))
    }) else {
        return;
    };
    // The cursor can sit on the second cell of a wide grapheme. Its ink still
    // originates at the leader, while only the cursor's own rectangle changes.
    let column = if snapshot
        .cell(cursor.row, cursor.column)
        .is_some_and(|cell| cell.flags.contains(CellFlags::WIDE_CONT))
    {
        cursor.column.saturating_sub(1)
    } else {
        cursor.column
    };
    let Some(cell_origin) = cell_origin(cursor.row, column, cell_dimensions) else {
        return;
    };
    let cursor_color = rgb_to_xrgb(cursor_color.0, cursor_color.1, cursor_color.2);
    let default_background = colors.default_background();
    let mut foreground = cursor_background;
    if foreground == cursor_color {
        foreground = rgb_to_xrgb(
            default_background.0,
            default_background.1,
            default_background.2,
        );
    }
    if foreground == cursor_color {
        foreground = contrasting_cursor_color(cursor_color);
    }
    if let Some(rects) = builtin_cell_rects(cell, cell_size, atlas.scale_factor()) {
        let Some(origin_y) = cell_origin.1.checked_sub(offset) else {
            return;
        };
        draw_builtin_rects(
            frame,
            frame_size,
            &rects,
            (cell_origin.0, origin_y),
            foreground,
            Some((cursor_origin, cursor_size)),
        );
        return;
    }
    let glyph = atlas.get_or_insert_cell(cell);
    let Some(destination_x) = cell_origin.0.checked_add(glyph.bearing_x) else {
        return;
    };
    let Some(baseline) = rounded_f64_i32(f64::from(cell_origin.1) + f64::from(atlas.ascent)) else {
        return;
    };
    let Some(destination_y) = baseline
        .checked_add(glyph.bearing_y)
        .and_then(|y| y.checked_sub(offset))
    else {
        return;
    };
    let source = GlyphSource {
        pixels: &atlas.pixels,
        rgba_pixels: atlas.is_color(glyph).then_some(&atlas.rgba_pixels),
        size: (atlas.width, atlas.height),
        ascent: atlas.ascent,
    };
    draw_glyph_in_cursor(
        frame,
        frame_size,
        source,
        glyph,
        (destination_x, destination_y),
        foreground,
        (cursor_origin, cursor_size),
    );
}

/// Pixel-exact shapes for block and box drawing characters, which bypass the
/// font so neighbouring cells join without blended seams.
pub(super) fn builtin_cell_rects(
    cell: &Cell,
    cell_size: (u32, u32),
    scale: f32,
) -> Option<Vec<CellRect>> {
    if cell.grapheme.is_some() {
        return None;
    }
    let width = if cell.flags.contains(CellFlags::WIDE) {
        cell_size.0.saturating_mul(2)
    } else {
        cell_size.0
    };
    box_drawing::builtin_rects(cell.c, width, cell_size.1, scale)
}

/// Fill built-in shape rectangles at a cell origin, optionally clipped to a
/// cursor rectangle.
pub(super) fn draw_builtin_rects(
    frame: &mut [u32],
    frame_size: (u32, u32),
    rects: &[CellRect],
    origin: (i32, i32),
    color: u32,
    clip: Option<((i32, i32), (u32, u32))>,
) {
    for rect in rects {
        let mut left = i64::from(origin.0) + i64::from(rect.x);
        let mut top = i64::from(origin.1) + i64::from(rect.y);
        let mut right = left + i64::from(rect.width);
        let mut bottom = top + i64::from(rect.height);
        if let Some((clip_origin, clip_size)) = clip {
            left = left.max(i64::from(clip_origin.0));
            top = top.max(i64::from(clip_origin.1));
            right = right.min(i64::from(clip_origin.0) + i64::from(clip_size.0));
            bottom = bottom.min(i64::from(clip_origin.1) + i64::from(clip_size.1));
        }
        if left >= right || top >= bottom {
            continue;
        }
        let (Ok(x), Ok(y), Ok(width), Ok(height)) = (
            i32::try_from(left),
            i32::try_from(top),
            u32::try_from(right - left),
            u32::try_from(bottom - top),
        ) else {
            continue;
        };
        fill_rect(
            frame,
            frame_size,
            (x, y),
            (width, height),
            color,
            rect.alpha,
        );
    }
}

/// Crop the cached glyph itself, retaining the destination stride and source
/// bearings. Repainting the whole glyph could recolor a wide neighbor or draw
/// outside a damaged band. No scratch bitmap or frame allocation is needed.
pub(super) fn draw_glyph_in_cursor(
    frame: &mut [u32],
    frame_size: (u32, u32),
    source: GlyphSource<'_>,
    mut glyph: crate::glyph_atlas::GlyphEntry,
    destination: (i32, i32),
    foreground: u32,
    cursor: ((i32, i32), (u32, u32)),
) {
    let (origin, size) = cursor;
    let left = i64::from(destination.0).max(i64::from(origin.0));
    let top = i64::from(destination.1).max(i64::from(origin.1));
    let right = (i64::from(destination.0) + i64::from(glyph.pixel_w))
        .min(i64::from(origin.0) + i64::from(size.0));
    let bottom = (i64::from(destination.1) + i64::from(glyph.pixel_h))
        .min(i64::from(origin.1) + i64::from(size.1));
    if left >= right || top >= bottom {
        return;
    }
    let (Ok(source_x), Ok(source_y), Ok(width), Ok(height)) = (
        u32::try_from(left - i64::from(destination.0)),
        u32::try_from(top - i64::from(destination.1)),
        u32::try_from(right - left),
        u32::try_from(bottom - top),
    ) else {
        return;
    };
    let (Some(atlas_x), Some(atlas_y)) = (
        glyph.atlas_x.checked_add(source_x),
        glyph.atlas_y.checked_add(source_y),
    ) else {
        return;
    };
    glyph.atlas_x = atlas_x;
    glyph.atlas_y = atlas_y;
    glyph.pixel_w = width;
    glyph.pixel_h = height;
    // left/top are maxima of i32 origins, so their conversions are exact.
    let destination = (left as i32, top as i32);
    if let Some(pixels) = source.rgba_pixels {
        draw_glyph_rgba(frame, frame_size, pixels, source.size, glyph, destination);
    } else {
        draw_glyph_a8(
            frame,
            frame_size,
            source.pixels,
            source.size,
            glyph,
            destination,
            foreground,
        );
    }
}

pub(super) fn draw_ime_preedit(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &mut GlyphAtlas,
    colors: &TerminalColors,
    cell_dimensions: (u16, u16),
    snapshot: &GridSnapshot,
    layout: &ImePreeditLayout,
) {
    let cell_size = (u32::from(cell_dimensions.0), u32::from(cell_dimensions.1));

    for &(row, column) in &layout.cleared_cells {
        let Some(origin) = cell_origin(row, column, cell_dimensions) else {
            continue;
        };
        let background = snapshot
            .rendered_cell(row, column)
            .map(|cell| resolve_cell_colors(colors, cell).background)
            .unwrap_or_else(|| {
                let background = colors.default_background();
                rgb_to_xrgb(background.0, background.1, background.2)
            });
        fill_rect(frame, frame_size, origin, cell_size, background, u8::MAX);
    }

    for glyph in &layout.glyphs {
        let Some(origin) = cell_origin(glyph.row, glyph.column, cell_dimensions) else {
            continue;
        };
        let Some(width) = cell_size.0.checked_mul(u32::from(glyph.width.max(1))) else {
            continue;
        };
        let (_, background) = ime_preedit_glyph_colors(colors, snapshot, glyph);
        fill_rect(
            frame,
            frame_size,
            origin,
            (width, cell_size.1),
            background,
            u8::MAX,
        );
    }

    for glyph in &layout.glyphs {
        if glyph.character == ' ' || glyph.character == '\0' {
            continue;
        }
        let Some(origin) = cell_origin(glyph.row, glyph.column, cell_dimensions) else {
            continue;
        };
        let (foreground, _) = ime_preedit_glyph_colors(colors, snapshot, glyph);
        let atlas_glyph = atlas.get_or_insert(GlyphKey {
            c: glyph.character,
            bold: false,
            italic: false,
        });
        let source = GlyphSource {
            pixels: &atlas.pixels,
            rgba_pixels: atlas.is_color(atlas_glyph).then_some(&atlas.rgba_pixels),
            size: (atlas.width, atlas.height),
            ascent: atlas.ascent,
        };
        draw_cell_glyph(frame, frame_size, source, atlas_glyph, origin, foreground);
    }

    let underline_height = CURSOR_THICKNESS.min(cell_size.1);
    for glyph in &layout.glyphs {
        let Some(origin) = cell_origin(glyph.row, glyph.column, cell_dimensions) else {
            continue;
        };
        let Some(width) = cell_size.0.checked_mul(u32::from(glyph.width.max(1))) else {
            continue;
        };
        let Some(y_offset) = cell_size.1.checked_sub(underline_height) else {
            continue;
        };
        let Some(y) = i32::try_from(y_offset)
            .ok()
            .and_then(|offset| origin.1.checked_add(offset))
        else {
            continue;
        };
        let (foreground, _) = ime_preedit_glyph_colors(colors, snapshot, glyph);
        fill_rect(
            frame,
            frame_size,
            (origin.0, y),
            (width, underline_height),
            foreground,
            u8::MAX,
        );
    }

    let Some((row, column)) = layout.caret_cell else {
        return;
    };
    let Some(origin) = cell_origin(row, column, cell_dimensions) else {
        return;
    };
    let background = ime_preedit_background_at(colors, snapshot, layout, row, column);
    fill_rect(
        frame,
        frame_size,
        origin,
        (CURSOR_THICKNESS.min(cell_size.0), cell_size.1),
        colors
            .cursor()
            .map(|color| rgb_to_xrgb(color.0, color.1, color.2))
            .unwrap_or_else(|| contrasting_cursor_color(background)),
        u8::MAX,
    );
}

pub(super) fn ime_preedit_glyph_colors(
    colors: &TerminalColors,
    snapshot: &GridSnapshot,
    glyph: &ImePreeditGlyph,
) -> (u32, u32) {
    let underlying = snapshot
        .rendered_cell(glyph.row, glyph.column)
        .map(|cell| resolve_cell_colors(colors, cell))
        .unwrap_or_else(|| {
            let foreground = colors.resolve_foreground(Color::Default, false);
            let background = colors.default_background();
            ResolvedCellColors {
                foreground: rgb_to_xrgb(foreground.0, foreground.1, foreground.2),
                background: rgb_to_xrgb(background.0, background.1, background.2),
            }
        });
    if glyph.selected {
        (underlying.background, underlying.foreground)
    } else {
        (underlying.foreground, underlying.background)
    }
}

pub(super) fn ime_preedit_background_at(
    colors: &TerminalColors,
    snapshot: &GridSnapshot,
    layout: &ImePreeditLayout,
    row: usize,
    column: usize,
) -> u32 {
    layout
        .glyphs
        .iter()
        .rev()
        .find(|glyph| {
            glyph.row == row
                && column >= glyph.column
                && column < glyph.column.saturating_add(usize::from(glyph.width.max(1)))
        })
        .map(|glyph| ime_preedit_glyph_colors(colors, snapshot, glyph).1)
        .or_else(|| {
            snapshot
                .rendered_cell(row, column)
                .map(|cell| resolve_cell_colors(colors, cell).background)
        })
        .unwrap_or_else(|| {
            let background = colors.default_background();
            rgb_to_xrgb(background.0, background.1, background.2)
        })
}

pub(super) fn cell_origin(
    row: usize,
    column: usize,
    cell_dimensions: (u16, u16),
) -> Option<(i32, i32)> {
    let x = i64::try_from(column)
        .ok()?
        .checked_mul(i64::from(cell_dimensions.0))?;
    let y = i64::try_from(row)
        .ok()?
        .checked_mul(i64::from(cell_dimensions.1))?;
    Some((i32::try_from(x).ok()?, i32::try_from(y).ok()?))
}

pub(super) fn cursor_rectangle(
    shape: CursorShape,
    cell_origin: (i32, i32),
    cell_size: (u32, u32),
) -> Option<((i32, i32), (u32, u32))> {
    match shape {
        CursorShape::Block => Some((cell_origin, cell_size)),
        CursorShape::Bar => Some((
            cell_origin,
            (CURSOR_THICKNESS.min(cell_size.0), cell_size.1),
        )),
        CursorShape::Underline => {
            let height = CURSOR_THICKNESS.min(cell_size.1);
            let y_offset = i32::try_from(cell_size.1.checked_sub(height)?).ok()?;
            let y = cell_origin.1.checked_add(y_offset)?;
            Some(((cell_origin.0, y), (cell_size.0, height)))
        }
    }
}

pub(super) fn contrasting_cursor_color(background: u32) -> u32 {
    let red = (background >> 16) & 0xff;
    let green = (background >> 8) & 0xff;
    let blue = background & 0xff;
    let luminance = red * 299 + green * 587 + blue * 114;

    if luminance >= 128_000 {
        0x0000_0000
    } else {
        0x00ff_ffff
    }
}

pub(super) fn draw_cell_glyph(
    frame: &mut [u32],
    frame_size: (u32, u32),
    source: GlyphSource<'_>,
    glyph: crate::glyph_atlas::GlyphEntry,
    cell_origin: (i32, i32),
    foreground: u32,
) {
    draw_cell_glyph_in_band(frame, frame_size, source, glyph, cell_origin, foreground, 0);
}

pub(super) fn draw_cell_glyph_in_band(
    frame: &mut [u32],
    frame_size: (u32, u32),
    source: GlyphSource<'_>,
    glyph: crate::glyph_atlas::GlyphEntry,
    cell_origin: (i32, i32),
    foreground: u32,
    vertical_offset: i32,
) {
    let Some(destination_x) = cell_origin.0.checked_add(glyph.bearing_x) else {
        return;
    };
    let Some(baseline) = rounded_f64_i32(f64::from(cell_origin.1) + f64::from(source.ascent))
    else {
        return;
    };
    let Some(destination_y) = baseline
        .checked_add(glyph.bearing_y)
        .and_then(|y| y.checked_sub(vertical_offset))
    else {
        return;
    };

    if let Some(rgba_pixels) = source.rgba_pixels {
        draw_glyph_rgba(
            frame,
            frame_size,
            rgba_pixels,
            source.size,
            glyph,
            (destination_x, destination_y),
        );
        return;
    }
    draw_glyph_a8(
        frame,
        frame_size,
        source.pixels,
        source.size,
        glyph,
        (destination_x, destination_y),
        foreground,
    );
}

pub(super) fn rounded_positive_i32(value: f32) -> Option<i32> {
    let rounded = rounded_i32(value)?;
    (rounded > 0).then_some(rounded)
}

pub(super) fn rounded_i32(value: f32) -> Option<i32> {
    rounded_f64_i32(f64::from(value))
}

pub(super) fn rounded_f64_i32(value: f64) -> Option<i32> {
    let rounded = value.round();
    if !rounded.is_finite() || rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        return None;
    }
    Some(rounded as i32)
}

#[cfg(test)]
#[path = "draw_tests.rs"]
mod tests;
