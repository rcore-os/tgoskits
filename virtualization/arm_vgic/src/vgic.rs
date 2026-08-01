// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::sync::Arc;

use ax_kspin::SpinNoIrq as Mutex;
use axdevice_base::Resource;
use axvm_types::GuestPhysAddrRange;

use crate::{
    ArmSpiIntId, ArmVgicController, ServiceHint, VgicError, VgicResult, host, interrupt::VgicInt,
    registers::GicRegister, vgicd::Vgicd,
};

pub(crate) const DEFAULT_VGIC_BASE: usize = 0x800_0000;
pub(crate) const VGICD_REGION_SIZE: usize = 0x10000;

/// Virtual Generic Interrupt Controller.
///
/// Manages virtual interrupt distribution for guest VMs.
pub struct Vgic {
    vgicd: Mutex<Vgicd>,
    spi_controller: Option<Arc<ArmVgicController>>,
    range: GuestPhysAddrRange,
    resources: [Resource; 1],
}

impl Default for Vgic {
    fn default() -> Self {
        Self::new()
    }
}

impl Vgic {
    /// Creates a new VGIC instance.
    pub fn new() -> Vgic {
        Vgic {
            vgicd: Mutex::new(Vgicd::new()),
            spi_controller: None,
            range: GuestPhysAddrRange::from_start_size(DEFAULT_VGIC_BASE.into(), VGICD_REGION_SIZE),
            resources: [Resource::MmioRange {
                base: DEFAULT_VGIC_BASE as u64,
                size: VGICD_REGION_SIZE as u64,
            }],
        }
    }

    /// Creates a 64-KiB distributor wrapper backed by durable SPI state.
    ///
    /// # Errors
    ///
    /// Returns [`VgicError::InvalidAccess`] unless the range is exactly 64 KiB
    /// and its base is 64-KiB aligned.
    pub fn with_spi_controller(
        range: GuestPhysAddrRange,
        controller: Arc<ArmVgicController>,
    ) -> VgicResult<Self> {
        let base = range.start.as_usize();
        let length = range.end.as_usize().saturating_sub(base);
        if !base.is_multiple_of(VGICD_REGION_SIZE) || length != VGICD_REGION_SIZE {
            return Err(VgicError::InvalidAccess {
                operation: "construct",
                offset: base,
                width: axdevice_base::AccessWidth::Dword,
            });
        }
        Ok(Self {
            vgicd: Mutex::new(Vgicd::new()),
            spi_controller: Some(controller),
            range,
            resources: [Resource::MmioRange {
                base: base as u64,
                size: length as u64,
            }],
        })
    }

    pub(crate) const fn range(&self) -> GuestPhysAddrRange {
        self.range
    }

    pub(crate) fn resources(&self) -> &[Resource] {
        &self.resources
    }
    pub(crate) fn handle_read8(&self, addr: usize) -> VgicResult<usize> {
        let value = self.handle_read32(addr)?;
        Ok((value >> (8 * (addr & 0x3))) & 0xff)
    }

    pub(crate) fn handle_read16(&self, addr: usize) -> VgicResult<usize> {
        let value = self.handle_read32(addr)?;
        Ok((value >> (8 * (addr & 0x3))) & 0xffff)
    }

    /// Handles 32-bit read access to VGIC registers.
    pub fn handle_read32(&self, addr: usize) -> VgicResult<usize> {
        match GicRegister::from_addr(addr as u32) {
            Some(reg) => match reg {
                GicRegister::GicdCtlr => Ok(self.vgicd.lock().ctrlr as usize),
                GicRegister::GicdTyper => Ok(self.vgicd.lock().typer as usize),
                GicRegister::GicdIidr => Ok(self.vgicd.lock().iidr as usize),
                // // GicRegister::GicdStatusr => self.read_statusr(),
                // // GicRegister::GicdIgroupr(idx) => self.read_igroupr(idx),
                GicRegister::GicdIsenabler(idx) if idx > 0 && self.spi_controller.is_some() => {
                    self.read_spi_enable_word(idx)
                }
                GicRegister::GicdIsenabler(idx) => Ok(self.vgicd.lock().vgicd_isenabler_read(idx)),
                // GicRegister::GicdIcenabler(idx) => self.read_icenabler(idx),
                // GicRegister::GicdIspendr(idx) => self.read_ispendr(idx),
                _ => {
                    // error!("Read register address: {:#x}", addr);
                    Ok(0)
                }
            },
            None => {
                // error!("Invalid read register address: {addr:#x}");
                Ok(0)
            }
        }
    }

    /// Handles 8-bit write access to VGIC registers.
    pub fn handle_write8(&self, addr: usize, value: usize) -> VgicResult {
        self.handle_write32(addr, value)
    }

    /// Handles 16-bit write access to VGIC registers.
    pub fn handle_write16(&self, addr: usize, value: usize) -> VgicResult {
        self.handle_write32(addr, value)
    }

    /// Handles 32-bit write access to VGIC registers.
    pub fn handle_write32(&self, addr: usize, value: usize) -> VgicResult {
        let _vcpu_id = host::current_vcpu_id();
        if let Some(reg) = GicRegister::from_addr(addr as u32) {
            match reg {
                GicRegister::GicdCtlr => self.vgicd.lock().vgicd_ctrlr_write(value),
                // GicRegister::GicdIsenabler(idx) => self.write_isenabler(idx, value),
                GicRegister::GicdIsenabler(idx) if idx > 0 && self.spi_controller.is_some() => {
                    return self.write_spi_enable_word(idx, value);
                }
                GicRegister::GicdIsenabler(idx) => {
                    self.vgicd.lock().vgicd_isenabler_write(idx, value)
                }
                _ => {
                    // error!("Write register address: {:#x}", addr);
                }
            }
        }
        Ok(())
    }

    // Removed, interrupt injection in arm_vcpu
    // pub fn inject_irq(&self, irq: u32) {
    //     self.vgicd.lock().inject_irq(irq);
    // }

    /// Fetches interrupt information for the given IRQ number.
    pub fn fetch_irq(&self, irq: u32) -> VgicResult<VgicInt> {
        self.vgicd.lock().fetch_irq(irq)
    }

    /// Placeholder method for unused operations.
    pub fn nothing(&self, _value: u32) {}

    fn read_spi_enable_word(&self, register_index: u32) -> VgicResult<usize> {
        let controller = self.spi_controller.as_ref().ok_or(VgicError::BadState {
            operation: "read SPI enable state",
            detail: alloc::string::String::from("no SPI controller is attached"),
        })?;
        let first_intid = register_index * 32;
        let mut value = 0usize;
        for bit in 0..32 {
            let raw_intid = first_intid + bit;
            let Ok(intid) = ArmSpiIntId::new(raw_intid) else {
                continue;
            };
            if controller.is_enabled(intid)? {
                value |= 1usize << bit;
            }
        }
        Ok(value)
    }

    fn write_spi_enable_word(&self, register_index: u32, value: usize) -> VgicResult {
        let controller = self.spi_controller.as_ref().ok_or(VgicError::BadState {
            operation: "write SPI enable state",
            detail: alloc::string::String::from("no SPI controller is attached"),
        })?;
        let first_intid = register_index * 32;
        for bit in 0..32 {
            if value & (1usize << bit) == 0 {
                continue;
            }
            let raw_intid = first_intid + bit;
            let Ok(intid) = ArmSpiIntId::new(raw_intid) else {
                continue;
            };
            match controller.set_enabled(intid, true) {
                Ok(ServiceHint::None) | Err(VgicError::UnregisteredSpi { .. }) => {}
                Ok(ServiceHint::Target(_)) => {
                    controller.set_enabled(intid, false)?;
                    return Err(VgicError::BadState {
                        operation: "write SPI enable state",
                        detail: alloc::string::String::from(
                            "enabling a pending SPI requires a runtime service consumer",
                        ),
                    });
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use axdevice_base::{AccessWidth, InterruptTriggerMode};
    use axvm_types::GuestPhysAddr;

    use super::*;

    fn wrapper() -> Vgic {
        let target = crate::VgicVcpuId::new(0);
        let controller = Arc::new(
            ArmVgicController::new([
                (
                    crate::ArmSpiRoute::new(ArmSpiIntId::new(512).unwrap(), target),
                    InterruptTriggerMode::EdgeTriggered,
                ),
                (
                    crate::ArmSpiRoute::new(ArmSpiIntId::new(1019).unwrap(), target),
                    InterruptTriggerMode::LevelTriggered,
                ),
            ])
            .unwrap(),
        );
        Vgic::with_spi_controller(
            GuestPhysAddrRange::from_start_size(0x9000_0000usize.into(), VGICD_REGION_SIZE),
            controller,
        )
        .unwrap()
    }

    #[test]
    fn explicit_range_does_not_alias_other_addresses() {
        let vgic = wrapper();
        assert!(matches!(
            vgic.read_register(GuestPhysAddr::from_usize(0x8000_0100), AccessWidth::Dword),
            Err(axdevice_base::DeviceError::OutOfRange { .. })
        ));
    }

    #[test]
    fn explicit_range_requires_one_aligned_distributor_window() {
        let controller = Arc::new(ArmVgicController::new([]).unwrap());
        assert!(
            Vgic::with_spi_controller(
                GuestPhysAddrRange::from_start_size(0x9000_0000usize.into(), VGICD_REGION_SIZE / 2),
                controller.clone(),
            )
            .is_err()
        );
        assert!(
            Vgic::with_spi_controller(
                GuestPhysAddrRange::from_start_size(0x9000_1000usize.into(), VGICD_REGION_SIZE),
                controller,
            )
            .is_err()
        );
    }

    #[test]
    fn access_must_fit_fully_inside_the_distributor_window() {
        let vgic = wrapper();
        assert!(matches!(
            vgic.read_register(
                GuestPhysAddr::from_usize(0x9000_0000 + VGICD_REGION_SIZE - 3),
                AccessWidth::Dword,
            ),
            Err(axdevice_base::DeviceError::OutOfRange { .. })
        ));
    }

    #[test]
    fn high_registered_spis_share_controller_enable_state() {
        let vgic = wrapper();
        let index_512 = 512 / 32;
        let index_1019 = 1019 / 32;
        let word_512 = vgic
            .read_register(
                GuestPhysAddr::from_usize(0x9000_0000 + 0x100 + index_512 * 4),
                AccessWidth::Dword,
            )
            .unwrap();
        let word_1019 = vgic
            .read_register(
                GuestPhysAddr::from_usize(0x9000_0000 + 0x100 + index_1019 * 4),
                AccessWidth::Dword,
            )
            .unwrap();
        assert_eq!(word_512, 1);
        assert_eq!(word_1019, 1 << (1019 % 32));
    }

    #[test]
    fn spi_enable_proxy_rejects_non_dword_access() {
        let vgic = wrapper();
        assert!(matches!(
            vgic.read_register(GuestPhysAddr::from_usize(0x9000_0104), AccessWidth::Byte),
            Err(axdevice_base::DeviceError::InvalidInput { .. })
        ));
        assert!(matches!(
            vgic.read_register(GuestPhysAddr::from_usize(0x9000_0103), AccessWidth::Dword,),
            Err(axdevice_base::DeviceError::InvalidInput { .. })
        ));
    }
}
