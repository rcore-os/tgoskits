use alloc::{sync::Arc, vec};
use core::any::Any;

use axfs_ng_vfs::{
    FileNode, FileNodeOps, FilesystemOps, Metadata, MetadataRequest, MetadataUpdate,
    NamespaceCompletion, NodeFlags, NodeIdentity, NodeOps, VfsError, VfsResult,
};
use axpoll::{IoEvents, Pollable};
use fatfs::{Read, Seek, SeekFrom, Write};

use super::{
    ff,
    fs::FatFilesystemGuard,
    state::{FatHandle, FatInode},
    util::{into_vfs_err, update_file_times},
};

pub struct FatFileNode {
    state: Arc<FatInode>,
}

impl FatFileNode {
    pub(super) fn from_state(state: Arc<FatInode>) -> FileNode {
        FileNode::new(Arc::new(Self { state }))
    }

    fn file<'a>(
        &'a self,
        guard: &'a mut FatFilesystemGuard<'_>,
    ) -> VfsResult<&'a mut ff::File<'static>> {
        match self.state.handle.borrow_mut(guard).as_mut() {
            Some(FatHandle::File(file)) => Ok(file),
            _ => Err(VfsError::BadState),
        }
    }
}

fn grow_file(block_size: usize, file: &mut ff::File<'static>, len: u64) -> VfsResult<()> {
    let mut pos = file.seek(SeekFrom::End(0)).map_err(into_vfs_err)?;
    let block = vec![0; block_size];
    while pos < len {
        let count = (block_size - (pos as usize & (block_size - 1))).min((len - pos) as usize);
        let written = file.write(&block[..count]).map_err(into_vfs_err)?;
        if written == 0 {
            return Err(VfsError::Io);
        }
        pos += written as u64;
    }
    Ok(())
}

impl NodeOps for FatFileNode {
    fn inode(&self) -> u64 {
        self.state.inode
    }

    fn identity(&self) -> Option<NodeIdentity> {
        Some(self.state.identity())
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        self.state.metadata(&self.state.fs.lock())
    }

    fn change_metadata(&self, request: &MetadataRequest<'_>) -> NamespaceCompletion<Metadata> {
        self.state.change_metadata(request)
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        let mut guard = self.state.fs.lock();
        update_file_times(self.file(&mut guard)?, update.atime, update.mtime);
        Ok(())
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        &*self.state.fs
    }

    fn len(&self) -> VfsResult<u64> {
        let mut guard = self.state.fs.lock();
        Ok(self.file(&mut guard)?.size().unwrap_or(0) as u64)
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        let mut guard = self.state.fs.lock();
        self.file(&mut guard)?.flush().map_err(into_vfs_err)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
    fn flags(&self) -> NodeFlags {
        NodeFlags::BLOCKING
    }
}

impl FileNodeOps for FatFileNode {
    fn read_at(&self, mut buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let mut guard = self.state.fs.lock();
        let file = self.file(&mut guard)?;
        file.seek(SeekFrom::Start(offset)).map_err(into_vfs_err)?;
        let mut total = 0;
        while !buf.is_empty() {
            let count = file.read(buf).map_err(into_vfs_err)?;
            if count == 0 {
                break;
            }
            total += count;
            buf = &mut buf[count..];
        }
        Ok(total)
    }

    fn write_at(&self, mut buf: &[u8], offset: u64) -> VfsResult<usize> {
        let mut guard = self.state.fs.lock();
        let block_size = guard.inner.bytes_per_sector() as usize;
        let file = self.file(&mut guard)?;
        if offset > file.size().unwrap_or(0) as u64 {
            grow_file(block_size, file, offset)?;
        }
        file.seek(SeekFrom::Start(offset)).map_err(into_vfs_err)?;
        let mut total = 0;
        while !buf.is_empty() {
            let count = file.write(buf).map_err(into_vfs_err)?;
            if count == 0 {
                break;
            }
            total += count;
            buf = &buf[count..];
        }
        Ok(total)
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let mut guard = self.state.fs.lock();
        let file = self.file(&mut guard)?;
        file.seek(SeekFrom::End(0)).map_err(into_vfs_err)?;
        let written = file.write(buf).map_err(into_vfs_err)?;
        Ok((written, file.size().unwrap_or(0) as u64))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        let mut guard = self.state.fs.lock();
        let block_size = guard.inner.bytes_per_sector() as usize;
        let file = self.file(&mut guard)?;
        if len <= file.size().unwrap_or(0) as u64 {
            file.seek(SeekFrom::Start(len)).map_err(into_vfs_err)?;
            file.truncate().map_err(into_vfs_err)
        } else {
            grow_file(block_size, file, len)
        }
    }
}

impl Pollable for FatFileNode {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }
    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}
