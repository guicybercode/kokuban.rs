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
    use crate::parser::sixel::SixelImage;

    fn decoder(graphics_support: GraphicsSupport) -> (TerminalDecoder, Grid) {
        (TerminalDecoder::new(graphics_support), Grid::new(12, 4, 32))
    }

    fn advance_past_sixel(grid: &mut Grid, image: &SixelImage) {
        let cell_height = usize::from(grid.cell_pixel_height);
        let display_rows = (image.height as usize).div_ceil(cell_height);
        for _ in 0..display_rows {
            grid.newline();
        }
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
        assert!(!grid.has_pending_terminal_events());
    }

    #[test]
    fn sixel_event_precedes_trailing_text_and_captures_its_cursor() {
        let (mut decoder, mut grid) = decoder(GraphicsSupport {
            kitty: false,
            sixel: true,
        });
        let input = b"\x1b[2;3H\x1bPq~\x1b\\X";

        let first = decoder.feed_until_event(input, &mut grid);

        assert_eq!(first.consumed, input.len() - 1);
        assert_eq!(grid.buffer.cell(1, 2).c, ' ');
        let mut events = first.events.into_iter();
        let (image, cursor_row, cursor_col) = match events.next() {
            Some(TerminalEvent::SixelGraphics {
                image,
                cursor_row,
                cursor_col,
            }) => (image, cursor_row, cursor_col),
            _ => panic!("expected one Sixel event"),
        };
        assert!(events.next().is_none());
        assert_eq!((cursor_row, cursor_col), (1, 2));
        assert_eq!((image.width, image.height), (1, 6));

        advance_past_sixel(&mut grid, &image);
        let second = decoder.feed_until_event(&input[first.consumed..], &mut grid);

        assert_eq!(second.consumed, 1);
        assert!(second.events.is_empty());
        assert_eq!(grid.buffer.cell(2, 2).c, 'X');
    }

    #[test]
    fn consecutive_sixel_events_observe_each_previous_cursor_advance() {
        const FIRST: &[u8] = b"\x1bPq~\x1b\\";
        const SECOND: &[u8] = b"\x1bPq~\x1b\\";
        let (mut decoder, mut grid) = decoder(GraphicsSupport {
            kitty: false,
            sixel: true,
        });
        let input = [b"\x1b[2;3H".as_slice(), FIRST, SECOND, b"X"].concat();

        let first = decoder.feed_until_event(&input, &mut grid);
        let first_image = match first.events.as_slice() {
            [TerminalEvent::SixelGraphics {
                image,
                cursor_row: 1,
                cursor_col: 2,
            }] => image,
            _ => panic!("expected the first Sixel event"),
        };
        advance_past_sixel(&mut grid, first_image);

        let second = decoder.feed_until_event(&input[first.consumed..], &mut grid);
        assert_eq!(second.consumed, SECOND.len());
        let second_image = match second.events.as_slice() {
            [TerminalEvent::SixelGraphics {
                image,
                cursor_row: 2,
                cursor_col: 2,
            }] => image,
            _ => panic!("expected the second Sixel event after the first advance"),
        };
        advance_past_sixel(&mut grid, second_image);

        let text_offset = first.consumed + second.consumed;
        let text = decoder.feed_until_event(&input[text_offset..], &mut grid);
        assert_eq!(text.consumed, 1);
        assert!(text.events.is_empty());
        assert_eq!(grid.buffer.cell(3, 2).c, 'X');
    }

    #[test]
    fn sixel_advance_precedes_following_response_and_kitty_snapshot() {
        const SIXEL: &[u8] = b"\x1bPq~\x1b\\";
        const CURSOR_QUERY: &[u8] = b"\x1b[6n";
        const KITTY: &[u8] = b"\x1b_Ga=d,d=c\x1b\\";
        let (mut decoder, mut grid) = decoder(GraphicsSupport {
            kitty: true,
            sixel: true,
        });
        let input = [b"\x1b[2;3H".as_slice(), SIXEL, CURSOR_QUERY, KITTY].concat();

        let sixel = decoder.feed_until_event(&input, &mut grid);
        let image = match sixel.events.as_slice() {
            [TerminalEvent::SixelGraphics { image, .. }] => image,
            _ => panic!("expected the Sixel event first"),
        };
        advance_past_sixel(&mut grid, image);

        let response = decoder.feed_until_event(&input[sixel.consumed..], &mut grid);
        assert!(matches!(
            response.events.as_slice(),
            [TerminalEvent::Response(response)] if response == b"\x1b[3;3R"
        ));

        let kitty_offset = sixel.consumed + response.consumed;
        let kitty = decoder.feed_until_event(&input[kitty_offset..], &mut grid);
        assert_eq!(kitty.consumed, KITTY.len());
        assert!(matches!(
            kitty.events.as_slice(),
            [TerminalEvent::KittyGraphics {
                cursor_row: 2,
                cursor_col: 2,
                ..
            }]
        ));
    }

    #[test]
    fn title_revisions_survive_a_sixel_event_boundary() {
        const BEFORE: &[u8] = b"\x1b]2;before\x1b\\";
        const SIXEL: &[u8] = b"\x1bPq~\x1b\\";
        const AFTER: &[u8] = b"\x1b]2;after\x1b\\";
        let (mut decoder, mut grid) = decoder(GraphicsSupport {
            kitty: false,
            sixel: true,
        });
        let input = [BEFORE, SIXEL, AFTER].concat();

        let first = decoder.feed_until_event(&input, &mut grid);

        assert_eq!(first.consumed, BEFORE.len() + SIXEL.len());
        assert!(matches!(
            first.events.as_slice(),
            [TerminalEvent::SixelGraphics { .. }]
        ));
        assert_eq!(grid.title(), "before");
        assert_eq!(grid.title_revision(), 1);

        let second = decoder.feed_until_event(&input[first.consumed..], &mut grid);

        assert_eq!(second.consumed, AFTER.len());
        assert!(second.events.is_empty());
        assert_eq!(grid.title(), "after");
        assert_eq!(grid.title_revision(), 2);
    }
}
