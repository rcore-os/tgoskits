use core::any::Any;

use axfs_ng_vfs::{Location, VfsResult};

use super::pts::PtsDir;
use crate::{
    StarryError, StarryResult,
    pseudofs::{DeviceOps, SimpleDir},
};

pub enum Ptmx {
    /// The traditional character device, which discovers its sibling pts.
    Devfs,
    /// The ptmx node supplied by a specific devpts instance.
    Devpts,
}

impl Ptmx {
    pub(crate) fn open_pty(&self, location: &Location) -> StarryResult<Location> {
        let pts = match self {
            Self::Devfs => location
                .parent()
                .ok_or(StarryError::NotFound)?
                .lookup_no_follow("pts")?,
            Self::Devpts => {
                // A file bind mount still refers to its original devpts node.
                let entry = location.entry().parent().ok_or(StarryError::NoSuchDevice)?;
                Location::new(location.mountpoint().clone(), entry)
            }
        };
        let directory = pts
            .entry()
            .downcast::<SimpleDir<PtsDir>>()
            .map_err(|_| StarryError::NoSuchDevice)?;
        directory.operations().open_pty(&pts)
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
