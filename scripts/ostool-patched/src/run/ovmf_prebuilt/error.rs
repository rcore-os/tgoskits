use std::{io, path::PathBuf};

/// Cache or fetch error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Hash of the downloaded file does not match the expected value.
    #[error("file hash {actual} does not match expected hash {expected}")]
    HashMismatch {
        /// Expected hash.
        expected: String,
        /// Actual hash.
        actual: String,
    },

    /// Failed to read the hash file.
    #[error("failed to read hash file: {path}")]
    HashRead {
        /// Path of the hash file.
        path: PathBuf,
        /// Source error.
        #[source]
        source: io::Error,
    },

    /// Failed to write the hash file.
    #[error("failed to write hash file: {path}")]
    HashWrite {
        /// Path of the hash file.
        path: PathBuf,
        /// Source error.
        #[source]
        source: io::Error,
    },

    /// Remote request failed.
    #[error("remote request failed")]
    Request(#[source] Box<ureq::Error>),

    /// Download failed.
    #[error("download failed")]
    Download(#[source] io::Error),

    /// Tarball decompression failed.
    #[error("tarball decompression failed")]
    Decompress(#[source] lzma_rs::error::Error),

    /// Failed to remove an old cache directory.
    #[error("failed to remove cache directory: {path}")]
    RemoveDir {
        /// Directory that could not be removed.
        path: PathBuf,
        /// Source error.
        #[source]
        source: io::Error,
    },

    /// Failed to read archive entries.
    #[error("failed to read archive entries")]
    ArchiveEntries(#[source] io::Error),

    /// Failed to read a specific archive entry.
    #[error("failed to read archive entry")]
    ArchiveEntry(#[source] io::Error),

    /// Failed to resolve the path of an archive entry.
    #[error("failed to resolve archive entry path")]
    ArchiveEntryPath(#[source] io::Error),

    /// Failed to create an output directory while unpacking.
    #[error("failed to create directory: {path}")]
    CreateDir {
        /// Directory path.
        path: PathBuf,
        /// Source error.
        #[source]
        source: io::Error,
    },

    /// Failed to unpack an archive entry to disk.
    #[error("failed to unpack archive entry to: {path}")]
    Unpack {
        /// Output path.
        path: PathBuf,
        /// Source error.
        #[source]
        source: io::Error,
    },
}
