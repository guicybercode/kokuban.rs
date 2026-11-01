//! Clipboard shortcuts and the selection state that follows the grid.

use super::draw::GridSnapshot;
use super::pointer::terminal_cell_at_pointer;
use super::sizing::TerminalDimensions;
use crate::grid::cell::{CellFlags, Color};
use crate::grid::Grid;
use crate::selection::{point_from_viewport, GridPoint, SelectionState};
use crate::terminal_colors::TerminalColors;
use winit::dpi::PhysicalPosition;
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClipboardAction {
    Copy,
    Paste,
    SelectAll,
}

#[derive(Default)]
pub(super) struct SelectionContext {
    revision: u64,
    dropped_rows: usize,
}

pub(super) fn sync_selection(
    selection: &mut SelectionState,
    context: &mut SelectionContext,
    grid: &Grid,
) {
    let dropped_rows = grid
        .total_lines_pushed
        .saturating_sub(grid.scrollback_len());
    if context.revision != grid.selection_revision() || dropped_rows < context.dropped_rows {
        selection.clear();
    } else {
        selection.rebase_after_eviction(dropped_rows - context.dropped_rows);
    }
    context.revision = grid.selection_revision();
    context.dropped_rows = dropped_rows;
}

pub(super) fn selection_point_at_pointer(
    grid: &Grid,
    position: PhysicalPosition<f64>,
    cell_size: (u16, u16),
) -> Option<GridPoint> {
    if !position.x.is_finite() || !position.y.is_finite() {
        return None;
    }
    let dimensions = TerminalDimensions {
        columns: u16::try_from(grid.cols()).ok()?,
        rows: u16::try_from(grid.rows()).ok()?,
    };
    let position = PhysicalPosition::new(position.x.max(0.0), position.y.max(0.0));
    // Mouse reports are (column, row), one-based; selection points are
    // (row, column), zero-based retained-grid coordinates.
    let (column, row) = terminal_cell_at_pointer(position, cell_size, dimensions)?;
    Some(point_from_viewport(grid, row - 1, column - 1))
}

pub(super) fn clipboard_action(
    key: Key<&str>,
    modifiers: ModifiersState,
) -> Option<ClipboardAction> {
    if modifiers == (ModifiersState::CONTROL | ModifiersState::SHIFT) {
        if let Key::Character(text) = key {
            if text.eq_ignore_ascii_case("c") {
                return Some(ClipboardAction::Copy);
            }
            if text.eq_ignore_ascii_case("v") {
                return Some(ClipboardAction::Paste);
            }
            if text.eq_ignore_ascii_case("a") {
                return Some(ClipboardAction::SelectAll);
            }
        }
    }
    if key == Key::Named(NamedKey::Insert) {
        if modifiers == ModifiersState::CONTROL {
            return Some(ClipboardAction::Copy);
        }
        if modifiers == ModifiersState::SHIFT {
            return Some(ClipboardAction::Paste);
        }
    }
    None
}

pub(super) fn apply_selection_to_snapshot(
    snapshot: &mut GridSnapshot,
    selection: &SelectionState,
    grid: &Grid,
    colors: &TerminalColors,
) {
    if !selection.is_active() {
        return;
    }
    let foreground = colors.selection_foreground();
    let background = colors.selection_background();
    for row in 0..snapshot.rows {
        for column in 0..snapshot.columns {
            if selection.contains_cell(grid, row, column) {
                let cell = &mut snapshot.cells[row * snapshot.columns + column];
                cell.fg = Color::Rgb(foreground.0, foreground.1, foreground.2);
                cell.bg = Color::Rgb(background.0, background.1, background.2);
                cell.flags.remove(CellFlags::REVERSE | CellFlags::FAINT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_selection_to_snapshot, clipboard_action, selection_point_at_pointer, sync_selection,
        ClipboardAction, SelectionContext,
    };
    use crate::grid::cell::{CellFlags, Color, UnderlineStyle};
    use crate::grid::Grid;
    use crate::linux_window::draw::snapshot_locked_grid;
    use crate::selection::{GridPoint, SelectionState};
    use crate::terminal_colors::TerminalColors;
    use winit::dpi::PhysicalPosition;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    fn selected(start: GridPoint, end: GridPoint) -> SelectionState {
        let mut selection = SelectionState::default();
        selection.start(start);
        selection.update(end);
        selection
    }

    fn write_row(grid: &mut Grid, row: usize, text: &str) {
        grid.set_cursor_pos(row, 0);
        for character in text.chars() {
            grid.put_char(character);
        }
    }

    #[test]
    fn pointer_selection_uses_zero_based_rows_and_columns_independently() {
        let grid = Grid::new(8, 3, 10);
        for ((x, y), expected) in [
            ((0.0, 0.0), GridPoint { row: 0, col: 0 }),
            ((39.0, 5.0), GridPoint { row: 0, col: 3 }),
            ((0.0, 20.0), GridPoint { row: 1, col: 0 }),
            ((31.0, 42.0), GridPoint { row: 2, col: 3 }),
            ((-100.0, -100.0), GridPoint { row: 0, col: 0 }),
            ((1_000.0, 1_000.0), GridPoint { row: 2, col: 7 }),
        ] {
            assert_eq!(
                selection_point_at_pointer(&grid, PhysicalPosition::new(x, y), (10, 20)),
                Some(expected),
                "pointer ({x}, {y}) must map to a zero-based selection cell"
            );
        }
    }

    #[test]
    fn pointer_selection_rejects_nonfinite_positions_and_zero_sized_cells() {
        let grid = Grid::new(8, 3, 10);
        for position in [
            PhysicalPosition::new(f64::NAN, 0.0),
            PhysicalPosition::new(0.0, f64::NAN),
            PhysicalPosition::new(f64::INFINITY, 0.0),
            PhysicalPosition::new(0.0, f64::NEG_INFINITY),
        ] {
            assert_eq!(selection_point_at_pointer(&grid, position, (10, 20)), None);
        }
        for cell_size in [(0, 20), (10, 0), (0, 0)] {
            assert_eq!(
                selection_point_at_pointer(&grid, PhysicalPosition::new(0.0, 0.0), cell_size),
                None
            );
        }
    }

    #[test]
    fn pointer_selection_snaps_wide_characters_and_tracks_visible_history() {
        let mut grid = Grid::new(8, 3, 10);
        grid.set_cursor_pos(1, 2);
        grid.put_char('日');
        let position = PhysicalPosition::new(39.0, 21.0);
        let point = selection_point_at_pointer(&grid, position, (10, 20)).unwrap();
        assert_eq!(point, GridPoint { row: 1, col: 2 });
        assert_eq!(selected(point, point).get_text(&grid), "日");

        grid.scroll_up(1);
        grid.scroll_viewport_up(1);
        let history_point = selection_point_at_pointer(&grid, position, (10, 20)).unwrap();
        assert_eq!(history_point, point);
        assert_eq!(selected(history_point, history_point).get_text(&grid), "日");
        grid.scroll_to_bottom();
        assert_eq!(
            selection_point_at_pointer(&grid, PhysicalPosition::new(39.0, 1.0), (10, 20)),
            Some(point)
        );
    }

    #[test]
    fn clipboard_shortcuts_accept_only_the_documented_modifier_combinations() {
        let clipboard_modifiers = ModifiersState::CONTROL | ModifiersState::SHIFT;
        for (key, expected) in [
            ("c", ClipboardAction::Copy),
            ("C", ClipboardAction::Copy),
            ("v", ClipboardAction::Paste),
            ("V", ClipboardAction::Paste),
            ("a", ClipboardAction::SelectAll),
            ("A", ClipboardAction::SelectAll),
        ] {
            assert_eq!(
                clipboard_action(Key::Character(key), clipboard_modifiers),
                Some(expected)
            );
        }
        for key in ["x", "cv", "ç", "", "\u{3}", "\u{16}"] {
            assert_eq!(
                clipboard_action(Key::Character(key), clipboard_modifiers),
                None
            );
        }
        assert_eq!(
            clipboard_action(Key::Named(NamedKey::Delete), clipboard_modifiers),
            None
        );
    }

    #[test]
    fn clipboard_shortcuts_leave_control_and_meta_keys_for_terminal_apps() {
        for modifiers in [
            ModifiersState::empty(),
            ModifiersState::CONTROL,
            ModifiersState::SHIFT,
            ModifiersState::ALT,
            ModifiersState::SUPER,
            ModifiersState::CONTROL | ModifiersState::ALT,
            ModifiersState::CONTROL | ModifiersState::SHIFT | ModifiersState::ALT,
            ModifiersState::CONTROL | ModifiersState::SHIFT | ModifiersState::SUPER,
        ] {
            for key in ["c", "v", "a", "C", "V", "A"] {
                assert_eq!(
                    clipboard_action(Key::Character(key), modifiers),
                    None,
                    "{key} with {modifiers:?} belongs to ordinary terminal input"
                );
            }
        }
    }

    #[test]
    fn insert_clipboard_shortcuts_leave_all_other_modifier_combinations_for_apps() {
        for combination in 0..16 {
            let mut modifiers = ModifiersState::empty();
            for (bit, modifier) in [
                ModifiersState::CONTROL,
                ModifiersState::SHIFT,
                ModifiersState::ALT,
                ModifiersState::SUPER,
            ]
            .into_iter()
            .enumerate()
            {
                if combination & (1 << bit) != 0 {
                    modifiers |= modifier;
                }
            }
            let expected = if modifiers == ModifiersState::CONTROL {
                Some(ClipboardAction::Copy)
            } else if modifiers == ModifiersState::SHIFT {
                Some(ClipboardAction::Paste)
            } else {
                None
            };
            assert_eq!(
                clipboard_action(Key::Named(NamedKey::Insert), modifiers),
                expected,
                "Insert with {modifiers:?} must route only the documented clipboard shortcuts"
            );
        }
    }

    #[test]
    fn selection_snapshot_covers_a_whole_wide_glyph_and_preserves_hidden_content() {
        let mut grid = Grid::new(5, 2, 10);
        write_row(&mut grid, 0, "a日b");
        for column in 1..=2 {
            let cell = grid.buffer.cell_mut(0, column);
            cell.fg = Color::Rgb(10, 20, 30);
            cell.bg = Color::Rgb(40, 50, 60);
            cell.flags
                .insert(CellFlags::REVERSE | CellFlags::FAINT | CellFlags::HIDDEN);
            cell.underline_style = UnderlineStyle::Curly;
            cell.underline_color = Color::Rgb(70, 80, 90);
        }
        let colors = TerminalColors::new((200, 210, 220), (11, 22, 33));
        let selection = selected(GridPoint { row: 0, col: 2 }, GridPoint { row: 0, col: 2 });
        let mut snapshot = snapshot_locked_grid(&grid);
        apply_selection_to_snapshot(&mut snapshot, &selection, &grid, &colors);

        for column in 1..=2 {
            let cell = snapshot.cell(0, column).unwrap();
            assert_eq!(cell.fg, Color::Rgb(11, 22, 33));
            assert_eq!(cell.bg, Color::Rgb(200, 210, 220));
            assert!(cell.flags.contains(CellFlags::HIDDEN));
            assert!(!cell.flags.intersects(CellFlags::REVERSE | CellFlags::FAINT));
            assert_eq!(cell.underline_style, UnderlineStyle::Curly);
            assert_eq!(cell.underline_color, Color::Rgb(70, 80, 90));
            assert_eq!(
                cell.flags & (CellFlags::WIDE | CellFlags::WIDE_CONT),
                grid.buffer.cell(0, column).flags & (CellFlags::WIDE | CellFlags::WIDE_CONT)
            );
            let resolved = colors.resolve_cell_colors(cell.fg, cell.bg, cell.flags);
            assert_eq!(resolved.foreground, (11, 22, 33));
            assert_eq!(resolved.background, (200, 210, 220));
        }
        for (row, column) in [(0, 0), (0, 3), (1, 1)] {
            let before = grid.buffer.cell(row, column);
            let after = snapshot.cell(row, column).unwrap();
            assert_eq!(
                (after.c, after.fg, after.bg, after.flags),
                (before.c, before.fg, before.bg, before.flags)
            );
        }
        // Highlighting modifies only the render snapshot, never application text.
        assert_eq!(grid.buffer.cell(0, 1).fg, Color::Rgb(10, 20, 30));
        assert!(grid
            .buffer
            .cell(0, 1)
            .flags
            .contains(CellFlags::REVERSE | CellFlags::FAINT));
        assert_eq!(selection.get_text(&grid), "日");
    }

    #[test]
    fn selection_snapshot_projects_scrollback_and_reverse_drag() {
        let mut grid = Grid::new(4, 2, 10);
        write_row(&mut grid, 0, "old");
        write_row(&mut grid, 1, "live");
        grid.scroll_up(1);
        grid.scroll_viewport_up(1);
        let selection = selected(GridPoint { row: 1, col: 1 }, GridPoint { row: 0, col: 2 });
        let mut snapshot = snapshot_locked_grid(&grid);
        apply_selection_to_snapshot(
            &mut snapshot,
            &selection,
            &grid,
            &TerminalColors::new((200, 210, 220), (11, 22, 33)),
        );
        assert_eq!(selection.get_text(&grid), "d\nli");
        for (row, column) in [(0, 2), (0, 3), (1, 0), (1, 1)] {
            assert_eq!(
                snapshot.cell(row, column).unwrap().bg,
                Color::Rgb(200, 210, 220)
            );
        }
        for (row, column) in [(0, 0), (0, 1), (1, 2), (1, 3)] {
            assert_eq!(snapshot.cell(row, column).unwrap().bg, Color::Default);
        }
    }

    #[test]
    fn selection_sync_rebases_evicted_history_once_and_clears_when_fully_lost() {
        let mut grid = Grid::new(4, 2, 1);
        write_row(&mut grid, 0, "old");
        write_row(&mut grid, 1, "keep");
        let mut selection = SelectionState::default();
        let mut context = SelectionContext::default();
        sync_selection(&mut selection, &mut context, &grid);
        selection.start(GridPoint { row: 1, col: 0 });
        selection.update(GridPoint { row: 1, col: 3 });
        for expected_row in [1, 0] {
            grid.scroll_up(1);
            sync_selection(&mut selection, &mut context, &grid);
            assert_eq!(selection.get_text(&grid), "keep");
            assert_eq!(selection.normalized().unwrap().0.row, expected_row);
            sync_selection(&mut selection, &mut context, &grid);
            assert_eq!(selection.normalized().unwrap().0.row, expected_row);
        }
        assert_eq!(context.dropped_rows, 1);
        grid.scroll_up(1);
        sync_selection(&mut selection, &mut context, &grid);
        assert!(!selection.is_active());
    }

    #[test]
    fn selection_sync_invalidates_coordinates_on_grid_revision_changes() {
        let changes: [fn(&mut Grid); 6] = [
            |grid| grid.erase_in_display(2),
            |grid| grid.erase_in_display(3),
            |grid| grid.resize(5, 3),
            |grid| grid.enter_alt_screen(),
            |grid| grid.reset_terminal_state(),
            |grid| {
                grid.scroll_top = 1;
                grid.scroll_up(1);
            },
        ];
        for change in changes {
            let mut grid = Grid::new(4, 3, 10);
            let mut selection = SelectionState::default();
            let mut context = SelectionContext::default();
            sync_selection(&mut selection, &mut context, &grid);
            selection.start(GridPoint { row: 0, col: 0 });
            change(&mut grid);
            sync_selection(&mut selection, &mut context, &grid);
            assert!(!selection.is_active());
            assert_eq!(context.revision, grid.selection_revision());
        }
        let mut grid = Grid::new(4, 3, 10);
        grid.enter_alt_screen();
        let mut context = SelectionContext::default();
        let mut selection = SelectionState::default();
        sync_selection(&mut selection, &mut context, &grid);
        selection.start(GridPoint { row: 0, col: 0 });
        grid.leave_alt_screen();
        sync_selection(&mut selection, &mut context, &grid);
        assert!(!selection.is_active());
    }

    #[test]
    fn repaint_invalidates_selection_without_changing_the_paste_target_screen() {
        let mut grid = Grid::new(4, 3, 10);
        grid.enter_alt_screen();
        grid.bracketed_paste = true;
        let paste_screen = grid.screen_revision();
        let mut context = SelectionContext::default();
        let mut selection = SelectionState::default();
        sync_selection(&mut selection, &mut context, &grid);
        selection.start(GridPoint { row: 0, col: 0 });
        grid.erase_in_display(2);
        sync_selection(&mut selection, &mut context, &grid);
        assert!(!selection.is_active());
        assert_eq!(grid.screen_revision(), paste_screen);
        assert!(grid.bracketed_paste);
        grid.scroll_top = 1;
        grid.scroll_up(1);
        grid.resize(5, 3);
        assert_eq!(grid.screen_revision(), paste_screen);
        grid.leave_alt_screen();
        assert_ne!(grid.screen_revision(), paste_screen);
    }

    #[test]
    fn unchanged_geometry_and_viewport_scrolling_preserve_selection() {
        let mut grid = Grid::new(4, 2, 10);
        write_row(&mut grid, 0, "keep");
        grid.scroll_up(1);
        let mut context = SelectionContext::default();
        let mut selection = SelectionState::default();
        sync_selection(&mut selection, &mut context, &grid);
        selection.start(GridPoint { row: 0, col: 0 });
        selection.update(GridPoint { row: 0, col: 3 });
        let revision = grid.selection_revision();
        grid.resize(4, 2);
        grid.scroll_viewport_up(1);
        sync_selection(&mut selection, &mut context, &grid);
        assert_eq!(selection.get_text(&grid), "keep");
        assert_eq!(grid.selection_revision(), revision);
        grid.scroll_to_bottom();
        sync_selection(&mut selection, &mut context, &grid);
        assert_eq!(selection.get_text(&grid), "keep");
    }
}
