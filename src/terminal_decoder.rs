use crate::grid::{Grid, TerminalEvent};
use crate::parser::ansi::{GraphicsSupport, Utf8Parser};

/// Bytes consumed and ordered protocol events produced by one decode step.
#[must_use]
pub(crate) struct DecodeStep {
    pub(crate) consumed: usize,
    pub(crate) events: Vec<TerminalEvent>,
}

/// Stateful terminal byte decoder shared by platform-specific runtimes.
pub(crate) struct TerminalDecoder {
    parser: Utf8Parser,
}

impl TerminalDecoder {
    pub(crate) fn new(graphics_support: GraphicsSupport) -> Self {
        Self {
            parser: Utf8Parser::with_graphics_support(graphics_support),
        }
    }

    /// Decode through the first protocol event so callers can handle it before
    /// applying any trailing bytes to the grid.
    pub(crate) fn feed_until_event(&mut self, input: &[u8], grid: &mut Grid) -> DecodeStep {
        let consumed = self.parser.feed_until_terminal_event(input, grid);
        debug_assert!(input.is_empty() || consumed > 0);

        DecodeStep {
            consumed,
            events: grid.drain_terminal_events(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalDecoder;
    use crate::grid::cell::Color;
    use crate::grid::{Grid, TerminalEvent};
    use crate::parser::ansi::GraphicsSupport;

    fn decoder(graphics_support: GraphicsSupport) -> (TerminalDecoder, Grid) {
        (TerminalDecoder::new(graphics_support), Grid::new(12, 4, 32))
    }

    #[test]
    fn preserves_parser_state_across_reads() {
        let (mut decoder, mut grid) = decoder(GraphicsSupport {
            kitty: false,
            sixel: false,
        });

        let first = decoder.feed_until_event(b"\x1b[31m\xe6\x97", &mut grid);
        let second = decoder.feed_until_event(b"\xa5", &mut grid);

        assert_eq!(first.consumed, 7);
        assert!(first.events.is_empty());
        assert_eq!(second.consumed, 1);
        assert!(second.events.is_empty());
        assert_eq!(grid.buffer.cell(0, 0).c, '日');
        assert_eq!(grid.buffer.cell(0, 0).fg, Color::Indexed(1));
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 2));
    }

    #[test]
    fn stops_at_an_event_before_decoding_trailing_input() {
        const CURSOR_QUERY: &[u8] = b"\x1b[6n";
        let (mut decoder, mut grid) = decoder(GraphicsSupport {
            kitty: false,
            sixel: false,
        });
        let input = [b"A".as_slice(), CURSOR_QUERY, b"B"].concat();

        let first = decoder.feed_until_event(&input, &mut grid);

        assert_eq!(first.consumed, 1 + CURSOR_QUERY.len());
        assert!(matches!(
            first.events.as_slice(),
            [TerminalEvent::Response(response)] if response == b"\x1b[1;2R"
        ));
        assert!(!grid.has_pending_terminal_events());
        assert_eq!(grid.buffer.cell(0, 0).c, 'A');
        assert_eq!(grid.buffer.cell(0, 1).c, ' ');

        grid.cursor_row = 1;
        grid.cursor_col = 3;
        let second = decoder.feed_until_event(&input[first.consumed..], &mut grid);

        assert_eq!(second.consumed, 1);
        assert!(second.events.is_empty());
        assert_eq!(grid.buffer.cell(1, 3).c, 'B');
    }

    #[test]
    fn disabled_graphics_do_not_queue_images_or_commands() {
        let (mut decoder, mut grid) = decoder(GraphicsSupport {
            kitty: false,
            sixel: false,
        });
        let input = b"\x1b_Ga=d,d=a\x1b\\\x1bPq~\x1b\\\x1b[c";

        let step = decoder.feed_until_event(input, &mut grid);

        assert_eq!(step.consumed, input.len());
        assert!(matches!(
            step.events.as_slice(),
            [TerminalEvent::Response(response)] if response == b"\x1b[?62;22c"
        ));
        assert!(grid.drain_sixel_images().is_empty());
        assert!(!grid.has_pending_terminal_events());
    }
}
