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

//! Host filesystem and console stream APIs for AxVisor.

extern crate alloc;

use alloc::string::String;

use ax_errno::{AxResult, ax_err_type};

const STDIN_HANDLE: usize = 0;
const STDOUT_HANDLE: usize = 1;

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

/// An opaque file/stream handle.
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

    /// Returns a handle to the host standard input stream.
    pub const fn stdin() -> Self {
        Self { raw: STDIN_HANDLE }
    }

    /// Returns a handle to the host standard output stream.
    pub const fn stdout() -> Self {
        Self { raw: STDOUT_HANDLE }
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
    raw: usize,
}

impl ReadDir {
    fn from_raw(raw: usize) -> Self {
        Self { raw }
    }

    /// Returns the next directory entry, or `None` when exhausted.
    pub fn next_entry(&mut self) -> AxResult<Option<DirEntry>> {
        read_dir_next(self.raw)
    }
}

impl Drop for ReadDir {
    fn drop(&mut self) {
        close_read_dir(self.raw);
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
    Ok(ReadDir::from_raw(open_read_dir(path)?))
}

/// Reads a whole host file into a string.
pub fn read_to_string(path: &str) -> AxResult<String> {
    fs_read_to_string(path)
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
    fs_create_dir_all(path)
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

/// Returns a handle to host stdin.
pub const fn stdin() -> File {
    File::stdin()
}

/// Returns a handle to host stdout.
pub const fn stdout() -> File {
    File::stdout()
}

/// Filesystem and console-stream APIs required by AxVisor.
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

    /// Opens a directory iterator for a filesystem path.
    fn open_read_dir(path: &str) -> AxResult<usize>;

    /// Returns the next entry from a directory iterator.
    fn read_dir_next(dir: usize) -> AxResult<Option<DirEntry>>;

    /// Closes a directory iterator.
    fn close_read_dir(dir: usize);

    /// Reads a whole file into a string.
    fn fs_read_to_string(path: &str) -> AxResult<String>;

    /// Creates a directory.
    fn fs_create_dir(path: &str) -> AxResult<()>;

    /// Creates a directory and all missing parent directories.
    fn fs_create_dir_all(path: &str) -> AxResult<()>;

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
