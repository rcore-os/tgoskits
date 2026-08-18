use alloc::{borrow::Cow, boxed::Box, sync::Arc, vec::Vec};
use core::{any::Any, task::Context};

use axfs_ng_vfs::{NodeFlags, NodeType, VfsError, VfsResult};
use axpoll::{IoEvents, Pollable};
use starry_vm::VmMutPtr;

use super::{
    descriptor::{
        USBDEVFS_CAP_BULK_CONTINUATION, USBDEVFS_CONNECTINFO, USBDEVFS_CONTROL,
        USBDEVFS_GET_CAPABILITIES, UsbdevfsConnectInfo, bus_name, device_name,
        parse_numeric_component, usb_device_id,
    },
    manager::UsbFsManager,
};
use crate::{
    StarryError,
    pseudofs::{Device, DeviceOps, NodeOpsMux, SimpleDir, SimpleDirOps, SimpleFs},
};

pub(super) struct UsbRootDir {
    pub(super) fs: Arc<SimpleFs>,
    pub(super) manager: Arc<UsbFsManager>,
}

impl SimpleDirOps for UsbRootDir {
    fn is_cacheable(&self) -> bool {
        false
    }

    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut names = self
            .manager
            .bus_numbers()
            .into_iter()
            .map(bus_name)
            .collect::<Vec<_>>();
        names.sort();
        Box::new(names.into_iter().map(Cow::Owned))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let Some(bus_num) = parse_numeric_component(name) else {
            return Err(VfsError::NotFound);
        };
        if !self.manager.bus_numbers().contains(&bus_num) {
            return Err(VfsError::NotFound);
        }

        let fs = self.fs.clone();
        let manager = self.manager.clone();
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            fs.clone(),
            Arc::new(UsbBusDir {
                fs,
                manager,
                bus_num,
            }),
        )))
    }
}

struct UsbBusDir {
    fs: Arc<SimpleFs>,
    manager: Arc<UsbFsManager>,
    bus_num: u8,
}

impl SimpleDirOps for UsbBusDir {
    fn is_cacheable(&self) -> bool {
        false
    }

    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut names = self
            .manager
            .device_numbers(self.bus_num)
            .into_iter()
            .map(device_name)
            .collect::<Vec<_>>();
        names.sort();
        Box::new(names.into_iter().map(Cow::Owned))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let Some(device_num) = parse_numeric_component(name) else {
            return Err(VfsError::NotFound);
        };
        if self
            .manager
            .device_snapshot(self.bus_num, device_num)
            .is_none()
        {
            return Err(VfsError::NotFound);
        }

        Ok(NodeOpsMux::File(Device::new(
            self.fs.clone(),
            NodeType::CharacterDevice,
            usb_device_id(self.bus_num, device_num),
            Arc::new(UsbDeviceOps {
                manager: self.manager.clone(),
                bus_num: self.bus_num,
                device_num,
            }),
        )))
    }
}

pub(super) struct UsbDeviceOps {
    pub(super) manager: Arc<UsbFsManager>,
    pub(super) bus_num: u8,
    pub(super) device_num: u8,
}

impl DeviceOps for UsbDeviceOps {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let snapshot = self
            .manager
            .device_snapshot(self.bus_num, self.device_num)
            .ok_or(VfsError::NotFound)?;
        let offset = offset as usize;
        if offset >= snapshot.descriptor_blob.len() {
            return Ok(0);
        }
        let data = &snapshot.descriptor_blob[offset..];
        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        let snapshot = self
            .manager
            .device_snapshot(self.bus_num, self.device_num)
            .ok_or(VfsError::NotFound)?;
        match cmd {
            USBDEVFS_CONNECTINFO => {
                (arg as *mut UsbdevfsConnectInfo)
                    .vm_write(UsbdevfsConnectInfo {
                        devnum: snapshot.device_num as u32,
                        slow: 0,
                        _padding: [0; 3],
                    })
                    .map_err(|error| VfsError::from(StarryError::from(error)))?;
                Ok(0)
            }
            USBDEVFS_GET_CAPABILITIES => {
                (arg as *mut u32)
                    .vm_write(USBDEVFS_CAP_BULK_CONTINUATION)
                    .map_err(|error| VfsError::from(StarryError::from(error)))?;
                Ok(0)
            }
            USBDEVFS_CONTROL => {
                Ok(self
                    .manager
                    .snapshot_device_ioctl(self.bus_num, self.device_num, cmd, arg)?)
            }
            _ => Err(VfsError::Unsupported),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }
}

impl Pollable for UsbDeviceOps {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}
