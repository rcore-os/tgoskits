//! Virtio-mmio register handling for the emulated network device.

use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceError, DeviceResult, EmuDeviceType};
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange};

use super::{VIRTIO_F_VERSION_1, VIRTIO_NET_F_MRG_RXBUF, VirtioNet, queue::QueueAddressKind};

const VIRTIO_MMIO_MAGIC_VALUE: usize = 0x000;
const VIRTIO_MMIO_VERSION: usize = 0x004;
const VIRTIO_MMIO_DEVICE_ID: usize = 0x008;
const VIRTIO_MMIO_VENDOR_ID: usize = 0x00c;
const VIRTIO_MMIO_DEVICE_FEATURES: usize = 0x010;
const VIRTIO_MMIO_DEVICE_FEATURES_SEL: usize = 0x014;
const VIRTIO_MMIO_DRIVER_FEATURES: usize = 0x020;
const VIRTIO_MMIO_DRIVER_FEATURES_SEL: usize = 0x024;
const VIRTIO_MMIO_QUEUE_SEL: usize = 0x030;
const VIRTIO_MMIO_QUEUE_NUM_MAX: usize = 0x034;
const VIRTIO_MMIO_QUEUE_NUM: usize = 0x038;
const VIRTIO_MMIO_QUEUE_READY: usize = 0x044;
pub(super) const VIRTIO_MMIO_QUEUE_NOTIFY: usize = 0x050;
const VIRTIO_MMIO_INTERRUPT_STATUS: usize = 0x060;
const VIRTIO_MMIO_INTERRUPT_ACK: usize = 0x064;
const VIRTIO_MMIO_STATUS: usize = 0x070;
const VIRTIO_MMIO_QUEUE_DESC_LOW: usize = 0x080;
const VIRTIO_MMIO_QUEUE_DESC_HIGH: usize = 0x084;
const VIRTIO_MMIO_QUEUE_DRIVER_LOW: usize = 0x090;
const VIRTIO_MMIO_QUEUE_DRIVER_HIGH: usize = 0x094;
const VIRTIO_MMIO_QUEUE_DEVICE_LOW: usize = 0x0a0;
const VIRTIO_MMIO_QUEUE_DEVICE_HIGH: usize = 0x0a4;
const VIRTIO_MMIO_CONFIG_GENERATION: usize = 0x0fc;
const VIRTIO_MMIO_CONFIG: usize = 0x100;

const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_MMIO_VERSION_2: u32 = 2;
const VIRTIO_ID_NET: u32 = 1;
const VIRTIO_VENDOR_ID: u32 = 0x1af4;
const VIRTIO_NET_F_MAC: u64 = 1 << 5;
const DEVICE_FEATURES: u64 = VIRTIO_NET_F_MAC | VIRTIO_NET_F_MRG_RXBUF | VIRTIO_F_VERSION_1;

impl VirtioNet {
    fn read_register(&self, offset: usize) -> u32 {
        let state = self.state.lock();
        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC,
            VIRTIO_MMIO_VERSION => VIRTIO_MMIO_VERSION_2,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_ID_NET,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_VENDOR_ID,
            VIRTIO_MMIO_DEVICE_FEATURES => match state.device_features_sel {
                0 => DEVICE_FEATURES as u32,
                1 => (DEVICE_FEATURES >> 32) as u32,
                _ => 0,
            },
            VIRTIO_MMIO_QUEUE_NUM_MAX => state
                .selected_queue()
                .map(|_| u32::from(super::queue::QUEUE_NUM_MAX))
                .unwrap_or(0),
            VIRTIO_MMIO_QUEUE_READY => state
                .selected_queue()
                .map(|queue| queue.ready_value())
                .unwrap_or(0),
            VIRTIO_MMIO_INTERRUPT_STATUS => state.interrupt_status,
            VIRTIO_MMIO_STATUS => state.status,
            VIRTIO_MMIO_CONFIG_GENERATION => 0,
            offset
                if (VIRTIO_MMIO_CONFIG..VIRTIO_MMIO_CONFIG + self.mac.len()).contains(&offset) =>
            {
                u32::from(self.mac[offset - VIRTIO_MMIO_CONFIG])
            }
            _ => 0,
        }
    }

    fn write_register(&self, offset: usize, value: u32) -> DeviceResult {
        let mut state = self.state.lock();
        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => state.device_features_sel = value,
            VIRTIO_MMIO_DRIVER_FEATURES => {
                let feature_word = state.driver_features_sel as usize;
                if let Some(features) = state.driver_features.get_mut(feature_word) {
                    *features = value;
                }
            }
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => state.driver_features_sel = value,
            VIRTIO_MMIO_QUEUE_SEL => state.queue_sel = value,
            VIRTIO_MMIO_QUEUE_NUM => {
                if let Some(queue) = state.selected_queue_mut() {
                    queue.set_size(value)?;
                }
            }
            VIRTIO_MMIO_QUEUE_READY => {
                if let Some(queue) = state.selected_queue_mut() {
                    queue.set_ready(value)?;
                }
            }
            VIRTIO_MMIO_QUEUE_NOTIFY => {}
            VIRTIO_MMIO_INTERRUPT_ACK => state.interrupt_status &= !value,
            VIRTIO_MMIO_STATUS if value == 0 => state.reset(),
            VIRTIO_MMIO_STATUS => state.status = value,
            VIRTIO_MMIO_QUEUE_DESC_LOW => {
                Self::set_queue_address(&mut state, QueueAddressKind::Descriptor, false, value)?;
            }
            VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                Self::set_queue_address(&mut state, QueueAddressKind::Descriptor, true, value)?;
            }
            VIRTIO_MMIO_QUEUE_DRIVER_LOW => {
                Self::set_queue_address(&mut state, QueueAddressKind::Driver, false, value)?;
            }
            VIRTIO_MMIO_QUEUE_DRIVER_HIGH => {
                Self::set_queue_address(&mut state, QueueAddressKind::Driver, true, value)?;
            }
            VIRTIO_MMIO_QUEUE_DEVICE_LOW => {
                Self::set_queue_address(&mut state, QueueAddressKind::Device, false, value)?;
            }
            VIRTIO_MMIO_QUEUE_DEVICE_HIGH => {
                Self::set_queue_address(&mut state, QueueAddressKind::Device, true, value)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn set_queue_address(
        state: &mut super::VirtioNetState,
        kind: QueueAddressKind,
        high: bool,
        value: u32,
    ) -> DeviceResult {
        if let Some(queue) = state.selected_queue_mut() {
            queue.set_address(kind, high, value)?;
        }
        Ok(())
    }

    fn mmio_offset(&self, address: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        let raw_address = address.as_usize();
        let offset =
            raw_address
                .checked_sub(self.base.as_usize())
                .ok_or(DeviceError::OutOfRange {
                    addr: raw_address as u64,
                })?;
        let access_size = match width {
            AccessWidth::Byte => 1,
            AccessWidth::Word => 2,
            AccessWidth::Dword => 4,
            AccessWidth::Qword => 8,
        };
        offset
            .checked_add(access_size)
            .filter(|end| *end <= self.size)
            .ok_or(DeviceError::OutOfRange {
                addr: raw_address as u64,
            })?;
        Ok(offset)
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for VirtioNet {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::VirtioNet
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        self.guest_address_range()
    }

    fn handle_read(&self, address: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        let offset = self.mmio_offset(address, width)?;
        let value = match width {
            AccessWidth::Byte => self.read_register(offset) & 0xff,
            AccessWidth::Word => self.read_register(offset) & 0xffff,
            AccessWidth::Dword | AccessWidth::Qword => self.read_register(offset),
        };
        Ok(value as usize)
    }

    fn handle_write(
        &self,
        address: GuestPhysAddr,
        width: AccessWidth,
        value: usize,
    ) -> DeviceResult {
        let offset = self.mmio_offset(address, width)?;
        let value = match width {
            AccessWidth::Byte => (value & 0xff) as u32,
            AccessWidth::Word => (value & 0xffff) as u32,
            AccessWidth::Dword | AccessWidth::Qword => value as u32,
        };
        self.write_register(offset, value)
    }
}
