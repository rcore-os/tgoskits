use alloc::{string::String, sync::Arc, vec::Vec};
use core::{any::Any, task::Context};

use ax_sync::Mutex;
use axfs_ng_vfs::{
    FileNodeOps, FilesystemOps, Metadata, MetadataUpdate, NodeFlags, NodeOps, NodePermission,
    NodeType, VfsError, VfsResult,
};
use axpoll::{IoEvents, Pollable};
use ddebug::ControlFile;
use inherit_methods_macro::inherit_methods;

use super::{SimpleFs, SimpleFsNode};
use crate::dyn_debug::{DynamicDebugOps, dynamic_debug_init};

pub struct DynDebugControlFile {
    node: SimpleFsNode,
    control: Mutex<ControlFile<DynamicDebugOps>>,
    snapshot: Mutex<Option<Vec<u8>>>,
}

impl DynDebugControlFile {
    fn new(fs: Arc<SimpleFs>, control: ControlFile<DynamicDebugOps>) -> Arc<Self> {
        Arc::new(Self {
            node: SimpleFsNode::new(fs, NodeType::RegularFile, NodePermission::default()),
            control: Mutex::new(control),
            snapshot: Mutex::new(None),
        })
    }

    fn apply_command(&self, data: &[u8]) -> VfsResult<()> {
        let content = core::str::from_utf8(data).map_err(|_| VfsError::InvalidInput)?;
        self.control
            .lock()
            .write(content)
            .map_err(|_| VfsError::InvalidInput)?;
        Ok(())
    }
}

#[inherit_methods(from = "self.node")]
impl NodeOps for DynDebugControlFile {
    fn inode(&self) -> u64;

    fn metadata(&self) -> VfsResult<Metadata>;

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

    fn filesystem(&self) -> &dyn FilesystemOps;

    fn sync(&self, data_only: bool) -> VfsResult<()>;

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn len(&self) -> VfsResult<u64> {
        let mut snapshot = self.snapshot.lock();
        if snapshot.is_none() {
            let data = self.control.lock().read().unwrap_or_else(|_| String::new());
            *snapshot = Some(data.into_bytes().to_vec());
        }
        Ok(snapshot.as_ref().unwrap().len() as u64)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

impl FileNodeOps for DynDebugControlFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let mut snapshot = self.snapshot.lock();
        if snapshot.is_none() || offset == 0 {
            let data = self.control.lock().read().unwrap_or_else(|_| String::new());
            *snapshot = Some(data.into_bytes().to_vec());
        }
        let data = snapshot.as_ref().unwrap();
        if offset >= data.len() as u64 {
            return Ok(0);
        }

        let data = &data[offset as usize..];
        let read = data.len().min(buf.len());
        buf[..read].copy_from_slice(&data[..read]);
        Ok(read)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        self.apply_command(buf)?;
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let written = self.write_at(buf, 0)?;
        Ok((written, 0))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        if len == 0 {
            // Shell redirection usually opens proc control files with O_TRUNC.
            return Ok(());
        }
        Err(VfsError::OperationNotPermitted)
    }

    fn set_symlink(&self, _target: &str) -> VfsResult<()> {
        Err(VfsError::OperationNotPermitted)
    }
}

impl Pollable for DynDebugControlFile {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}

/// Creates a control file for dynamic debug. This file can be used to enable/disable dynamic debug sites at runtime.
pub fn create_dyn_debug_control_file(fs: Arc<SimpleFs>) -> Arc<DynDebugControlFile> {
    let control = dynamic_debug_init();
    DynDebugControlFile::new(fs, control)
}
