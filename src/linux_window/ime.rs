//! IME support: commit/preedit payloads with their size limits, the cursor
//! area reported to the input method, and the preedit layout over the grid.

use super::{cell_origin, GridAccessError, GridSnapshot};
use crate::grid::cell::CellFlags;
use crate::grid::Grid;
use crate::input::linux::{ime_commit_payload, ImeCommitPayload};
use std::sync::Mutex;
use unicode_width::UnicodeWidthChar;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::Ime;

pub(super) const MAX_IME_PREEDIT_BYTES: usize = 4 * 1024;

pub(super) const MAX_IME_PREEDIT_RENDER_CELLS: usize = 1024;

pub(super) const IME_PREEDIT_REPLACEMENT: char = '\u{fffd}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImeCursorArea {
    pub(super) position: PhysicalPosition<i32>,
    pub(super) size: PhysicalSize<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImePreeditCursor {
    pub(super) selection_start: usize,
    pub(super) selection_end: usize,
    pub(super) caret: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImePreedit {
    pub(super) text: String,
    pub(super) cursor: Option<ImePreeditCursor>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ImePreeditPayload {
    Clear,
    Text(ImePreedit),
    TooManyBytes { byte_count: usize, limit: usize },
    TooManyRenderCells { cell_count: usize, limit: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImePreeditGlyph {
    pub(super) character: char,
    pub(super) byte_start: usize,
    pub(super) byte_end: usize,
    pub(super) row: usize,
    pub(super) column: usize,
    pub(super) width: u8,
    pub(super) selected: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ImePreeditLayout {
    pub(super) glyphs: Vec<ImePreeditGlyph>,
    pub(super) cleared_cells: Vec<(usize, usize)>,
    pub(super) caret_cell: Option<(usize, usize)>,
}

pub(super) fn ime_payload_from_event(event: Ime) -> Option<ImeCommitPayload> {
    match event {
        Ime::Commit(text) => Some(ime_commit_payload(text)),
        // Preedit is provisional UI state. Sending it to the PTY would duplicate
        // every revision before Winit emits the final commit.
        Ime::Enabled | Ime::Preedit(_, _) | Ime::Disabled => None,
    }
}

pub(super) fn ime_preedit_payload(
    text: String,
    cursor: Option<(usize, usize)>,
) -> ImePreeditPayload {
    ime_preedit_payload_with_limits(
        text,
        cursor,
        MAX_IME_PREEDIT_BYTES,
        MAX_IME_PREEDIT_RENDER_CELLS,
    )
}

fn ime_preedit_payload_with_limits(
    text: String,
    cursor: Option<(usize, usize)>,
    byte_limit: usize,
    render_cell_limit: usize,
) -> ImePreeditPayload {
    if text.is_empty() {
        return ImePreeditPayload::Clear;
    }
    let byte_count = text.len();
    if byte_count > byte_limit {
        return ImePreeditPayload::TooManyBytes {
            byte_count,
            limit: byte_limit,
        };
    }

    let mut cell_count = 0_usize;
    for character in text.chars() {
        let render_cells = character.width().unwrap_or(1).max(1);
        cell_count = cell_count.saturating_add(render_cells);
        if cell_count > render_cell_limit {
            return ImePreeditPayload::TooManyRenderCells {
                cell_count,
                limit: render_cell_limit,
            };
        }
    }

    let cursor = cursor.and_then(|(anchor, caret)| {
        (anchor <= text.len()
            && caret <= text.len()
            && text.is_char_boundary(anchor)
            && text.is_char_boundary(caret))
        .then_some(ImePreeditCursor {
            selection_start: anchor.min(caret),
            selection_end: anchor.max(caret),
            caret,
        })
    });
    ImePreeditPayload::Text(ImePreedit { text, cursor })
}

pub(super) fn replace_ime_preedit(
    current: &mut Option<ImePreedit>,
    next: Option<ImePreedit>,
) -> bool {
    if *current == next {
        return false;
    }
    *current = next;
    true
}

pub(super) fn reveal_ime_input_viewport(grid: &Mutex<Grid>) -> Result<bool, GridAccessError> {
    let mut grid = grid.lock().map_err(|_| GridAccessError::Poisoned)?;
    let changed = grid.scroll_offset != 0;
    grid.scroll_to_bottom();
    Ok(changed)
}

pub(super) fn ime_cursor_area(
    input_cursor: (usize, usize),
    grid_dimensions: (usize, usize),
    cell_dimensions: (u16, u16),
    surface_dimensions: (u32, u32),
) -> Option<ImeCursorArea> {
    let (columns, rows) = grid_dimensions;
    let last_row = rows.checked_sub(1)?;
    let last_column = columns.checked_sub(1)?;
    if input_cursor.0 > last_row || input_cursor.1 > columns {
        return None;
    }
    let row = input_cursor.0;
    let column = input_cursor.1.min(last_column);
    let cell_size = (u32::from(cell_dimensions.0), u32::from(cell_dimensions.1));
    let (surface_width, surface_height) = surface_dimensions;
    if cell_size.0 == 0 || cell_size.1 == 0 || surface_width == 0 || surface_height == 0 {
        return None;
    }

    let origin = cell_origin(row, column, cell_dimensions)?;
    let desired_x = u32::try_from(origin.0).ok()?;
    let desired_y = u32::try_from(origin.1).ok()?;
    let area_width = cell_size.0.min(surface_width);
    let area_height = cell_size.1.min(surface_height);
    let x = desired_x.min(surface_width.checked_sub(area_width)?);
    let y = desired_y.min(surface_height.checked_sub(area_height)?);

    Some(ImeCursorArea {
        position: PhysicalPosition::new(i32::try_from(x).ok()?, i32::try_from(y).ok()?),
        size: PhysicalSize::new(area_width, area_height),
    })
}

pub(super) fn sync_ime_cursor_area_with<F>(
    ime_active: bool,
    last_area: &mut Option<ImeCursorArea>,
    next_area: Option<ImeCursorArea>,
    set_area: F,
) where
    F: FnOnce(ImeCursorArea),
{
    if !ime_active {
        return;
    }
    let Some(next_area) = next_area else {
        return;
    };
    if *last_area == Some(next_area) {
        return;
    }

    set_area(next_area);
    *last_area = Some(next_area);
}

#[cfg(test)]
fn layout_ime_preedit(
    preedit: &ImePreedit,
    input_cursor: (usize, usize),
    grid_dimensions: (usize, usize),
) -> ImePreeditLayout {
    layout_ime_preedit_with_mode(preedit, input_cursor, grid_dimensions, true, None)
}

pub(super) fn layout_ime_preedit_for_snapshot(
    preedit: &ImePreedit,
    snapshot: &GridSnapshot,
) -> ImePreeditLayout {
    layout_ime_preedit_with_mode(
        preedit,
        snapshot.input_cursor,
        (snapshot.columns, snapshot.rows),
        snapshot.auto_wrap,
        Some(snapshot),
    )
}

fn layout_ime_preedit_with_mode(
    preedit: &ImePreedit,
    input_cursor: (usize, usize),
    grid_dimensions: (usize, usize),
    auto_wrap: bool,
    snapshot: Option<&GridSnapshot>,
) -> ImePreeditLayout {
    let (columns, rows) = grid_dimensions;
    if columns == 0 || rows == 0 || input_cursor.0 >= rows || input_cursor.1 > columns {
        return ImePreeditLayout::default();
    }

    let mut layout = ImePreeditLayout::default();
    let mut row = input_cursor.0;
    let mut column = input_cursor.1;
    let mut wrap_pending = snapshot
        .map(|snapshot| snapshot.wrap_pending)
        .unwrap_or(column == columns);
    let mut previous_cell = None;
    let mut last_visible_cell = clamp_ime_layout_position(input_cursor, grid_dimensions);

    for (byte_start, character) in preedit.text.char_indices() {
        let byte_end = byte_start + character.len_utf8();
        let (mut display_character, mut width) = match character.width() {
            Some(width) => (character, u8::try_from(width.min(2)).unwrap_or(2)),
            None => (IME_PREEDIT_REPLACEMENT, 1),
        };
        if usize::from(width) > columns {
            display_character = IME_PREEDIT_REPLACEMENT;
            width = 1;
        }
        let selected = preedit.cursor.is_some_and(|cursor| {
            cursor.selection_start < byte_end && byte_start < cursor.selection_end
        });
        let width_usize = usize::from(width);
        let mut renderable = true;

        let attaches_to_previous = width == 0 && previous_cell.is_some();
        if !attaches_to_previous && (wrap_pending || column >= columns) {
            wrap_pending = false;
            if auto_wrap {
                row = row.saturating_add(1);
                column = 0;
            } else {
                column = column.min(columns - 1);
            }
        }
        if width != 0 {
            if width_usize > columns.saturating_sub(column) {
                if auto_wrap {
                    row = row.saturating_add(1);
                    column = 0;
                } else {
                    renderable = false;
                }
            }
        }
        if preedit
            .cursor
            .is_some_and(|cursor| cursor.caret == byte_start)
        {
            layout.caret_cell =
                ime_layout_caret_position((row, column), grid_dimensions, last_visible_cell);
        }

        if width == 0 {
            let position =
                previous_cell.or_else(|| clamp_ime_layout_position((row, column), grid_dimensions));
            if let Some((glyph_row, glyph_column)) =
                position.filter(|(row, column)| *row < rows && *column < columns)
            {
                let has_cluster_anchor = previous_cell == Some((glyph_row, glyph_column))
                    && layout
                        .glyphs
                        .iter()
                        .any(|glyph| glyph.row == glyph_row && glyph.column == glyph_column);
                if !auto_wrap && !has_cluster_anchor {
                    prepare_ime_no_wrap_write(&mut layout, snapshot, glyph_row, glyph_column, 1);
                }
                layout.glyphs.push(ImePreeditGlyph {
                    character: display_character,
                    byte_start,
                    byte_end,
                    row: glyph_row,
                    column: glyph_column,
                    width,
                    selected,
                });
                last_visible_cell = Some(
                    last_visible_cell
                        .map(|cell| cell.max((glyph_row, glyph_column)))
                        .unwrap_or((glyph_row, glyph_column)),
                );
            }
            continue;
        }

        if !renderable {
            if !auto_wrap {
                previous_cell = None;
            }
            continue;
        }

        if row < rows {
            if !auto_wrap {
                prepare_ime_no_wrap_write(&mut layout, snapshot, row, column, width_usize);
            }
            layout.glyphs.push(ImePreeditGlyph {
                character: display_character,
                byte_start,
                byte_end,
                row,
                column,
                width,
                selected,
            });
            let last_column = column
                .saturating_add(width_usize.saturating_sub(1))
                .min(columns - 1);
            last_visible_cell = Some(
                last_visible_cell
                    .map(|cell| cell.max((row, last_column)))
                    .unwrap_or((row, last_column)),
            );
        }
        previous_cell = Some((row, column));
        column = column.saturating_add(width_usize);
        wrap_pending = column >= columns;
    }
    normalize_ime_preedit_cluster_selection(&mut layout.glyphs);

    if preedit
        .cursor
        .is_some_and(|cursor| cursor.caret == preedit.text.len())
    {
        layout.caret_cell =
            ime_layout_caret_position((row, column), grid_dimensions, last_visible_cell);
    }
    layout
}

fn prepare_ime_no_wrap_write(
    layout: &mut ImePreeditLayout,
    snapshot: Option<&GridSnapshot>,
    row: usize,
    column: usize,
    width: usize,
) {
    let end = column.saturating_add(width);
    layout.cleared_cells.retain(|&(cell_row, cell_column)| {
        cell_row != row || cell_column < column || cell_column >= end
    });

    let mut removed_anchors = Vec::new();
    let mut kept = Vec::with_capacity(layout.glyphs.len());
    for glyph in std::mem::take(&mut layout.glyphs) {
        let glyph_width = usize::from(glyph.width.max(1));
        let glyph_end = glyph.column.saturating_add(glyph_width);
        let overlaps = glyph.row == row && glyph.column < end && column < glyph_end;
        if !overlaps {
            kept.push(glyph);
            continue;
        }

        if glyph.width > 0 {
            removed_anchors.push((glyph.row, glyph.column));
            for cleared_column in glyph.column..glyph_end {
                if cleared_column < column || cleared_column >= end {
                    layout.cleared_cells.push((row, cleared_column));
                }
            }
        }
    }
    kept.retain(|glyph| glyph.width != 0 || !removed_anchors.contains(&(glyph.row, glyph.column)));
    layout.glyphs = kept;

    if let Some(snapshot) = snapshot {
        for target_column in column..end {
            let Some(cell) = snapshot.cell(row, target_column) else {
                continue;
            };
            let partner = if cell.flags.contains(CellFlags::WIDE_CONT) {
                target_column.checked_sub(1)
            } else if cell.flags.contains(CellFlags::WIDE) {
                target_column.checked_add(1)
            } else {
                None
            };
            if let Some(partner) = partner.filter(|partner| *partner < column || *partner >= end) {
                layout.cleared_cells.push((row, partner));
            }
        }
    }

    layout.cleared_cells.sort_unstable();
    layout.cleared_cells.dedup();
}

fn ime_layout_caret_position(
    position: (usize, usize),
    grid_dimensions: (usize, usize),
    last_visible_cell: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    let (columns, rows) = grid_dimensions;
    if position.0 >= rows {
        return last_visible_cell;
    }
    Some((position.0, position.1.min(columns.checked_sub(1)?)))
}

fn normalize_ime_preedit_cluster_selection(glyphs: &mut [ImePreeditGlyph]) {
    let mut selected_anchors = glyphs
        .iter()
        .filter(|glyph| glyph.selected)
        .map(|glyph| (glyph.row, glyph.column))
        .collect::<Vec<_>>();
    selected_anchors.sort_unstable();
    selected_anchors.dedup();

    for glyph in glyphs {
        if selected_anchors
            .binary_search(&(glyph.row, glyph.column))
            .is_ok()
        {
            glyph.selected = true;
        }
    }
}

fn clamp_ime_layout_position(
    position: (usize, usize),
    grid_dimensions: (usize, usize),
) -> Option<(usize, usize)> {
    let (columns, rows) = grid_dimensions;
    Some((
        position.0.min(rows.checked_sub(1)?),
        position.1.min(columns.checked_sub(1)?),
    ))
}

#[cfg(test)]
#[path = "ime_tests.rs"]
mod tests;
