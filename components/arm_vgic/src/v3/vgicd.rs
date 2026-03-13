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

use core::cell::UnsafeCell;

use axaddrspace::{device::AccessWidth, GuestPhysAddr, GuestPhysAddrRange, HostPhysAddr};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};
use axerrno::AxResult;
use axvisor_api::memory::phys_to_virt;
use bitmaps::Bitmap;
use log::debug;

use super::{
    registers::*,
    utils::{perform_mmio_read, perform_mmio_write},
};

/// Default size for GICD region.
pub const DEFAULT_GICD_SIZE: usize = 0x10000; // 64K

/// Virtual Generic Interrupt Controller (VGIC) Distributor (D) implementation.
///
/// For GIC version 3.
pub struct VGicD {
    /// The address of the VGicD in the guest physical address space.
    pub addr: GuestPhysAddr,
    /// The size of the VGicD in bytes.
    pub size: usize,

    /// IRQs assigned to this VGicD.
    pub assigned_irqs: UnsafeCell<Bitmap<{ MAX_IRQ_V3 }>>,

    /// The host physical address of the VGicD.
    ///
    /// TODO: move host gicd access to a separate crate, maybe arm_gic_driver.
    pub host_gicd_addr: HostPhysAddr,
}

impl VGicD {
    /// Creates a new VGicD instance.
    pub fn new(addr: GuestPhysAddr, size: Option<usize>) -> Self {
        let size = size.unwrap_or(DEFAULT_GICD_SIZE);

        Self {
            addr,
            size,
            assigned_irqs: UnsafeCell::new(Bitmap::new()),
            host_gicd_addr: crate::api_reexp::get_host_gicd_base(),
        }
    }

    /// Assigns an IRQ to a specific CPU.
    pub fn assign_irq(&self, irq: u32, cpu_phys_id: usize, target_cpu_affinity: (u8, u8, u8, u8)) {
        debug!(
            "Physically assigning IRQ {irq} to CPU {cpu_phys_id} with affinity {target_cpu_affinity:?}"
        );

        if irq >= MAX_IRQ_V3 as u32 {
            panic!("IRQ {} is out of range for VGicD", irq);
        }
        unsafe {
            (*self.assigned_irqs.get()).set(irq as usize, true);
        }

        // TODO: update host GICD_ITARGETSR and GICD_IROUTER registers
        let gicd_itargetsr_paddr = self.host_gicd_addr + GICD_ITARGETSR + irq as usize;
        let gicd_itargetsr_vaddr = phys_to_virt(gicd_itargetsr_paddr);
        unsafe {
            core::ptr::write_volatile(
                gicd_itargetsr_vaddr.as_mut_ptr_of::<u8>(),
                1u8 << (cpu_phys_id),
            );
        }

        let gicd_irouter_paddr = self.host_gicd_addr + GICD_IROUTER + (irq as usize) * 8;
        let gicd_irouter_vaddr = phys_to_virt(gicd_irouter_paddr);
        unsafe {
            core::ptr::write_volatile(
                gicd_irouter_vaddr.as_mut_ptr_of::<u64>(),
                (target_cpu_affinity.0 as u64) << 32
                    | 1 << 31 // set the routing mode bit
                    | (target_cpu_affinity.1 as u64) << 16
                    | (target_cpu_affinity.2 as u64) << 8
                    | target_cpu_affinity.3 as u64,
            );
        }
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for VGicD {
    fn emu_type(&self) -> axdevice_base::EmuDeviceType {
        EmuDeviceType::GPPTDistributor
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.addr, self.size)
    }

    fn handle_read(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
    ) -> axerrno::AxResult<usize> {
        let gicd_base = self.host_gicd_addr;
        let reg = addr - self.addr;

        debug!("vGICD read reg {reg:#x} width {width:?}");

        match reg {
            reg if GICD_IROUTER_RANGE.contains(&reg) => {
                let irq = (reg - GICD_IROUTER) as u32 / 8;

                if self.is_irq_assigned(irq) && self.is_irq_spi(irq) {
                    perform_mmio_read(gicd_base + reg, width)
                } else {
                    // If the IRQ is not assigned, return 0
                    Ok(0)
                }
            }
            reg if GICD_ITARGETSR_RANGE.contains(&reg) => {
                let irq = (reg - GICD_ITARGETSR) as u32;

                if self.is_irq_assigned(irq) && self.is_irq_spi(irq) {
                    perform_mmio_read(gicd_base + reg, width)
                } else {
                    // If the IRQ is not assigned, return 0
                    Ok(0)
                }
            }
            reg if GICD_ICENABLER_RANGE.contains(&reg)
                || GICD_ISENABLER_RANGE.contains(&reg)
                || GICD_ICPENDR_RANGE.contains(&reg)
                || GICD_ISPENDR_RANGE.contains(&reg)
                || GICD_ICACTIVER_RANGE.contains(&reg)
                || GICD_ISACTIVER_RANGE.contains(&reg) =>
            {
                self.irq_masked_read(reg, reg & 0x7f, 0, width, true)
            }
            reg if GICD_IGROUPR_RANGE.contains(&reg) => {
                self.irq_masked_read(reg, reg & 0x7f, 0, width, false)
            }
            reg if GICD_IGRPMODR_RANGE.contains(&reg) => {
                self.irq_masked_read(reg, reg & 0x7f, 0, width, false)
            }
            reg if GICD_ICFGR_RANGE.contains(&reg) => {
                self.irq_masked_read(reg, reg & 0xff, 1, width, false)
            }
            reg if GICD_IPRIORITYR_RANGE.contains(&reg) => {
                self.irq_masked_read(reg, reg & 0x3ff, 3, width, false)
            }
            reg if GICDV3_PIDR0_RANGE.contains(&reg)
                || GICDV3_PIDR4_RANGE.contains(&reg)
                || GICDV3_CIDR0_RANGE.contains(&reg)
                || reg == GICD_CTLR
                || reg == GICD_TYPER
                || reg == GICD_IIDR
                || reg == GICD_TYPER2 =>
            {
                // read-only
                // ignore write
                perform_mmio_read(gicd_base + reg, width)
            }
            _ => {
                todo!("vgicdv3 read unimplemented for reg {:#x}", reg);
            }
        }
    }

    fn handle_write(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
        val: usize,
    ) -> axerrno::AxResult {
        let gicd_base = self.host_gicd_addr;
        let reg = addr - self.addr;

        debug!("vGICD write reg {reg:#x} width {width:?} val {val:#x}");

        match reg {
            reg if GICD_IROUTER_RANGE.contains(&reg) => {
                let irq = (reg - GICD_IROUTER) as u32 / 8;

                if self.is_irq_assigned(irq) && self.is_irq_spi(irq) {
                    perform_mmio_write(gicd_base + reg, width, val)
                } else {
                    // If the IRQ is not assigned, ignore the write
                    Ok(())
                }
            }
            reg if GICD_ITARGETSR_RANGE.contains(&reg) => {
                let irq = (reg - GICD_ITARGETSR) as u32; // it was wrong in hVisor

                if self.is_irq_assigned(irq) && self.is_irq_spi(irq) {
                    perform_mmio_write(gicd_base + reg, width, val)
                } else {
                    // If the IRQ is not assigned, ignore the write
                    Ok(())
                }
            }
            reg if GICD_ICENABLER_RANGE.contains(&reg)
                || GICD_ISENABLER_RANGE.contains(&reg)
                || GICD_ICPENDR_RANGE.contains(&reg)
                || GICD_ISPENDR_RANGE.contains(&reg)
                || GICD_ICACTIVER_RANGE.contains(&reg)
                || GICD_ISACTIVER_RANGE.contains(&reg) =>
            {
                self.irq_masked_write(reg, reg & 0x7f, 0, width, true, val)
            }
            reg if GICD_IGROUPR_RANGE.contains(&reg) => {
                self.irq_masked_write(reg, reg & 0x7f, 0, width, false, val)
            }
            reg if GICD_IGRPMODR_RANGE.contains(&reg) => {
                self.irq_masked_write(reg, reg & 0x7f, 0, width, false, val)
            }
            reg if GICD_ICFGR_RANGE.contains(&reg) => {
                self.irq_masked_write(reg, reg & 0xff, 1, width, false, val)
            }
            reg if GICD_IPRIORITYR_RANGE.contains(&reg) => {
                self.irq_masked_write(reg, reg & 0x3ff, 3, width, false, val)
            }
            reg if GICDV3_PIDR0_RANGE.contains(&reg)
                || GICDV3_PIDR4_RANGE.contains(&reg)
                || GICDV3_CIDR0_RANGE.contains(&reg)
                || reg == GICD_CTLR
                || reg == GICD_TYPER
                || reg == GICD_IIDR
                || reg == GICD_TYPER2 =>
            {
                // read-only
                // ignore write
                Ok(())
            }
            _ => {
                todo!("vgicdv3 write unimplemented for reg {:#x}", reg);
            }
        }
    }
}

impl VGicD {
    /// Checks if an IRQ is assigned to this VGicD.
    pub fn is_irq_assigned(&self, irq: u32) -> bool {
        unsafe { (*self.assigned_irqs.get()).get(irq as usize) }
    }

    /// Checks if an IRQ is a Software Generated Interrupt (SGI).
    pub fn is_irq_sgi(&self, irq: u32) -> bool {
        // Check if the IRQ is a Software Generated Interrupt (SGI)
        irq < 16
    }

    /// Checks if an IRQ is a Shared Peripheral Interrupt (SPI).
    pub fn is_irq_spi(&self, irq: u32) -> bool {
        // Check if the IRQ is a Shared Peripheral Interrupt (SPI)
        (16..1020).contains(&irq)
    }

    /// Returns the mask of bits for the irqs assigned to this VGicD, in a bit-field reg.
    pub fn irq_access_mask(
        &self,
        reg_offset: usize,
        bits_per_irq_shift: usize,
        width: AccessWidth,
    ) -> usize {
        if bits_per_irq_shift > 3 {
            panic!(
                "bits_per_irq_shift must be <= 3, got {}",
                bits_per_irq_shift
            );
        }

        // How many IRQs there are in the mmio region the access width covers?
        let irqs_in_access_width = width.size() << (3 - bits_per_irq_shift);
        // The first IRQ at the given register offset.
        let first_irq = reg_offset << (3 - bits_per_irq_shift);
        // The mask of a single IRQ in the bit-field register.
        let single_irq_mask = (1 << (bits_per_irq_shift + 1)) - 1;

        let mut mask = 0;
        for irq in 0..irqs_in_access_width {
            if self.is_irq_assigned((first_irq + irq) as _) {
                // If the IRQ is assigned, set the corresponding bits in the mask.
                mask |= single_irq_mask << (irq << bits_per_irq_shift);
            }
        }

        mask
    }

    /// Performs masked read access to GICD registers.
    pub fn irq_masked_read(
        &self,
        offset: usize,
        reg_offset: usize,
        bits_per_irq_shift: usize,
        width: AccessWidth,
        _is_poke: bool,
    ) -> AxResult<usize> {
        let mask = self.irq_access_mask(reg_offset, bits_per_irq_shift, width);

        Ok(perform_mmio_read(self.host_gicd_addr + offset, width)? & mask)
    }

    /// Performs masked write access to GICD registers.
    pub fn irq_masked_write(
        &self,
        offset: usize,
        reg_offset: usize,
        bits_per_irq_shift: usize,
        width: AccessWidth,
        is_poke: bool,
        val: usize,
    ) -> AxResult<()> {
        let mask = self.irq_access_mask(reg_offset, bits_per_irq_shift, width);

        if is_poke {
            perform_mmio_write(self.host_gicd_addr + offset, width, val & mask)
        } else {
            let _lock = GICD_LOCK.lock();

            let current_value = perform_mmio_read(self.host_gicd_addr + offset, width)?;
            let new_value = (current_value & !mask) | (val & mask);
            perform_mmio_write(self.host_gicd_addr + offset, width, new_value)
        }
    }
}

// Todo: move this lock to arceos or axvisor
static GICD_LOCK: spin::Mutex<()> = spin::Mutex::new(());
