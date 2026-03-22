pub mod unix;

pub use unix::Pty;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum PtyError {
    #[error("Failed to open PTY: {0}")]
    OpenPty(#[from] nix::Error),
    #[error("Fork failed: {0}")]
    Fork(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
