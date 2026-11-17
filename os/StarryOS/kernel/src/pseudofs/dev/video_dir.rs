//! Dynamic `/dev/videoX` entries backed by the current usbfs device snapshots.

use alloc::{borrow::Cow, boxed::Box, format, sync::Arc, vec::Vec};

use axfs_ng_vfs::{DeviceId, NodePermission, NodeType, VfsError, VfsResult};

use super::{uvc_camera, video};
use crate::pseudofs::{
    DeviceBacking, DirMapping, NodeOpsMux, SimpleDirOps, SimpleFsContext,
    usbfs::UsbDeviceSnapshotInfo,
};

/// Keep the static `/dev` entries stable for nested mounts while resolving
/// camera entries against the current USB topology.
pub(super) struct UvcDevRoot {
    static_entries: DirMapping,
    cameras: UvcVideoDir,
}

impl UvcDevRoot {
    pub(super) fn new(static_entries: DirMapping, fs: SimpleFsContext) -> Self {
        Self {
            static_entries,
            cameras: UvcVideoDir::new(fs),
        }
    }
}

impl SimpleDirOps for UvcDevRoot {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            self.static_entries
                .child_names()
                .chain(self.cameras.child_names()),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match self.static_entries.lookup_child(name) {
            Err(VfsError::NotFound) => self.cameras.lookup_child(name),
            result => result,
        }
    }

    fn is_cacheable(&self) -> bool {
        false
    }

    fn is_cacheable_child(&self, name: &str) -> bool {
        self.static_entries.child_names().any(|child| child == name)
    }
}

struct CameraSlot {
    snapshot: UsbDeviceSnapshotInfo,
    device: Option<Arc<DeviceBacking>>,
}

pub(super) struct UvcVideoDir {
    fs: SimpleFsContext,
    slots: ax_sync::Mutex<Vec<Option<CameraSlot>>>,
    // USB construction may sleep. Its callbacks never enter this gate, and
    // it keeps the existing initialization order without holding the index.
    construction: ax_sync::Mutex<()>,
}

impl UvcVideoDir {
    pub(super) fn new(fs: SimpleFsContext) -> Self {
        Self {
            fs,
            slots: ax_sync::Mutex::new(Vec::new()),
            construction: ax_sync::Mutex::new(()),
        }
    }

    fn refresh(&self, snapshots: &[UsbDeviceSnapshotInfo]) {
        let mut retired = Vec::new();
        let mut slots = self.slots.lock();
        for slot in slots.iter_mut() {
            if slot.as_ref().is_some_and(|slot| {
                !snapshots.iter().any(|snap| {
                    snap.bus_num == slot.snapshot.bus_num
                        && snap.device_num == slot.snapshot.device_num
                        && snap.generation == slot.snapshot.generation
                        && snap.descriptor_blob == slot.snapshot.descriptor_blob
                })
            }) {
                retired.push(slot.take());
            }
        }
        for snapshot in snapshots {
            if slots.iter().flatten().any(|slot| {
                slot.snapshot.bus_num == snapshot.bus_num
                    && slot.snapshot.device_num == snapshot.device_num
                    && slot.snapshot.generation == snapshot.generation
                    && slot.snapshot.descriptor_blob == snapshot.descriptor_blob
            }) {
                continue;
            }
            let new_slot = Some(CameraSlot {
                snapshot: snapshot.clone(),
                device: None,
            });
            if let Some(index) = slots.iter().position(Option::is_none) {
                slots[index] = new_slot;
            } else {
                slots.push(new_slot);
            }
        }
        drop(slots);
        drop(retired);
    }
}

impl SimpleDirOps for UvcVideoDir {
    fn is_cacheable(&self) -> bool {
        false
    }

    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        self.refresh(&uvc_camera::collect_uvc_snapshots());
        let names = self
            .slots
            .lock()
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.as_ref().map(|_| format!("video{index}")))
            .collect::<Vec<_>>();
        Box::new(names.into_iter().map(Cow::Owned))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let index = name
            .strip_prefix("video")
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or(VfsError::NotFound)?;
        self.refresh(&uvc_camera::collect_uvc_snapshots());
        let construction = self.construction.lock();
        let snapshot = {
            let slots = self.slots.lock();
            let slot = slots
                .get(index)
                .and_then(Option::as_ref)
                .ok_or(VfsError::NotFound)?;
            if let Some(device) = &slot.device {
                return Ok(device.clone().into());
            }
            slot.snapshot.clone()
        };
        let inode = self
            .fs
            .allocate_inode(NodeType::CharacterDevice, NodePermission::default())?;
        let camera = uvc_camera::create_camera_driver(&snapshot).map_err(VfsError::from)?;
        let events = camera.event_source();
        let driver: Arc<ax_sync::Mutex<dyn ax_media::V4L2DriverOps>> =
            Arc::new(ax_sync::Mutex::new(camera));
        let video = ax_media::VideoDevice::new(driver, "uvc");
        let device = DeviceBacking::from_inode(
            inode,
            DeviceId::new(81, index as u32),
            Arc::new(video::V4l2DevNode::from_input(video, events)),
        );
        let result = {
            let mut slots = self.slots.lock();
            match slots.get_mut(index).and_then(Option::as_mut) {
                Some(slot)
                    if slot.snapshot.bus_num == snapshot.bus_num
                        && slot.snapshot.device_num == snapshot.device_num
                        && slot.snapshot.generation == snapshot.generation
                        && slot.snapshot.descriptor_blob == snapshot.descriptor_blob =>
                {
                    let published = slot.device.get_or_insert_with(|| device.clone());
                    Ok(published.clone().into())
                }
                _ => Err(VfsError::NotFound),
            }
        };
        // Failed or losing candidates may clean up drivers after both guards.
        drop(construction);
        drop(device);
        result
    }
}
