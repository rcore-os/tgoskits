use core::ops::Deref;

use alloc::{sync::Arc, vec::Vec};
use axerrno::LinuxError;

use crate::{VfsError, VfsResult};

use super::NodeOps;

const IO_BUF_SIZE: usize = 8 * 1024;

pub trait FileNodeOps<M>: NodeOps<M> {
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
}

#[repr(transparent)]
pub struct FileNode<M>(Arc<dyn FileNodeOps<M>>);
impl<M> Deref for FileNode<M> {
    type Target = dyn FileNodeOps<M>;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}
impl<M> From<FileNode<M>> for Arc<dyn NodeOps<M>> {
    fn from(node: FileNode<M>) -> Self {
        node.0.clone()
    }
}

impl<M> FileNode<M> {
    pub fn new(ops: Arc<dyn FileNodeOps<M>>) -> Self {
        Self(ops)
    }

    pub fn inner(&self) -> &Arc<dyn FileNodeOps<M>> {
        &self.0
    }

    pub fn downcast<T: Send + Sync + 'static>(self: &Arc<Self>) -> VfsResult<Arc<T>> {
        self.0
            .clone()
            .into_any()
            .downcast()
            .map_err(|_| VfsError::EINVAL)
    }

    /// Reads the contents of a file starting from a given offset, returning the
    /// number of bytes read.
    pub fn read_to_end(&self, buf: &mut Vec<u8>, off: u64) -> VfsResult<usize> {
        let len = self.0.len()?;
        buf.reserve(len as usize);

        let mut chunk = [0u8; IO_BUF_SIZE];
        let mut read = 0;
        let mut off = off.min(len);
        if off % IO_BUF_SIZE as u64 != 0 {
            let read_len = IO_BUF_SIZE - (off % IO_BUF_SIZE as u64) as usize;
            let read_len = read_len.min((len - off) as usize);
            let n = self.read_at(&mut chunk[..read_len], off)?;
            buf.extend_from_slice(&chunk[..n]);
            read += n;
            off += n as u64;
        }

        for i in (off..len).step_by(IO_BUF_SIZE) {
            let n = self.read_at(&mut chunk, i)?;
            buf.extend_from_slice(&chunk[..n]);
            read += n;
        }
        if read as u64 != len {
            Err(LinuxError::EIO)
        } else {
            Ok(read)
        }
    }

    /// Writes the entire contents of a bytes vector into a file, starting from
    /// a given offset. Returns the number of bytes written.
    pub fn write_all(&self, mut buf: &[u8], mut off: u64) -> VfsResult<usize> {
        let mut written = 0;
        if off % IO_BUF_SIZE as u64 != 0 {
            let write_len = IO_BUF_SIZE - (off % IO_BUF_SIZE as u64) as usize;
            let write_len = write_len.min(buf.len());
            let n = self.write_at(&buf[..write_len], off)?;
            buf = &buf[write_len..];
            written += n;
            off += n as u64;
        }

        for chunk in buf.chunks(IO_BUF_SIZE) {
            let n = self.write_at(chunk, off)?;
            written += n;
            off += n as u64;
        }
        if written != buf.len() {
            Err(LinuxError::EIO)
        } else {
            Ok(written)
        }
    }
}
