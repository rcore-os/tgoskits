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

#[cfg(not(test))]
use ax_kspin::SpinNoIrq as Mutex;
#[cfg(test)]
use ax_kspin::SpinRaw as Mutex;

use crate::{VgicResult, interrupt::VgicInt, registers::GicRegister, vgicd::Vgicd};

/// Virtual Generic Interrupt Controller.
///
/// Manages virtual interrupt distribution for guest VMs.
pub struct Vgic {
    vgicd: Mutex<Vgicd>,
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
        }
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
                GicRegister::GicdIsenabler(idx) => Ok(self.vgicd.lock().vgicd_isenabler_read(idx)),
                GicRegister::GicdIcenabler(idx) => Ok(self.vgicd.lock().vgicd_isenabler_read(idx)),
                GicRegister::GicdItargetsr(idx) if idx < 8 => {
                    Ok(usize::from(crate::api_reexp::current_cpu_target()) * 0x0101_0101)
                }
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
    pub fn handle_write8(&self, addr: usize, value: usize) {
        self.handle_write32(addr, value);
    }

    /// Handles 16-bit write access to VGIC registers.
    pub fn handle_write16(&self, addr: usize, value: usize) {
        self.handle_write32(addr, value);
    }

    /// Handles 32-bit write access to VGIC registers.
    pub fn handle_write32(&self, addr: usize, value: usize) {
        if let Some(reg) = GicRegister::from_addr(addr as u32) {
            match reg {
                GicRegister::GicdCtlr => self.vgicd.lock().vgicd_ctrlr_write(value),
                // GicRegister::GicdIsenabler(idx) => self.write_isenabler(idx, value),
                GicRegister::GicdIsenabler(idx) => {
                    self.vgicd.lock().vgicd_isenabler_write(idx, value)
                }
                GicRegister::GicdIcenabler(idx) => {
                    self.vgicd.lock().vgicd_icenabler_write(idx, value)
                }
                _ => {
                    // error!("Write register address: {:#x}", addr);
                }
            }
        }
    }

    // Removed, interrupt injection in arm_vcpu
    // pub fn inject_irq(&self, irq: u32) {
    //     self.vgicd.lock().inject_irq(irq);
    // }

    /// Fetches interrupt information for the given IRQ number.
    pub fn fetch_irq(&self, irq: u32) -> VgicInt {
        self.vgicd.lock().fetch_irq(irq)
    }

    /// Placeholder method for unused operations.
    pub fn nothing(&self, _value: u32) {}
}

#[cfg(test)]
mod tests {
    use super::Vgic;

    #[test]
    fn private_itargetsr_reports_current_cpu_target() {
        let vgic = Vgic::new();

        assert_eq!(vgic.handle_read32(0x800).unwrap(), 0x0101_0101);
        assert_eq!(vgic.handle_read32(0x81c).unwrap(), 0x0101_0101);
    }

    #[test]
    fn icenabler_write_disables_virtual_and_host_irq() {
        crate::api_reexp::reset_host_irq_enables();
        let vgic = Vgic::new();

        vgic.handle_write32(0x104, (1 << 3) | (1 << 9));
        vgic.handle_write32(0x184, 1 << 3);

        assert!(!crate::api_reexp::host_irq_is_enabled(35));
        assert!(crate::api_reexp::host_irq_is_enabled(41));
        assert_eq!(vgic.handle_read32(0x184).unwrap(), 1 << 9);
    }
}
