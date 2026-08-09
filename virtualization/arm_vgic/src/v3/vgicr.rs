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

use core::ptr;

use ax_kspin::SpinNoIrq;
use ax_memory_addr::PhysAddr;
use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceError, DeviceResult,
    Resource,
};
use axvm_types::{GuestPhysAddr, HostPhysAddr};
use log::{debug, trace};
use spin::Once;

use super::{
    registers::*,
    utils::{perform_mmio_read, perform_mmio_write},
};
use crate::host;

/// Default size per GICR region.
pub const DEFAULT_SIZE_PER_GICR: usize = 0x20000; // 128K: 64K for SGI/PPI, then 64K for LPI

const HOST_IPI_SGI: usize = 0;
// ArceOS AArch64's host systick is wired to PPI26 on the supported platforms.
const HOST_TIMER_PPI: usize = 26;
const HOST_PRIVATE_INTERRUPT_MASK: usize =
    (1 << HOST_IPI_SGI) | (1 << MAINTENACE_INTERRUPT) | (1 << HOST_TIMER_PPI);

/// Removes host-owned SGI/PPI lines from a guest write-one-to-clear mask.
fn mask_host_private_interrupts(value: usize) -> usize {
    value & !HOST_PRIVATE_INTERRUPT_MASK
}

/// Virtual GICR registers.
pub struct VGicRRegs {
    /// LPI configuration table base address.
    pub propbaser: usize,
}

/// Virtual GICv3 Redistributor.
pub struct VGicR {
    /// The address of the VGicR in the guest physical address space.
    pub addr: GuestPhysAddr,
    /// The size of the VGicR in bytes.
    pub size: usize,
    /// Stable guest resources declared to the V3 device runtime.
    resources: [Resource; 1],

    /// CPU ID associated with this redistributor.
    pub cpu_id: usize,
    /// Whether this is the last redistributor exposed to the VM.
    is_last: bool,
    /// Host physical base address of GICR for this CPU.
    pub host_gicr_base_this_cpu: HostPhysAddr,

    /// Virtual GICR registers.
    pub regs: SpinNoIrq<VGicRRegs>,
}

impl VGicR {
    /// Accesses the registers immutably under the internal lock.
    fn with_regs<R>(&self, f: impl FnOnce(&VGicRRegs) -> R) -> R {
        f(&self.regs.lock())
    }

    /// Accesses the registers mutably under the internal lock.
    fn with_regs_mut<R>(&self, f: impl FnOnce(&mut VGicRRegs) -> R) -> R {
        f(&mut self.regs.lock())
    }

    /// Creates a new VGicR instance.
    pub fn new(addr: GuestPhysAddr, size: Option<usize>, cpu_id: usize, is_last: bool) -> Self {
        let size = size.unwrap_or(DEFAULT_SIZE_PER_GICR);
        let host_gicr_base_this_cpu = crate::api_reexp::get_host_gicr_base() + cpu_id * size;

        Self {
            addr,
            size,
            resources: [Resource::MmioRange {
                base: addr.as_usize() as u64,
                size: size as u64,
            }],
            cpu_id,
            is_last,
            host_gicr_base_this_cpu,
            regs: SpinNoIrq::new(VGicRRegs { propbaser: 0 }),
        }
    }
}

impl VGicR {
    fn contains(&self, addr: GuestPhysAddr) -> bool {
        let base = self.addr.as_usize();
        let end = base.saturating_add(self.size);
        let addr = addr.as_usize();
        addr >= base && addr < end
    }

    /// Reads a guest-visible GIC redistributor register.
    pub fn read_register(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        if !self.contains(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        let gicr_base = self.host_gicr_base_this_cpu;
        let reg = addr - self.addr;

        debug!(
            "vGICR ({} @ {:#x}) read reg {:#x} width {:?}",
            self.cpu_id, self.addr, reg, width
        );

        let result = match reg {
            GICR_CTLR => {
                // TODO: is cross vcpu access allowed?
                perform_mmio_read(gicr_base + reg, width)
            }
            GICR_TYPER => {
                let mut value = perform_mmio_read(gicr_base + reg, width)?;

                if self.is_last {
                    value |= GICR_TYPER_LAST;
                }

                Ok(value)
            }
            GICR_IIDR | GICR_IMPL_DEF_IDENT_REGS_START..=GICR_IMPL_DEF_IDENT_REGS_END => {
                // Make these read-only registers accessible.
                perform_mmio_read(gicr_base + reg, width)
            }
            GICR_PENDBASER => {
                // every redist have its own pending tbl
                perform_mmio_read(gicr_base + reg, width)
            }
            GICR_PROPBASER => {
                // all the redist share one prop tbl
                // mmio_perform_access(gicr_base, mmio);

                Ok(self.with_regs(|r| r.propbaser))
            }
            GICR_SYNCR => {
                // always return 0 for synchronization register
                Ok(0)
            }
            GICR_SETLPIR | GICR_CLRLPIR | GICR_INVALLR => perform_mmio_read(gicr_base + reg, width),
            reg if reg == GICR_STATUSR
                || reg == GICR_WAKER
                || reg == GICR_IGROUPR
                || reg == GICR_ISENABLER
                || reg == GICR_ICENABLER
                || reg == GICR_ISPENDR
                || reg == GICR_ICPENDR
                || reg == GICR_ISACTIVER
                || reg == GICR_ICACTIVER
                || reg == GICR_IGRPMODR
                || GICR_IPRIORITYR_RANGE.contains(&reg)
                || GICR_ICFGR_RANGE.contains(&reg) =>
            {
                perform_mmio_read(gicr_base + reg, width)
            }
            _ => {
                todo!("vgicr read unimplemented for reg {:#x}", reg);
            }
        };
        Ok(result?)
    }

    /// Writes a guest-visible GIC redistributor register.
    pub fn write_register(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        value: usize,
    ) -> DeviceResult<()> {
        if !self.contains(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        let gicr_base = self.host_gicr_base_this_cpu;
        let reg = addr - self.addr;

        debug!(
            "vGICR ({} @ {:#x}) write reg {:#x} width {:?} value {:#x}",
            self.cpu_id, self.addr, reg, width, value
        );

        let result = match reg {
            GICR_CTLR => {
                // TODO: is cross zone access allowed?
                perform_mmio_write(gicr_base + reg, width, value)
            }
            GICR_PENDBASER => {
                // every redist have its own pending tbl
                perform_mmio_write(gicr_base + reg, width, value)
            }
            GICR_PROPBASER => {
                // all the redist share one prop tbl
                self.with_regs_mut(|r| r.propbaser = value);
                Ok(())
            }
            GICR_SETLPIR | GICR_CLRLPIR | GICR_INVALLR => {
                perform_mmio_write(gicr_base + reg, width, value)
            }
            GICR_INVLPIR => {
                // Presume that this write is to enable an LPI.
                // Or we need to check all the proptbl created by vm.
                enable_one_lpi((value & 0xffffffff) - 8192); // ⬅️Why?
                Ok(())
            }
            reg if reg == GICR_STATUSR
                || reg == GICR_WAKER
                || reg == GICR_IGROUPR
                || reg == GICR_ISENABLER
                || reg == GICR_ICENABLER
                || reg == GICR_ISPENDR
                || reg == GICR_ICPENDR
                || reg == GICR_ISACTIVER
                || reg == GICR_ICACTIVER
                || reg == GICR_IGRPMODR
                || GICR_IPRIORITYR_RANGE.contains(&reg)
                || GICR_ICFGR_RANGE.contains(&reg) =>
            {
                // Guest CPU-interface initialization clears every private
                // interrupt. Keep host kick, maintenance, and timer lines
                // enabled on the physical redistributor backing this vGICR.
                let value = if reg == GICR_ICENABLER {
                    mask_host_private_interrupts(value)
                } else {
                    value
                };
                perform_mmio_write(gicr_base + reg, width, value)
            }
            _ => {
                todo!("vgicr write unimplemented for reg {:#x}", reg);
            }
        };
        Ok(result?)
    }
}

impl Device for VGicR {
    fn name(&self) -> &str {
        "aarch64-gic-redistributor"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let addr = GuestPhysAddr::from_usize(access.addr as usize);
        if access.is_read {
            self.read_register(addr, access.width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
        } else {
            self.write_register(addr, access.width, access.data as usize)
                .map(|_| BusResponse::Write)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HOST_IPI_SGI, HOST_TIMER_PPI, mask_host_private_interrupts};

    #[test]
    fn guest_disable_write_preserves_host_private_interrupts() {
        let guest_disable_mask = u32::MAX as usize;
        let filtered = mask_host_private_interrupts(guest_disable_mask);

        assert_eq!(
            filtered & ((1 << HOST_IPI_SGI) | (1 << 25) | (1 << HOST_TIMER_PPI)),
            0,
        );
        assert_ne!(filtered & (1 << 30), 0);
    }
}

// todo: move the lpi prop table to arm-gic-driver, and find a good interface to use it.
/// LPI property table for managing Locality-specific Peripheral Interrupts.
pub struct LpiPropTable {
    frame: PhysAddr,
    frame_pages: usize,
    _host_gicr_base: HostPhysAddr,
}

impl Drop for LpiPropTable {
    fn drop(&mut self) {
        trace!("LpiPropTable dropped, frame: {:?}", self.frame);
        host::dealloc_contiguous_frames(self.frame, self.frame_pages);
    }
}

impl LpiPropTable {
    fn new(
        host_gicd_typer: u32,
        host_gicr_base: HostPhysAddr,
        size_per_gicr: Option<usize>,
        cpu_num: usize,
    ) -> Self {
        let size_per_gicr = size_per_gicr.unwrap_or(DEFAULT_SIZE_PER_GICR);
        let id_bits = (host_gicd_typer >> 19) & 0x1f;
        let page_num: usize = ((1 << (id_bits + 1)) - 8192) / ax_memory_addr::PAGE_SIZE_4K;

        debug!(
            "Creating LPI prop table: id_bits: {id_bits}, page_num: {page_num}, size_per_gicr: \
             {size_per_gicr}"
        );

        let f = host::alloc_contiguous_frames(page_num, ax_memory_addr::PAGE_SIZE_4K)
            .expect("Failed to allocate contiguous frames for LPI prop table");
        let propreg = f.as_usize() | 0x78f;
        for id in 0..cpu_num {
            let propbaser = host_gicr_base + id * size_per_gicr + GICR_PROPBASER;
            let propbaser = host::phys_to_virt(propbaser);
            debug!("Setting propbaser for CPU {id}: {propbaser:#x} -> {propreg:#x}");
            unsafe {
                ptr::write_volatile(propbaser.as_mut_ptr_of::<u64>(), propreg as _);
            }
        }
        Self {
            frame: f,
            frame_pages: page_num,
            _host_gicr_base: host_gicr_base,
        }
    }

    fn enable_one_lpi(&self, lpi: usize) {
        debug!("Enabling one LPI: {lpi}");
        let addr = self.frame + lpi;
        let val = 0b1;

        let addr = host::phys_to_virt(addr);
        // no priority
        unsafe {
            ptr::write_volatile(addr.as_mut_ptr_of::<u8>(), val);
        }
    }
}

/// Global LPI property table instance.
pub static LPT: Once<SpinNoIrq<LpiPropTable>> = Once::new();

/// Gets or initializes the global LPI property table.
pub fn get_lpt(
    host_gicd_typer: u32,
    host_gicr_base: HostPhysAddr,
    size_per_gicr: Option<usize>,
) -> &'static SpinNoIrq<LpiPropTable> {
    if !LPT.is_completed() {
        LPT.call_once(|| {
            SpinNoIrq::new(LpiPropTable::new(
                host_gicd_typer,
                host_gicr_base,
                size_per_gicr,
                host::host_cpu_num(),
            ))
        });
    }

    LPT.get().unwrap()
}

/// Enables a single LPI by updating the property table.
pub fn enable_one_lpi(lpi: usize) {
    let lpt = get_lpt(
        crate::api_reexp::read_vgicd_typer(),
        crate::api_reexp::get_host_gicr_base(),
        None, // Use default size
    );
    let lpt = lpt.lock();
    lpt.enable_one_lpi(lpi);
}
