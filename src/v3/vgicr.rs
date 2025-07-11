use core::{cell::UnsafeCell, ptr};

use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange, HostPhysAddr};
use axdevice_base::BaseDeviceOps;
use axvisor_api::memory::phys_to_virt;
use log::{debug, trace};
use memory_addr::PhysAddr;
use spin::{Mutex, Once};

use super::{
    registers::{
        GICD_TYPER, GICR_CLRLPIR, GICR_CTLR, GICR_ICACTIVER, GICR_ICENABLER, GICR_ICFGR,
        GICR_ICFGR_RANGE, GICR_ICPENDR, GICR_IGROUPR, GICR_IGRPMODR, GICR_IIDR,
        GICR_IMPL_DEF_IDENT_REGS_END, GICR_IMPL_DEF_IDENT_REGS_START, GICR_INVALLR, GICR_INVLPIR,
        GICR_IPRIORITYR, GICR_IPRIORITYR_RANGE, GICR_ISACTIVER, GICR_ISENABLER, GICR_ISPENDR,
        GICR_PENDBASER, GICR_PROPBASER, GICR_SETLPIR, GICR_SGI_BASE, GICR_STATUSR, GICR_SYNCR,
        GICR_TYPER, GICR_TYPER_LAST, GICR_WAKER, MAINTENACE_INTERRUPT,
    },
    utils::{perform_mmio_read, perform_mmio_write},
};

pub const DEFAULT_SIZE_PER_GICR: usize = 0x20000; // 128K: 64K for SGI/PPI, then 64K for LPI

pub struct VGicRRegs {
    pub propbaser: usize,
}

pub struct VGicR {
    /// The address of the VGicR in the guest physical address space.
    pub addr: GuestPhysAddr,
    /// The size of the VGicR in bytes.
    pub size: usize,

    pub cpu_id: usize,
    pub host_gicr_base_this_cpu: HostPhysAddr,

    pub regs: UnsafeCell<VGicRRegs>,
}

impl VGicR {
    pub fn regs(&self) -> &VGicRRegs {
        unsafe { &*self.regs.get() }
    }

    #[allow(clippy::mut_from_ref)]
    pub fn regs_mut(&self) -> &mut VGicRRegs {
        unsafe { &mut *self.regs.get() }
    }

    pub fn new(addr: GuestPhysAddr, size: Option<usize>, cpu_id: usize) -> Self {
        let size = size.unwrap_or(DEFAULT_SIZE_PER_GICR);
        let host_gicr_base_this_cpu = axvisor_api::arch::get_host_gicr_base() + cpu_id * size;

        Self {
            addr,
            size,
            cpu_id,
            host_gicr_base_this_cpu,
            regs: UnsafeCell::new(VGicRRegs { propbaser: 0 }),
        }
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for VGicR {
    fn emu_type(&self) -> axdevice_base::EmuDeviceType {
        axdevice_base::EmuDeviceType::EmuDeviceTInterruptController
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.addr, self.size)
    }

    fn handle_read(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
    ) -> axerrno::AxResult<usize> {
        let gicr_base = self.host_gicr_base_this_cpu;
        let reg = addr - self.addr;

        debug!(
            "vGICR ({} @ {:#x}) read reg {:#x} width {:?}",
            self.cpu_id, self.addr, reg, width
        );

        match reg {
            GICR_CTLR => {
                // TODO: is cross vcpu access allowed?
                perform_mmio_read(gicr_base + reg, width)
            }
            GICR_TYPER => {
                let mut value = perform_mmio_read(gicr_base + reg, width)?;

                // TODO: set GICR_TYPER_LAST if it is the last redistributor of a VM.
                if true {
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

                Ok(self.regs().propbaser)
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
        }
    }

    fn handle_write(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
        value: usize,
    ) -> axerrno::AxResult<()> {
        let gicr_base = self.host_gicr_base_this_cpu;
        let reg = addr - self.addr;

        debug!(
            "vGICR ({} @ {:#x}) write reg {:#x} width {:?} value {:#x}",
            self.cpu_id, self.addr, reg, width, value
        );

        match reg {
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
                self.regs_mut().propbaser = value;
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
                let mut value = value;
                // avoid linux disable maintenance interrupt
                if reg == GICR_ICENABLER {
                    value &= !(1 << MAINTENACE_INTERRUPT);
                    // value &= !(1 << SGI_IPI_ID);
                }
                perform_mmio_write(gicr_base + reg, width, value)
            }
            _ => {
                todo!("vgicr write unimplemented for reg {:#x}", reg);
            }
        }
    }
}

// todo: move the lpi prop table to arm-gic-driver, and find a good interface to use it.
pub struct LpiPropTable {
    frame: PhysAddr,
    frame_pages: usize,
    host_gicr_base: HostPhysAddr,
}

impl Drop for LpiPropTable {
    fn drop(&mut self) {
        trace!("LpiPropTable dropped, frame: {:?}", self.frame);
        axvisor_api::memory::dealloc_contiguous_frames(self.frame, self.frame_pages);
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
        let page_num: usize = ((1 << (id_bits + 1)) - 8192) / memory_addr::PAGE_SIZE_4K;

        debug!(
            "Creating LPI prop table: id_bits: {}, page_num: {}, size_per_gicr: {}",
            id_bits, page_num, size_per_gicr
        );

        let f = axvisor_api::memory::alloc_contiguous_frames(page_num, 0)
            .expect("Failed to allocate contiguous frames for LPI prop table");
        let propreg = f.as_usize() | 0x78f;
        for id in 0..cpu_num {
            let propbaser = host_gicr_base + id * size_per_gicr + GICR_PROPBASER;
            let propbaser = phys_to_virt(propbaser);
            debug!(
                "Setting propbaser for CPU {}: {:#x} -> {:#x}",
                id, propbaser, propreg
            );
            unsafe {
                ptr::write_volatile(propbaser.as_mut_ptr_of::<u64>(), propreg as _);
            }
        }
        Self {
            frame: f,
            frame_pages: page_num,
            host_gicr_base,
        }
    }

    fn enable_one_lpi(&self, lpi: usize) {
        debug!("Enabling one LPI: {}", lpi);
        let addr = self.frame + lpi;
        let val = 0b1;

        let addr = phys_to_virt(addr);
        // no priority
        unsafe {
            ptr::write_volatile(addr.as_mut_ptr_of::<u8>(), val);
        }
    }
}

pub static LPT: Once<Mutex<LpiPropTable>> = Once::new();

pub fn get_lpt(
    host_gicd_typer: u32,
    host_gicr_base: HostPhysAddr,
    size_per_gicr: Option<usize>,
) -> &'static Mutex<LpiPropTable> {
    if !LPT.is_completed() {
        LPT.call_once(|| {
            Mutex::new(LpiPropTable::new(
                host_gicd_typer,
                host_gicr_base,
                size_per_gicr,
                axvisor_api::host::get_host_cpu_num(),
            ))
        });
    }

    LPT.get().unwrap()
}

pub fn enable_one_lpi(lpi: usize) {
    let lpt = get_lpt(
        axvisor_api::arch::read_vgicd_typer(),
        axvisor_api::arch::get_host_gicr_base(),
        None, // Use default size
    );
    let lpt = lpt.lock();
    lpt.enable_one_lpi(lpi);
}
