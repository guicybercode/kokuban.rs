//! Busca incremental no texto retido do grid (scrollback + tela).
//!
//! Lógica pura, sem AppKit: a janela só alimenta a consulta e lê os
//! resultados. As linhas quebradas automaticamente (soft wrap) são unidas em
//! uma linha lógica antes da comparação, do mesmo modo que a cópia de seleção.

use crate::grid::{cell::CellFlags, Grid};

/// Limite de resultados por consulta, para manter a busca barata em
/// históricos grandes.
pub const MAX_MATCHES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// Linha retida (0 = linha mais antiga do histórico).
    pub row: i64,
    /// Coluna da primeira célula do resultado.
    pub col: usize,
    /// Extensão em células, contando as duas metades de glifos largos.
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Current,
    Other,
}

#[derive(Debug, Default)]
pub struct SearchState {
    pub query: String,
    pub matches: Vec<SearchMatch>,
    pub current: Option<usize>,
    rows_seen: usize,
}

impl SearchState {
    pub fn new(query: String) -> Self {
        Self {
            query,
            ..Default::default()
        }
    }

    /// Verdadeiro quando chegaram linhas novas desde o último `recompute`.
    pub fn needs_recompute(&self, grid: &Grid) -> bool {
        self.rows_seen != grid.retained_rows()
    }

    /// Refaz a busca sobre todo o texto retido. O resultado atual é mantido
    /// quando o mesmo (linha, coluna) ainda existe; senão volta ao último
    /// resultado (o mais próximo do fim).
    pub fn recompute(&mut self, grid: &Grid) {
        let previous = self.current.and_then(|i| self.matches.get(i).copied());
        self.matches.clear();
        self.rows_seen = grid.retained_rows();
        let needle = self.query.to_lowercase();
        if needle.is_empty() {
            self.current = None;
            return;
        }

        let retained = grid.retained_rows();
        let mut text = String::new();
        // Origem (linha, coluna) de cada byte de `text`.
        let mut origins: Vec<(usize, usize)> = Vec::new();
        let mut row = 0;
        'outer: while row < retained {
            text.clear();
            origins.clear();
            // Une as linhas quebradas automaticamente em uma linha lógica.
            let mut r = row;
            loop {
                let len = grid.retained_row_len(r).min(grid.cols());
                for col in 0..len {
                    let cell = grid.retained_cell_data(r, col);
                    if cell.flags.contains(CellFlags::WIDE_CONT) || cell.c == '\0' {
                        continue;
                    }
                    if let Some(grapheme) = cell.grapheme.as_deref() {
                        text.push_str(grapheme);
                    } else {
                        text.push(cell.c);
                    }
                    origins.resize(text.len(), (r, col));
                }
                let wrapped = grid.retained_row_wrapped(r);
                r += 1;
                if !wrapped || r >= retained {
                    break;
                }
            }
            let next_row = r;

            let lower = text.to_lowercase();
            // `to_lowercase` pode mudar o comprimento em bytes; só mapeamos
            // offsets diretamente quando ele é preservado, senão caímos para a
            // comparação caractere a caractere.
            if lower.len() == text.len() {
                let mut from = 0;
                while let Some(pos) = lower[from..].find(&needle) {
                    let start = from + pos;
                    let end = start + needle.len();
                    if !self.push_match(grid, &origins, start, end) {
                        break 'outer;
                    }
                    from = start + lower[start..].chars().next().map_or(1, char::len_utf8);
                    if from > lower.len() {
                        break;
                    }
                }
            } else {
                let chars: Vec<(usize, String)> = text
                    .char_indices()
                    .map(|(i, c)| (i, c.to_lowercase().collect::<String>()))
                    .collect();
                let flat: String = chars.iter().map(|(_, s)| s.as_str()).collect();
                // Mapa de byte em `flat` → byte em `text`.
                let mut map = Vec::with_capacity(flat.len() + 1);
                for (i, (orig, s)) in chars.iter().enumerate() {
                    let next = chars.get(i + 1).map_or(text.len(), |(o, _)| *o);
                    for _ in 0..s.len() {
                        map.push((*orig, next));
                    }
                }
                map.push((text.len(), text.len()));
                let mut from = 0;
                while let Some(pos) = flat[from..].find(&needle) {
                    let start = from + pos;
                    let end = start + needle.len();
                    let (s, _) = map[start];
                    let (_, e) = map[end.saturating_sub(1)];
                    if !self.push_match(grid, &origins, s, e) {
                        break 'outer;
                    }
                    from = start + flat[start..].chars().next().map_or(1, char::len_utf8);
                    if from > flat.len() {
                        break;
                    }
                }
            }
            row = next_row.max(row + 1);
        }

        self.current = if self.matches.is_empty() {
            None
        } else if let Some(prev) = previous {
            self.matches
                .iter()
                .position(|m| m.row == prev.row && m.col == prev.col)
                .or(Some(self.matches.len() - 1))
        } else {
            Some(self.matches.len() - 1)
        };
    }

    /// Converte um intervalo de bytes da linha lógica em um ou mais
    /// `SearchMatch` (um por linha física). Retorna `false` ao atingir o
    /// limite de resultados.
    fn push_match(
        &mut self,
        grid: &Grid,
        origins: &[(usize, usize)],
        start: usize,
        end: usize,
    ) -> bool {
        if start >= end || end > origins.len() {
            return true;
        }
        let (first_row, first_col) = origins[start];
        let (last_row, last_col) = origins[end - 1];
        for row in first_row..=last_row {
            let col = if row == first_row { first_col } else { 0 };
            let mut last = if row == last_row {
                last_col
            } else {
                grid.retained_row_len(row).min(grid.cols()).saturating_sub(1)
            };
            // Inclui a segunda metade de um glifo largo no fim do resultado.
            if grid
                .retained_cell_data(row, last)
                .flags
                .contains(CellFlags::WIDE)
                && last + 1 < grid.cols()
            {
                last += 1;
            }
            if last < col {
                continue;
            }
            if self.matches.len() >= MAX_MATCHES {
                return false;
            }
            self.matches.push(SearchMatch {
                row: i64::try_from(row).unwrap_or(i64::MAX),
                col,
                len: last - col + 1,
            });
        }
        true
    }

    pub fn next(&mut self) {
        if self.matches.is_empty() {
            self.current = None;
            return;
        }
        self.current = Some(match self.current {
            Some(i) if i + 1 < self.matches.len() => i + 1,
            _ => 0,
        });
    }

    pub fn prev(&mut self) {
        if self.matches.is_empty() {
            self.current = None;
            return;
        }
        self.current = Some(match self.current {
            Some(0) | None => self.matches.len() - 1,
            Some(i) => i - 1,
        });
    }

    pub fn current_match(&self) -> Option<SearchMatch> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }

    /// Classifica a célula visível em (linha da viewport, coluna).
    pub fn contains_cell(&self, grid: &Grid, vis_row: usize, col: usize) -> Option<MatchKind> {
        if self.matches.is_empty() || vis_row >= grid.rows() || col >= grid.cols() {
            return None;
        }
        let row = grid
            .scrollback_len()
            .saturating_sub(grid.scroll_offset)
            .saturating_add(vis_row);
        let row = i64::try_from(row).unwrap_or(i64::MAX);
        // Os resultados estão ordenados por linha; localiza o primeiro desta linha.
        let first = self.matches.partition_point(|m| m.row < row);
        for (idx, m) in self.matches.iter().enumerate().skip(first) {
            if m.row != row {
                break;
            }
            if col >= m.col && col < m.col + m.len {
                return Some(if self.current == Some(idx) {
                    MatchKind::Current
                } else {
                    MatchKind::Other
                });
            }
        }
        None
    }

    /// Posiciona a viewport de modo que o resultado atual fique visível,
    /// perto do terço superior da tela. Não mexe na viewport sem resultado.
    pub fn scroll_to_current(&self, grid: &mut Grid) {
        let Some(m) = self.current_match() else {
            return;
        };
        let row = usize::try_from(m.row).unwrap_or(0);
        let sb_len = grid.scrollback_len();
        let rows = grid.rows();
        let top = sb_len.saturating_sub(grid.scroll_offset);
        if row >= top && row < top + rows {
            return;
        }
        let margin = rows / 3;
        let desired_top = row.saturating_sub(margin);
        grid.scroll_offset = sb_len.saturating_sub(desired_top).min(sb_len);
    }
}

#[cfg(test)]
mod tests {
    use super::{MatchKind, SearchMatch, SearchState};
    use crate::grid::Grid;
    use crate::parser::ansi::Utf8Parser;

    fn grid_with(cols: usize, rows: usize, text: &str) -> Grid {
        let mut grid = Grid::new(cols, rows, 100);
        let mut parser = Utf8Parser::new();
        parser.feed(text.as_bytes(), &mut grid);
        grid
    }

    fn search(grid: &Grid, query: &str) -> SearchState {
        let mut state = SearchState::new(query.to_string());
        state.recompute(grid);
        state
    }

    #[test]
    fn finds_simple_match() {
        let grid = grid_with(20, 5, "hello world\r\nfoo bar\r\n");
        let state = search(&grid, "world");
        assert_eq!(state.matches, vec![SearchMatch { row: 0, col: 6, len: 5 }]);
        assert_eq!(state.current, Some(0));
    }

    #[test]
    fn match_is_case_insensitive() {
        let grid = grid_with(20, 5, "Hello WORLD\r\n");
        let state = search(&grid, "hello world");
        assert_eq!(state.matches.len(), 1);
        assert_eq!(state.matches[0].col, 0);
        assert_eq!(state.matches[0].len, 11);
        let state = search(&grid, "WoRlD");
        assert_eq!(state.matches.len(), 1);
    }

    #[test]
    fn match_spans_soft_wrap() {
        // 8 colunas: "abcdefgh" na linha 0 e "ijkl" na 1, unidas por soft wrap.
        let grid = grid_with(8, 4, "abcdefghijkl");
        let state = search(&grid, "ghij");
        assert_eq!(
            state.matches,
            vec![
                SearchMatch { row: 0, col: 6, len: 2 },
                SearchMatch { row: 1, col: 0, len: 2 },
            ]
        );
        // Sem soft wrap (quebra explícita) não há união.
        let grid = grid_with(8, 4, "abcdefgh\r\nijkl\r\n");
        let state = search(&grid, "ghij");
        assert!(state.matches.is_empty());
    }

    #[test]
    fn next_and_prev_wrap_around() {
        let grid = grid_with(20, 6, "a1 a2\r\na3\r\nxx\r\n");
        let mut state = search(&grid, "a");
        assert_eq!(state.matches.len(), 3);
        // Começa no último (mais perto do fim).
        assert_eq!(state.current, Some(2));
        state.next();
        assert_eq!(state.current, Some(0));
        state.next();
        assert_eq!(state.current, Some(1));
        state.prev();
        assert_eq!(state.current, Some(0));
        state.prev();
        assert_eq!(state.current, Some(2));
    }

    #[test]
    fn no_match_and_empty_query() {
        let grid = grid_with(20, 5, "hello\r\n");
        let mut state = search(&grid, "zzz");
        assert!(state.matches.is_empty());
        assert_eq!(state.current, None);
        state.next();
        assert_eq!(state.current, None);
        let state = search(&grid, "");
        assert!(state.matches.is_empty());
        assert_eq!(state.current, None);
    }

    #[test]
    fn recompute_keeps_current_after_more_output() {
        let mut grid = grid_with(20, 4, "foo 1\r\nfoo 2\r\n");
        let mut state = search(&grid, "foo");
        assert_eq!(state.matches.len(), 2);
        state.prev();
        assert_eq!(state.current, Some(0));
        let mut parser = Utf8Parser::new();
        parser.feed(b"foo 3\r\nfoo 4\r\n", &mut grid);
        assert!(state.needs_recompute(&grid));
        state.recompute(&grid);
        assert_eq!(state.matches.len(), 4);
        assert_eq!(state.current_match(), Some(SearchMatch { row: 0, col: 0, len: 3 }));
    }

    #[test]
    fn wide_chars_inside_match_count_cells() {
        let grid = grid_with(20, 4, "x日本y\r\n");
        let state = search(&grid, "日本y");
        // 日(2) 本(2) y(1) = 5 células a partir da coluna 1.
        assert_eq!(state.matches, vec![SearchMatch { row: 0, col: 1, len: 5 }]);
        assert_eq!(state.contains_cell(&grid, 0, 0), None);
        assert_eq!(state.contains_cell(&grid, 0, 2), Some(MatchKind::Current));
        assert_eq!(state.contains_cell(&grid, 0, 5), Some(MatchKind::Current));
        assert_eq!(state.contains_cell(&grid, 0, 6), None);
    }

    #[test]
    fn contains_cell_follows_scroll_offset() {
        let mut grid = grid_with(10, 2, "aaa\r\nbbb\r\nccc\r\nddd");
        let state = search(&grid, "bbb");
        assert_eq!(state.matches.len(), 1);
        assert_eq!(grid.scrollback_len(), 2);
        // Na viewport padrão (ccc, ddd) nada está destacado.
        assert_eq!(state.contains_cell(&grid, 0, 0), None);
        state.scroll_to_current(&mut grid);
        assert!(grid.scroll_offset > 0);
        let top = grid.scrollback_len() - grid.scroll_offset;
        let vis = 1 - top;
        assert_eq!(state.contains_cell(&grid, vis, 1), Some(MatchKind::Current));
        // Outro resultado não atual usa `Other`.
        let mut state = search(&grid, "b");
        assert_eq!(state.matches.len(), 3);
        state.next();
        assert_eq!(state.contains_cell(&grid, vis, 0), Some(MatchKind::Current));
        assert_eq!(state.contains_cell(&grid, vis, 1), Some(MatchKind::Other));
    }
}
