//! Validated ITS command decoding and mapping transitions.

use alloc::vec::Vec;

use super::{ItsAction, ItsDevice, ItsState, Translation};
use crate::{
    CollectionId, EventId, GicVcpuId, ItsDeviceId, LPI_INTID_BASE, LpiId, VgicError, VgicResult,
};

impl ItsState {
    pub(super) fn execute(
        &mut self,
        words: [u64; 4],
        offset: u64,
        lpi_limit: u32,
        processor_targets: &[GicVcpuId],
        actions: &mut Vec<ItsAction>,
    ) -> VgicResult {
        let opcode = words[0] as u8;
        let device = ItsDeviceId::new((words[0] >> 32) as u32);
        let event = EventId::new(words[1] as u32);
        match opcode {
            0x08 => {
                self.map_device(device, words);
                Ok(())
            }
            0x09 => self.map_collection(words, processor_targets, opcode, offset),
            0x0a => {
                let lpi = checked_lpi((words[1] >> 32) as u32, lpi_limit, opcode, offset)?;
                self.map_translation(device, event, lpi, words, opcode, offset)
            }
            0x0b => {
                let lpi = checked_lpi(event.raw(), lpi_limit, opcode, offset)?;
                self.map_translation(device, event, lpi, words, opcode, offset)
            }
            0x01 => self.move_translation(device, event, words, offset),
            0x03 | 0x04 => {
                let (lpi, target) = self.translate(device, event)?;
                actions.push(ItsAction::SetPending {
                    target,
                    lpi,
                    pending: opcode == 0x03,
                });
                Ok(())
            }
            0x0f => {
                self.require_device_event(device, event, opcode, offset)?;
                self.translations.remove(&(device, event));
                Ok(())
            }
            0x0c => self.require_device_event(device, event, opcode, offset),
            0x0d => self
                .require_collection(CollectionId::new(words[2] as u16), opcode, offset)
                .map(|_| ()),
            0x05 => Ok(()),
            _ => Err(VgicError::InvalidItsCommand {
                opcode,
                offset,
                detail: "unsupported ITS command opcode".into(),
            }),
        }
    }

    fn map_device(&mut self, device: ItsDeviceId, words: [u64; 4]) {
        if words[2] & (1 << 63) != 0 {
            let event_bits = ((words[1] & 0x1f) + 1) as u8;
            self.devices.insert(device, ItsDevice { event_bits });
        } else {
            self.devices.remove(&device);
            self.translations
                .retain(|(mapped_device, _), _| *mapped_device != device);
        }
    }

    fn map_collection(
        &mut self,
        words: [u64; 4],
        processor_targets: &[GicVcpuId],
        opcode: u8,
        offset: u64,
    ) -> VgicResult {
        let collection = CollectionId::new(words[2] as u16);
        if words[2] & (1 << 63) != 0 {
            let target = GicVcpuId::new(((words[2] >> 16) & 0xffff) as usize);
            if !processor_targets.contains(&target) {
                return Err(VgicError::InvalidItsCommand {
                    opcode,
                    offset,
                    detail: alloc::format!(
                        "processor number {} has no attached Redistributor",
                        target.raw()
                    ),
                });
            }
            self.collections.insert(collection, target);
        } else {
            self.collections.remove(&collection);
            self.translations
                .retain(|_, translation| translation.collection != collection);
        }
        Ok(())
    }

    fn map_translation(
        &mut self,
        device: ItsDeviceId,
        event: EventId,
        lpi: LpiId,
        words: [u64; 4],
        opcode: u8,
        offset: u64,
    ) -> VgicResult {
        self.require_device_event_capacity(device, event, opcode, offset)?;
        let collection = CollectionId::new(words[2] as u16);
        self.require_collection(collection, opcode, offset)?;
        self.translations
            .insert((device, event), Translation { lpi, collection });
        Ok(())
    }

    fn move_translation(
        &mut self,
        device: ItsDeviceId,
        event: EventId,
        words: [u64; 4],
        offset: u64,
    ) -> VgicResult {
        let collection = CollectionId::new(words[2] as u16);
        self.require_collection(collection, 0x01, offset)?;
        let translation = self
            .translations
            .get_mut(&(device, event))
            .ok_or_else(|| invalid_mapping(0x01, offset, device, event))?;
        translation.collection = collection;
        Ok(())
    }

    fn require_device_event_capacity(
        &self,
        device: ItsDeviceId,
        event: EventId,
        opcode: u8,
        offset: u64,
    ) -> VgicResult {
        let entry = self
            .devices
            .get(&device)
            .ok_or_else(|| VgicError::InvalidItsCommand {
                opcode,
                offset,
                detail: alloc::format!("device {} is not mapped", device.raw()),
            })?;
        if entry.event_bits < 32 && event.raw() >= (1u32 << entry.event_bits) {
            return Err(VgicError::InvalidItsCommand {
                opcode,
                offset,
                detail: alloc::format!(
                    "event {} exceeds device {} capacity of {} bits",
                    event.raw(),
                    device.raw(),
                    entry.event_bits
                ),
            });
        }
        Ok(())
    }

    fn require_device_event(
        &self,
        device: ItsDeviceId,
        event: EventId,
        opcode: u8,
        offset: u64,
    ) -> VgicResult {
        if self.translations.contains_key(&(device, event)) {
            Ok(())
        } else {
            Err(invalid_mapping(opcode, offset, device, event))
        }
    }

    fn require_collection(
        &self,
        collection: CollectionId,
        opcode: u8,
        offset: u64,
    ) -> VgicResult<GicVcpuId> {
        self.collections
            .get(&collection)
            .copied()
            .ok_or_else(|| VgicError::InvalidItsCommand {
                opcode,
                offset,
                detail: alloc::format!("collection {} is not mapped", collection.raw()),
            })
    }
}

fn checked_lpi(raw: u32, limit: u32, opcode: u8, offset: u64) -> VgicResult<LpiId> {
    if raw < LPI_INTID_BASE || raw > limit {
        return Err(VgicError::InvalidItsCommand {
            opcode,
            offset,
            detail: alloc::format!("LPI INTID {raw} is outside {LPI_INTID_BASE}..={limit}"),
        });
    }
    LpiId::new(raw)
}

fn invalid_mapping(opcode: u8, offset: u64, device: ItsDeviceId, event: EventId) -> VgicError {
    VgicError::InvalidItsCommand {
        opcode,
        offset,
        detail: alloc::format!(
            "translation ({}, {}) is not mapped",
            device.raw(),
            event.raw()
        ),
    }
}
