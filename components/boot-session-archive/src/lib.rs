//! A small, bounded wire format for files that exist only for one OS boot.
//!
//! The host encoder places the archive in a per-run DTB. The target parser is
//! allocation-free so an OS can validate the complete archive before exposing
//! any file to userspace.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::str;

use thiserror::Error;

/// `/chosen` property carrying the archive in a per-run DTB.
pub const FDT_PROPERTY_NAME: &str = "starry,session-archive";
/// Guest tmpfs directory where StarryOS materializes archive entries.
pub const GUEST_ROOT: &str = "/tmp/starry-session";

const MAGIC: &[u8; 8] = b"STARRYSA";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 16;
const ENTRY_HEADER_LEN: usize = 8;
const MAX_FILE_COUNT: usize = 128;
const MAX_PATH_LEN: usize = 255;
const MAX_FILE_SIZE: usize = 8 * 1024 * 1024;
const MAX_ARCHIVE_SIZE: usize = 16 * 1024 * 1024;

/// A validated file borrowed from an archive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveEntry<'a> {
    path: &'a str,
    mode: u16,
    contents: &'a [u8],
}

impl<'a> ArchiveEntry<'a> {
    /// Normalized path relative to [`GUEST_ROOT`].
    pub const fn path(&self) -> &'a str {
        self.path
    }

    /// POSIX permission bits, without a file-type field.
    pub const fn mode(&self) -> u16 {
        self.mode
    }

    /// Complete file contents.
    pub const fn contents(&self) -> &'a [u8] {
        self.contents
    }
}

/// An input file for the host-side encoder.
#[cfg(feature = "alloc")]
#[derive(Clone, Copy, Debug)]
pub struct ArchiveInput<'a> {
    /// Normalized path relative to [`GUEST_ROOT`].
    pub path: &'a str,
    /// POSIX permission bits, without a file-type field.
    pub mode: u16,
    /// Complete file contents.
    pub contents: &'a [u8],
}

/// Errors returned when an archive violates the bounded wire contract.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ArchiveError {
    /// The fixed header is missing or truncated.
    #[error("boot session archive is truncated")]
    Truncated,
    /// The magic or format version is not supported.
    #[error("boot session archive has an unsupported format")]
    UnsupportedFormat,
    /// A declared length does not match the supplied bytes.
    #[error("boot session archive has an invalid declared length")]
    InvalidLength,
    /// The archive exceeds a fixed resource bound.
    #[error("boot session archive exceeds a resource bound")]
    ResourceLimit,
    /// A path is not normalized, relative UTF-8.
    #[error("boot session archive contains an invalid path")]
    InvalidPath,
    /// Two entries would materialize the same path.
    #[error("boot session archive contains a duplicate path")]
    DuplicatePath,
    /// Permission bits contain fields other than the low POSIX mode bits.
    #[error("boot session archive contains an invalid file mode")]
    InvalidMode,
}

/// A completely validated archive.
#[derive(Clone, Copy, Debug)]
pub struct Archive<'a> {
    bytes: &'a [u8],
    entry_count: usize,
}

impl<'a> Archive<'a> {
    /// Validates the full archive before any entry can be observed.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ArchiveError> {
        if bytes.len() < HEADER_LEN {
            return Err(ArchiveError::Truncated);
        }
        if bytes.len() > MAX_ARCHIVE_SIZE {
            return Err(ArchiveError::ResourceLimit);
        }
        if &bytes[..MAGIC.len()] != MAGIC
            || read_u16(bytes, 8)? != VERSION
            || read_u16(bytes, 10)? as usize > MAX_FILE_COUNT
        {
            return Err(ArchiveError::UnsupportedFormat);
        }
        if read_u32(bytes, 12)? as usize != bytes.len() {
            return Err(ArchiveError::InvalidLength);
        }

        let entry_count = read_u16(bytes, 10)? as usize;
        let mut cursor = HEADER_LEN;
        for _ in 0..entry_count {
            let entry_start = cursor;
            let (entry, next) = parse_entry(bytes, cursor)?;
            if path_seen_before(bytes, entry_start, entry.path())? {
                return Err(ArchiveError::DuplicatePath);
            }
            cursor = next;
        }
        if cursor != bytes.len() {
            return Err(ArchiveError::InvalidLength);
        }
        Ok(Self { bytes, entry_count })
    }

    /// Returns the number of files in the archive.
    pub const fn len(&self) -> usize {
        self.entry_count
    }

    /// Returns whether the archive contains no files.
    pub const fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// Iterates over entries after whole-archive validation has succeeded.
    pub fn entries(&self) -> ArchiveEntries<'a> {
        ArchiveEntries {
            bytes: self.bytes,
            cursor: HEADER_LEN,
            remaining: self.entry_count,
        }
    }
}

/// Iterator over already validated archive entries.
pub struct ArchiveEntries<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: usize,
}

impl<'a> Iterator for ArchiveEntries<'a> {
    type Item = ArchiveEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let (entry, next) = parse_entry(self.bytes, self.cursor)
            .expect("Archive::parse validated every entry boundary");
        self.cursor = next;
        self.remaining -= 1;
        Some(entry)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for ArchiveEntries<'_> {}

/// Encodes a complete host-side archive.
#[cfg(feature = "alloc")]
pub fn encode<'a>(
    files: impl IntoIterator<Item = ArchiveInput<'a>>,
) -> Result<Vec<u8>, ArchiveError> {
    let mut bytes = Vec::from(&MAGIC[..]);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    let mut count = 0_usize;

    for file in files {
        validate_path(file.path)?;
        validate_mode(file.mode)?;
        if file.contents.len() > MAX_FILE_SIZE {
            return Err(ArchiveError::ResourceLimit);
        }
        if archive_contains_path(&bytes, count, file.path)? {
            return Err(ArchiveError::DuplicatePath);
        }
        count = count.checked_add(1).ok_or(ArchiveError::ResourceLimit)?;
        if count > MAX_FILE_COUNT {
            return Err(ArchiveError::ResourceLimit);
        }
        let path_len = u16::try_from(file.path.len()).map_err(|_| ArchiveError::ResourceLimit)?;
        let data_len =
            u32::try_from(file.contents.len()).map_err(|_| ArchiveError::ResourceLimit)?;
        let new_len = bytes
            .len()
            .checked_add(ENTRY_HEADER_LEN)
            .and_then(|len| len.checked_add(file.path.len()))
            .and_then(|len| len.checked_add(file.contents.len()))
            .ok_or(ArchiveError::ResourceLimit)?;
        if new_len > MAX_ARCHIVE_SIZE {
            return Err(ArchiveError::ResourceLimit);
        }
        bytes.extend_from_slice(&path_len.to_le_bytes());
        bytes.extend_from_slice(&file.mode.to_le_bytes());
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend_from_slice(file.path.as_bytes());
        bytes.extend_from_slice(file.contents);
    }

    bytes[10..12].copy_from_slice(&(count as u16).to_le_bytes());
    let archive_len = bytes.len() as u32;
    bytes[12..16].copy_from_slice(&archive_len.to_le_bytes());
    Ok(bytes)
}

fn parse_entry(bytes: &[u8], cursor: usize) -> Result<(ArchiveEntry<'_>, usize), ArchiveError> {
    let header_end = cursor
        .checked_add(ENTRY_HEADER_LEN)
        .ok_or(ArchiveError::InvalidLength)?;
    if header_end > bytes.len() {
        return Err(ArchiveError::Truncated);
    }
    let path_len = read_u16(bytes, cursor)? as usize;
    let mode = read_u16(bytes, cursor + 2)?;
    let data_len = read_u32(bytes, cursor + 4)? as usize;
    if path_len == 0 || path_len > MAX_PATH_LEN || data_len > MAX_FILE_SIZE {
        return Err(ArchiveError::ResourceLimit);
    }
    validate_mode(mode)?;
    let path_end = header_end
        .checked_add(path_len)
        .ok_or(ArchiveError::InvalidLength)?;
    let data_end = path_end
        .checked_add(data_len)
        .ok_or(ArchiveError::InvalidLength)?;
    if data_end > bytes.len() {
        return Err(ArchiveError::Truncated);
    }
    let path =
        str::from_utf8(&bytes[header_end..path_end]).map_err(|_| ArchiveError::InvalidPath)?;
    validate_path(path)?;
    Ok((
        ArchiveEntry {
            path,
            mode,
            contents: &bytes[path_end..data_end],
        },
        data_end,
    ))
}

fn validate_path(path: &str) -> Result<(), ArchiveError> {
    if path.is_empty()
        || path.len() > MAX_PATH_LEN
        || path.starts_with('/')
        || path.ends_with('/')
        || path.as_bytes().contains(&0)
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(ArchiveError::InvalidPath);
    }
    Ok(())
}

fn validate_mode(mode: u16) -> Result<(), ArchiveError> {
    if mode & !0o777 != 0 {
        return Err(ArchiveError::InvalidMode);
    }
    Ok(())
}

fn path_seen_before(bytes: &[u8], end: usize, needle: &str) -> Result<bool, ArchiveError> {
    let mut cursor = HEADER_LEN;
    while cursor < end {
        let (entry, next) = parse_entry(bytes, cursor)?;
        if entry.path() == needle {
            return Ok(true);
        }
        cursor = next;
    }
    Ok(false)
}

#[cfg(feature = "alloc")]
fn archive_contains_path(bytes: &[u8], count: usize, needle: &str) -> Result<bool, ArchiveError> {
    let mut cursor = HEADER_LEN;
    for _ in 0..count {
        let (entry, next) = parse_entry(bytes, cursor)?;
        if entry.path() == needle {
            return Ok(true);
        }
        cursor = next;
    }
    Ok(false)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ArchiveError> {
    let raw: [u8; 2] = bytes
        .get(offset..offset + 2)
        .ok_or(ArchiveError::Truncated)?
        .try_into()
        .expect("slice length was checked");
    Ok(u16::from_le_bytes(raw))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ArchiveError> {
    let raw: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or(ArchiveError::Truncated)?
        .try_into()
        .expect("slice length was checked");
    Ok(u32::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{Archive, ArchiveError, ArchiveInput, encode};

    #[test]
    fn archive_round_trip_preserves_paths_modes_and_contents() {
        let bytes = encode([
            ArchiveInput {
                path: "bin/helper",
                mode: 0o700,
                contents: b"helper",
            },
            ArchiveInput {
                path: "credentials/pmk",
                mode: 0o600,
                contents: &[0x5a; 32],
            },
        ])
        .unwrap();
        let archive = Archive::parse(&bytes).unwrap();
        let entries = archive.entries().collect::<Vec<_>>();

        assert_eq!(archive.len(), 2);
        assert_eq!(entries[0].path(), "bin/helper");
        assert_eq!(entries[0].mode(), 0o700);
        assert_eq!(entries[0].contents(), b"helper");
        assert_eq!(entries[1].path(), "credentials/pmk");
        assert_eq!(entries[1].contents(), &[0x5a; 32]);
    }

    #[test]
    fn parser_rejects_truncation_trailing_bytes_and_path_escape() {
        let bytes = encode([ArchiveInput {
            path: "file",
            mode: 0o600,
            contents: b"value",
        }])
        .unwrap();
        assert_eq!(
            Archive::parse(&bytes[..bytes.len() - 1]).unwrap_err(),
            ArchiveError::InvalidLength
        );

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            Archive::parse(&trailing).unwrap_err(),
            ArchiveError::InvalidLength
        );
        assert_eq!(
            encode([ArchiveInput {
                path: "../credential",
                mode: 0o600,
                contents: b"value",
            }])
            .unwrap_err(),
            ArchiveError::InvalidPath
        );
    }

    #[test]
    fn encoder_rejects_duplicate_paths_and_non_permission_mode_bits() {
        assert_eq!(
            encode([
                ArchiveInput {
                    path: "same",
                    mode: 0o600,
                    contents: b"first",
                },
                ArchiveInput {
                    path: "same",
                    mode: 0o600,
                    contents: b"second",
                },
            ])
            .unwrap_err(),
            ArchiveError::DuplicatePath
        );
        assert_eq!(
            encode([ArchiveInput {
                path: "file",
                mode: 0o10_600,
                contents: b"value",
            }])
            .unwrap_err(),
            ArchiveError::InvalidMode
        );
    }
}
