use alloc::{borrow::Cow, boxed::Box, format, string::String, sync::Arc, vec::Vec};

use axfs_ng_vfs::{Filesystem, NodeType, VfsResult};

use super::{
    descriptor::{UsbDeviceSnapshot, bus_name, device_name},
    irq,
    manager::UsbFsManager,
};
use crate::pseudofs::{NodeOpsMux, SimpleDir, SimpleDirOps, SimpleFile, SimpleFs};

const SYSFS_MAGIC: u32 = 0x6265_6572;

pub(super) fn new_bus_usb_sysfs() -> Filesystem {
    SimpleFs::new_with("sysfs".into(), SYSFS_MAGIC, |fs| {
        SimpleDir::new_maker(
            fs.clone(),
            Arc::new(SysUsbDir {
                fs,
                manager: irq::manager(),
            }),
        )
    })
}

fn dir(fs: Arc<SimpleFs>, ops: impl SimpleDirOps) -> NodeOpsMux {
    NodeOpsMux::Dir(SimpleDir::new_maker(fs, Arc::new(ops)))
}

fn text_file(fs: Arc<SimpleFs>, text: impl Into<Vec<u8>>) -> NodeOpsMux {
    let text = text.into();
    SimpleFile::new_regular(fs, move || -> VfsResult<Vec<u8>> { Ok(text.clone()) }).into()
}

fn symlink(fs: Arc<SimpleFs>, target: impl Into<String>) -> NodeOpsMux {
    let target = target.into().into_bytes();
    SimpleFile::new(fs, NodeType::Symlink, move || -> VfsResult<Vec<u8>> {
        Ok(target.clone())
    })
    .into()
}

struct SysUsbDir {
    fs: Arc<SimpleFs>,
    manager: Option<Arc<UsbFsManager>>,
}

impl SimpleDirOps for SysUsbDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["devices", "device-nodes"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "devices" => Ok(dir(
                self.fs.clone(),
                SysUsbDevicesDir {
                    fs: self.fs.clone(),
                    manager: self.manager.clone(),
                },
            )),
            "device-nodes" => Ok(dir(
                self.fs.clone(),
                SysUsbDeviceNodesDir {
                    fs: self.fs.clone(),
                    manager: self.manager.clone(),
                },
            )),
            _ => Err(ax_errno::AxError::NotFound),
        }
    }
}

struct SysUsbDevicesDir {
    fs: Arc<SimpleFs>,
    manager: Option<Arc<UsbFsManager>>,
}

impl SysUsbDevicesDir {
    fn snapshots(&self) -> Vec<UsbDeviceSnapshot> {
        let Some(manager) = &self.manager else {
            return Vec::new();
        };
        let mut snapshots = Vec::new();
        for bus_num in manager.bus_numbers() {
            for device_num in manager.device_numbers(bus_num) {
                if let Some(snapshot) = manager.device_snapshot(bus_num, device_num) {
                    snapshots.push(snapshot);
                }
            }
        }
        snapshots.sort_by_key(|snapshot| (snapshot.bus_num, snapshot.device_num));
        snapshots
    }
}

impl SimpleDirOps for SysUsbDevicesDir {
    fn is_cacheable(&self) -> bool {
        false
    }

    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            self.snapshots()
                .into_iter()
                .map(|snapshot| Cow::Owned(sysfs_device_name(&snapshot))),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        self.snapshots()
            .into_iter()
            .find(|snapshot| sysfs_device_name(snapshot) == name)
            .ok_or(ax_errno::AxError::NotFound)?;
        Ok(symlink(self.fs.clone(), format!("../device-nodes/{name}")))
    }
}

struct SysUsbDeviceNodesDir {
    fs: Arc<SimpleFs>,
    manager: Option<Arc<UsbFsManager>>,
}

impl SysUsbDeviceNodesDir {
    fn snapshots(&self) -> Vec<UsbDeviceSnapshot> {
        let Some(manager) = &self.manager else {
            return Vec::new();
        };
        let mut snapshots = Vec::new();
        for bus_num in manager.bus_numbers() {
            for device_num in manager.device_numbers(bus_num) {
                if let Some(snapshot) = manager.device_snapshot(bus_num, device_num) {
                    snapshots.push(snapshot);
                }
            }
        }
        snapshots.sort_by_key(|snapshot| (snapshot.bus_num, snapshot.device_num));
        snapshots
    }
}

impl SimpleDirOps for SysUsbDeviceNodesDir {
    fn is_cacheable(&self) -> bool {
        false
    }

    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            self.snapshots()
                .into_iter()
                .map(|snapshot| Cow::Owned(sysfs_device_name(&snapshot))),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let snapshot = self
            .snapshots()
            .into_iter()
            .find(|snapshot| sysfs_device_name(snapshot) == name)
            .ok_or(ax_errno::AxError::NotFound)?;
        Ok(dir(
            self.fs.clone(),
            SysUsbDeviceDir {
                fs: self.fs.clone(),
                snapshot,
            },
        ))
    }
}

struct SysUsbDeviceDir {
    fs: Arc<SimpleFs>,
    snapshot: UsbDeviceSnapshot,
}

impl SimpleDirOps for SysUsbDeviceDir {
    fn is_cacheable(&self) -> bool {
        false
    }

    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            [
                "uevent",
                "dev",
                "busnum",
                "devnum",
                "speed",
                "descriptors",
                "bConfigurationValue",
                "idVendor",
                "idProduct",
                "bDeviceClass",
                "bDeviceSubClass",
                "bDeviceProtocol",
                "subsystem",
            ]
            .into_iter()
            .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "uevent" => Ok(text_file(self.fs.clone(), self.uevent())),
            "dev" => Ok(text_file(self.fs.clone(), format!("{}\n", self.dev_id()))),
            "busnum" => Ok(text_file(
                self.fs.clone(),
                format!("{}\n", self.snapshot.bus_num),
            )),
            "devnum" => Ok(text_file(
                self.fs.clone(),
                format!("{}\n", self.snapshot.device_num),
            )),
            "speed" => Ok(text_file(self.fs.clone(), "480\n")),
            "descriptors" => Ok(text_file(
                self.fs.clone(),
                self.snapshot.descriptor_blob.clone(),
            )),
            "bConfigurationValue" => Ok(text_file(
                self.fs.clone(),
                format!("{}\n", self.active_configuration()),
            )),
            "idVendor" => Ok(text_file(
                self.fs.clone(),
                format!("{:04x}\n", self.vendor_id()),
            )),
            "idProduct" => Ok(text_file(
                self.fs.clone(),
                format!("{:04x}\n", self.product_id()),
            )),
            "bDeviceClass" => Ok(text_file(
                self.fs.clone(),
                format!("{:02x}\n", self.device_class()),
            )),
            "bDeviceSubClass" => Ok(text_file(
                self.fs.clone(),
                format!("{:02x}\n", self.device_subclass()),
            )),
            "bDeviceProtocol" => Ok(text_file(
                self.fs.clone(),
                format!("{:02x}\n", self.device_protocol()),
            )),
            "subsystem" => Ok(symlink(self.fs.clone(), "../../usb")),
            _ => Err(ax_errno::AxError::NotFound),
        }
    }
}

impl SysUsbDeviceDir {
    fn minor(&self) -> u32 {
        (self.snapshot.bus_num.saturating_sub(1) as u32) * 128
            + self.snapshot.device_num.saturating_sub(1) as u32
    }

    fn dev_id(&self) -> String {
        format!("189:{}", self.minor())
    }

    fn uevent(&self) -> Vec<u8> {
        format!(
            concat!(
                "MAJOR=189\n",
                "MINOR={}\n",
                "DEVNAME=bus/usb/{}/{}\n",
                "DEVTYPE=usb_device\n",
                "DRIVER=usb\n",
                "PRODUCT={:x}/{:x}/{:x}\n",
                "TYPE={}/{}/{}\n",
                "BUSNUM={}\n",
                "DEVNUM={}\n",
            ),
            self.minor(),
            bus_name(self.snapshot.bus_num),
            device_name(self.snapshot.device_num),
            self.vendor_id(),
            self.product_id(),
            self.device_version(),
            self.device_class(),
            self.device_subclass(),
            self.device_protocol(),
            bus_name(self.snapshot.bus_num),
            device_name(self.snapshot.device_num),
        )
        .into_bytes()
    }

    fn descriptor_u8(&self, offset: usize) -> u8 {
        self.snapshot
            .descriptor_blob
            .get(offset)
            .copied()
            .unwrap_or_default()
    }

    fn descriptor_u16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.descriptor_u8(offset), self.descriptor_u8(offset + 1)])
    }

    fn vendor_id(&self) -> u16 {
        self.descriptor_u16(8)
    }

    fn product_id(&self) -> u16 {
        self.descriptor_u16(10)
    }

    fn device_version(&self) -> u16 {
        self.descriptor_u16(12)
    }

    fn device_class(&self) -> u8 {
        self.descriptor_u8(4)
    }

    fn device_subclass(&self) -> u8 {
        self.descriptor_u8(5)
    }

    fn device_protocol(&self) -> u8 {
        self.descriptor_u8(6)
    }

    fn active_configuration(&self) -> u8 {
        if self.snapshot.descriptor_blob.len() > 23
            && self.snapshot.descriptor_blob[18] == 9
            && self.snapshot.descriptor_blob[19] == 0x02
        {
            return self.snapshot.descriptor_blob[23];
        }
        1
    }
}

fn sysfs_device_name(snapshot: &UsbDeviceSnapshot) -> String {
    if snapshot.device_num == 1 {
        format!("usb{}", snapshot.bus_num)
    } else {
        format!("{}-{}", snapshot.bus_num, snapshot.device_num)
    }
}
