use crate::grid::Grid;

#[derive(Debug, Clone, Copy)]
pub struct GridPoint {
    pub row: i64,
    pub col: usize,
}

#[derive(Debug, Default)]
pub struct SelectionState {
    anchor: Option<GridPoint>,
    end: Option<GridPoint>,
}

impl SelectionState {
    pub fn start(&mut self, point: GridPoint) {
        self.anchor = Some(point);
        self.end = Some(point);
    }

    pub fn update(&mut self, point: GridPoint) {
        self.end = Some(point);
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.end = None;
    }

    pub fn is_active(&self) -> bool {
        self.anchor.is_some() && self.end.is_some()
    }

    /// Returns (start, end) normalized so start <= end in reading order.
    pub fn normalized(&self) -> Option<(GridPoint, GridPoint)> {
        let a = self.anchor?;
        let b = self.end?;
        if a.row < b.row || (a.row == b.row && a.col <= b.col) {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    /// Check if a visible (row, col) falls within the selection.
    /// Converts viewport coords to absolute coords using scroll_offset and scrollback_len.
    pub fn contains(&self, vis_row: usize, col: usize, scroll_offset: usize, scrollback_len: usize) -> bool {
        let (start, end) = match self.normalized() {
            Some(pair) => pair,
            None => return false,
        };
        let abs_row = scrollback_len as i64 - scroll_offset as i64 + vis_row as i64;
        if abs_row < start.row || abs_row > end.row {
            return false;
        }
        if abs_row == start.row && abs_row == end.row {
            return col >= start.col && col <= end.col;
        }
        if abs_row == start.row {
            return col >= start.col;
        }
        if abs_row == end.row {
            return col <= end.col;
        }
        true
    }

    /// Extract selected text from the grid.
    pub fn get_text(&self, grid: &Grid) -> String {
        let (start, end) = match self.normalized() {
            Some(pair) => pair,
            None => return String::new(),
        };

        let sb_len = grid.scrollback_len() as i64;
        let mut lines = Vec::new();

        for abs_row in start.row..=end.row {
            let col_start = if abs_row == start.row { start.col } else { 0 };
            let col_end = if abs_row == end.row {
                end.col
            } else {
                grid.cols().saturating_sub(1)
            };

            let mut line = String::new();
            for col in col_start..=col_end {
                let c = if abs_row < sb_len {
                    // Read from scrollback
                    grid.scrollback_cell(abs_row as usize, col)
                } else {
                    let buffer_row = (abs_row - sb_len) as usize;
                    if buffer_row < grid.rows() {
                        grid.buffer.cell(buffer_row, col).c
                    } else {
                        ' '
                    }
                };
                line.push(c);
            }
            // Trim trailing spaces
            let trimmed = line.trim_end();
            lines.push(trimmed.to_string());
        }

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::{GridPoint, SelectionState};

    #[test]
    fn normalizes_reverse_drag_order() {
        let mut selection = SelectionState::default();
        selection.start(GridPoint { row: 4, col: 8 });
        selection.update(GridPoint { row: 2, col: 3 });

        let (start, end) = selection.normalized().unwrap();
        assert_eq!((start.row, start.col), (2, 3));
        assert_eq!((end.row, end.col), (4, 8));
    }
}
