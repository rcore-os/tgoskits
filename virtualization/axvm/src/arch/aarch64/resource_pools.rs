//! AArch64 machine-owned automatic device and MSI resource windows.

use arm_vgic::{ArmVgicConfig, LPI_INTID_BASE};
use axdevice::ResourcePools;
use axdevice_base::*;

use crate::AxVmResult;

const AUTO_MMIO: core::ops::Range<u64> = 0x0b00_0000..0x1000_0000;
const AUTO_MSI_ID_END: u32 = 0x1_0000;

pub(super) fn create(vgic: &ArmVgicConfig) -> AxVmResult<ResourcePools> {
    let controller = vgic.controller_id();
    let spi_count = match vgic {
        ArmVgicConfig::V2(config) => config.spi_count(),
        ArmVgicConfig::V3(config) => config.spi_count(),
    };
    let spi_end = 32usize
        .checked_add(spi_count)
        .ok_or_else(|| crate::AxVmError::invalid_config("AArch64 automatic SPI range overflows"))?;

    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(AUTO_MMIO)?;
    pools.add_auto_controller_inputs(
        controller,
        ControllerInputId::new(32)..ControllerInputId::new(spi_end),
    )?;

    for assigned in vgic.assigned_spis() {
        let owner = std::format!("aarch64-physical-spi-{}", assigned.intid().raw());
        pools.reserve_wired_host_irq(
            owner,
            controller,
            ControllerInputId::new(assigned.intid().raw() as usize),
            assigned.host_irq(),
            assigned.trigger(),
        )?;
    }

    if let ArmVgicConfig::V3(config) = vgic {
        let lpi_end = config.lpi_limit().checked_add(1).ok_or_else(|| {
            crate::AxVmError::invalid_config("AArch64 automatic LPI range overflows")
        })?;
        for its in config.its() {
            pools.add_auto_msi_domain(
                controller,
                its.id(),
                MsiDeviceId::new(0)..MsiDeviceId::new(AUTO_MSI_ID_END),
                MsiEventId::new(0)..MsiEventId::new(AUTO_MSI_ID_END),
                LpiId::new(LPI_INTID_BASE)..LpiId::new(lpi_end),
            )?;
        }
    }
    Ok(pools)
}
