//! Dynamic `/dev/videoX` entries backed by the current usbfs device snapshots.

use alloc::{borrow::Cow, boxed::Box, format, sync::Arc, vec::Vec};

use axfs_ng_vfs::{DeviceId, NodeType, VfsError, VfsResult};

use super::{uvc_camera, video};
use crate::pseudofs::{
    Device, DirMapping, NodeOpsMux, SimpleDirOps, SimpleFs, usbfs::UsbDeviceSnapshotInfo,
};

/// Keep the static `/dev` entries stable for nested mounts while resolving
/// camera entries against the current USB topology.
pub(super) struct UvcDevRoot {
    static_entries: DirMapping,
    cameras: UvcVideoDir,
}

impl UvcDevRoot {
    pub(super) fn new(static_entries: DirMapping, fs: Arc<SimpleFs>) -> Self {
        Self {
            static_entries,
            cameras: UvcVideoDir::new(fs),
        }
    }
}

impl SimpleDirOps for UvcDevRoot {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(self.static_entries.child_names().chain(self.cameras.child_names()))
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
    device: Option<Arc<Device>>,
}

pub(super) struct UvcVideoDir {
    fs: Arc<SimpleFs>,
    slots: ax_sync::Mutex<Vec<Option<CameraSlot>>>,
}

impl UvcVideoDir {
    pub(super) fn new(fs: Arc<SimpleFs>) -> Self {
        Self {
            fs,
            slots: ax_sync::Mutex::new(Vec::new()),
        }
    }

    fn refresh(&self, snapshots: &[UsbDeviceSnapshotInfo]) {
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
                *slot = None;
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
        let mut slots = self.slots.lock();
        let slot = slots
            .get_mut(index)
            .and_then(Option::as_mut)
            .ok_or(VfsError::NotFound)?;
        if let Some(device) = &slot.device {
            return Ok(NodeOpsMux::File(device.clone()));
        }
        let camera = uvc_camera::create_camera_driver(&slot.snapshot).map_err(VfsError::from)?;
        let events = camera.event_source();
        let driver: Arc<ax_sync::Mutex<dyn ax_media::V4L2DriverOps>> =
            Arc::new(ax_sync::Mutex::new(camera));
        let video = ax_media::VideoDevice::new(driver, "uvc");
        let device = Device::new(
            self.fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(81, index as u32),
            Arc::new(video::V4l2DevNode::from_input(video, events)),
        );
        slot.device = Some(device.clone());
        Ok(NodeOpsMux::File(device))
    }
}
