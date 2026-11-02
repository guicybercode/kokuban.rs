//! Classification of the launch result and of when the window still
//! forwards input.

pub(super) fn combine_launch_results(
    run_result: Result<(), String>,
    reader_join_result: Result<(), String>,
    finish_result: Result<(), String>,
) -> Result<(), String> {
    run_result.and(reader_join_result).and(finish_result)
}

/// Input stops once the event loop is exiting or the last pane closed and
/// the window only waits for its final frame.
pub(super) fn terminal_accepts_input(exiting: bool, terminal_closed: bool) -> bool {
    !exiting && !terminal_closed
}

#[cfg(test)]
mod tests {
    use super::{combine_launch_results, terminal_accepts_input};

    #[test]
    fn launch_result_preserves_worker_failures_before_late_finish_errors() {
        assert_eq!(
            combine_launch_results(
                Ok(()),
                Err("reader failure".to_string()),
                Err("finish failure".to_string()),
            ),
            Err("reader failure".to_string())
        );
        assert_eq!(
            combine_launch_results(
                Err("event loop failure".to_string()),
                Err("reader failure".to_string()),
                Err("finish failure".to_string()),
            ),
            Err("event loop failure".to_string())
        );
        assert_eq!(
            combine_launch_results(Ok(()), Ok(()), Err("finish failure".to_string())),
            Err("finish failure".to_string())
        );
        assert_eq!(combine_launch_results(Ok(()), Ok(()), Ok(())), Ok(()));
    }

    #[test]
    fn terminal_stops_accepting_input_while_exiting_or_after_the_last_pane_closes() {
        assert!(terminal_accepts_input(false, false));
        assert!(!terminal_accepts_input(true, false));
        assert!(!terminal_accepts_input(false, true));
        assert!(!terminal_accepts_input(true, true));
    }
}
