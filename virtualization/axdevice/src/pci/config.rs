//! Conventional Type-0 config image and guest-writable root state.

use alloc::vec::Vec;

use super::{
    PciBdf, PciEndpointIdentity, PciResult,
    address::CONFIG_SPACE_SIZE,
    bar::{BarState, ResolvedBarPlan},
    function::PciConfigByte,
};

const COMMAND_MEMORY_SPACE_ENABLE: u8 = 0x02;

#[derive(Clone)]
pub(crate) struct PowerOnConfig {
    bytes: [u8; CONFIG_SPACE_SIZE],
    write_mask: [u8; CONFIG_SPACE_SIZE],
}

impl PowerOnConfig {
    pub(crate) fn build(
        identity: PciEndpointIdentity,
        bars: &[ResolvedBarPlan],
        config_bytes: &[PciConfigByte],
    ) -> PciResult<Self> {
        if identity.vendor_id() == u16::MAX {
            return Err(super::PciError::InvalidEndpointIdentity {
                detail: "vendor ID 0xffff denotes an absent function",
            });
        }
        let mut bytes = [0; CONFIG_SPACE_SIZE];
        let mut write_mask = [0; CONFIG_SPACE_SIZE];
        bytes[0..2].copy_from_slice(&identity.vendor_id().to_le_bytes());
        bytes[2..4].copy_from_slice(&identity.device_id().to_le_bytes());
        write_mask[4] = COMMAND_MEMORY_SPACE_ENABLE;
        let class = identity.class();
        bytes[8] = identity.revision();
        bytes[9] = class.programming_interface();
        bytes[10] = class.subclass();
        bytes[11] = class.base();
        bytes[14] = 0;
        for patch in config_bytes {
            let offset = usize::from(patch.offset.value());
            bytes[offset] = patch.value;
            write_mask[offset] = patch.write_mask;
        }
        for bar in bars {
            let offset = bar.index.config_offset();
            bytes[offset..offset + 4]
                .copy_from_slice(&(bar.address as u32 & 0xffff_fff0).to_le_bytes());
        }
        Ok(Self { bytes, write_mask })
    }
}

pub(crate) struct FunctionState {
    bdf: PciBdf,
    power_on: PowerOnConfig,
    config: [u8; CONFIG_SPACE_SIZE],
    bars: Vec<BarState>,
}

pub(crate) enum BarWriteAction {
    Probe { bar: usize },
    Relocate { bar: usize, candidate: u64 },
}

impl FunctionState {
    pub(crate) fn new(bdf: PciBdf, power_on: PowerOnConfig, bars: &[ResolvedBarPlan]) -> Self {
        Self {
            bdf,
            config: power_on.bytes,
            power_on,
            bars: bars.iter().copied().map(BarState::new).collect(),
        }
    }

    pub(crate) const fn bdf(&self) -> PciBdf {
        self.bdf
    }

    pub(crate) fn memory_decode_enabled(&self) -> bool {
        self.config[4] & COMMAND_MEMORY_SPACE_ENABLE != 0
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

    pub(crate) fn reset(&mut self) {
        self.config = self.power_on.bytes;
        for bar in &mut self.bars {
            bar.reset();
        }
    }

    fn bar_dword(&self, offset: usize) -> Option<usize> {
        if !(0x10..0x28).contains(&offset) {
            return None;
        }
        let slot = ((offset - 0x10) / 4) as u8;
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
        let power_on = PowerOnConfig::build(identity, &[plan], &[]).unwrap();
        let mut state = FunctionState::new(PciBdf::bus_zero(1), power_on, &[plan]);

        state.write_non_bar(0, 4, 0);

        assert_eq!(state.read(0, 4), 0x5678_1234);
    }
}
