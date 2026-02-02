//! WARNING: Identical mapping only!!

use core::{cell::UnsafeCell, ptr};

use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange, HostPhysAddr};
use axdevice_base::BaseDeviceOps;
use axvisor_api::memory::phys_to_virt;
use log::{debug, trace};
use memory_addr::PhysAddr;
use spin::{Mutex, Once};

use crate::v3::vgicr::get_lpt;

use super::{
    registers::*,
    utils::{enable_one_lpi, perform_mmio_read, perform_mmio_write},
};

/// Virtual GITS registers.
#[derive(Default)]
pub struct VirtualGitsRegs {
    /// Collection table base address register.
    pub ct_baser: usize,
    /// Device table base address register.
    pub dt_baser: usize,

    /// Command queue base address register.
    pub cbaser: usize,
    /// Command queue read pointer.
    pub creadr: usize,
    /// Command queue write pointer.
    pub cwriter: usize,
}

/// Default size for GITS region.
pub const DEFAULT_GITS_SIZE: usize = 0x20_000; // 128Ki: two 64-Ki frames

/// Virtual GICv3 Interrupt Translation Service.
pub struct Gits {
    /// Guest physical address.
    pub addr: GuestPhysAddr,
    /// Size of the GITS region.
    pub size: usize,

    /// Host physical base address of GITS.
    pub host_gits_base: HostPhysAddr,
    /// Whether this is a root VM.
    pub is_root_vm: bool,

    /// Virtual GITS registers.
    pub regs: UnsafeCell<VirtualGitsRegs>,
}

impl Gits {
    fn regs(&self) -> &VirtualGitsRegs {
        unsafe { &*self.regs.get() }
    }

    #[allow(clippy::mut_from_ref)]
    fn regs_mut(&self) -> &mut VirtualGitsRegs {
        unsafe { &mut *self.regs.get() }
    }

    /// Creates a new GITS instance.
    pub fn new(
        addr: GuestPhysAddr,
        size: Option<usize>,
        host_gits_base: HostPhysAddr,
        is_root_vm: bool,
    ) -> Self {
        let size = size.unwrap_or(DEFAULT_GITS_SIZE); // 4K
        let regs = UnsafeCell::new(VirtualGitsRegs::default());

        // ensure cmdq and lpi prop table is initialized before VMs are up
        let _ = get_cmdq(host_gits_base);
        let _ = get_lpt(
            crate::api_reexp::read_vgicd_typer(),
            crate::api_reexp::get_host_gicr_base(),
            None, // Use default size
        );

        Self {
            addr,
            size,
            host_gits_base,
            is_root_vm,
            regs,
        }
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for Gits {
    fn emu_type(&self) -> axdevice_base::EmuDeviceType {
        // todo: determine the correct type
        axdevice_base::EmuDeviceType::GPPTITS
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.addr, self.size)
    }

    fn handle_read(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
    ) -> axerrno::AxResult<usize> {
        let gits_base = self.host_gits_base;
        let reg = addr - self.addr;
        // let reg = mmio.address;

        debug!(
            "vGITS({} @ {:#x}) read reg {:#x} width {:?}",
            if self.is_root_vm { "root" } else { "non-root" },
            self.addr.as_usize(),
            reg,
            width
        );

        // mmio_perform_access(gits_base, mmio);
        match reg {
            GITS_CTRL => perform_mmio_read(gits_base + reg, width),
            GITS_CBASER => Ok(self.regs().cbaser),
            GITS_DT_BASER => {
                if self.is_root_vm {
                    perform_mmio_read(gits_base + reg, width)
                } else {
                    Ok(
                        (self.regs().dt_baser)
                            & (1usize.unbounded_shl(width.size() as u32 * 8) - 1),
                    )
                }
            }
            GITS_CT_BASER => {
                if self.is_root_vm {
                    perform_mmio_read(gits_base + reg, width)
                } else {
                    Ok(
                        (self.regs().ct_baser)
                            & (1usize.unbounded_shl(width.size() as u32 * 8) - 1),
                    )
                }
            }
            GITS_CWRITER => Ok(self.regs().cwriter),
            GITS_CREADR => Ok(self.regs().creadr),
            GITS_TYPER => perform_mmio_read(gits_base + reg, width),
            _ => perform_mmio_read(gits_base + reg, width),
        }
    }

    fn handle_write(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
        val: usize,
    ) -> axerrno::AxResult {
        let gits_base = self.host_gits_base;
        let reg = addr - self.addr;
        // let reg = mmio.address;

        debug!(
            "vGITS({} @ {:#x}) write reg {:#x} width {:?} value {:#x}",
            if self.is_root_vm { "root" } else { "non-root" },
            self.addr.as_usize(),
            reg,
            width,
            val
        );

        // mmio_perform_access(gits_base, mmio);
        match reg {
            GITS_CTRL => perform_mmio_write(gits_base + reg, width, val),
            GITS_CBASER => {
                if self.is_root_vm {
                    perform_mmio_write(gits_base + reg, width, val)?;
                }

                self.regs_mut().cbaser = val;
                Ok(())
            }
            GITS_DT_BASER => {
                if self.is_root_vm {
                    perform_mmio_write(gits_base + reg, width, val)
                } else {
                    self.regs_mut().dt_baser = val;
                    Ok(())
                }
            }
            GITS_CT_BASER => {
                if self.is_root_vm {
                    perform_mmio_write(gits_base + reg, width, val)
                } else {
                    self.regs_mut().ct_baser = val;
                    Ok(())
                }
            }
            GITS_CWRITER => {
                self.regs_mut().cwriter = val;

                if val != 0 {
                    let regs = self.regs();
                    let cbaser = regs.cbaser;
                    let creadr = regs.creadr;

                    let mut cmdq = get_cmdq(self.host_gits_base).lock();
                    self.regs_mut().creadr = cmdq.insert_cmd(cbaser, creadr, val);
                }

                Ok(())
            }
            GITS_CREADR => {
                panic!("GITS_CREADR should not be written by guest!");
            }
            GITS_TYPER => perform_mmio_write(gits_base + reg, width, val),
            _ => perform_mmio_write(gits_base + reg, width, val),
        }
    }
}

/// Command queue for GITS commands.
pub struct Cmdq {
    phy_addr: PhysAddr,
    readr: usize,
    writer: usize,

    host_gits_base: HostPhysAddr,

    dt_addr: HostPhysAddr, // device table addr
    ct_addr: HostPhysAddr, // command table addr
}

impl Drop for Cmdq {
    fn drop(&mut self) {
        trace!("Cmdq dealloc 16 frames: {:?}", self.phy_addr);
        axvisor_api::memory::dealloc_contiguous_frames(self.phy_addr, 16)
    }
}

/// Bytes per GITS command.
pub const BYTES_PER_CMD: usize = 0x20;
/// Quadwords per GITS command.
pub const QWORD_PER_CMD: usize = BYTES_PER_CMD >> 3; // 8 bytes per qword

impl Cmdq {
    fn new(host_gits_base: HostPhysAddr) -> Self {
        let phy_addr = axvisor_api::memory::alloc_contiguous_frames(16, 0).unwrap();
        trace!("Cmdq alloc 16 frames: {phy_addr:?}");
        let mut r = Self {
            phy_addr,
            readr: 0,
            writer: 0,
            host_gits_base,
            dt_addr: 0.into(),
            ct_addr: 0.into(),
        };
        r.init_real_cbaser();
        r
    }

    fn init_real_cbaser(&mut self) {
        let cbaser_addr = self.host_gits_base + GITS_CBASER;
        let cwriter_addr = self.host_gits_base + GITS_CWRITER;
        let cbaser_val = 0xb80000000000040f | self.phy_addr.as_usize();
        let ctlr_addr = self.host_gits_base + GITS_CTRL;

        let cbaser_ptr = phys_to_virt(cbaser_addr).as_mut_ptr_of::<u64>();
        let cwriter_ptr = phys_to_virt(cwriter_addr).as_mut_ptr_of::<u64>();
        let ctlr_ptr = phys_to_virt(ctlr_addr).as_mut_ptr_of::<u64>();

        unsafe {
            let origin_ctrl = ptr::read_volatile(ctlr_ptr);
            debug!("origin_ctrl: {origin_ctrl:#x}");
            ptr::write_volatile(ctlr_ptr, origin_ctrl & 0xfffffffffffffffeu64); // turn off, vm will turn on this ctrl

            ptr::write_volatile(cbaser_ptr, cbaser_val as u64);
            ptr::write_volatile(cwriter_ptr, 0); // init cwriter

            self.init_dummy_dt_ct_baser();

            // wait for the vm to turn it on
        }
    }

    fn init_dummy_dt_ct_baser(&mut self) {
        let dt_baser_addr = self.host_gits_base + GITS_DT_BASER;
        let ct_baser_addr = self.host_gits_base + GITS_CT_BASER;

        let dt_baser_ptr = phys_to_virt(dt_baser_addr).as_mut_ptr_of::<u64>();
        let ct_baser_ptr = phys_to_virt(ct_baser_addr).as_mut_ptr_of::<u64>();

        unsafe {
            let dt_baser = ptr::read_volatile(dt_baser_ptr);
            let ct_baser = ptr::read_volatile(ct_baser_ptr);

            // alloc 64 KiB (16 * 4-KiB frames) each for dt and ct
            let dt_addr = axvisor_api::memory::alloc_contiguous_frames(16, 4).unwrap();
            let ct_addr = axvisor_api::memory::alloc_contiguous_frames(16, 4).unwrap();

            let dt_baser = dt_baser
                | (dt_addr.as_usize() as u64 & 0x0000_ffff_ffff_f000)
                | (1 << 63)     // set valid bit
                // | (0 << 62)     // not indirect table
                | (0b111 << 59) // inner cache: 0b111
                | (0b01 << 10)  // inner shareable
                // | (0b00 << 8)   // 4-KiB page size
                | (16 - 1)      // 16 frames, 64 KiB
                ;
            let ct_baser = ct_baser
                | (ct_addr.as_usize() as u64 & 0x0000_ffff_ffff_f000)
                | (1 << 63)     // set valid bit
                // | (0 << 62)     // not indirect table
                | (0b111 << 59) // inner cache: 0b111
                | (0b01 << 10)  // inner shareable
                // | (0b00 << 8)   // 4-KiB page size
                | (16 - 1); // 16 frames, 64 KiB
            debug!(
                "setting dt_baser: {dt_baser:#x}, ct_baser: {ct_baser:#x}, dt_addr: {dt_addr:?}, ct_addr: {ct_addr:?}"
            );
            ptr::write_volatile(dt_baser_ptr, dt_baser);
            ptr::write_volatile(ct_baser_ptr, ct_baser);
            self.dt_addr = dt_addr;
            self.ct_addr = ct_addr;
        }
    }

    // it's ok to add qemu-args: -trace gicv3_gits_cmd_*, remember to remain `enable one lpi`
    fn analyze_cmd(&self, value: [u64; 4]) {
        let code = (value[0] & 0xff) as usize;
        match code {
            0x0b => {
                let id = value[0] & 0xffffffff00000000;
                let event = value[1] & 0xffffffff;
                let icid = value[2] & 0xffff;
                enable_one_lpi((event - 8192) as _);
                debug!(
                    "MAPI cmd, for device {:#x}, event = intid = {:#x} -> icid {:#x}",
                    id >> 32,
                    event,
                    icid
                );
            }
            0x08 => {
                let id = value[0] & 0xffffffff00000000;
                let itt_base = (value[2] & 0x000fffffffffffff) >> 8;
                debug!(
                    "MAPD cmd, set ITT: {:#x} to device {:#x}",
                    itt_base,
                    id >> 32
                );
            }
            0x0a => {
                let id = value[0] & 0xffffffff00000000;
                let event = value[1] & 0xffffffff;
                let intid = value[1] >> 32;
                let icid = value[2] & 0xffff;
                enable_one_lpi((intid - 8192) as _);
                debug!(
                    "MAPTI cmd, for device {:#x}, event {:#x} -> icid {:#x} + intid {:#x}",
                    id >> 32,
                    event,
                    icid,
                    intid
                );
            }
            0x09 => {
                let icid = value[2] & 0xffff;
                let rd_base = (value[2] >> 16) & 0x7ffffffff;
                debug!("MAPC cmd, icid {icid:#x} -> redist {rd_base:#x}");
            }
            0x05 => {
                debug!("SYNC cmd");
            }
            0x04 => {
                debug!("CLEAR cmd");
            }
            0x0f => {
                debug!("DISCARD cmd");
            }
            0x03 => {
                debug!("INT cmd");
            }
            0x0c => {
                debug!("INV cmd");
            }
            0x0d => {
                debug!("INVALL cmd");
            }
            _ => {
                debug!("other cmd, code: 0x{code:x}");
            }
        }
    }

    /// WARNING: this function supports only GPA-HPA identical mapping!
    fn insert_cmd(&mut self, vm_cbaser: usize, vm_creadr: usize, vm_writer: usize) -> usize {
        let vm_addr = vm_cbaser & 0xf_ffff_ffff_f000;

        let origin_vm_readr = vm_creadr;

        // todo: handle wrap around
        let cmd_size = vm_writer - origin_vm_readr;
        let cmd_num = cmd_size / BYTES_PER_CMD;

        trace!(
            "vm_cbaser: {vm_cbaser:#x}, vm_creadr: {vm_creadr:#x}, vm_writer: {vm_writer:#x}, vm_addr: {vm_addr:#x}"
        );
        debug!("cmd size: {cmd_size:#x}, cmd num: {cmd_num:#x}");

        let mut vm_cmdq_addr = PhysAddr::from_usize(vm_addr + origin_vm_readr);
        let mut real_cmdq_addr = self.phy_addr + self.readr;

        for _cmd_id in 0..cmd_num {
            let vm_cmdq_ptr = phys_to_virt(vm_cmdq_addr).as_mut_ptr_of::<[u64; QWORD_PER_CMD]>();
            let mut real_cmdq_ptr = phys_to_virt(real_cmdq_addr).as_mut_ptr_of::<u64>();

            unsafe {
                let v = ptr::read_volatile(vm_cmdq_ptr);
                self.analyze_cmd(v);

                for &one in v.iter().take(QWORD_PER_CMD) {
                    ptr::write_volatile(real_cmdq_ptr, one);
                    real_cmdq_addr += 8;
                    real_cmdq_ptr = real_cmdq_ptr.add(1);
                }
            }
            vm_cmdq_addr += BYTES_PER_CMD;
            vm_cmdq_addr = (ring_ptr_update(vm_cmdq_addr.as_usize() - vm_addr) + vm_addr).into();
            real_cmdq_addr =
                (ring_ptr_update(real_cmdq_addr - self.phy_addr) + self.phy_addr.as_usize()).into();
        }

        self.writer += cmd_size;
        self.writer = ring_ptr_update(self.writer); // ring buffer ptr
        let cwriter_addr = self.host_gits_base + GITS_CWRITER;
        let creadr_addr = self.host_gits_base + GITS_CREADR;

        let cwriter_ptr = phys_to_virt(cwriter_addr).as_mut_ptr_of::<u64>();
        let creadr_ptr = phys_to_virt(creadr_addr).as_mut_ptr_of::<u64>();
        // let ctlr_ptr = phys_to_virt(self.host_gits_base + GITS_CTRL).as_mut_ptr_of::<u64>();
        unsafe {
            ptr::write_volatile(cwriter_ptr, self.writer as _);
            loop {
                self.readr = (ptr::read_volatile(creadr_ptr)) as usize; // hw readr
                if self.readr == self.writer {
                    debug!(
                        "readr={:#x}, writer={:#x}, its cmd end",
                        self.readr, self.writer
                    );
                    break;
                }
            }
        }

        vm_writer
    }
}

static CMDQ: Once<Mutex<Cmdq>> = Once::new();

fn get_cmdq(host_gits_base: HostPhysAddr) -> &'static Mutex<Cmdq> {
    if !CMDQ.is_completed() {
        CMDQ.call_once(|| Mutex::new(Cmdq::new(host_gits_base)));
    }

    CMDQ.get().unwrap()
}

fn ring_ptr_update(val: usize) -> usize {
    if val >= 0x10000 {
        val - 0x10000
    } else {
        val
    }
}
