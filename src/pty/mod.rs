pub mod unix;

pub(crate) use unix::CancellableWriteOutcome;
pub use unix::Pty;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum PtyError {
    #[error("Failed to open PTY: {0}")]
    OpenPty(#[from] nix::Error),
    #[error("Fork failed: {0}")]
    Fork(String),
    #[error("Invalid shell environment: {0}")]
    InvalidShell(String),
    #[error("Child process failed during {0}")]
    ChildSetup(&'static str),
    #[error("Timed out while starting the child process")]
    ChildStartupTimeout,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
