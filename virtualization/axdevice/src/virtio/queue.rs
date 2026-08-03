//! Split-virtqueue configuration and ring bookkeeping.

use alloc::format;
use core::num::NonZeroU16;

use axdevice_base::{DeviceError, DeviceResult};

use super::memory::{GuestRead, GuestWrite, read_u16, write_u16, write_u32};
use crate::{DeviceManagerError, DeviceManagerResult};

const VIRTQ_DESCRIPTOR_SIZE: u64 = 16;
const VIRTQ_AVAIL_HEADER_SIZE: u64 = 4;
const VIRTQ_USED_HEADER_SIZE: u64 = 4;

pub(crate) const QUEUE_NUM_MAX: u16 = 256;

#[derive(Clone, Copy)]
pub(crate) enum QueueAddressKind {
    Descriptor,
    Driver,
    Device,
}

#[derive(Clone, Copy)]
struct QueueSize(NonZeroU16);

impl QueueSize {
    fn new(value: u32) -> DeviceResult<Self> {
        if value == 0 || value > u32::from(QUEUE_NUM_MAX) || !value.is_power_of_two() {
            return Err(DeviceError::InvalidInput {
                operation: "configure virtio queue size",
                detail: format!("queue size {value} must be a power of two in 1..={QUEUE_NUM_MAX}"),
            });
        }
        let value = u16::try_from(value).map_err(|_| DeviceError::InvalidInput {
            operation: "configure virtio queue size",
            detail: format!("queue size {value} does not fit the split-ring index width"),
        })?;
        Ok(Self(NonZeroU16::new(value).ok_or_else(|| {
            DeviceError::InvalidInput {
                operation: "configure virtio queue size",
                detail: "queue size must not be zero".into(),
            }
        })?))
    }

    fn get(self) -> u16 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Default)]
struct QueueAddress {
    value: u64,
    configured: bool,
}

impl QueueAddress {
    fn set_half(&mut self, high: bool, value: u32) {
        if high {
            self.value = (self.value & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
        } else {
            self.value = (self.value & 0xffff_ffff_0000_0000) | u64::from(value);
        }
        self.configured = true;
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct QueueState {
    size: Option<QueueSize>,
    ready: bool,
    descriptor: QueueAddress,
    driver: QueueAddress,
    device: QueueAddress,
    last_avail: u16,
}

impl QueueState {
    pub(crate) fn set_size(&mut self, value: u32) -> DeviceResult {
        self.ensure_not_ready("change virtio queue size")?;
        self.size = Some(QueueSize::new(value)?);
        Ok(())
    }

    pub(crate) fn set_ready(&mut self, value: u32) -> DeviceResult {
        match value {
            0 => {
                self.ready = false;
                self.last_avail = 0;
                Ok(())
            }
            1 => {
                self.validate_layout()?;
                self.ready = true;
                Ok(())
            }
            _ => Err(DeviceError::InvalidInput {
                operation: "configure virtio queue ready state",
                detail: format!("queue ready value {value} must be zero or one"),
            }),
        }
    }

    pub(crate) fn ready_value(&self) -> u32 {
        u32::from(self.ready)
    }

    pub(crate) fn set_address(
        &mut self,
        kind: QueueAddressKind,
        high: bool,
        value: u32,
    ) -> DeviceResult {
        self.ensure_not_ready("change virtio queue address")?;
        let address = match kind {
            QueueAddressKind::Descriptor => &mut self.descriptor,
            QueueAddressKind::Driver => &mut self.driver,
            QueueAddressKind::Device => &mut self.device,
        };
        address.set_half(high, value);
        Ok(())
    }

    pub(crate) fn active(
        &self,
        operation: &'static str,
    ) -> DeviceManagerResult<Option<QueueSnapshot>> {
        if !self.ready {
            return Ok(None);
        }
        let size = self.size.ok_or_else(|| DeviceManagerError::InvalidConfig {
            operation,
            detail: "ready queue has no validated size".into(),
        })?;
        for (name, address) in [
            ("descriptor", self.descriptor),
            ("driver", self.driver),
            ("device", self.device),
        ] {
            if !address.configured {
                return Err(DeviceManagerError::InvalidConfig {
                    operation,
                    detail: format!("ready queue has no {name} address"),
                });
            }
        }
        Ok(Some(QueueSnapshot {
            size,
            descriptor: self.descriptor.value,
            driver: self.driver.value,
            device: self.device.value,
            last_avail: self.last_avail,
        }))
    }

    pub(crate) fn complete_available(&mut self) {
        self.last_avail = self.last_avail.wrapping_add(1);
    }

    fn validate_layout(&self) -> DeviceResult {
        let size = self.size.ok_or_else(|| DeviceError::InvalidState {
            operation: "enable virtio queue",
            detail: "queue size has not been configured".into(),
        })?;
        let descriptor = self.configured_address(self.descriptor, "descriptor")?;
        let driver = self.configured_address(self.driver, "driver")?;
        let device = self.configured_address(self.device, "device")?;
        validate_alignment(descriptor, 16, "descriptor")?;
        validate_alignment(driver, 2, "driver")?;
        validate_alignment(device, 4, "device")?;

        let queue_len = u64::from(size.get());
        validate_device_range(descriptor, queue_len * VIRTQ_DESCRIPTOR_SIZE, "descriptor")?;
        validate_device_range(driver, 6 + queue_len * 2, "driver")?;
        validate_device_range(device, 6 + queue_len * 8, "device")?;
        Ok(())
    }

    fn configured_address(&self, address: QueueAddress, name: &'static str) -> DeviceResult<u64> {
        address
            .configured
            .then_some(address.value)
            .ok_or_else(|| DeviceError::InvalidState {
                operation: "enable virtio queue",
                detail: format!("queue {name} address has not been configured"),
            })
    }

    fn ensure_not_ready(&self, operation: &'static str) -> DeviceResult {
        if self.ready {
            Err(DeviceError::InvalidState {
                operation,
                detail: "queue is already ready".into(),
            })
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct QueueSnapshot {
    size: QueueSize,
    descriptor: u64,
    driver: u64,
    device: u64,
    last_avail: u16,
}

impl QueueSnapshot {
    pub(crate) fn last_avail(self) -> u16 {
        self.last_avail
    }

    pub(crate) fn pending_count(self, read: GuestRead<'_>) -> DeviceManagerResult<u16> {
        let available = read_u16(read, self.driver, 2, "read virtio available index")?;
        let pending = available.wrapping_sub(self.last_avail);
        if pending > self.size.get() {
            return Err(invalid_chain(format!(
                "available index advanced by {pending}, exceeding queue size {}",
                self.size.get()
            )));
        }
        Ok(pending)
    }

    pub(crate) fn available_head(
        self,
        read: GuestRead<'_>,
        available_index: u16,
    ) -> DeviceManagerResult<u16> {
        let slot = u64::from(available_index % self.size.get());
        let offset = VIRTQ_AVAIL_HEADER_SIZE
            .checked_add(slot * 2)
            .ok_or_else(|| invalid_chain("available-ring offset overflow".into()))?;
        read_u16(read, self.driver, offset, "read virtio available head")
    }

    pub(crate) fn size(self) -> u16 {
        self.size.get()
    }

    pub(crate) fn descriptor_table(self) -> u64 {
        self.descriptor
    }

    pub(crate) fn write_used(
        self,
        read: GuestRead<'_>,
        write: GuestWrite<'_>,
        head: u16,
        length: usize,
    ) -> DeviceManagerResult {
        let used_index = read_u16(read, self.device, 2, "read virtio used index")?;
        let slot = u64::from(used_index % self.size.get());
        let entry = VIRTQ_USED_HEADER_SIZE
            .checked_add(slot * 8)
            .ok_or_else(|| invalid_chain("used-ring offset overflow".into()))?;
        let length = u32::try_from(length).map_err(|_| {
            invalid_chain(format!(
                "used length {length} does not fit a 32-bit ring entry"
            ))
        })?;
        write_u32(
            write,
            self.device,
            entry,
            u32::from(head),
            "write virtio used descriptor",
        )?;
        write_u32(
            write,
            self.device,
            entry + 4,
            length,
            "write virtio used length",
        )?;
        write_u16(
            write,
            self.device,
            2,
            used_index.wrapping_add(1),
            "advance virtio used index",
        )
    }
}

fn validate_alignment(address: u64, alignment: u64, name: &str) -> DeviceResult {
    if address.is_multiple_of(alignment) {
        Ok(())
    } else {
        Err(DeviceError::InvalidInput {
            operation: "enable virtio queue",
            detail: format!("queue {name} address {address:#x} is not {alignment}-byte aligned"),
        })
    }
}

fn validate_device_range(address: u64, length: u64, name: &str) -> DeviceResult {
    let end = address
        .checked_add(length)
        .ok_or_else(|| DeviceError::InvalidInput {
            operation: "enable virtio queue",
            detail: format!("queue {name} range {address:#x}+{length:#x} overflows"),
        })?;
    usize::try_from(address)
        .and_then(|_| usize::try_from(end))
        .map_err(|_| DeviceError::InvalidInput {
            operation: "enable virtio queue",
            detail: format!("queue {name} range does not fit the host address width"),
        })?;
    Ok(())
}

fn invalid_chain(detail: alloc::string::String) -> DeviceManagerError {
    DeviceManagerError::InvalidInput {
        operation: "validate virtio descriptor chain",
        detail,
    }
}
