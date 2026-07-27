use alloc::{string::String, sync::Arc, vec::Vec};

use ax_sync::PiMutex;
use axfs_ng_vfs::{NodePermission, VfsError, VfsResult};
use ddebug::ControlFile;

use super::SimpleFs;
use crate::{
    dyn_debug::{DynamicDebugOps, dynamic_debug_init},
    pseudofs::{DirectRwFsFileOps, SpecialFsFile},
};

pub struct DynDebugControlObj {
    control: PiMutex<ControlFile<DynamicDebugOps>>,
    snapshot: PiMutex<Option<Vec<u8>>>,
}

impl DynDebugControlObj {
    fn new(control: ControlFile<DynamicDebugOps>) -> Self {
        Self {
            control: PiMutex::new(control),
            snapshot: PiMutex::new(None),
        }
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

impl DirectRwFsFileOps for DynDebugControlObj {
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
}

/// Creates a control file for dynamic debug. This file can be used to enable/disable dynamic debug sites at runtime.
pub fn create_dyn_debug_control_file(fs: Arc<SimpleFs>) -> Arc<SpecialFsFile<DynDebugControlObj>> {
    let control = dynamic_debug_init();
    let obj = DynDebugControlObj::new(control);
    SpecialFsFile::new_regular_with_perm(fs, obj, NodePermission::from_bits_truncate(0o644))
}
