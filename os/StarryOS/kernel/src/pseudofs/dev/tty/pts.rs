use alloc::{borrow::Cow, boxed::Box, string::ToString, sync::Arc, vec::Vec};
use core::sync::atomic::Ordering;

use axfs_ng_vfs::{DeviceId, MetadataUpdate, NodeOps, NodePermission, NodeType, VfsResult};
use flatten_objects::FlattenObjects;

use crate::{
    StarryError, StarryResult,
    pseudofs::{
        Device, NodeOpsMux, SimpleDirOps, SimpleFs,
        dev::tty::{Ptmx, pty::PtyDriver},
    },
    sync::IrqMutex,
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

/// Selects whether a devpts mount reuses the initial instance or creates one.
#[derive(Clone, Copy)]
pub(crate) enum DevPtsMount {
    Legacy(DevPtsOptions),
    NewInstance(DevPtsOptions),
}

/// Indexes one devpts instance can hand out at the same time.
const PTY_SLOTS: usize = 16;

/// PTY index space and mount options owned by one devpts filesystem instance.
pub(crate) struct PtsInstance {
    options: IrqMutex<DevPtsOptions>,
    table: IrqMutex<FlattenObjects<Arc<Device>, PTY_SLOTS>>,
    /// Slaves that outlived their master: the node is gone, the index is not
    /// yet free.
    hidden: IrqMutex<[bool; PTY_SLOTS]>,
}

impl PtsInstance {
    pub(crate) fn new(options: DevPtsOptions) -> Arc<Self> {
        Arc::new(Self {
            options: IrqMutex::new(options),
            table: IrqMutex::new(FlattenObjects::new()),
            hidden: IrqMutex::new([false; PTY_SLOTS]),
        })
    }

    pub(crate) fn update_options(&self, options: DevPtsOptions) {
        *self.options.lock() = options;
    }

    /// Linux devpts_pty_kill() removes the node when the master closes, even
    /// while the slave is still open.
    pub(crate) fn hide_slave(&self, index: u32) {
        if let Some(hidden) = self.hidden.lock().get_mut(index as usize) {
            *hidden = true;
        }
    }

    /// Frees the index once neither end of the pty is open.
    pub(crate) fn release_slave(&self, index: u32) {
        let removed = {
            let mut table = self.table.lock();
            if let Some(hidden) = self.hidden.lock().get_mut(index as usize) {
                *hidden = false;
            }
            table.remove(index as usize)
        };
        drop(removed);
    }

    pub(crate) fn add_slave(&self, fs: Arc<SimpleFs>, pty: Arc<PtyDriver>) -> StarryResult<u32> {
        let options = *self.options.lock();
        let terminal = pty.terminal.clone();
        let device = Device::new(fs, NodeType::CharacterDevice, DeviceId::default(), pty);
        device.update_metadata(MetadataUpdate {
            mode: Some(options.slave_mode),
            owner: Some((0, options.slave_gid)),
            ..MetadataUpdate::default()
        })?;

        let mut table = self.table.lock();
        let pty_number = table
            .add(device)
            .map_err(|_| StarryError::TooManyOpenFiles)? as u32;
        terminal.pty_number.store(pty_number, Ordering::Release);
        table
            .get(pty_number as usize)
            .unwrap()
            .set_device_id(DeviceId::new(136, pty_number));
        Ok(pty_number)
    }

    fn ptmx(self: &Arc<Self>, fs: Arc<SimpleFs>) -> VfsResult<Arc<Device>> {
        let options = *self.options.lock();
        let device = Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(5, 2),
            Arc::new(Ptmx::new(fs, self.clone())),
        );
        device.update_metadata(MetadataUpdate {
            mode: Some(options.ptmx_mode),
            ..MetadataUpdate::default()
        })?;
        Ok(device)
    }
}

/// /dev/pts directory
pub struct PtsDir {
    fs: Arc<SimpleFs>,
    instance: Arc<PtsInstance>,
}

impl PtsDir {
    pub(crate) fn new(fs: Arc<SimpleFs>, instance: Arc<PtsInstance>) -> Self {
        Self { fs, instance }
    }
}

impl SimpleDirOps for PtsDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut names = Vec::from([Cow::Borrowed("ptmx")]);
        let hidden = *self.instance.hidden.lock();
        names.extend(
            self.instance
                .table
                .lock()
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
            return self
                .instance
                .ptmx(self.fs.clone())
                .map(|device| NodeOpsMux::File(device));
        }

        let id = name
            .parse::<usize>()
            .map_err(|_| StarryError::InvalidData)?;
        if self.instance.hidden.lock().get(id).copied().unwrap_or(false) {
            return Err(StarryError::NotFound.into());
        }
        let pty = self
            .instance
            .table
            .lock()
            .get(id)
            .ok_or(StarryError::NotFound)?
            .clone();
        Ok(NodeOpsMux::File(pty))
    }
}
