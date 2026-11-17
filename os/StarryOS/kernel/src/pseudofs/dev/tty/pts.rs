use alloc::{
    borrow::Cow,
    boxed::Box,
    string::ToString,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::Ordering;

use axfs_ng_vfs::{DeviceId, MetadataUpdate, NodePermission, NodeType, VfsResult};
use flatten_objects::FlattenObjects;

use crate::{
    StarryError, StarryResult,
    pseudofs::{
        DeviceBacking, NodeOpsMux, SimpleDirOps, SimpleFsContext,
        dev::tty::{Ptmx, pty::PtyDriver},
    },
    sync::RawSpinLock,
};

/// Per-mount devpts configuration.
#[derive(Clone, Copy)]
pub(crate) struct DevPtsOptions {
    pub(crate) slave_mode: NodePermission,
    pub(crate) slave_gid: u32,
    pub(crate) ptmx_mode: NodePermission,
}

impl DevPtsOptions {
    /// Configuration used by the boot-time `/dev/pts` instance.
    pub(crate) const fn root() -> Self {
        Self {
            slave_mode: NodePermission::from_bits_truncate(0o666),
            slave_gid: 0,
            ptmx_mode: NodePermission::from_bits_truncate(0o666),
        }
    }

    /// Linux-compatible defaults for a newly mounted devpts instance.
    pub(crate) const fn mounted() -> Self {
        Self {
            slave_mode: NodePermission::from_bits_truncate(0o600),
            slave_gid: 0,
            ptmx_mode: NodePermission::empty(),
        }
    }
}

/// Indexes one devpts instance can hand out at the same time.
const PTY_SLOTS: usize = 16;

/// PTY index space and mount options owned by one devpts filesystem instance.
pub(crate) struct PtsInstance {
    options: DevPtsOptions,
    table: RawSpinLock<FlattenObjects<Arc<DeviceBacking>, PTY_SLOTS>>,
    ptmx: Arc<DeviceBacking>,
    /// Slaves that outlived their master: the node is gone, the index is not
    /// yet free.
    hidden: RawSpinLock<[bool; PTY_SLOTS]>,
}

impl PtsInstance {
    pub(crate) fn new(context: SimpleFsContext, options: DevPtsOptions) -> VfsResult<Arc<Self>> {
        let inode = context.allocate_inode(NodeType::CharacterDevice, options.ptmx_mode)?;
        Ok(Arc::new_cyclic(|instance: &Weak<Self>| Self {
            options,
            table: RawSpinLock::new(FlattenObjects::new()),
            hidden: RawSpinLock::new([false; PTY_SLOTS]),
            ptmx: DeviceBacking::from_inode(
                inode,
                DeviceId::new(5, 2),
                Arc::new(Ptmx::new(context, instance.clone())),
            ),
        }))
    }

    pub(crate) fn ptmx_backing(&self) -> Arc<DeviceBacking> {
        self.ptmx.clone()
    }

    /// Linux devpts_pty_kill() removes the node when the master closes, even
    /// while the slave is still open.
    pub(crate) fn hide_slave(&self, index: u32) {
        if let Some(hidden) = self.hidden.lock_irqsave().get_mut(index as usize) {
            *hidden = true;
        }
    }

    /// Frees the index once neither end of the pty is open.
    pub(crate) fn release_slave(&self, index: u32) {
        let removed = {
            let mut table = self.table.lock_irqsave();
            if let Some(hidden) = self.hidden.lock_irqsave().get_mut(index as usize) {
                *hidden = false;
            }
            table.remove(index as usize)
        };
        drop(removed);
    }

    pub(crate) fn add_slave(
        &self,
        fs: SimpleFsContext,
        pty: Arc<PtyDriver>,
        uid: u32,
    ) -> StarryResult<u32> {
        let options = self.options;
        let terminal = pty.terminal.clone();
        let device = DeviceBacking::new(fs, NodeType::CharacterDevice, DeviceId::default(), pty)?;
        device.update_metadata(MetadataUpdate {
            mode: Some(options.slave_mode),
            owner: Some((uid, options.slave_gid)),
            ..MetadataUpdate::default()
        })?;

        let result = {
            let mut table = self.table.lock_irqsave();
            // The outside candidate keeps a failed insertion's last Drop out of this guard.
            table
                .add(device.clone())
                .map(|index| {
                    let number = index as u32;
                    device.set_device_id(DeviceId::new(136, number));
                    terminal.pty_number.store(number, Ordering::Release);
                    number
                })
                .map_err(|_| StarryError::TooManyOpenFiles)
        };
        drop(device);
        result
    }
}

/// /dev/pts directory
pub struct PtsDir {
    instance: Arc<PtsInstance>,
}

impl PtsDir {
    pub(crate) fn new(instance: Arc<PtsInstance>) -> Self {
        Self { instance }
    }
}

impl SimpleDirOps for PtsDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut names = Vec::from([Cow::Borrowed("ptmx")]);
        let hidden = *self.instance.hidden.lock_irqsave();
        names.extend(
            self.instance
                .table
                .lock_irqsave()
                .ids()
                .filter(|&id| !hidden[id])
                .map(|it| Cow::Owned(it.to_string())),
        );
        Box::new(names.into_iter())
    }

    fn is_cacheable(&self) -> bool {
        // A freed index is handed to the next pty, so a cached entry would
        // still name the previous pty's slave.
        false
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        if name == "ptmx" {
            return Ok(self.instance.ptmx_backing().into());
        }

        let id = name
            .parse::<usize>()
            .map_err(|_| StarryError::InvalidData)?;
        if self
            .instance
            .hidden
            .lock_irqsave()
            .get(id)
            .copied()
            .unwrap_or(false)
        {
            return Err(StarryError::NotFound.into());
        }
        let pty = self
            .instance
            .table
            .lock_irqsave()
            .get(id)
            .ok_or(StarryError::NotFound)?
            .clone();
        Ok(pty.into())
    }
}
