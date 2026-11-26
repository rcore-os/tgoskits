use alloc::sync::Arc;
use core::ops::Deref;

use axpoll::Pollable;

use super::NodeOps;
use crate::{VfsError, VfsResult};

/// Specifies whether preallocation may extend the visible file size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreallocationMode {
    /// Reserve storage and extend the file to cover the requested range.
    ExtendSize,
    /// Reserve storage without changing the visible file size.
    KeepSize,
}

/// One storage or mapping operation applied to a regular-file byte range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileRangeOperation {
    Allocate(PreallocationMode),
    PunchHole,
    ZeroRange(PreallocationMode),
    CollapseRange,
    InsertRange,
}

/// Allocation state of one file-to-device extent mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileExtentState {
    Initialized,
    Unwritten,
    Inline,
}

/// Mapping namespace selected for a file extent query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileExtentTarget {
    Data,
    ExtendedAttributes,
}

/// One byte-addressed extent mapping returned by a filesystem backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileExtent {
    pub logical_start: u64,
    pub physical_start: u64,
    pub length: u64,
    pub state: FileExtentState,
    pub merged: bool,
}

/// Bounded result of a file extent query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileExtentMap {
    pub extents: alloc::vec::Vec<FileExtent>,
    pub mapped_extents: usize,
    pub complete: bool,
}

pub trait FileNodeOps: NodeOps + Pollable {
    /// Authorizes a fresh existing-file open and prepares any copy-up before fd
    /// publication. Layered owners override this and use their actual gate.
    fn prepare_write_open(
        &self,
        request: &crate::FileOpenRequest<'_>,
    ) -> crate::NamespaceCompletion<crate::Metadata> {
        request.native(self)
    }

    /// Selects a fixed native endpoint outside every admission/backend gate.
    /// Layered files must select their bound backing using saved credentials;
    /// the default only accepts this actual receiver at native depth zero.
    fn resolve_file_io(&self, request: crate::FileIoRequest<'_>) -> VfsResult<crate::FileIoTarget> {
        request.native(self)
    }

    /// Reads a number of bytes starting from a given offset.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize>;

    /// Writes a number of bytes starting from a given offset.
    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize>;

    /// Appends data to the file.
    ///
    /// Returns `(written, offset)` where `written` is the number of bytes
    /// written and `offset` is the new file size.
    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)>;

    /// Writes one complete input request. `None` chooses append; `Some` uses
    /// the supplied file offset. The returned offset is the end of this write.
    /// Filesystems with deferred data completion override this to publish once
    /// for the whole request. Source reads may fault or reenter file reads and
    /// must run outside physical filesystem and cache protection.
    fn write_iter(
        &self,
        source: &mut dyn crate::Read,
        length: usize,
        offset: Option<u64>,
    ) -> VfsResult<(usize, u64)> {
        write_file_iter(self, source, length, offset)
    }

    /// Sets the size of the file.
    fn set_len(&self, len: u64) -> VfsResult<()>;

    /// Applies one storage or mapping operation to a byte range.
    fn operate_range(
        &self,
        _offset: u64,
        _len: u64,
        _operation: FileRangeOperation,
    ) -> VfsResult<()> {
        Err(VfsError::OperationNotSupported)
    }

    /// Reserves backing storage for a byte range.
    fn preallocate(&self, offset: u64, len: u64, mode: PreallocationMode) -> VfsResult<()> {
        self.operate_range(offset, len, FileRangeOperation::Allocate(mode))
    }

    /// Queries allocated mappings without exposing filesystem disk structs.
    fn map_extents(
        &self,
        _offset: u64,
        _len: u64,
        _target: FileExtentTarget,
        _extent_limit: usize,
    ) -> VfsResult<FileExtentMap> {
        Err(VfsError::OperationNotSupported)
    }

    /// Manipulates the underlying device parameters of special files.
    fn ioctl(&self, _cmd: u32, _arg: usize) -> VfsResult<usize> {
        Err(VfsError::NotATty)
    }
}

/// Streams input to resident-buffer producers. Native deferred writers use
/// this only when their mapping format lacks an independent data endpoint;
/// selection must happen before consuming input or allocating write targets.
pub fn write_file_iter<N: FileNodeOps + ?Sized>(
    node: &N,
    source: &mut dyn crate::Read,
    length: usize,
    offset: Option<u64>,
) -> VfsResult<(usize, u64)> {
    let append = offset.is_none();
    let mut position = match offset {
        Some(offset) => offset,
        None => node.len()?,
    };
    let mut total = 0;
    let mut buffer = [0; ax_io::DEFAULT_BUF_SIZE];
    while total < length {
        let count = (length - total).min(buffer.len());
        let read = source
            .read(&mut buffer[..count])
            .map_err(crate::io_error_to_vfs_error)?;
        if read > count {
            return Err(VfsError::BadState);
        }
        if read == 0 {
            break;
        }
        let mut consumed = 0;
        while consumed < read {
            let result = if append {
                node.append(&buffer[consumed..read])
            } else {
                node.write_at(&buffer[consumed..read], position)
                    .and_then(|written| {
                        position
                            .checked_add(written as u64)
                            .map(|end| (written, end))
                            .ok_or(VfsError::FileTooLarge)
                    })
            };
            let (written, end) = match result {
                Ok(result) => result,
                Err(VfsError::WouldBlock) if total != 0 => return Ok((total, position)),
                Err(error) => return Err(error),
            };
            if written > read - consumed {
                return Err(VfsError::BadState);
            }
            if written == 0 {
                return Ok((total, position));
            }
            position = end;
            consumed += written;
            total += written;
            if append && consumed < read {
                return Ok((total, position));
            }
        }
    }
    Ok((total, position))
}

#[derive(Clone)]
#[repr(transparent)]
pub struct FileNode(Arc<dyn FileNodeOps>);

impl Deref for FileNode {
    type Target = dyn FileNodeOps;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl From<FileNode> for Arc<dyn NodeOps> {
    fn from(node: FileNode) -> Self {
        node.0.clone()
    }
}

impl FileNode {
    pub fn new(ops: Arc<dyn FileNodeOps>) -> Self {
        Self(ops)
    }

    pub fn inner(&self) -> &Arc<dyn FileNodeOps> {
        &self.0
    }

    pub fn downcast<T: FileNodeOps>(self: &Arc<Self>) -> VfsResult<Arc<T>> {
        self.0
            .clone()
            .into_any()
            .downcast()
            .map_err(|_| VfsError::InvalidInput)
    }
}
