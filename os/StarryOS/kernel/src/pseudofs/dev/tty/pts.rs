use alloc::{borrow::Cow, boxed::Box, string::ToString, sync::Arc, vec::Vec};
use core::sync::atomic::Ordering;

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axfs_ng_vfs::{DeviceId, NodeType, VfsResult};
use flatten_objects::FlattenObjects;

use crate::pseudofs::{
    Device, NodeOpsMux, SimpleDirOps, SimpleFs,
    dev::tty::{Ptmx, pty::PtyDriver},
};

static PTS_TABLE: SpinNoIrq<FlattenObjects<Arc<Device>, 16>> =
    SpinNoIrq::new(FlattenObjects::new());

pub fn add_slave(fs: Arc<SimpleFs>, pty: Arc<PtyDriver>) -> AxResult<u32> {
    let terminal = pty.terminal.clone();
    let mut table = PTS_TABLE.lock();
    let pty_number = table
        .add(Device::new(
            fs,
            NodeType::CharacterDevice,
            DeviceId::default(),
            pty,
        ))
        .map_err(|_| AxError::TooManyOpenFiles)? as u32;
    terminal.pty_number.store(pty_number, Ordering::Release);
    table
        .get(pty_number as usize)
        .unwrap()
        .set_device_id(DeviceId::new(136, pty_number));
    Ok(pty_number)
}

/// /dev/pts directory
pub struct PtsDir {
    fs: Arc<SimpleFs>,
}

impl PtsDir {
    pub fn new(fs: Arc<SimpleFs>) -> Self {
        Self { fs }
    }
}

impl SimpleDirOps for PtsDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut names = Vec::from([Cow::Borrowed("ptmx")]);
        names.extend(PTS_TABLE.lock().ids().map(|it| Cow::Owned(it.to_string())));
        Box::new(names.into_iter())
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        if name == "ptmx" {
            return Ok(NodeOpsMux::File(Device::new(
                self.fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(5, 2),
                Arc::new(Ptmx(self.fs.clone())),
            )));
        }

        let id = name.parse::<usize>().map_err(|_| AxError::InvalidData)?;
        let pty = PTS_TABLE.lock().get(id).ok_or(AxError::NotFound)?.clone();
        Ok(NodeOpsMux::File(pty))
    }
}
