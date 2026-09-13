use alloc::sync::Arc;

use axdevice_base::{AccessWidth, DeviceError, DeviceResult};
use axvm_types::GuestPhysAddr;

use super::{
    COMMON_CONFIG_SIZE, CONFIG_GENERATION, DEVICE_CONFIG_OFFSET, DEVICE_FEATURE,
    DEVICE_FEATURE_SELECT, DEVICE_STATUS, DRIVER_FEATURE, DRIVER_FEATURE_SELECT, ISR_CONFIG_OFFSET,
    InterruptTransitionRequest, MSIX_CONFIG, NOTIFY_CONFIG_OFFSET, NUM_QUEUES, QUEUE_DESC,
    QUEUE_DEVICE, QUEUE_DRIVER, QUEUE_ENABLE, QUEUE_MSIX_VECTOR, QUEUE_NOTIFY_OFF, QUEUE_SELECT,
    QUEUE_SIZE, VirtioPciTransport, VirtioPciWriteOutcome, access_in_region, feature_word,
    invalid_queue, map_pci_error, reject_processing_queue, require_width,
};
use crate::{GuestMemory, pci::InterruptTransition};

impl<D: super::VirtioDeviceCore> VirtioPciTransport<D> {
    pub fn read_mmio(&self, offset: u64, width: AccessWidth) -> DeviceResult<u64> {
        if access_in_region(offset, width, 0, COMMON_CONFIG_SIZE) {
            let _activity = self.acquire_control_activity()?;
            return self.read_common(offset, width);
        }
        if offset == ISR_CONFIG_OFFSET {
            require_width(width, AccessWidth::Byte)?;
            return Err(DeviceError::Unsupported {
                operation: "read VirtIO PCI ISR through an unbound transport",
                detail: "use read_bar_with_interrupt to publish the line transition".into(),
            });
        }
        if access_in_region(
            offset,
            width,
            DEVICE_CONFIG_OFFSET,
            self.device_config_size as u64,
        ) {
            let _activity = self.acquire_control_activity()?;
            return self
                .core
                .read_device_config(offset - DEVICE_CONFIG_OFFSET, width);
        }
        Err(DeviceError::OutOfRange { addr: offset })
    }

    /// Reads one BAR-relative VirtIO transport access.
    ///
    /// PCI direct BAR accesses and PCI_CFG-selected accesses must both call
    /// this entry point so the transport has only one register state source.
    pub fn read_bar(&self, offset: u64, width: AccessWidth) -> DeviceResult<u64> {
        self.read_mmio(offset, width)
    }

    /// Reads one BAR-relative access and returns any ISR line transition intent.
    pub fn read_bar_with_interrupt(
        &self,
        offset: u64,
        width: AccessWidth,
    ) -> DeviceResult<(u64, InterruptTransitionRequest)> {
        if offset == ISR_CONFIG_OFFSET {
            require_width(width, AccessWidth::Byte)?;
            let activity = self.acquire_control_activity()?;
            let interrupt = self.interrupts.read_isr();
            Ok((
                interrupt.value as u64,
                InterruptTransitionRequest::new(
                    Arc::clone(&self.interrupts),
                    interrupt.transition,
                    Some(activity),
                ),
            ))
        } else {
            Ok((
                self.read_bar(offset, width)?,
                InterruptTransitionRequest::without_activity(
                    Arc::clone(&self.interrupts),
                    InterruptTransition::None,
                ),
            ))
        }
    }

    /// Writes one BAR-relative access with an explicit BME/DMA snapshot.
    pub fn write_bar_with_dma(
        &self,
        offset: u64,
        width: AccessWidth,
        value: u64,
        dma_enabled: bool,
        memory: &mut dyn GuestMemory,
    ) -> DeviceResult<VirtioPciWriteOutcome> {
        self.write_mmio_with_dma(offset, width, value, dma_enabled, memory)
    }

    /// Writes a transport register with the current PCI DMA authorization.
    ///
    /// A queue notification received while `dma_enabled` is false is treated
    /// as a stopped queue, not as a malformed queue.  It returns before ring
    /// validation, descriptor access, or device-core invocation.
    pub fn write_mmio_with_dma(
        &self,
        offset: u64,
        width: AccessWidth,
        value: u64,
        dma_enabled: bool,
        memory: &mut dyn GuestMemory,
    ) -> DeviceResult<VirtioPciWriteOutcome> {
        if access_in_region(offset, width, 0, COMMON_CONFIG_SIZE) {
            return self.write_common(offset, width, value);
        }
        if offset == NOTIFY_CONFIG_OFFSET {
            require_width(width, AccessWidth::Word)?;
            return self.notify_queue(value as u16, dma_enabled, memory);
        }
        if offset == ISR_CONFIG_OFFSET {
            require_width(width, AccessWidth::Byte)?;
            return Err(DeviceError::ReadOnly);
        }
        if access_in_region(
            offset,
            width,
            DEVICE_CONFIG_OFFSET,
            self.device_config_size as u64,
        ) {
            let _activity = self.acquire_control_activity()?;
            self.core
                .write_device_config(offset - DEVICE_CONFIG_OFFSET, width, value)?;
            return Ok(VirtioPciWriteOutcome::None);
        }
        Err(DeviceError::OutOfRange { addr: offset })
    }

    fn read_common(&self, offset: u64, width: AccessWidth) -> DeviceResult<u64> {
        let state = self.state.lock();
        let queue_index = state.queue_select as usize;
        let queue = state.queues.get(queue_index);
        if let Some(access) = QueueAddressAccess::decode(offset, width)? {
            let queue = queue.ok_or_else(|| invalid_queue(state.queue_select))?;
            let address = match access.register {
                QueueAddressRegister::Descriptor => queue.queue.desc_table_addr,
                QueueAddressRegister::Driver => queue.queue.avail_ring_addr,
                QueueAddressRegister::Device => queue.queue.used_ring_addr,
            };
            return Ok(access.read(address.as_usize() as u64));
        }
        match offset {
            DEVICE_FEATURE_SELECT => {
                require_width(width, AccessWidth::Dword)?;
                Ok(state.device_feature_select as u64)
            }
            DEVICE_FEATURE => {
                require_width(width, AccessWidth::Dword)?;
                feature_word(self.core.device_features(), state.device_feature_select)
            }
            DRIVER_FEATURE_SELECT => {
                require_width(width, AccessWidth::Dword)?;
                Ok(state.driver_feature_select as u64)
            }
            DRIVER_FEATURE => {
                require_width(width, AccessWidth::Dword)?;
                feature_word(state.driver_features, state.driver_feature_select)
            }
            MSIX_CONFIG => {
                require_width(width, AccessWidth::Word)?;
                Ok(u16::MAX as u64)
            }
            NUM_QUEUES => {
                require_width(width, AccessWidth::Word)?;
                Ok(state.queues.len() as u64)
            }
            DEVICE_STATUS => {
                require_width(width, AccessWidth::Byte)?;
                Ok(state.status as u64)
            }
            CONFIG_GENERATION => {
                require_width(width, AccessWidth::Byte)?;
                Ok(state.config_generation as u64)
            }
            QUEUE_SELECT => {
                require_width(width, AccessWidth::Word)?;
                Ok(state.queue_select as u64)
            }
            QUEUE_SIZE => {
                require_width(width, AccessWidth::Word)?;
                Ok(state.queue_size as u64)
            }
            QUEUE_MSIX_VECTOR => {
                require_width(width, AccessWidth::Word)?;
                Ok(u16::MAX as u64)
            }
            QUEUE_ENABLE => {
                require_width(width, AccessWidth::Word)?;
                Ok(queue.map_or(0, |queue| queue.enabled as u64))
            }
            QUEUE_NOTIFY_OFF => {
                require_width(width, AccessWidth::Word)?;
                Ok(queue_index as u64)
            }
            _ => Err(DeviceError::OutOfRange { addr: offset }),
        }
    }

    fn write_common(
        &self,
        offset: u64,
        width: AccessWidth,
        value: u64,
    ) -> DeviceResult<VirtioPciWriteOutcome> {
        if offset == DEVICE_STATUS {
            require_width(width, AccessWidth::Byte)?;
            if value == 0 {
                let interrupt = self.reset()?;
                return Ok(VirtioPciWriteOutcome::Reset { interrupt });
            }
        }

        let _activity = self.acquire_control_activity()?;
        let mut state = self.state.lock();
        let selected_queue = state.queue_select as usize;
        let selected_queue_id = state.queue_select;
        if let Some(access) = QueueAddressAccess::decode(offset, width)? {
            let queue = state
                .queues
                .get_mut(selected_queue)
                .ok_or_else(|| invalid_queue(selected_queue_id))?;
            reject_processing_queue(queue)?;
            let current = match access.register {
                QueueAddressRegister::Descriptor => queue.queue.desc_table_addr,
                QueueAddressRegister::Driver => queue.queue.avail_ring_addr,
                QueueAddressRegister::Device => queue.queue.used_ring_addr,
            };
            let address =
                GuestPhysAddr::from(access.merge(current.as_usize() as u64, value) as usize);
            match access.register {
                QueueAddressRegister::Descriptor => queue.queue.set_desc_table_addr(address),
                QueueAddressRegister::Driver => queue.queue.set_avail_ring_addr(address),
                QueueAddressRegister::Device => queue.queue.set_used_ring_addr(address),
            }
            .map_err(map_pci_error)?;
            return Ok(VirtioPciWriteOutcome::None);
        }
        match offset {
            DEVICE_FEATURE_SELECT => {
                require_width(width, AccessWidth::Dword)?;
                state.device_feature_select = value as u32;
            }
            DRIVER_FEATURE_SELECT => {
                require_width(width, AccessWidth::Dword)?;
                state.ensure_feature_negotiation_open()?;
                state.driver_feature_select = value as u32;
            }
            DRIVER_FEATURE => {
                require_width(width, AccessWidth::Dword)?;
                state.ensure_feature_negotiation_open()?;
                if state.driver_feature_select > 1 {
                    return Ok(VirtioPciWriteOutcome::None);
                }
                let shift = state.driver_feature_select * 32;
                let mask = 0xffff_ffff_u64 << shift;
                state.driver_features = (state.driver_features & !mask) | ((value << shift) & mask);
            }
            DEVICE_STATUS => {
                let status = value as u8;
                state.write_driver_status(status, self.core.device_features())?;
            }
            QUEUE_SELECT => {
                require_width(width, AccessWidth::Word)?;
                if value as usize >= state.queues.len() {
                    return Err(invalid_queue(value as u16));
                }
                reject_processing_queue(&state.queues[value as usize])?;
                state.queue_select = value as u16;
                state.queue_size = state.queues[value as usize].queue.size;
            }
            QUEUE_SIZE => {
                require_width(width, AccessWidth::Word)?;
                let queue = state
                    .queues
                    .get_mut(selected_queue)
                    .ok_or_else(|| invalid_queue(selected_queue_id))?;
                reject_processing_queue(queue)?;
                queue.queue.set_size(value as u16).map_err(map_pci_error)?;
                state.queue_size = value as u16;
            }
            QUEUE_ENABLE => {
                require_width(width, AccessWidth::Word)?;
                let queue = state
                    .queues
                    .get_mut(selected_queue)
                    .ok_or_else(|| invalid_queue(selected_queue_id))?;
                reject_processing_queue(queue)?;
                if value != 0 {
                    queue.queue.validate_layout().map_err(map_pci_error)?;
                }
                queue.enabled = value != 0;
                if !queue.enabled {
                    queue.queue.set_ready(false);
                }
            }
            MSIX_CONFIG | QUEUE_MSIX_VECTOR => {
                require_width(width, AccessWidth::Word)?;
            }
            DEVICE_FEATURE | NUM_QUEUES | CONFIG_GENERATION | QUEUE_NOTIFY_OFF => {
                return Err(DeviceError::ReadOnly);
            }
            _ => return Err(DeviceError::OutOfRange { addr: offset }),
        }
        Ok(VirtioPciWriteOutcome::None)
    }
}

#[derive(Clone, Copy)]
enum QueueAddressRegister {
    Descriptor,
    Driver,
    Device,
}

#[derive(Clone, Copy)]
enum QueueAddressLane {
    Low,
    High,
    Full,
}

#[derive(Clone, Copy)]
struct QueueAddressAccess {
    register: QueueAddressRegister,
    lane: QueueAddressLane,
}

impl QueueAddressAccess {
    fn decode(offset: u64, width: AccessWidth) -> DeviceResult<Option<Self>> {
        let (register, high_lane) = if offset == QUEUE_DESC || offset == QUEUE_DESC + 4 {
            (QueueAddressRegister::Descriptor, offset == QUEUE_DESC + 4)
        } else if offset == QUEUE_DRIVER || offset == QUEUE_DRIVER + 4 {
            (QueueAddressRegister::Driver, offset == QUEUE_DRIVER + 4)
        } else if offset == QUEUE_DEVICE || offset == QUEUE_DEVICE + 4 {
            (QueueAddressRegister::Device, offset == QUEUE_DEVICE + 4)
        } else {
            return Ok(None);
        };

        let lane = match (high_lane, width) {
            (false, AccessWidth::Dword) => QueueAddressLane::Low,
            (false, AccessWidth::Qword) => QueueAddressLane::Full,
            (true, AccessWidth::Dword) => QueueAddressLane::High,
            _ => {
                return Err(DeviceError::InvalidWidth {
                    expected: AccessWidth::Dword,
                    actual: width,
                });
            }
        };
        Ok(Some(Self { register, lane }))
    }

    const fn read(self, address: u64) -> u64 {
        match self.lane {
            QueueAddressLane::Low => address & u32::MAX as u64,
            QueueAddressLane::High => address >> 32,
            QueueAddressLane::Full => address,
        }
    }

    const fn merge(self, current: u64, value: u64) -> u64 {
        match self.lane {
            QueueAddressLane::Low => (current & !u32::MAX as u64) | (value & u32::MAX as u64),
            QueueAddressLane::High => {
                (current & u32::MAX as u64) | ((value & u32::MAX as u64) << 32)
            }
            QueueAddressLane::Full => value,
        }
    }
}
