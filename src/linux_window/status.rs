//! Classification of the reader/writer thread exits and of the launch result.

use crate::terminal_reader::ReaderExit;
use crate::terminal_writer::WriterExit;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReaderStatus {
    Normal,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WriterStatus {
    Normal,
    Failed(String),
}

pub(super) fn classify_reader_exit(exit: &ReaderExit) -> ReaderStatus {
    match exit {
        ReaderExit::Shutdown | ReaderExit::Eof => ReaderStatus::Normal,
        ReaderExit::WaitFailed(error) => ReaderStatus::Failed(format!(
            "terminal reader could not wait for PTY output: {error}"
        )),
        ReaderExit::ReadFailed(error) => ReaderStatus::Failed(format!(
            "terminal reader could not read PTY output: {error}"
        )),
        ReaderExit::ResponseWriteFailed(error) => ReaderStatus::Failed(format!(
            "terminal reader could not write a protocol response: {error}"
        )),
        ReaderExit::GridPoisoned => {
            ReaderStatus::Failed("terminal reader lost access to the terminal grid".to_string())
        }
        ReaderExit::DecoderStalled => {
            ReaderStatus::Failed("terminal decoder stopped making progress".to_string())
        }
        ReaderExit::UnexpectedProtocolEvent => ReaderStatus::Failed(
            "terminal reader received a graphics event in text-only mode".to_string(),
        ),
    }
}

pub(super) fn reader_status_result(status: ReaderStatus) -> Result<(), String> {
    match status {
        ReaderStatus::Normal => Ok(()),
        ReaderStatus::Failed(error) => Err(error),
    }
}

pub(super) fn classify_writer_exit(exit: &WriterExit) -> WriterStatus {
    match exit {
        WriterExit::Shutdown => WriterStatus::Normal,
        WriterExit::ChannelClosed => {
            WriterStatus::Failed("terminal writer lost its input queue".to_string())
        }
        WriterExit::WriteFailed(error) => WriterStatus::Failed(format!(
            "terminal writer could not write keyboard input to the PTY: {error}"
        )),
    }
}

pub(super) fn writer_status_result(status: WriterStatus) -> Result<(), String> {
    match status {
        WriterStatus::Normal => Ok(()),
        WriterStatus::Failed(error) => Err(error),
    }
}

pub(super) fn combine_launch_results(
    run_result: Result<(), String>,
    reader_join_result: Result<(), String>,
    writer_join_result: Result<(), String>,
    finish_result: Result<(), String>,
) -> Result<(), String> {
    run_result
        .and(reader_join_result)
        .and(writer_join_result)
        .and(finish_result)
}

pub(super) fn terminal_accepts_input(exiting: bool, reader_status: Option<&ReaderStatus>) -> bool {
    !exiting && reader_status.is_none()
}

#[cfg(test)]
mod tests {
    use super::{
        classify_reader_exit, classify_writer_exit, combine_launch_results, terminal_accepts_input,
        ReaderStatus, WriterStatus,
    };
    use crate::terminal_reader::ReaderExit;
    use crate::terminal_writer::WriterExit;
    use std::io;

    #[test]
    fn shutdown_and_eof_are_normal_reader_exits() {
        assert_eq!(
            classify_reader_exit(&ReaderExit::Shutdown),
            ReaderStatus::Normal
        );
        assert_eq!(classify_reader_exit(&ReaderExit::Eof), ReaderStatus::Normal);
    }

    #[test]
    fn reader_failures_include_actionable_context() {
        let failures = [
            (
                ReaderExit::WaitFailed(io::Error::from_raw_os_error(5)),
                "wait for PTY output",
            ),
            (
                ReaderExit::ReadFailed(io::Error::from_raw_os_error(5)),
                "read PTY output",
            ),
            (
                ReaderExit::ResponseWriteFailed(io::Error::from_raw_os_error(32)),
                "write a protocol response",
            ),
            (ReaderExit::GridPoisoned, "terminal grid"),
            (ReaderExit::DecoderStalled, "stopped making progress"),
            (
                ReaderExit::UnexpectedProtocolEvent,
                "graphics event in text-only mode",
            ),
        ];

        for (exit, expected_context) in failures {
            let ReaderStatus::Failed(message) = classify_reader_exit(&exit) else {
                panic!("reader failure was classified as a normal exit");
            };
            assert!(
                message.contains(expected_context),
                "missing `{expected_context}` in `{message}`"
            );
        }
    }

    #[test]
    fn writer_shutdown_is_normal_and_failures_include_actionable_context() {
        assert_eq!(
            classify_writer_exit(&WriterExit::Shutdown),
            WriterStatus::Normal
        );

        let channel_status = classify_writer_exit(&WriterExit::ChannelClosed);
        let WriterStatus::Failed(channel_message) = channel_status else {
            panic!("closed writer queue should be a failure");
        };
        assert!(channel_message.contains("input queue"));

        let write_status =
            classify_writer_exit(&WriterExit::WriteFailed(io::Error::from_raw_os_error(5)));
        let WriterStatus::Failed(write_message) = write_status else {
            panic!("PTY write error should be a failure");
        };
        assert!(write_message.contains("write keyboard input to the PTY"));
        assert!(write_message.contains("5"));
    }

    #[test]
    fn launch_result_preserves_worker_failures_before_late_finish_errors() {
        assert_eq!(
            combine_launch_results(
                Ok(()),
                Ok(()),
                Err("native writer failure".to_string()),
                Err("input queue disconnected".to_string()),
            ),
            Err("native writer failure".to_string())
        );
        assert_eq!(
            combine_launch_results(
                Ok(()),
                Err("reader failure".to_string()),
                Err("writer failure".to_string()),
                Err("finish failure".to_string()),
            ),
            Err("reader failure".to_string())
        );
        assert_eq!(
            combine_launch_results(
                Err("event loop failure".to_string()),
                Err("reader failure".to_string()),
                Err("writer failure".to_string()),
                Err("finish failure".to_string()),
            ),
            Err("event loop failure".to_string())
        );
        assert_eq!(
            combine_launch_results(Ok(()), Ok(()), Ok(()), Err("finish failure".to_string()),),
            Err("finish failure".to_string())
        );
    }

    #[test]
    fn terminal_stops_accepting_input_while_exiting_or_after_the_reader_exits() {
        assert!(terminal_accepts_input(false, None));
        assert!(!terminal_accepts_input(true, None));
        assert!(!terminal_accepts_input(false, Some(&ReaderStatus::Normal)));
        assert!(!terminal_accepts_input(
            false,
            Some(&ReaderStatus::Failed("read failed".to_string()))
        ));
        assert!(!terminal_accepts_input(true, Some(&ReaderStatus::Normal)));
    }
}
