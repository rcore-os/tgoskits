use alloc::sync::{Arc, Weak};
use core::any::Any;

use axfs_ng_vfs::VfsResult;

use crate::{
    StarryResult,
    pseudofs::{DeviceOps, SimpleFsContext, dev::tty::pts::PtsInstance},
};

pub struct Ptmx {
    fs: SimpleFsContext,
    instance: Weak<PtsInstance>,
}

impl Ptmx {
    pub(crate) fn new(fs: SimpleFsContext, instance: Weak<PtsInstance>) -> Self {
        Self { fs, instance }
    }

    pub fn create_pty(&self, uid: u32) -> StarryResult<Arc<dyn DeviceOps>> {
        let instance = self
            .instance
            .upgrade()
            .ok_or(crate::StarryError::BadState)?;
        let (master, slave, link) = super::pty::create_pty_pair();
        let pty_number = instance.add_slave(self.fs.clone(), slave, uid)?;
        link.bind(&instance, pty_number);
        Ok(master)
    }
}

// This is implemented as null-ops since opening `Ptmx` would result in a new
// tty file and these implementations wouldn't actually be used
impl DeviceOps for Ptmx {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        unreachable!()
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        unreachable!()
    }

    fn ioctl(
        &self,
        _current: &crate::task::UserTaskRef,
        _cmd: u32,
        _arg: usize,
    ) -> VfsResult<usize> {
        unreachable!()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
