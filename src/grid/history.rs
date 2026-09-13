use super::{cell::Cell, DEFAULT_CELL};

/// An immutable history row with a materialized prefix and a default-cell suffix.
/// Its logical width never changes when cells are omitted from the allocation.
#[derive(Debug, Clone)]
pub(crate) struct HistoryRow {
    cells: Vec<Cell>,
    cols: usize,
}

impl HistoryRow {
    /// The caller has proved that every omitted cell equals `Cell::default()`.
    pub(super) fn from_prefix(cells: Vec<Cell>, cols: usize) -> Self {
        assert!(cells.len() <= cols);
        Self { cells, cols }
    }

    pub(super) fn from_cells(mut cells: Vec<Cell>) -> Self {
        let cols = cells.len();
        let end = cells
            .iter()
            .rposition(|cell| cell != &DEFAULT_CELL)
            .map_or(0, |col| col + 1);
        cells.truncate(end);
        // Reflow produces full-width allocations; release the omitted suffix.
        cells.shrink_to_fit();
        Self { cells, cols }
    }

    /// Logical width, used for retention budgets and column projection.
    pub(super) fn len(&self) -> usize {
        self.cols
    }

    pub(super) fn get(&self, col: usize) -> Option<&Cell> {
        if col >= self.cols {
            return None;
        }
        Some(self.cells.get(col).unwrap_or(&DEFAULT_CELL))
    }

    /// Materialize the logical row for the editable buffer and reflow.
    pub(super) fn into_cells(mut self) -> Vec<Cell> {
        self.cells.resize(self.cols, DEFAULT_CELL);
        self.cells
    }

    #[cfg(test)]
    pub(super) fn materialized_len(&self) -> usize {
        self.cells.len()
    }
}
