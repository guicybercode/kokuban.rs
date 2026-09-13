use super::{cell::Cell, DEFAULT_CELL};

/// An immutable history row with a materialized prefix and a default-cell suffix.
/// Its logical width never changes when cells are omitted from the allocation.
#[derive(Debug, Clone)]
pub(crate) struct HistoryRow {
    cells: HistoryCells,
    cols: usize,
}

#[derive(Debug, Clone)]
enum HistoryCells {
    Single(Cell),
    // Empty prefixes use Vec::new(), which already needs no allocation.
    Cells(Vec<Cell>),
}

impl HistoryRow {
    /// The caller has proved that every omitted cell equals `Cell::default()`.
    pub(super) fn from_prefix(mut cells: Vec<Cell>, cols: usize) -> Self {
        assert!(cells.len() <= cols);
        match cells.len() {
            0 => Self {
                cells: HistoryCells::Cells(Vec::new()),
                cols,
            },
            1 => Self::from_single(cells.pop().unwrap(), cols),
            _ => Self {
                cells: HistoryCells::Cells(cells),
                cols,
            },
        }
    }

    /// Store one complete cell with a proven default suffix without allocating.
    pub(super) fn from_single(cell: Cell, cols: usize) -> Self {
        assert!(cols >= 1);
        Self {
            cells: HistoryCells::Single(cell),
            cols,
        }
    }

    pub(super) fn from_cells(mut cells: Vec<Cell>) -> Self {
        let cols = cells.len();
        let end = cells
            .iter()
            .rposition(|cell| cell != &DEFAULT_CELL)
            .map_or(0, |col| col + 1);
        cells.truncate(end);
        // Reflow produces full-width allocations; release the omitted suffix.
        // Empty and single-cell storage drop that allocation in from_prefix.
        if end > 1 {
            cells.shrink_to_fit();
        }
        Self::from_prefix(cells, cols)
    }

    /// Logical width, used for retention budgets and column projection.
    pub(super) fn len(&self) -> usize {
        self.cols
    }

    pub(super) fn get(&self, col: usize) -> Option<&Cell> {
        if col >= self.cols {
            return None;
        }
        Some(match &self.cells {
            HistoryCells::Single(cell) if col == 0 => cell,
            HistoryCells::Single(_) => &DEFAULT_CELL,
            HistoryCells::Cells(cells) => cells.get(col).unwrap_or(&DEFAULT_CELL),
        })
    }

    /// Materialize the logical row for the editable buffer and reflow.
    pub(super) fn into_cells(self) -> Vec<Cell> {
        let mut cells = match self.cells {
            HistoryCells::Single(cell) => {
                let mut cells = Vec::with_capacity(self.cols);
                cells.push(cell);
                cells
            }
            HistoryCells::Cells(cells) => cells,
        };
        cells.resize(self.cols, DEFAULT_CELL);
        cells
    }

    #[cfg(test)]
    pub(super) fn materialized_len(&self) -> usize {
        match &self.cells {
            HistoryCells::Single(_) => 1,
            HistoryCells::Cells(cells) => cells.len(),
        }
    }

    #[cfg(test)]
    pub(super) fn allocated_capacity(&self) -> usize {
        match &self.cells {
            HistoryCells::Single(_) => 0,
            HistoryCells::Cells(cells) => cells.capacity(),
        }
    }
}
