use alloc::sync::{Arc, Weak};
use core::any::Any;

use axfs_ng_vfs::{Location, PathHandle, VfsResult};

use crate::{
    StarryError, StarryResult,
    pseudofs::{
        DeviceOps, SimpleDir,
        dev::tty::pts::{PtsDir, PtsInstance},
    },
};

pub(crate) enum Ptmx {
    Devfs,
    Devpts(Weak<PtsInstance>),
}

impl Ptmx {
    pub(crate) fn create_pty(
        &self,
        location: &Location,
        uid: u32,
    ) -> StarryResult<Arc<dyn DeviceOps>> {
        match self {
            Self::Devfs => {
                // Linux devpts_acquire selects the sibling mount of this open.
                let pts = location
                    .parent()
                    .ok_or(StarryError::NoSuchDevice)?
                    .lookup_no_follow("pts")?;
                let pts = PathHandle::acquire(pts)?;
                let directory = pts
                    .location()
                    .entry()
                    .downcast::<SimpleDir<PtsDir>>()
                    .map_err(|_| StarryError::NoSuchDevice)?;
                directory.operations().create_pty(uid)
            }
            Self::Devpts(instance) => instance
                .upgrade()
                .ok_or(StarryError::NoSuchDevice)?
                .create_pty(uid),
        }
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
