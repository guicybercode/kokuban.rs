//! Lógica dos atalhos de pane, independente de janela.
//!
//! `apply_pane_action` muda a árvore (split, fechar, foco, redimensionar,
//! busca, navegação de prompts) e devolve em `PaneActionOutcome` o que a
//! janela ainda precisa fazer (transição de foco, encerrar, retirar o pane
//! fechado, redesenhar). O que depende da plataforma (viewport, tamanho da
//! célula, zoom da fonte, diálogo de confirmação) fica no `PaneActionHost`.

use super::pane::Pane;
use super::PaneTree;
use crate::grid::Grid;
use crate::input::keybind::PaneAction;
use crate::layout::{PaneId, PixelRect, SplitDirection};
use crate::search::SearchState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomDirection {
    In,
    Out,
    Reset,
}

/// O que a janela fornece para aplicar um atalho de pane.
pub trait PaneActionHost {
    /// Área da janela ocupada pelos panes, em pixels físicos.
    fn viewport(&self) -> PixelRect;
    /// `(largura, altura)` da célula, em pixels físicos.
    fn cell_size(&self) -> (f32, f32);
    fn status_bar_height(&self) -> f32;
    /// Passo de redimensionamento (`[keybind.resize] step`), em pixels.
    fn resize_step(&self) -> f32;
    /// Muda o tamanho da fonte e reencaixa os panes de `tree`; a janela
    /// redesenha por conta própria (macOS: `perform_zoom`).
    fn zoom(&mut self, tree: &mut PaneTree, direction: ZoomDirection);
    /// Pergunta antes de fechar `id` (`pane` é `None` quando o id não existe
    /// mais). Verdadeiro quando um diálogo assumiu e a ação para aqui.
    fn confirm_close(&mut self, id: PaneId, pane: Option<&Pane>) -> bool;
}

/// Efeitos de `apply_pane_action` que a janela ainda precisa aplicar.
#[must_use]
#[derive(Default)]
pub struct PaneActionOutcome {
    /// Pane focado antes da ação, quando o foco mudou (inclusive quando o
    /// último pane fechou e não há mais foco).
    pub focus_changed_from: Option<PaneId>,
    /// O último pane fechou: a janela deve encerrar.
    pub terminate: bool,
    /// Pane retirado da árvore, para `retire` fora do lock.
    pub closed_pane: Option<Pane>,
    /// A geometria dos panes mudou (a árvore já foi reencaixada).
    pub needs_relayout: bool,
    /// Verdadeiro quando a ação rodou na árvore: a janela deve redesenhar,
    /// limpar a atenção do pane focado e acordar o leitor. Falso quando um
    /// diálogo de confirmação assumiu ou o host tratou o zoom.
    pub render: bool,
}

pub fn apply_pane_action(
    tree: &mut PaneTree,
    action: PaneAction,
    host: &mut dyn PaneActionHost,
) -> PaneActionOutcome {
    let (cell_w, cell_h) = host.cell_size();
    let previous_focus = tree.focused_pane().map(|pane| pane.id);
    let mut outcome = PaneActionOutcome::default();

    match action {
        PaneAction::SplitVertical | PaneAction::SplitHorizontal => {
            let direction = if action == PaneAction::SplitVertical {
                SplitDirection::Vertical
            } else {
                SplitDirection::Horizontal
            };
            if let Err(error) = tree.split(direction, cell_w, cell_h) {
                log::error!("Failed to split {direction:?}: {error}");
            }
            relayout(tree, host);
            outcome.needs_relayout = true;
        }
        PaneAction::ClosePane => {
            let id = tree.focused;
            if host.confirm_close(id, tree.pane(id)) {
                return outcome;
            }
            let closed = tree.close(id);
            outcome.closed_pane = closed.closed_pane;
            if closed.should_terminate {
                outcome.terminate = true;
                outcome.focus_changed_from = previous_focus;
                return outcome;
            }
            relayout(tree, host);
            outcome.needs_relayout = true;
        }
        PaneAction::FocusLeft => tree.focus_neighbor(SplitDirection::Vertical, false),
        PaneAction::FocusRight => tree.focus_neighbor(SplitDirection::Vertical, true),
        PaneAction::FocusUp => tree.focus_neighbor(SplitDirection::Horizontal, false),
        PaneAction::FocusDown => tree.focus_neighbor(SplitDirection::Horizontal, true),
        PaneAction::FocusAttention => {
            if let Some(target) = tree.next_attention_pane() {
                tree.focused = target;
            }
        }
        PaneAction::ResizeLeft
        | PaneAction::ResizeRight
        | PaneAction::ResizeUp
        | PaneAction::ResizeDown => {
            let (cell, positive) = match action {
                PaneAction::ResizeLeft => (cell_w, false),
                PaneAction::ResizeRight => (cell_w, true),
                PaneAction::ResizeUp => (cell_h, false),
                _ => (cell_h, true),
            };
            let magnitude = (host.resize_step() / cell) * 0.01;
            tree.resize_focused(if positive { magnitude } else { -magnitude });
            relayout(tree, host);
            outcome.needs_relayout = true;
        }
        PaneAction::ZoomIn | PaneAction::ZoomOut | PaneAction::ZoomReset => {
            let direction = match action {
                PaneAction::ZoomIn => ZoomDirection::In,
                PaneAction::ZoomOut => ZoomDirection::Out,
                _ => ZoomDirection::Reset,
            };
            host.zoom(tree, direction);
            return outcome;
        }
        PaneAction::Search => {
            if let Some(pane) = tree.focused_pane_mut() {
                if pane.search.is_none() {
                    let mut search = SearchState::new(pane.last_search_query.clone());
                    search.recompute(&pane.grid);
                    search.scroll_to_current(&mut pane.grid);
                    pane.search = Some(search);
                }
            }
        }
        PaneAction::PrevPrompt => {
            if let Some(pane) = tree.focused_pane_mut() {
                scroll_to_prev_prompt(&mut pane.grid);
            }
        }
        PaneAction::NextPrompt => {
            if let Some(pane) = tree.focused_pane_mut() {
                scroll_to_next_prompt(&mut pane.grid);
            }
        }
    }

    let current_focus = tree.focused_pane().map(|pane| pane.id);
    if current_focus != previous_focus {
        outcome.focus_changed_from = previous_focus;
    }
    outcome.render = true;
    outcome
}

fn relayout(tree: &mut PaneTree, host: &dyn PaneActionHost) {
    let (cell_w, cell_h) = host.cell_size();
    tree.relayout(host.viewport(), cell_w, cell_h, host.status_bar_height());
}

/// Linha absoluta no topo da viewport (ou do cursor, sem rolagem) de onde
/// a navegação de prompts parte.
fn prompt_navigation_origin(grid: &Grid) -> usize {
    if grid.scroll_offset > 0 {
        grid.scrollback_len().saturating_sub(grid.scroll_offset)
    } else {
        grid.current_absolute_row()
    }
}

/// `scroll_offset` que mostra a linha absoluta `target_row` com duas linhas
/// de contexto acima; `None` quando a linha já saiu do histórico.
fn scroll_offset_for_prompt(grid: &Grid, target_row: usize) -> Option<usize> {
    let sb_len = grid.scrollback_len();
    // O histórico guarda as últimas `sb_len` linhas: o intervalo absoluto é
    // [total_pushed - sb_len, total_pushed); a tela começa em total_pushed.
    let evicted = grid.total_lines_pushed.saturating_sub(sb_len);
    if target_row < evicted {
        return None;
    }
    let sb_index = target_row - evicted;
    Some(
        sb_len
            .saturating_sub(sb_index)
            .saturating_sub(2)
            .min(sb_len),
    )
}

/// Rola até o prompt anterior (OSC 133 A) acima da viewport, se houver.
pub fn scroll_to_prev_prompt(grid: &mut Grid) {
    let origin = prompt_navigation_origin(grid);
    if let Some(target_row) = grid.marks.prev_prompt(origin) {
        if let Some(offset) = scroll_offset_for_prompt(grid, target_row) {
            grid.scroll_offset = offset;
        }
    }
}

/// Rola até o próximo prompt abaixo da viewport; sem um no histórico,
/// volta ao fim.
pub fn scroll_to_next_prompt(grid: &mut Grid) {
    let origin = prompt_navigation_origin(grid);
    let target = grid
        .marks
        .next_prompt(origin)
        .filter(|&row| row < grid.total_lines_pushed)
        .and_then(|row| scroll_offset_for_prompt(grid, row));
    match target {
        Some(offset) => grid.scroll_offset = offset,
        // O alvo está na tela visível ou além: volta ao fim.
        None => grid.scroll_to_bottom(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_pane_action, scroll_to_next_prompt, scroll_to_prev_prompt, PaneActionHost,
        PaneActionOutcome, ZoomDirection,
    };
    use crate::grid::marks::PromptMarkKind;
    use crate::grid::Grid;
    use crate::input::keybind::PaneAction;
    use crate::layout::{PaneId, PixelRect, SplitDirection};
    use crate::pane::pane::Pane;
    use crate::pane::PaneTree;
    use crate::parser::ansi::{GraphicsSupport, Utf8Parser};
    use crate::renderer::kitty_handler::KittyHandlerOptions;

    const CELL: (f32, f32) = (10.0, 20.0);
    const VIEWPORT: PixelRect = PixelRect {
        x: 0.0,
        y: 0.0,
        width: 800.0,
        height: 480.0,
    };

    struct TestHost {
        confirm: bool,
        confirmed: Vec<(PaneId, bool)>,
        zooms: Vec<ZoomDirection>,
        resize_step: f32,
    }

    impl TestHost {
        fn new() -> Self {
            Self {
                confirm: false,
                confirmed: Vec::new(),
                zooms: Vec::new(),
                resize_step: 20.0,
            }
        }
    }

    impl PaneActionHost for TestHost {
        fn viewport(&self) -> PixelRect {
            VIEWPORT
        }
        fn cell_size(&self) -> (f32, f32) {
            CELL
        }
        fn status_bar_height(&self) -> f32 {
            0.0
        }
        fn resize_step(&self) -> f32 {
            self.resize_step
        }
        fn zoom(&mut self, _tree: &mut PaneTree, direction: ZoomDirection) {
            self.zooms.push(direction);
        }
        fn confirm_close(&mut self, id: PaneId, pane: Option<&Pane>) -> bool {
            self.confirmed.push((id, pane.is_some()));
            self.confirm
        }
    }

    fn tree() -> PaneTree {
        let mut tree = PaneTree::new(
            80,
            24,
            1_000,
            KittyHandlerOptions::from_megabytes(1, false),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            std::sync::Arc::new(|| {}),
            &[],
        )
        .expect("pane tree should spawn its initial shell");
        tree.relayout(VIEWPORT, CELL.0, CELL.1, 0.0);
        tree
    }

    fn retire_all(mut tree: PaneTree) {
        for pane in tree.take_all_panes() {
            pane.retire();
        }
    }

    fn apply(tree: &mut PaneTree, host: &mut TestHost, action: PaneAction) -> PaneActionOutcome {
        apply_pane_action(tree, action, host)
    }

    /// Apply an action whose outcome the test does not inspect.
    fn step(tree: &mut PaneTree, host: &mut TestHost, action: PaneAction) {
        let outcome = apply(tree, host, action);
        assert!(
            outcome.closed_pane.is_none(),
            "{action:?} must not detach a pane"
        );
    }

    fn assert_plain(outcome: &PaneActionOutcome) {
        assert!(outcome.render);
        assert!(!outcome.terminate);
        assert!(outcome.closed_pane.is_none());
    }

    #[test]
    fn split_relayouts_and_moves_focus_to_the_new_pane() {
        let mut tree = tree();
        let mut host = TestHost::new();

        let outcome = apply(&mut tree, &mut host, PaneAction::SplitVertical);
        assert_plain(&outcome);
        assert!(outcome.needs_relayout);
        assert_eq!(outcome.focus_changed_from, Some(1));
        assert_eq!(tree.pane_count(), 2);
        assert_eq!(tree.focused, 2);
        let (left, right) = (tree.pane(1).unwrap().rect, tree.pane(2).unwrap().rect);
        assert!(right.x > left.x, "vertical split puts the new pane right");
        assert!(left.width + right.width < VIEWPORT.width);

        let outcome = apply(&mut tree, &mut host, PaneAction::SplitHorizontal);
        assert_eq!(outcome.focus_changed_from, Some(2));
        assert_eq!(tree.pane_count(), 3);
        assert!(tree.pane(3).unwrap().rect.y > tree.pane(2).unwrap().rect.y);
        retire_all(tree);
    }

    #[test]
    fn focus_actions_follow_geometry_and_report_only_real_changes() {
        let mut tree = tree();
        let mut host = TestHost::new();
        step(&mut tree, &mut host, PaneAction::SplitVertical);
        assert_eq!(tree.focused, 2);

        let outcome = apply(&mut tree, &mut host, PaneAction::FocusLeft);
        assert_plain(&outcome);
        assert!(!outcome.needs_relayout);
        assert_eq!(outcome.focus_changed_from, Some(2));
        assert_eq!(tree.focused, 1);

        // No pane further left: focus stays, nothing to report, still rendered.
        let outcome = apply(&mut tree, &mut host, PaneAction::FocusLeft);
        assert_plain(&outcome);
        assert_eq!(outcome.focus_changed_from, None);
        assert_eq!(tree.focused, 1);

        assert_eq!(
            apply(&mut tree, &mut host, PaneAction::FocusRight).focus_changed_from,
            Some(1)
        );
        assert_eq!(tree.focused, 2);
        step(&mut tree, &mut host, PaneAction::SplitHorizontal);
        assert_eq!(tree.focused, 3);
        assert_eq!(
            apply(&mut tree, &mut host, PaneAction::FocusUp).focus_changed_from,
            Some(3)
        );
        assert_eq!(tree.focused, 2);
        assert_eq!(
            apply(&mut tree, &mut host, PaneAction::FocusDown).focus_changed_from,
            Some(2)
        );
        assert_eq!(tree.focused, 3);
        retire_all(tree);
    }

    #[test]
    fn focus_attention_jumps_to_the_flagged_pane() {
        let mut tree = tree();
        let mut host = TestHost::new();
        step(&mut tree, &mut host, PaneAction::SplitVertical);
        step(&mut tree, &mut host, PaneAction::SplitVertical);
        assert_eq!(tree.focused, 3);

        let outcome = apply(&mut tree, &mut host, PaneAction::FocusAttention);
        assert_plain(&outcome);
        assert_eq!(outcome.focus_changed_from, None, "nothing flagged");

        tree.pane_mut(1).unwrap().attention = true;
        let outcome = apply(&mut tree, &mut host, PaneAction::FocusAttention);
        assert_eq!(outcome.focus_changed_from, Some(3));
        assert_eq!(tree.focused, 1);
        // The caller clears the flag in its epilogue, as the window does.
        assert!(tree.clear_focused_attention());
        retire_all(tree);
    }

    #[test]
    fn resize_moves_the_divider_by_the_step_in_cells() {
        let mut tree = tree();
        let mut host = TestHost::new();
        step(&mut tree, &mut host, PaneAction::SplitVertical);
        step(&mut tree, &mut host, PaneAction::FocusLeft);
        let before = tree.pane(1).unwrap().rect.width;

        let outcome = apply(&mut tree, &mut host, PaneAction::ResizeRight);
        assert_plain(&outcome);
        assert!(outcome.needs_relayout);
        assert_eq!(outcome.focus_changed_from, None);
        let after = tree.pane(1).unwrap().rect.width;
        // delta = (20 / 10) * 0.01 = 0.02 of the 800px viewport.
        assert!((after - before - 16.0).abs() < 0.01, "{before} -> {after}");

        step(&mut tree, &mut host, PaneAction::ResizeLeft);
        assert!((tree.pane(1).unwrap().rect.width - before).abs() < 0.01);

        step(&mut tree, &mut host, PaneAction::SplitHorizontal);
        let top = tree.pane(1).unwrap().rect.height;
        step(&mut tree, &mut host, PaneAction::ResizeUp);
        // delta = (20 / 20) * 0.01 = 0.01 of the 480px viewport.
        assert!((tree.pane(1).unwrap().rect.height - (top - 4.8)).abs() < 0.01);
        step(&mut tree, &mut host, PaneAction::ResizeDown);
        assert!((tree.pane(1).unwrap().rect.height - top).abs() < 0.01);
        retire_all(tree);
    }

    #[test]
    fn close_detaches_the_focused_pane_or_stops_at_the_confirmation() {
        let mut tree = tree();
        let mut host = TestHost::new();
        step(&mut tree, &mut host, PaneAction::SplitVertical);
        let full_cols = 80;
        assert!(tree.pane(1).unwrap().grid.cols() < full_cols);

        host.confirm = true;
        let outcome = apply(&mut tree, &mut host, PaneAction::ClosePane);
        assert!(!outcome.render, "the dialog owns the next render");
        assert!(!outcome.terminate && !outcome.needs_relayout);
        assert!(outcome.closed_pane.is_none());
        assert_eq!(outcome.focus_changed_from, None);
        assert_eq!(host.confirmed, [(2, true)]);
        assert_eq!(tree.pane_count(), 2);

        host.confirm = false;
        let outcome = apply(&mut tree, &mut host, PaneAction::ClosePane);
        assert!(outcome.render && outcome.needs_relayout && !outcome.terminate);
        assert_eq!(outcome.focus_changed_from, Some(2));
        assert_eq!(tree.focused, 1);
        assert_eq!(tree.pane_count(), 1);
        assert_eq!(tree.pane(1).unwrap().grid.cols(), full_cols);
        outcome
            .closed_pane
            .expect("closed pane is handed back")
            .retire();

        let outcome = apply(&mut tree, &mut host, PaneAction::ClosePane);
        assert!(outcome.terminate);
        assert!(!outcome.render);
        assert_eq!(outcome.focus_changed_from, Some(1));
        assert_eq!(tree.pane_count(), 0);
        outcome
            .closed_pane
            .expect("last pane is handed back")
            .retire();
    }

    #[test]
    fn zoom_is_delegated_to_the_host() {
        let mut tree = tree();
        let mut host = TestHost::new();
        for (action, direction) in [
            (PaneAction::ZoomIn, ZoomDirection::In),
            (PaneAction::ZoomOut, ZoomDirection::Out),
            (PaneAction::ZoomReset, ZoomDirection::Reset),
        ] {
            let outcome = apply(&mut tree, &mut host, action);
            assert!(!outcome.render && !outcome.terminate && !outcome.needs_relayout);
            assert_eq!(outcome.focus_changed_from, None);
            assert_eq!(host.zooms.last(), Some(&direction));
        }
        assert_eq!(host.zooms.len(), 3);
        retire_all(tree);
    }

    #[test]
    fn search_opens_once_with_the_last_query() {
        let mut tree = tree();
        let mut host = TestHost::new();
        {
            let pane = tree.focused_pane_mut().unwrap();
            let mut parser = Utf8Parser::new();
            parser.feed(b"needle one\r\nneedle two\r\n", &mut pane.grid);
            pane.last_search_query = "needle".to_string();
        }

        let outcome = apply(&mut tree, &mut host, PaneAction::Search);
        assert_plain(&outcome);
        let pane = tree.focused_pane().unwrap();
        let search = pane.search.as_ref().expect("search opens");
        assert_eq!(search.query, "needle");
        assert_eq!(search.matches.len(), 2);
        assert_eq!(search.current, Some(1));

        // Reopening keeps the state the user is typing into.
        tree.focused_pane_mut()
            .unwrap()
            .search
            .as_mut()
            .unwrap()
            .query = "typed".into();
        step(&mut tree, &mut host, PaneAction::Search);
        assert_eq!(
            tree.focused_pane().unwrap().search.as_ref().unwrap().query,
            "typed"
        );
        retire_all(tree);
    }

    /// 4 linhas visíveis, 100 de histórico; cada prompt é uma linha
    /// `prompt N` seguida de 9 linhas de saída.
    fn grid_with_prompts(prompts: usize) -> Grid {
        let mut grid = Grid::new(20, 4, 100);
        let mut parser = Utf8Parser::new();
        for n in 0..prompts {
            grid.marks
                .push(PromptMarkKind::PromptStart, grid.current_absolute_row());
            parser.feed(format!("prompt {n}\r\n").as_bytes(), &mut grid);
            for line in 0..9 {
                parser.feed(format!("out {n}.{line}\r\n").as_bytes(), &mut grid);
            }
        }
        grid
    }

    #[test]
    fn prompt_navigation_scrolls_between_marks_with_context() {
        let mut grid = grid_with_prompts(3);
        // 30 lines fed into 4 visible rows: 27 pushed to scrollback, the
        // cursor sits at absolute row 30 and the prompts at rows 0, 10, 20.
        assert_eq!(grid.scrollback_len(), 27);
        assert_eq!(grid.current_absolute_row(), 30);
        assert_eq!(grid.scroll_offset, 0);

        // Legacy macOS formula, kept as is: `scroll_offset = sb_len -
        // sb_index - 2`, so the viewport top lands two rows below the prompt.
        scroll_to_prev_prompt(&mut grid);
        assert_eq!(grid.scroll_offset, 27 - 20 - 2);
        // From there the same prompt is still the nearest one above the top.
        scroll_to_prev_prompt(&mut grid);
        assert_eq!(grid.scroll_offset, 27 - 20 - 2);

        grid.scroll_offset = 27 - 10; // top exactly on prompt 1
        scroll_to_prev_prompt(&mut grid);
        assert_eq!(grid.scroll_offset, 27 - 2, "prompt 0 at row 0");
        scroll_to_prev_prompt(&mut grid);
        assert_eq!(grid.scroll_offset, 27 - 2, "no earlier prompt: stays");

        scroll_to_next_prompt(&mut grid);
        assert_eq!(grid.scroll_offset, 27 - 10 - 2);
        scroll_to_next_prompt(&mut grid);
        assert_eq!(grid.scroll_offset, 27 - 20 - 2);
        scroll_to_next_prompt(&mut grid);
        assert_eq!(grid.scroll_offset, 0, "no later prompt: back to the bottom");

        // A prompt that scrolled out of the history is ignored.
        let mut grid = Grid::new(20, 4, 5);
        let mut parser = Utf8Parser::new();
        grid.marks.push(PromptMarkKind::PromptStart, 0);
        for n in 0..20 {
            parser.feed(format!("line {n}\r\n").as_bytes(), &mut grid);
        }
        scroll_to_prev_prompt(&mut grid);
        assert_eq!(grid.scroll_offset, 0);

        // Next prompt on the visible screen snaps to the bottom.
        let mut grid = Grid::new(20, 4, 100);
        parser.feed(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n", &mut grid);
        grid.scroll_offset = grid.scrollback_len();
        grid.marks
            .push(PromptMarkKind::PromptStart, grid.total_lines_pushed + 1);
        scroll_to_next_prompt(&mut grid);
        assert_eq!(grid.scroll_offset, 0);
    }

    #[test]
    fn prompt_actions_scroll_the_focused_pane() {
        let mut tree = tree();
        let mut host = TestHost::new();
        {
            let pane = tree.focused_pane_mut().unwrap();
            pane.grid = grid_with_prompts(2);
        }
        let outcome = apply(&mut tree, &mut host, PaneAction::PrevPrompt);
        assert_plain(&outcome);
        assert_eq!(outcome.focus_changed_from, None);
        // 20 lines fed, 17 pushed; prompt 1 at row 10.
        assert_eq!(tree.focused_pane().unwrap().grid.scroll_offset, 17 - 10 - 2);
        step(&mut tree, &mut host, PaneAction::NextPrompt);
        assert_eq!(tree.focused_pane().unwrap().grid.scroll_offset, 0);
        retire_all(tree);
    }

    #[test]
    fn split_direction_helper_matches_layout() {
        // Guard against swapping the two split actions.
        let mut tree = tree();
        let mut host = TestHost::new();
        step(&mut tree, &mut host, PaneAction::SplitHorizontal);
        let (first, second) = (tree.pane(1).unwrap().rect, tree.pane(2).unwrap().rect);
        let (top, bottom) = VIEWPORT.split(SplitDirection::Horizontal, 0.5);
        assert_eq!(first, top);
        assert_eq!(second, bottom);
        retire_all(tree);
    }
}
