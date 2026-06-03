// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Host filesystem APIs for AxVisor.

extern crate alloc;

use alloc::{string::String, vec::Vec};

use ax_errno::{AxResult, ax_err_type};

/// File type used by the host filesystem abstraction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileType {
    File,
    Dir,
    Symlink,
    Other,
}

impl FileType {
    /// Returns true if this entry represents a directory.
    pub const fn is_dir(self) -> bool {
        matches!(self, Self::Dir)
    }

    /// Returns true if this entry represents a regular file.
    pub const fn is_file(self) -> bool {
        matches!(self, Self::File)
    }

    /// Returns true if this entry represents a symbolic link.
    pub const fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }
}

/// File metadata returned by the host filesystem abstraction.
#[derive(Clone, Debug)]
pub struct Metadata {
    len: u64,
    file_type: FileType,
    perm_mode: u32,
}

impl Metadata {
    /// Creates new metadata.
    pub const fn new(len: u64, file_type: FileType, perm_mode: u32) -> Self {
        Self {
            len,
            file_type,
            perm_mode,
        }
    }

    /// Returns the file size in bytes.
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Returns true if the file size is zero bytes.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the file type.
    pub const fn file_type(&self) -> FileType {
        self.file_type
    }

    /// Returns the raw permission/mode bits.
    pub const fn permissions_mode(&self) -> u32 {
        self.perm_mode
    }

    /// Returns true if this metadata describes a directory.
    pub const fn is_dir(&self) -> bool {
        self.file_type.is_dir()
    }

    /// Returns true if this metadata describes a regular file.
    pub const fn is_file(&self) -> bool {
        self.file_type.is_file()
    }
}

/// A directory entry returned by host directory iteration.
#[derive(Clone, Debug)]
pub struct DirEntry {
    file_name: String,
    path: String,
    file_type: FileType,
}

impl DirEntry {
    /// Creates a new directory entry.
    pub fn new(file_name: String, path: String, file_type: FileType) -> Self {
        Self {
            file_name,
            path,
            file_type,
        }
    }

    /// Returns the entry file name.
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    /// Returns the full host path of the entry.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the entry file type.
    pub const fn file_type(&self) -> FileType {
        self.file_type
    }
}

/// An opaque file handle.
#[derive(Debug)]
pub struct File {
    raw: usize,
}

impl File {
    /// Opens a host file for reading.
    pub fn open(path: &str) -> AxResult<Self> {
        Ok(Self {
            raw: open_file(path)?,
        })
    }

    /// Creates or truncates a host file for writing.
    pub fn create(path: &str) -> AxResult<Self> {
        Ok(Self {
            raw: create_file(path)?,
        })
    }

    /// Returns the raw host-provided handle.
    pub const fn as_raw(&self) -> usize {
        self.raw
    }

    /// Returns metadata for this file handle.
    pub fn metadata(&self) -> AxResult<Metadata> {
        file_metadata(self.raw)
    }

    /// Reads bytes into `buf`.
    pub fn read(&mut self, buf: &mut [u8]) -> AxResult<usize> {
        file_read(self.raw, buf)
    }

    /// Reads exactly `buf.len()` bytes.
    pub fn read_exact(&mut self, mut buf: &mut [u8]) -> AxResult<()> {
        while !buf.is_empty() {
            let n = self.read(buf)?;
            if n == 0 {
                return Err(ax_err_type!(Io, "unexpected EOF"));
            }
            buf = &mut buf[n..];
        }
        Ok(())
    }

    /// Writes bytes from `buf`.
    pub fn write(&mut self, buf: &[u8]) -> AxResult<usize> {
        file_write(self.raw, buf)
    }

    /// Writes all bytes from `buf`.
    pub fn write_all(&mut self, mut buf: &[u8]) -> AxResult<()> {
        while !buf.is_empty() {
            let n = self.write(buf)?;
            if n == 0 {
                return Err(ax_err_type!(Io, "short write"));
            }
            buf = &buf[n..];
        }
        Ok(())
    }

    /// Flushes pending output for this file handle.
    pub fn flush(&mut self) -> AxResult<()> {
        file_flush(self.raw)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        close_file(self.raw);
    }
}

impl core::fmt::Write for File {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_all(s.as_bytes()).map_err(|_| core::fmt::Error)
    }
}

/// A host directory iterator handle.
#[derive(Debug)]
pub struct ReadDir {
    entries: alloc::vec::IntoIter<DirEntry>,
}

impl ReadDir {
    fn new(entries: Vec<DirEntry>) -> Self {
        Self {
            entries: entries.into_iter(),
        }
    }

    /// Returns the next directory entry, or `None` when exhausted.
    pub fn next_entry(&mut self) -> AxResult<Option<DirEntry>> {
        Ok(self.entries.next())
    }
}

impl Iterator for ReadDir {
    type Item = AxResult<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_entry() {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => None,
            Err(err) => Some(Err(err)),
        }
    }
}

/// Opens a host directory iterator.
pub fn read_dir(path: &str) -> AxResult<ReadDir> {
    Ok(ReadDir::new(fs_read_dir(path)?))
}

/// Reads a whole host file into a string.
pub fn read_to_string(path: &str) -> AxResult<String> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    if let Ok(metadata) = file.metadata() {
        bytes.reserve(metadata.len() as usize);
    }

    let mut buf = [0; 4096];
    loop {
        let read_len = file.read(&mut buf)?;
        if read_len == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..read_len]);
    }

    String::from_utf8(bytes).map_err(|_| ax_err_type!(InvalidData, "file is not valid UTF-8"))
}

/// Returns filesystem metadata for `path`.
pub fn metadata(path: &str) -> AxResult<Metadata> {
    path_metadata(path)
}

/// Creates a directory.
pub fn create_dir(path: &str) -> AxResult<()> {
    fs_create_dir(path)
}

/// Creates a directory and all missing parent directories.
pub fn create_dir_all(path: &str) -> AxResult<()> {
    if path.is_empty() {
        return Err(ax_err_type!(InvalidInput, "empty path"));
    }

    let mut current = String::new();
    if path.starts_with('/') {
        current.push('/');
    }

    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if !current.ends_with('/') && !current.is_empty() {
            current.push('/');
        }
        current.push_str(component);

        match create_dir(&current) {
            Ok(()) => {}
            Err(err)
                if matches!(
                    ax_errno::AxErrorKind::try_from(err.canonicalize()),
                    Ok(ax_errno::AxErrorKind::AlreadyExists)
                ) =>
            {
                if !metadata(&current)?.is_dir() {
                    return Err(err);
                }
            }
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

/// Removes an empty directory.
pub fn remove_dir(path: &str) -> AxResult<()> {
    fs_remove_dir(path)
}

/// Removes a file.
pub fn remove_file(path: &str) -> AxResult<()> {
    fs_remove_file(path)
}

/// Renames or moves a file or directory.
pub fn rename(from: &str, to: &str) -> AxResult<()> {
    fs_rename(from, to)
}

/// Returns the current working directory.
pub fn current_dir() -> AxResult<String> {
    fs_current_dir()
}

/// Changes the current working directory.
pub fn set_current_dir(path: &str) -> AxResult<()> {
    fs_set_current_dir(path)
}

/// Filesystem APIs required by AxVisor.
#[crate::api_def]
pub trait FsIf {
    /// Opens a host file for reading.
    fn open_file(path: &str) -> AxResult<usize>;

    /// Creates or truncates a host file for writing.
    fn create_file(path: &str) -> AxResult<usize>;

    /// Closes a previously opened file handle.
    fn close_file(file: usize);

    /// Returns metadata for a file handle.
    fn file_metadata(file: usize) -> AxResult<Metadata>;

    /// Reads bytes from a file handle into `buf`.
    fn file_read(file: usize, buf: &mut [u8]) -> AxResult<usize>;

    /// Writes bytes from `buf` to a file handle.
    fn file_write(file: usize, buf: &[u8]) -> AxResult<usize>;

    /// Flushes a file handle.
    fn file_flush(file: usize) -> AxResult<()>;

    /// Returns metadata for a filesystem path.
    fn path_metadata(path: &str) -> AxResult<Metadata>;

    /// Reads directory entries for a filesystem path.
    fn fs_read_dir(path: &str) -> AxResult<Vec<DirEntry>>;

    /// Creates a directory.
    fn fs_create_dir(path: &str) -> AxResult<()>;

    /// Removes an empty directory.
    fn fs_remove_dir(path: &str) -> AxResult<()>;

    /// Removes a file.
    fn fs_remove_file(path: &str) -> AxResult<()>;

    /// Renames or moves a file or directory.
    fn fs_rename(from: &str, to: &str) -> AxResult<()>;

    /// Returns the current working directory.
    fn fs_current_dir() -> AxResult<String>;

    /// Changes the current working directory.
    fn fs_set_current_dir(path: &str) -> AxResult<()>;
}
