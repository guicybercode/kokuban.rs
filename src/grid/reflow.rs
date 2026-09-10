use super::{buffer::RowMetadata, cell::{Cell, CellFlags}};

#[derive(Debug, Clone)]
pub(crate) struct RetainedRow {
    pub cells: Vec<Cell>,
    pub metadata: RowMetadata,
}

impl RetainedRow {
    pub fn blank(cols: usize) -> Self {
        Self { cells: vec![Cell::default(); cols], metadata: RowMetadata::default() }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Cursor {
    pub row: usize,
    pub col: usize,
    pub pending: bool,
    pub retained_row: Option<usize>,
}

pub(crate) struct Reflow {
    pub rows: Vec<RetainedRow>,
    pub cursors: Vec<Cursor>,
}

/// Repack complete logical lines, preserving printed spaces and complete glyphs.
/// Soft-wide-wrap padding and continuation cells are layout, not content.
pub(crate) fn reflow(source: &[RetainedRow], cols: usize, cursors: &[Cursor]) -> Reflow {
    let mut rows = vec![RetainedRow::blank(cols)];
    let mut mapped = vec![Cursor { row: 0, col: 0, pending: false, retained_row: None }; cursors.len()];
    let mut out_row = 0;
    let mut out_col = 0;
    for (source_row, row) in source.iter().enumerate() {
        let old_cols = row.cells.len();
        let mut len = row.metadata.len.min(old_cols);
        for cursor in cursors.iter().filter(|cursor| cursor.row == source_row) {
            let col = if cursor.pending { (cursor.col + 1).min(old_cols) } else { cursor.col.min(old_cols) };
            len = len.max(col);
        }
        let mut positions = vec![(out_row, out_col); old_cols + 1];
        let mut source_col = 0;
        while source_col < len {
            let cell = &row.cells[source_col];
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                source_col += 1;
                continue;
            }
            let old_width = if cell.flags.contains(CellFlags::WIDE)
                && row.cells.get(source_col + 1).is_some_and(|cell| cell.flags.contains(CellFlags::WIDE_CONT)) { 2 } else { 1 };
            let width = cell.display_width().min(cols);
            if out_col + width > cols {
                rows[out_row].metadata.wrapped = true;
                rows.push(RetainedRow::blank(cols));
                out_row += 1;
                out_col = 0;
            }
            positions[source_col] = (out_row, out_col);
            let mut leader = cell.clone();
            leader.flags.remove(CellFlags::WIDE | CellFlags::WIDE_CONT);
            if cell.display_width() == 2 { leader.flags.insert(CellFlags::WIDE); }
            rows[out_row].cells[out_col] = leader;
            if width == 2 {
                let mut continuation = cell.clone();
                continuation.c = '\0';
                continuation.grapheme = None;
                continuation.flags = CellFlags::WIDE_CONT;
                rows[out_row].cells[out_col + 1] = continuation;
            }
            if old_width == 2 { positions[source_col + 1] = (out_row, out_col + usize::from(width == 2)); }
            out_col += width;
            if source_col < row.metadata.len { rows[out_row].metadata.len = out_col; }
            source_col += old_width;
            positions[source_col.min(old_cols)] = (out_row, out_col);
        }
        // A cursor in unused source-row space must remain on that logical
        // line when its new position coincides with a margin. Reserve an empty
        // continuation so later output cannot overwrite the next hard line.
        if source_row + 1 < source.len() && !row.metadata.wrapped
            && out_col == cols && cursors.iter().any(|cursor| cursor.row == source_row
            && !cursor.pending && cursor.col == len && len < old_cols)
        {
            rows[out_row].metadata.wrapped = true;
            rows.push(RetainedRow::blank(cols));
            out_row += 1;
            out_col = 0;
            positions[len] = (out_row, 0);
        }
        for (index, cursor) in cursors.iter().enumerate().filter(|(_, cursor)| cursor.row == source_row) {
            let col = if cursor.pending { (cursor.col + 1).min(old_cols) } else { cursor.col.min(old_cols) };
            let (row, col) = positions[col];
            mapped[index] = Cursor { row, col: col.min(cols - 1), pending: col == cols, retained_row: None };
        }
        if !row.metadata.wrapped && source_row + 1 < source.len() {
            rows.push(RetainedRow::blank(cols));
            out_row += 1;
            out_col = 0;
        }
    }
    Reflow { rows, cursors: mapped }
}
