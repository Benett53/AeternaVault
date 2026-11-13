//! Error types of the backup engine.
//!
//! Messages here are written for the log file (English, precise). The GUI
//! translates them into calm, localized sentences via
//! [`crate::i18n::Lang::error_message`].

use std::io;
use std::path::PathBuf;

pub type EngineResult<T> = Result<T, EngineError>;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("no backup sources are selected")]
    NoSources,

    #[error("no destination folder is set")]
    NoDestination,

    #[error("the destination {0} is not reachable")]
    DestinationUnavailable(PathBuf),

    #[error("none of the folders to back up could be read: {0}")]
    SourcesUnavailable(String),

    #[error("the source {source_path} lies inside the destination {destination}")]
    SourceInsideDestination {
        source_path: PathBuf,
        destination: PathBuf,
    },

    #[error(
        "not enough free space at the destination ({needed} bytes needed, {available} available)"
    )]
    NotEnoughSpace { needed: u64, available: u64 },

    #[error("the backup {0} could not be found")]
    SnapshotNotFound(String),

    #[error("the backup index {path} could not be read: {message}")]
    Manifest { path: PathBuf, message: String },

    #[error("the operation was cancelled")]
    Cancelled,

    #[error("the encrypted backups are locked; a passphrase is needed")]
    Locked,

    #[error("encryption is turned on but no encrypted vault exists at the destination")]
    EncryptionNotSetUp,

    #[error("another backup is already running for this destination")]
    AlreadyRunning,

    #[error("{0}")]
    Crypto(#[from] crate::engine::crypto::CryptoError),

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
}

impl EngineError {
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// Short helper to attach a path to an I/O error.
pub trait IoContext<T> {
    fn at(self, what: &str, path: &std::path::Path) -> EngineResult<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn at(self, what: &str, path: &std::path::Path) -> EngineResult<T> {
        self.map_err(|e| EngineError::io(format!("{what} {}", path.display()), e))
    }
}
