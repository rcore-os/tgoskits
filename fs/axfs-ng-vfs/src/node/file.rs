use alloc::sync::Arc;
use core::ops::Deref;

use super::NodeOps;
use crate::{FsPollable, VfsError, VfsResult};

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

pub trait FileNodeOps: NodeOps + FsPollable {
    /// Reads a number of bytes starting from a given offset.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize>;

    /// Writes a number of bytes starting from a given offset.
    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize>;

    /// Appends data to the file.
    ///
    /// Returns `(written, offset)` where `written` is the number of bytes
    /// written and `offset` is the new file size.
    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)>;

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
