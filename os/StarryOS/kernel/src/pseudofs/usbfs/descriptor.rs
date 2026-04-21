use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use core::mem::size_of;

use axfs_ng_vfs::{DeviceId, VfsResult};
use crab_usb::{DeviceInfo, usb_if};
use linux_raw_sys::general::{
    _IOC_DIRSHIFT, _IOC_NRSHIFT, _IOC_READ, _IOC_SIZESHIFT, _IOC_TYPESHIFT, _IOC_WRITE,
};

use crate::mm::UserConstPtr;

pub(super) const USBFS_MAGIC: u32 = 0x9fa2;
const USB_MAJOR: u32 = 189;
pub(super) const USBDEVFS_CAP_BULK_CONTINUATION: u32 = 0x02;
pub(super) const USB_REQ_GET_CONFIGURATION: u8 = 0x08;
pub(super) const USB_REQTYPE_DEVICE_TO_HOST_STANDARD_DEVICE: u8 = 0x80;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct UsbdevfsCtrlTransfer {
    pub(super) b_request_type: u8,
    pub(super) b_request: u8,
    pub(super) w_value: u16,
    pub(super) w_index: u16,
    pub(super) w_length: u16,
    pub(super) timeout: u32,
    pub(super) data: *mut u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct UsbdevfsConnectInfo {
    pub(super) devnum: u32,
    pub(super) slow: u8,
    pub(super) _padding: [u8; 3],
}

const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    (dir << _IOC_DIRSHIFT)
        | ((ty as u32) << _IOC_TYPESHIFT)
        | ((nr as u32) << _IOC_NRSHIFT)
        | ((size as u32) << _IOC_SIZESHIFT)
}

const fn ior<T>(ty: u8, nr: u8) -> u32 {
    ioc(_IOC_READ, ty, nr, size_of::<T>())
}

const fn iowr<T>(ty: u8, nr: u8) -> u32 {
    ioc(_IOC_READ | _IOC_WRITE, ty, nr, size_of::<T>())
}

pub(super) const USBDEVFS_CONTROL: u32 = iowr::<UsbdevfsCtrlTransfer>(b'U', 0);
pub(super) const USBDEVFS_CONNECTINFO: u32 = ior::<UsbdevfsConnectInfo>(b'U', 17);
pub(super) const USBDEVFS_GET_CAPABILITIES: u32 = ior::<u32>(b'U', 26);

#[derive(Clone)]
pub(super) struct UsbDeviceSnapshot {
    pub(super) bus_num: u8,
    pub(super) device_num: u8,
    pub(super) active_configuration: u8,
    pub(super) descriptor_blob: Vec<u8>,
}

pub(super) fn read_usbdevfs_ctrltransfer(arg: usize) -> VfsResult<UsbdevfsCtrlTransfer> {
    let bytes = UserConstPtr::<u8>::from(arg).get_as_slice(size_of::<UsbdevfsCtrlTransfer>())?;
    let mut index = 0usize;
    let read_u8 = |bytes: &[u8], index: &mut usize| {
        let value = bytes[*index];
        *index += 1;
        value
    };
    let read_u16 = |bytes: &[u8], index: &mut usize| {
        let value = u16::from_le_bytes([bytes[*index], bytes[*index + 1]]);
        *index += 2;
        value
    };
    let read_u32 = |bytes: &[u8], index: &mut usize| {
        let value = u32::from_le_bytes([
            bytes[*index],
            bytes[*index + 1],
            bytes[*index + 2],
            bytes[*index + 3],
        ]);
        *index += 4;
        value
    };
    let read_usize = |bytes: &[u8], index: &mut usize| {
        let mut raw = [0u8; size_of::<usize>()];
        raw.copy_from_slice(&bytes[*index..*index + size_of::<usize>()]);
        *index += size_of::<usize>();
        usize::from_le_bytes(raw)
    };

    let b_request_type = read_u8(bytes, &mut index);
    let b_request = read_u8(bytes, &mut index);
    let w_value = read_u16(bytes, &mut index);
    let w_index = read_u16(bytes, &mut index);
    let w_length = read_u16(bytes, &mut index);
    let timeout = read_u32(bytes, &mut index);
    let data = read_usize(bytes, &mut index) as *mut u8;

    Ok(UsbdevfsCtrlTransfer {
        b_request_type,
        b_request,
        w_value,
        w_index,
        w_length,
        timeout,
        data,
    })
}

pub(super) fn snapshot_device_info(
    bus_num: u8,
    next_device_num: &mut u8,
    stable_id_to_device_num: &mut BTreeMap<usize, u8>,
    info: &DeviceInfo,
) -> UsbDeviceSnapshot {
    let stable_id = info.id();
    let device_num = match stable_id_to_device_num.get(&stable_id).copied() {
        Some(device_num) => device_num,
        None => {
            let device_num = *next_device_num;
            *next_device_num = (*next_device_num).saturating_add(1);
            stable_id_to_device_num.insert(stable_id, device_num);
            device_num
        }
    };

    UsbDeviceSnapshot {
        bus_num,
        device_num,
        active_configuration: info
            .configurations()
            .first()
            .map(|config| config.configuration_value)
            .unwrap_or(0),
        descriptor_blob: serialize_descriptor_blob(info),
    }
}

fn serialize_descriptor_blob(info: &DeviceInfo) -> Vec<u8> {
    let mut out = Vec::new();
    let desc = info.descriptor();
    out.push(18);
    out.push(0x01);
    out.extend_from_slice(&desc.usb_version.to_le_bytes());
    out.push(desc.class);
    out.push(desc.subclass);
    out.push(desc.protocol);
    out.push(desc.max_packet_size_0);
    out.extend_from_slice(&desc.vendor_id.to_le_bytes());
    out.extend_from_slice(&desc.product_id.to_le_bytes());
    out.extend_from_slice(&desc.device_version.to_le_bytes());
    out.push(
        desc.manufacturer_string_index
            .map(|index| index.get())
            .unwrap_or(0),
    );
    out.push(
        desc.product_string_index
            .map(|index| index.get())
            .unwrap_or(0),
    );
    out.push(
        desc.serial_number_string_index
            .map(|index| index.get())
            .unwrap_or(0),
    );
    out.push(desc.num_configurations);

    for config in info.configurations() {
        let mut config_blob = Vec::new();
        for interface in &config.interfaces {
            for alt in &interface.alt_settings {
                config_blob.push(9);
                config_blob.push(0x04);
                config_blob.push(alt.interface_number);
                config_blob.push(alt.alternate_setting);
                config_blob.push(alt.num_endpoints);
                config_blob.push(alt.class);
                config_blob.push(alt.subclass);
                config_blob.push(alt.protocol);
                config_blob.push(alt.string_index.map(|index| index.get()).unwrap_or(0));

                for endpoint in &alt.endpoints {
                    config_blob.push(7);
                    config_blob.push(0x05);
                    config_blob.push(endpoint.address);
                    config_blob.push(endpoint_attributes(endpoint.transfer_type));
                    config_blob.extend_from_slice(&endpoint.max_packet_size.to_le_bytes());
                    config_blob.push(endpoint.interval);
                }
            }
        }

        let total_length = (9 + config_blob.len()) as u16;
        out.push(9);
        out.push(0x02);
        out.extend_from_slice(&total_length.to_le_bytes());
        out.push(config.num_interfaces);
        out.push(config.configuration_value);
        out.push(config.string_index.map(|index| index.get()).unwrap_or(0));
        out.push(config.attributes);
        out.push(config.max_power);
        out.extend_from_slice(&config_blob);
    }

    out
}

fn endpoint_attributes(transfer_type: usb_if::descriptor::EndpointType) -> u8 {
    match transfer_type {
        usb_if::descriptor::EndpointType::Control => 0,
        usb_if::descriptor::EndpointType::Isochronous => 1,
        usb_if::descriptor::EndpointType::Bulk => 2,
        usb_if::descriptor::EndpointType::Interrupt => 3,
    }
}

pub(super) fn bus_name(bus_num: u8) -> String {
    format!("{bus_num:03}")
}

pub(super) fn device_name(device_num: u8) -> String {
    format!("{device_num:03}")
}

pub(super) fn parse_numeric_component(name: &str) -> Option<u8> {
    if name.len() != 3 {
        return None;
    }
    name.parse().ok()
}

pub(super) fn usb_device_id(bus_num: u8, device_num: u8) -> DeviceId {
    let minor = ((bus_num.saturating_sub(1) as u32) * 128) + device_num.saturating_sub(1) as u32;
    DeviceId::new(USB_MAJOR, minor)
}
