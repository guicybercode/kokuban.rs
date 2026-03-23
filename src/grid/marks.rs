#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMarkKind {
    PromptStart,
    CommandStart,
    CommandExecuted,
    CommandFinished { exit_code: i32 },
}

#[derive(Debug, Clone)]
pub struct PromptMark {
    pub kind: PromptMarkKind,
    pub absolute_row: usize,
}

#[derive(Debug, Default)]
pub struct MarkIndex {
    marks: Vec<PromptMark>,
}

impl MarkIndex {
    pub fn push(&mut self, kind: PromptMarkKind, absolute_row: usize) {
        self.marks.push(PromptMark { kind, absolute_row });
    }

    /// Find the nearest PromptStart mark ABOVE (strictly less than) the given row.
    pub fn prev_prompt(&self, current_row: usize) -> Option<usize> {
        // marks are in insertion order (monotonically increasing)
        // binary search for the last mark < current_row
        let mut result = None;
        for mark in self.marks.iter().rev() {
            if mark.absolute_row < current_row && mark.kind == PromptMarkKind::PromptStart {
                result = Some(mark.absolute_row);
                break;
            }
        }
        result
    }

    /// Find the nearest PromptStart mark BELOW (strictly greater than) the given row.
    pub fn next_prompt(&self, current_row: usize) -> Option<usize> {
        for mark in &self.marks {
            if mark.absolute_row > current_row && mark.kind == PromptMarkKind::PromptStart {
                return Some(mark.absolute_row);
            }
        }
        None
    }

    /// Get all PromptStart absolute rows that are currently visible.
    pub fn visible_prompt_rows(
        &self,
        scroll_offset: usize,
        scrollback_len: usize,
        visible_rows: usize,
    ) -> Vec<usize> {
        let start_abs = scrollback_len.saturating_sub(scroll_offset);
        let end_abs = start_abs + visible_rows;

        let mut rows = Vec::new();
        for mark in &self.marks {
            if mark.kind == PromptMarkKind::PromptStart
                && mark.absolute_row >= start_abs
                && mark.absolute_row < end_abs
            {
                rows.push(mark.absolute_row - start_abs);
            }
        }
        rows
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }
}
