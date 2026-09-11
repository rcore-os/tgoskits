//! Conventional Type-0 config image and guest-writable root state.

use alloc::vec::Vec;

use super::{
    PciBdf, PciCapabilityEffectRegion, PciCapabilityId, PciCapabilityLayout, PciCapabilitySnapshot,
    PciEndpointIdentity, PciResult, ResolvedPciIntx,
    bar::{BarState, ResolvedBarPlan},
    config_layout::{
        COMMAND_BUS_MASTER_ENABLE, COMMAND_INTERRUPT_DISABLE, COMMAND_MEMORY_SPACE_ENABLE,
        CONFIG_BAR_END, CONFIG_BAR_MEMORY_ADDRESS_MASK, CONFIG_BAR_REGISTER_SIZE, CONFIG_BAR_START,
        CONFIG_BASE_CLASS_OFFSET, CONFIG_CAPABILITY_POINTER_OFFSET, CONFIG_COMMAND_OFFSET,
        CONFIG_DEVICE_ID_OFFSET, CONFIG_HEADER_TYPE_OFFSET, CONFIG_INTERRUPT_LINE_OFFSET,
        CONFIG_INTERRUPT_PIN_OFFSET, CONFIG_PROGRAMMING_INTERFACE_OFFSET, CONFIG_REVISION_OFFSET,
        CONFIG_SPACE_SIZE, CONFIG_STATUS_OFFSET, CONFIG_SUBCLASS_OFFSET,
        CONFIG_SUBSYSTEM_DEVICE_ID_OFFSET, CONFIG_SUBSYSTEM_VENDOR_ID_OFFSET,
        CONFIG_VENDOR_ID_OFFSET, STATUS_CAPABILITIES_LIST,
    },
    function::PciConfigByte,
    runtime::{PciCommandRevision, PciCommandState},
};

#[derive(Clone)]
pub(crate) struct PowerOnConfig {
    bytes: [u8; CONFIG_SPACE_SIZE],
    write_mask: [u8; CONFIG_SPACE_SIZE],
    capabilities: Vec<PciCapabilityLayout>,
}

impl PowerOnConfig {
    pub(crate) fn build(
        identity: PciEndpointIdentity,
        bars: &[ResolvedBarPlan],
        config_bytes: &[PciConfigByte],
        capabilities: &[PciCapabilityLayout],
        intx: Option<ResolvedPciIntx>,
    ) -> PciResult<Self> {
        if identity.vendor_id() == u16::MAX {
            return Err(super::PciError::InvalidEndpointIdentity {
                detail: "vendor ID 0xffff denotes an absent function",
            });
        }
        let mut bytes = [0; CONFIG_SPACE_SIZE];
        let mut write_mask = [0; CONFIG_SPACE_SIZE];
        bytes[CONFIG_VENDOR_ID_OFFSET..CONFIG_VENDOR_ID_OFFSET + 2]
            .copy_from_slice(&identity.vendor_id().to_le_bytes());
        bytes[CONFIG_DEVICE_ID_OFFSET..CONFIG_DEVICE_ID_OFFSET + 2]
            .copy_from_slice(&identity.device_id().to_le_bytes());
        bytes[CONFIG_SUBSYSTEM_VENDOR_ID_OFFSET..CONFIG_SUBSYSTEM_VENDOR_ID_OFFSET + 2]
            .copy_from_slice(&identity.subsystem_vendor_id().to_le_bytes());
        bytes[CONFIG_SUBSYSTEM_DEVICE_ID_OFFSET..CONFIG_SUBSYSTEM_DEVICE_ID_OFFSET + 2]
            .copy_from_slice(&identity.subsystem_device_id().to_le_bytes());
        if let Some(intx) = intx {
            bytes[CONFIG_INTERRUPT_LINE_OFFSET] = intx.guest_line_byte();
            bytes[CONFIG_INTERRUPT_PIN_OFFSET] = intx.pin().config_encoding();
        }
        write_mask[CONFIG_COMMAND_OFFSET] = COMMAND_MEMORY_SPACE_ENABLE | COMMAND_BUS_MASTER_ENABLE;
        write_mask[CONFIG_COMMAND_OFFSET + 1] = COMMAND_INTERRUPT_DISABLE;
        let class = identity.class();
        bytes[CONFIG_REVISION_OFFSET] = identity.revision();
        bytes[CONFIG_PROGRAMMING_INTERFACE_OFFSET] = class.programming_interface();
        bytes[CONFIG_SUBCLASS_OFFSET] = class.subclass();
        bytes[CONFIG_BASE_CLASS_OFFSET] = class.base();
        bytes[CONFIG_HEADER_TYPE_OFFSET] = 0;
        for patch in config_bytes {
            let offset = usize::from(patch.offset.value());
            bytes[offset] = patch.value;
            write_mask[offset] = patch.write_mask;
        }
        for bar in bars {
            let offset = bar.index.config_offset();
            bytes[offset..offset + 4].copy_from_slice(
                &(bar.address as u32 & CONFIG_BAR_MEMORY_ADDRESS_MASK).to_le_bytes(),
            );
        }
        if let Some(first) = capabilities.first() {
            bytes[CONFIG_STATUS_OFFSET] |= STATUS_CAPABILITIES_LIST;
            bytes[CONFIG_CAPABILITY_POINTER_OFFSET] = first.offset().value() as u8;
        }
        for (index, capability) in capabilities.iter().enumerate() {
            let base = usize::from(capability.offset().value());
            bytes[base] = capability.id().value();
            bytes[base + 1] = capabilities
                .get(index + 1)
                .map_or(0, |next| next.offset().value() as u8);
            bytes[base + 2..base + usize::from(capability.length())]
                .copy_from_slice(capability.body());
            write_mask[base + 2..base + usize::from(capability.length())]
                .copy_from_slice(capability.write_mask());
        }
        Ok(Self {
            bytes,
            write_mask,
            capabilities: capabilities.to_vec(),
        })
    }
}

pub(crate) struct FunctionState {
    bdf: PciBdf,
    has_intx: bool,
    power_on: PowerOnConfig,
    config: [u8; CONFIG_SPACE_SIZE],
    bars: Vec<BarState>,
    command_revision: PciCommandRevision,
}

pub(crate) enum BarWriteAction {
    Probe { bar: usize },
    Relocate { bar: usize, candidate: u64 },
}

impl FunctionState {
    pub(crate) fn new(
        bdf: PciBdf,
        power_on: PowerOnConfig,
        bars: &[ResolvedBarPlan],
        has_intx: bool,
    ) -> Self {
        Self {
            bdf,
            has_intx,
            config: power_on.bytes,
            power_on,
            bars: bars.iter().copied().map(BarState::new).collect(),
            command_revision: PciCommandRevision::initial(),
        }
    }

    pub(crate) const fn bdf(&self) -> PciBdf {
        self.bdf
    }

    pub(crate) const fn has_intx(&self) -> bool {
        self.has_intx
    }

    pub(crate) fn memory_decode_enabled(&self) -> bool {
        self.config[CONFIG_COMMAND_OFFSET] & COMMAND_MEMORY_SPACE_ENABLE != 0
    }

    pub(crate) fn command_state(&self) -> PciCommandState {
        PciCommandState::new(
            self.config[CONFIG_COMMAND_OFFSET] & COMMAND_MEMORY_SPACE_ENABLE != 0,
            self.config[CONFIG_COMMAND_OFFSET] & COMMAND_BUS_MASTER_ENABLE != 0,
            self.config[CONFIG_COMMAND_OFFSET + 1] & COMMAND_INTERRUPT_DISABLE != 0,
            self.command_revision,
        )
    }

    pub(crate) fn command_write_changes(&self, offset: usize, size: usize, value: u64) -> bool {
        let mut candidate = self.config;
        merge_bytes(
            &mut candidate,
            offset,
            size,
            value,
            &self.power_on.write_mask,
        );
        candidate[CONFIG_COMMAND_OFFSET] & COMMAND_MEMORY_SPACE_ENABLE
            != self.config[CONFIG_COMMAND_OFFSET] & COMMAND_MEMORY_SPACE_ENABLE
            || candidate[CONFIG_COMMAND_OFFSET] & COMMAND_BUS_MASTER_ENABLE
                != self.config[CONFIG_COMMAND_OFFSET] & COMMAND_BUS_MASTER_ENABLE
            || candidate[CONFIG_COMMAND_OFFSET + 1] & COMMAND_INTERRUPT_DISABLE
                != self.config[CONFIG_COMMAND_OFFSET + 1] & COMMAND_INTERRUPT_DISABLE
    }

    pub(crate) fn bump_command_revision(&mut self) -> PciResult {
        self.command_revision = self.command_revision.next()?;
        Ok(())
    }

    pub(crate) fn bars(&self) -> &[BarState] {
        &self.bars
    }

    pub(crate) fn read(&self, offset: usize, size: usize) -> u64 {
        if let Some(bar) = self.bar_dword(offset) {
            let dword = self.bars[bar].raw_dword().to_le_bytes();
            return read_bytes(&dword, offset % 4, size);
        }
        read_bytes(&self.config, offset, size)
    }

    pub(crate) fn config_effect(
        &self,
        offset: usize,
        size: usize,
        width: crate::AccessWidth,
        write: bool,
    ) -> PciResult<
        Option<(
            PciCapabilityId,
            PciCapabilityEffectRegion,
            u8,
            PciCapabilitySnapshot,
        )>,
    > {
        for capability in &self.power_on.capabilities {
            let Some(effect) = capability.effect_for_access(offset, size, write, width)? else {
                continue;
            };
            let relative = offset
                .checked_sub(usize::from(capability.offset().value()))
                .ok_or(super::PciError::InvalidConfigAccess {
                    offset: offset as u16,
                    width,
                    detail: "capability effect offset underflows",
                })?;
            return Ok(Some((
                capability.id(),
                effect,
                relative as u8,
                capability.snapshot(&self.config),
            )));
        }
        Ok(None)
    }

    pub(crate) fn intersects_config_effect(&self, offset: usize, size: usize) -> bool {
        self.power_on
            .capabilities
            .iter()
            .any(|capability| capability.intersects_effect(offset, size))
    }

    /// Classifies one BAR write after merging the guest lanes into a full
    /// dword. The size probe is recognized only when the merged dword equals
    /// all ones in one access; lane-wise accumulation across multiple writes
    /// is intentionally not tracked, matching the design's four-row contract
    /// rather than hardware register latching.
    pub(crate) fn prepare_bar_write(
        &self,
        offset: usize,
        size: usize,
        value: u64,
    ) -> Option<BarWriteAction> {
        let bar = self.bar_dword(offset)?;
        let mut dword = self.bars[bar].committed_dword().to_le_bytes();
        merge_bytes(&mut dword, offset % 4, size, value, &[u8::MAX; 4]);
        let merged = u32::from_le_bytes(dword);
        if merged == u32::MAX {
            return Some(BarWriteAction::Probe { bar });
        }
        Some(BarWriteAction::Relocate {
            bar,
            candidate: BarState::candidate_address(merged),
        })
    }

    pub(crate) fn write_non_bar(&mut self, offset: usize, size: usize, value: u64) {
        merge_bytes(
            &mut self.config,
            offset,
            size,
            value,
            &self.power_on.write_mask,
        );
    }

    pub(crate) fn apply_probe(&mut self, bar: usize) {
        self.bars[bar].set_probe();
    }

    pub(crate) fn finish_relocation(&mut self, bar: usize, accepted: Option<u64>) {
        self.bars[bar].finish_relocation(accepted);
    }

    pub(crate) fn reset(&mut self) -> PciResult {
        self.config = self.power_on.bytes;
        for bar in &mut self.bars {
            bar.reset();
        }
        self.bump_command_revision()
    }

    fn bar_dword(&self, offset: usize) -> Option<usize> {
        if !(CONFIG_BAR_START..CONFIG_BAR_END).contains(&offset) {
            return None;
        }
        let slot = ((offset - CONFIG_BAR_START) / CONFIG_BAR_REGISTER_SIZE) as u8;
        self.bars.iter().position(|bar| slot == bar.index().value())
    }
}

pub(crate) fn read_bytes(bytes: &[u8], offset: usize, size: usize) -> u64 {
    bytes[offset..offset + size]
        .iter()
        .enumerate()
        .fold(0, |value, (index, byte)| {
            value | (u64::from(*byte) << (index * 8))
        })
}

fn merge_bytes(bytes: &mut [u8], offset: usize, size: usize, value: u64, masks: &[u8]) {
    for index in 0..size {
        let mask = masks[offset + index];
        let update = (value >> (index * 8)) as u8;
        bytes[offset + index] = (bytes[offset + index] & !mask) | (update & mask);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PciBarIndex, PciClass, PciMemoryBar};

    #[test]
    fn function_state_keeps_unimplemented_header_fields_read_only() {
        let identity = PciEndpointIdentity::new(0x1234, 0x5678, PciClass::new(0x05, 0x00, 0x00));
        let bar = PciMemoryBar::new(PciBarIndex::new(2).unwrap(), 0x1_0000).unwrap();
        let plan = ResolvedBarPlan {
            index: bar.index(),
            size: bar.size(),
            address: 0x2000_0000,
        };
        let power_on = PowerOnConfig::build(identity, &[plan], &[], &[], None).unwrap();
        let mut state = FunctionState::new(PciBdf::bus_zero(1), power_on, &[plan], false);

        state.write_non_bar(0, 4, 0);

        assert_eq!(state.read(0, 4), 0x5678_1234);
    }
}
