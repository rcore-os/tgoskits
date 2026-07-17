//! Generation-checked directory iteration.

use alloc::{collections::VecDeque, string::String};

use axfs_ng_vfs::{Location, NodeType, VfsError, VfsResult};

use crate::lifecycle::{FsOpenHandleLease, FsRuntimeError};

/// A single entry returned by [`crate::FsContext::read_dir`].
pub struct ReadDirEntry {
    /// Entry name (file or directory name, not the full path).
    pub name: String,
    /// Inode number.
    pub ino: u64,
    /// Type of the node (file, directory, symlink, etc.).
    pub node_type: NodeType,
    /// Byte offset inside the directory (used for seeking).
    pub offset: u64,
}

/// Iterator returned by [`crate::FsContext::read_dir`].
pub struct ReadDir {
    dir: Location,
    buf: VecDeque<ReadDirEntry>,
    offset: u64,
    ended: bool,
    lease: Option<FsOpenHandleLease>,
}

impl ReadDir {
    /// Maximum number of entries to buffer per `read_dir` syscall.
    // TODO: tune this
    pub const BUF_SIZE: usize = 128;

    pub(super) fn new(dir: Location, lease: Option<FsOpenHandleLease>) -> Self {
        Self {
            dir,
            buf: VecDeque::new(),
            offset: 0,
            ended: false,
            lease,
        }
    }
}

impl Iterator for ReadDir {
    type Item = VfsResult<ReadDirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let _operation = match self
            .lease
            .as_ref()
            .map(FsOpenHandleLease::begin_operation)
            .transpose()
        {
            Ok(operation) => operation,
            Err(error) => return Some(Err(map_lifecycle_error(error))),
        };
        if self.ended {
            return None;
        }

        if self.buf.is_empty() {
            self.buf.clear();
            let result = self.dir.read_dir(
                self.offset,
                &mut |name: &str, ino: u64, node_type: NodeType, offset: u64| {
                    self.buf.push_back(ReadDirEntry {
                        name: name.into(),
                        ino,
                        node_type,
                        offset,
                    });
                    self.offset = offset;
                    self.buf.len() < Self::BUF_SIZE
                },
            );

            if self.buf.is_empty() {
                if let Err(error) = result {
                    return Some(Err(error));
                }
                self.ended = true;
                return None;
            }
        }

        self.buf.pop_front().map(Ok)
    }
}

fn map_lifecycle_error(error: FsRuntimeError) -> VfsError {
    error.into_ax_error()
}
