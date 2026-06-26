use alloc::{
    collections::{BTreeMap, btree_set::BTreeSet},
    vec::Vec,
};

use ax_kspin::SpinRaw as Mutex;
use spin::Once;
use usb_if::descriptor::{
    ConfigurationDescriptor, DeviceDescriptor, parse_concatenated_config_descriptors,
};

use crate::{
    Descriptor, DeviceId, PlatformDevice, ProbeError,
    probe::OnProbeError,
    register::{DriverRegister, ProbeKind},
};

static USB: Once<Mutex<System>> = Once::new();

pub type FnOnProbe = for<'a> fn(ProbeUsb<'a>) -> Result<(), OnProbeError>;
pub type FnOnRemove = fn(UsbRemove);

/// Stable identity for a USB device on a specific host controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UsbDeviceKey {
    pub host: DeviceId,
    pub device: usize,
}

impl UsbDeviceKey {
    pub const fn new(host: DeviceId, device: usize) -> Self {
        Self { host, device }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbDeviceId {
    pub vendor_id: u16,
    pub product_id: u16,
}

/// USB class tuple from a device or interface descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbClass {
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

/// USB class match pattern. `None` means wildcard for that field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbClassId {
    pub class: u8,
    pub subclass: Option<u8>,
    pub protocol: Option<u8>,
}

impl UsbClassId {
    pub const fn new(class: u8, subclass: Option<u8>, protocol: Option<u8>) -> Self {
        Self {
            class,
            subclass,
            protocol,
        }
    }

    pub const fn matches(&self, class: UsbClass) -> bool {
        self.class == class.class
            && match self.subclass {
                Some(subclass) => subclass == class.subclass,
                None => true,
            }
            && match self.protocol {
                Some(protocol) => protocol == class.protocol,
                None => true,
            }
    }
}

/// Select whether a class match applies to the device descriptor or interfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbClassMatch {
    Device(UsbClassId),
    Interface(UsbClassId),
}

/// Matched USB interface descriptor metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbInterfaceInfo {
    pub interface_number: u8,
    pub alternate_setting: u8,
    pub class: UsbClass,
}

/// Snapshot of a USB device used by rdrive's USB probe backend.
#[derive(Clone, Debug)]
pub struct UsbDevice {
    key: UsbDeviceKey,
    bus_num: u8,
    device_num: u8,
    descriptor_blob: Vec<u8>,
}

impl UsbDevice {
    pub fn new(key: UsbDeviceKey, bus_num: u8, device_num: u8, descriptor_blob: Vec<u8>) -> Self {
        Self {
            key,
            bus_num,
            device_num,
            descriptor_blob,
        }
    }

    pub const fn key(&self) -> UsbDeviceKey {
        self.key
    }

    pub const fn bus_num(&self) -> u8 {
        self.bus_num
    }

    pub const fn device_num(&self) -> u8 {
        self.device_num
    }

    pub fn descriptor_blob(&self) -> &[u8] {
        &self.descriptor_blob
    }

    pub fn device_id(&self) -> Option<UsbDeviceId> {
        device_id_from_descriptor_blob(&self.descriptor_blob)
    }
}

/// Information passed to a USB driver's probe callback.
#[derive(Clone, Copy, Debug)]
pub struct UsbInfo<'a> {
    device: &'a UsbDevice,
    device_id: Option<UsbDeviceId>,
    device_class: Option<UsbClass>,
    interface: Option<UsbInterfaceInfo>,
}

impl<'a> UsbInfo<'a> {
    fn new(device: &'a UsbDevice, interface: Option<UsbInterfaceInfo>) -> Self {
        Self {
            device,
            device_id: device.device_id(),
            device_class: device_class_from_descriptor_blob(device.descriptor_blob()),
            interface,
        }
    }

    pub const fn key(&self) -> UsbDeviceKey {
        self.device.key
    }

    pub const fn bus_num(&self) -> u8 {
        self.device.bus_num
    }

    pub const fn device_num(&self) -> u8 {
        self.device.device_num
    }

    pub const fn device_id(&self) -> Option<UsbDeviceId> {
        self.device_id
    }

    pub const fn device_class(&self) -> Option<UsbClass> {
        self.device_class
    }

    pub const fn interface(&self) -> Option<UsbInterfaceInfo> {
        self.interface
    }

    pub fn descriptor_blob(&self) -> &[u8] {
        self.device.descriptor_blob()
    }
}

pub struct ProbeUsb<'a> {
    info: UsbInfo<'a>,
    platform: PlatformDevice,
}

impl<'a> ProbeUsb<'a> {
    fn new(info: UsbInfo<'a>, platform: PlatformDevice) -> Self {
        Self { info, platform }
    }

    pub const fn info(&self) -> UsbInfo<'a> {
        self.info
    }

    pub fn into_platform_device(self) -> PlatformDevice {
        self.platform
    }

    pub fn into_parts(self) -> (UsbInfo<'a>, PlatformDevice) {
        (self.info, self.platform)
    }
}

/// Information passed to a USB driver's remove callback.
#[derive(Clone, Copy, Debug)]
pub struct UsbRemove {
    key: UsbDeviceKey,
    device_id: DeviceId,
    bus_num: u8,
    device_num: u8,
    interface: Option<UsbInterfaceInfo>,
}

impl UsbRemove {
    pub const fn key(&self) -> UsbDeviceKey {
        self.key
    }

    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    pub const fn bus_num(&self) -> u8 {
        self.bus_num
    }

    pub const fn device_num(&self) -> u8 {
        self.device_num
    }

    pub const fn interface(&self) -> Option<UsbInterfaceInfo> {
        self.interface
    }
}

pub(crate) fn sync_host_with(
    host: DeviceId,
    registers: &[DriverRegister],
    devices: &[UsbDevice],
    stop_if_fail: bool,
) -> Result<(), ProbeError> {
    USB.call_once(|| Mutex::new(System::new()))
        .lock()
        .sync_host(host, registers, devices, stop_if_fail)
}

struct System {
    bindings: BTreeMap<UsbProbeTarget, UsbBinding>,
}

impl System {
    fn new() -> Self {
        Self {
            bindings: BTreeMap::new(),
        }
    }

    fn sync_host(
        &mut self,
        host: DeviceId,
        registers: &[DriverRegister],
        devices: &[UsbDevice],
        stop_if_fail: bool,
    ) -> Result<(), ProbeError> {
        let current = matching_targets(registers, devices);
        let stale = self
            .bindings
            .iter()
            .filter(|(target, binding)| {
                target.device.host == host
                    && !current.contains(&UsbProbeMatch {
                        target: **target,
                        identity: binding.identity,
                    })
            })
            .map(|(target, _)| *target)
            .collect::<Vec<_>>();
        for target in stale {
            self.remove_binding(target);
        }
        for device in devices {
            self.probe_one(device, registers, stop_if_fail)?;
        }
        Ok(())
    }

    fn probe_one(
        &mut self,
        device: &UsbDevice,
        registers: &[DriverRegister],
        stop_if_fail: bool,
    ) -> Result<(), ProbeError> {
        for register in registers {
            for probe in register.probe_kinds {
                let ProbeKind::Usb {
                    ids,
                    classes,
                    on_probe,
                    on_remove,
                } = probe
                else {
                    continue;
                };

                for info in matching_infos(device, ids, classes) {
                    let target = UsbProbeTarget::from_info(info);
                    if self.bindings.contains_key(&target) {
                        continue;
                    }

                    let mut descriptor = Descriptor::new();
                    descriptor.name = register.name;
                    let device_id = descriptor.device_id();
                    let res = (on_probe)(ProbeUsb::new(info, PlatformDevice::new(descriptor)));
                    match res {
                        Ok(()) => {
                            self.bindings.insert(
                                target,
                                UsbBinding {
                                    identity: UsbProbeIdentity::new(register.name, *on_probe),
                                    remove: *on_remove,
                                    info: UsbRemove {
                                        key: info.key(),
                                        device_id,
                                        bus_num: info.bus_num(),
                                        device_num: info.device_num(),
                                        interface: info.interface(),
                                    },
                                },
                            );
                        }
                        Err(OnProbeError::NotMatch) => {}
                        Err(err) => {
                            if stop_if_fail {
                                return Err(err.into());
                            }
                            warn!("Probe failed for [{}]: {}", register.name, err);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn remove_binding(&mut self, target: UsbProbeTarget) {
        let Some(binding) = self.bindings.remove(&target) else {
            return;
        };
        if let Some(remove) = binding.remove {
            remove(binding.info);
        }
        crate::edit(|manager| {
            manager.dev_container.remove(binding.info.device_id());
        });
    }
}

struct UsbBinding {
    identity: UsbProbeIdentity,
    remove: Option<FnOnRemove>,
    info: UsbRemove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UsbProbeTarget {
    device: UsbDeviceKey,
    interface: Option<u8>,
}

impl UsbProbeTarget {
    fn from_info(info: UsbInfo<'_>) -> Self {
        Self {
            device: info.key(),
            interface: info.interface().map(|interface| interface.interface_number),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UsbProbeIdentity {
    driver: &'static str,
    on_probe: usize,
}

impl UsbProbeIdentity {
    fn new(driver: &'static str, on_probe: FnOnProbe) -> Self {
        Self {
            driver,
            on_probe: on_probe as usize,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UsbProbeMatch {
    target: UsbProbeTarget,
    identity: UsbProbeIdentity,
}

impl UsbProbeMatch {
    fn new(identity: UsbProbeIdentity, info: UsbInfo<'_>) -> Self {
        Self {
            target: UsbProbeTarget::from_info(info),
            identity,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UsbProbeDedupeKey {
    device: UsbDeviceKey,
    interface: Option<u8>,
}

impl UsbProbeDedupeKey {
    fn from_info(info: UsbInfo<'_>) -> Self {
        Self {
            device: info.key(),
            interface: info.interface().map(|interface| interface.interface_number),
        }
    }
}

fn matching_infos<'a>(
    device: &'a UsbDevice,
    ids: &[UsbDeviceId],
    classes: &[UsbClassMatch],
) -> Vec<UsbInfo<'a>> {
    let device_id = device.device_id();
    if !matches_usb_id(device_id, ids) {
        return Vec::new();
    }

    if classes.is_empty() {
        return vec![UsbInfo::new(device, None)];
    }

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let device_class = device_class_from_descriptor_blob(device.descriptor_blob());
    let interfaces = interfaces_from_descriptor_blob(device.descriptor_blob());

    for class_match in classes {
        match class_match {
            UsbClassMatch::Device(class_id) => {
                if device_class.is_some_and(|class| class_id.matches(class)) {
                    push_matching_info(&mut out, &mut seen, UsbInfo::new(device, None));
                }
            }
            UsbClassMatch::Interface(class_id) => {
                for interface in &interfaces {
                    if class_id.matches(interface.class) {
                        push_matching_info(
                            &mut out,
                            &mut seen,
                            UsbInfo::new(device, Some(*interface)),
                        );
                    }
                }
            }
        }
    }

    out
}

fn matching_targets(
    registers: &[DriverRegister],
    devices: &[UsbDevice],
) -> BTreeSet<UsbProbeMatch> {
    let mut out = BTreeSet::new();
    for device in devices {
        for register in registers {
            for probe in register.probe_kinds {
                let ProbeKind::Usb {
                    ids,
                    classes,
                    on_probe,
                    ..
                } = probe
                else {
                    continue;
                };
                let identity = UsbProbeIdentity::new(register.name, *on_probe);
                for info in matching_infos(device, ids, classes) {
                    out.insert(UsbProbeMatch::new(identity, info));
                }
            }
        }
    }
    out
}

fn push_matching_info<'a>(
    out: &mut Vec<UsbInfo<'a>>,
    seen: &mut BTreeSet<UsbProbeDedupeKey>,
    info: UsbInfo<'a>,
) {
    if seen.insert(UsbProbeDedupeKey::from_info(info)) {
        out.push(info);
    }
}

fn matches_usb_id(device_id: Option<UsbDeviceId>, ids: &[UsbDeviceId]) -> bool {
    ids.is_empty() || device_id.is_some_and(|device_id| ids.contains(&device_id))
}

fn device_class_from_descriptor_blob(blob: &[u8]) -> Option<UsbClass> {
    if blob.first().copied()? != 18 || blob.get(1).copied()? != 0x01 {
        return None;
    }
    Some(UsbClass {
        class: *blob.get(4)?,
        subclass: *blob.get(5)?,
        protocol: *blob.get(6)?,
    })
}

fn device_id_from_descriptor_blob(blob: &[u8]) -> Option<UsbDeviceId> {
    if blob.first().copied()? != 18 || blob.get(1).copied()? != 0x01 {
        return None;
    }
    let vendor_id = u16::from_le_bytes([*blob.get(8)?, *blob.get(9)?]);
    let product_id = u16::from_le_bytes([*blob.get(10)?, *blob.get(11)?]);
    Some(UsbDeviceId {
        vendor_id,
        product_id,
    })
}

fn interfaces_from_descriptor_blob(blob: &[u8]) -> Vec<UsbInterfaceInfo> {
    let Some(device_len) = blob.first().copied().map(usize::from) else {
        return Vec::new();
    };
    let mut cursor = device_len;
    let mut out = Vec::new();

    while cursor + 2 <= blob.len() {
        let len = blob[cursor] as usize;
        let desc_type = blob[cursor + 1];
        if len == 0 || cursor + len > blob.len() {
            break;
        }

        if desc_type == 0x04 && len >= 9 {
            out.push(UsbInterfaceInfo {
                interface_number: blob[cursor + 2],
                alternate_setting: blob[cursor + 3],
                class: UsbClass {
                    class: blob[cursor + 5],
                    subclass: blob[cursor + 6],
                    protocol: blob[cursor + 7],
                },
            });
        }
        cursor += len;
    }

    out
}
