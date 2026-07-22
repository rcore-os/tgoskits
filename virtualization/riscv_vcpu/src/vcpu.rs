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

use core::marker::PhantomData;

use riscv::register::{scause, sie, sstatus};
use riscv_decode::{
    Instruction,
    types::{IType, SType},
};
#[cfg(feature = "sstc")]
use riscv_h::register::vstimecmp;
use riscv_h::register::{
    henvcfg, hgeie, hie, hstatus, htimedelta, hvip,
    vsatp::{self, Vsatp},
    vscause::{self, Vscause},
    vsepc,
    vsie::{self, Vsie},
    vsscratch,
    vsstatus::{self, Vsstatus},
    vstval,
    vstvec::{self, Vstvec},
};
use rustsbi::{Forward, RustSBI};
use sbi_spec::{hsm, legacy, pmu, rfnc, srst};

use crate::{
    EID_HVC, RiscvVcpuCreateConfig,
    consts::traps::irq::S_EXT,
    guest_mem,
    host::RiscvHostOps,
    registers::hgatp_value,
    regs::*,
    sbi_console::*,
    trap::Exception,
    types::{
        RiscvAccessFlags, RiscvAccessWidth, RiscvGuestPhysAddr, RiscvGuestVirtAddr,
        RiscvNestedPagingConfig, RiscvVcpuError, RiscvVcpuResult, RiscvVmExit,
    },
    vpmu::VirtualPmu,
};

unsafe extern "C" {
    fn _run_guest(state: *mut VmCpuRegisters);
}

const TINST_PSEUDO_STORE: u32 = 0x3020;
const TINST_PSEUDO_LOAD: u32 = 0x3000;
const EID_TIME: usize = 0x5449_4D45;
const FID_SET_TIMER: usize = 0;
#[cfg(feature = "sstc")]
const SYSTEM_OPCODE: u32 = 0x73;
#[cfg(feature = "sstc")]
const CSR_STIMECMP: u16 = 0x14d;

#[inline]
fn instr_is_pseudo(ins: u32) -> bool {
    ins == TINST_PSEUDO_STORE || ins == TINST_PSEUDO_LOAD
}

/// A virtual CPU within a guest
pub struct RiscvVcpu<H: RiscvHostOps> {
    regs: VmCpuRegisters,
    sbi: RISCVVCpuSbi,
    _host: PhantomData<fn() -> H>,
}

/// Backward-compatible mixed-case vCPU alias.
pub type RiscvVCpu<H> = RiscvVcpu<H>;

/// Backward-compatible upper-case vCPU alias.
pub type RISCVVCpu<H> = RiscvVcpu<H>;

#[derive(RustSBI)]
struct RISCVVCpuSbi {
    #[rustsbi(pmu)]
    pmu: VirtualPmu,
    #[rustsbi(console, fence, reset, info, hsm, timer)]
    forward: Forward,
}

#[cfg(feature = "sstc")]
/// Result of reading an instruction for virtual-instruction emulation.
enum VirtualInstructionRead {
    Instruction(u32),
    Handled(RiscvVmExit),
}

/// Result of decoding the trapped guest load/store instruction.
enum InstructionDecode {
    Decoded(Instruction, usize),
    Handled(RiscvVmExit),
}

impl Default for RISCVVCpuSbi {
    #[inline]
    fn default() -> Self {
        Self {
            pmu: VirtualPmu::default(),
            forward: Forward,
        }
    }
}

impl<H: RiscvHostOps> Default for RiscvVcpu<H> {
    fn default() -> Self {
        Self {
            regs: VmCpuRegisters::default(),
            sbi: RISCVVCpuSbi::default(),
            _host: PhantomData,
        }
    }
}

impl<H: RiscvHostOps> RiscvVcpu<H> {
    /// Creates a new RISC-V vCPU.
    pub fn new(
        _vm_id: usize,
        _vcpu_id: usize,
        config: RiscvVcpuCreateConfig,
    ) -> RiscvVcpuResult<Self> {
        let mut regs = VmCpuRegisters::default();
        // Setup the guest's general purpose registers.
        // `a0` is the hartid
        regs.guest_regs.gprs.set_reg(GprIndex::A0, config.hart_id);
        // `a1` is the address of the device tree blob.
        regs.guest_regs.gprs.set_reg(GprIndex::A1, config.dtb_addr);
        regs.virtual_hs_csrs.henvcfg = config.isa.henvcfg().bits();

        Ok(Self {
            regs,
            sbi: RISCVVCpuSbi::default(),
            _host: PhantomData,
        })
    }

    /// Completes architecture-specific setup.
    pub fn setup(&mut self, _config: ()) -> RiscvVcpuResult {
        // Set sstatus.
        let mut sstatus = sstatus::read();
        sstatus.set_sie(false);
        sstatus.set_spie(false);
        sstatus.set_spp(sstatus::SPP::Supervisor);
        self.regs.guest_regs.sstatus = sstatus.bits();

        // Set hstatus.
        let mut hstatus = hstatus::read();
        hstatus.set_spv(true);
        hstatus.set_vsxl(hstatus::VsxlValues::Vsxl64);
        // Set SPVP bit in order to accessing VS-mode memory from HS-mode.
        hstatus.set_spvp(true);
        // Let the guest execute its normal supervisor instructions without
        // spuriously trapping them back to the hypervisor.
        hstatus.set_vtvm(false);
        hstatus.set_vtw(false);
        hstatus.set_vtsr(false);
        unsafe {
            hstatus.write();
        }
        self.regs.guest_regs.hstatus = hstatus.bits();

        let mut hie = hie::Hie::from_bits(0);
        hie.set_vssie(true);
        hie.set_vstie(true);
        hie.set_vseie(true);
        #[cfg(feature = "sstc")]
        {
            // Start with no guest timer deadline armed; a zeroed vstimecmp
            // would be observed as already expired and inject a spurious timer
            // interrupt before Linux programs its first clockevent.
            self.regs.vs_csrs.vstimecmp = usize::MAX;
        }
        self.regs.virtual_hs_csrs.hie = hie.bits();
        self.regs.virtual_hs_csrs.hvip = 0;
        self.regs.virtual_hs_csrs.hgeie = 0;

        Ok(())
    }

    /// Sets the guest entry point.
    pub fn set_entry(&mut self, entry: RiscvGuestPhysAddr) -> RiscvVcpuResult {
        self.regs.guest_regs.sepc = entry.as_usize();
        Ok(())
    }

    /// Sets the nested page table used by guest-stage translation.
    pub fn set_nested_page_table(&mut self, config: RiscvNestedPagingConfig) -> RiscvVcpuResult {
        let expected_mode = match config.levels {
            3 => 8,
            4 => 9,
            _ => {
                return Err(RiscvVcpuError::InvalidInput);
            }
        };
        if config.mode != expected_mode || config.root_paddr.as_usize() & 0x3fff != 0 {
            return Err(RiscvVcpuError::InvalidInput);
        }

        self.regs.virtual_hs_csrs.hgatp = hgatp_value(config.mode, config.root_paddr.as_usize());
        Ok(())
    }

    /// Runs the vCPU until a host-visible exit occurs.
    pub fn run(&mut self) -> RiscvVcpuResult<RiscvVmExit> {
        unsafe {
            sstatus::clear_sie();
            sie::set_sext();
            sie::set_ssoft();
            // Keep the current HS timer enable state instead of forcing it on
            // for every VM entry. Guest timer re-arming and host timer users
            // must manage `stimer` explicitly, otherwise a pending HS timer can
            // preempt the guest on every re-entry and starve VS interrupt work.
        }
        unsafe {
            // Safe to run the guest as it only touches memory assigned to it by being owned
            // by its page table
            _run_guest(&mut self.regs);
        }
        unsafe {
            sie::clear_sext();
            sie::clear_ssoft();
            sstatus::set_sie();
        }
        self.vmexit_handler()
    }

    /// Binds the vCPU to the current physical CPU.
    pub fn bind(&mut self) -> RiscvVcpuResult {
        // Load the vCPU's CSRs from the stored state.
        unsafe {
            henvcfg::Henvcfg::from_bits(self.regs.virtual_hs_csrs.henvcfg).write();
            let vsatp = Vsatp::from_bits(self.regs.vs_csrs.vsatp);
            vsatp.write();
            let vstvec = Vstvec::from_bits(self.regs.vs_csrs.vstvec);
            vstvec.write();
            let vsepc = self.regs.vs_csrs.vsepc;
            vsepc::write(vsepc);
            let vstval = self.regs.vs_csrs.vstval;
            vstval::write(vstval);
            let htimedelta = self.regs.vs_csrs.htimedelta;
            htimedelta::write(htimedelta);
            let vscause = Vscause::from_bits(self.regs.vs_csrs.vscause);
            vscause.write();
            let vsscratch = self.regs.vs_csrs.vsscratch;
            vsscratch::write(vsscratch);
            let vsstatus = Vsstatus::from_bits(self.regs.vs_csrs.vsstatus);
            vsstatus.write();
            let vsie = Vsie::from_bits(self.regs.vs_csrs.vsie);
            vsie.write();
            #[cfg(feature = "sstc")]
            vstimecmp::write(self.regs.vs_csrs.vstimecmp);
            let hie = hie::Hie::from_bits(self.regs.virtual_hs_csrs.hie);
            hie.write();
            // Restore latched virtual pending interrupts as part of the vCPU
            // context so VM exits do not silently drop timer or external IRQs.
            let hvip = hvip::Hvip::from_bits(self.regs.virtual_hs_csrs.hvip);
            hvip.write();
            hgeie::write(self.regs.virtual_hs_csrs.hgeie);
            core::arch::asm!(
                "csrw hgatp, {hgatp}",
                hgatp = in(reg) self.regs.virtual_hs_csrs.hgatp,
            );
            core::arch::riscv64::hfence_gvma_all();
        }
        self.sbi.pmu.backend_bind();
        Ok(())
    }

    /// Unbinds the vCPU from the current physical CPU.
    pub fn unbind(&mut self) -> RiscvVcpuResult {
        self.sbi.pmu.backend_unbind();
        // Store the vCPU's CSRs to the stored state.
        unsafe {
            self.regs.vs_csrs.vsatp = vsatp::read().bits();
            self.regs.vs_csrs.vstvec = vstvec::read().bits();
            self.regs.vs_csrs.vsepc = vsepc::read();
            self.regs.vs_csrs.vstval = vstval::read();
            self.regs.vs_csrs.htimedelta = htimedelta::read();
            self.regs.vs_csrs.vscause = vscause::read().bits();
            self.regs.vs_csrs.vsscratch = vsscratch::read();
            self.regs.vs_csrs.vsstatus = vsstatus::read().bits();
            self.regs.vs_csrs.vsie = vsie::read().bits();
            #[cfg(feature = "sstc")]
            {
                self.regs.vs_csrs.vstimecmp = vstimecmp::read();
            }
            self.regs.virtual_hs_csrs.hie = hie::read().bits();
            self.regs.virtual_hs_csrs.hvip = hvip::read().bits();
            self.regs.virtual_hs_csrs.hgeie = hgeie::read();
            core::arch::asm!(
                "csrr {hgatp}, hgatp",
                hgatp = out(reg) self.regs.virtual_hs_csrs.hgatp,
            );
            hie::Hie::from_bits(0).write();
            // Clear host-side pending state after saving it to avoid leaking a
            // previous guest's virtual IRQs into later host/guest execution.
            hvip::Hvip::from_bits(0).write();
            hgeie::write(0);
            // `henvcfg` is hart-local execution policy. Clear it before the
            // physical CPU can run another vCPU so capabilities never leak
            // between guest CPU models.
            henvcfg::Henvcfg::from_bits(0).write();
            #[cfg(feature = "sstc")]
            vstimecmp::write(usize::MAX);
            core::arch::asm!("csrw hgatp, x0");
            core::arch::riscv64::hfence_gvma_all();
        }
        Ok(())
    }

    /// Set one of the vCPU's general purpose registers.
    pub fn set_gpr(&mut self, index: usize, val: usize) {
        match index {
            0 => {
                // Do nothing, x0 is hardwired to zero
            }
            1..=31 => {
                if let Some(gpr_index) = GprIndex::from_raw(index as u32) {
                    self.set_gpr_from_gpr_index(gpr_index, val);
                } else {
                    warn!("RISCVVCpu: Failed to map general purpose register index: {index}");
                }
            }
            _ => {
                warn!("RISCVVCpu: Unsupported general purpose register index: {index}");
            }
        }
    }

    /// Injects a virtual interrupt into the guest.
    pub fn inject_interrupt(&mut self, vector: usize) -> RiscvVcpuResult {
        if vector != S_EXT {
            return Err(RiscvVcpuError::Unsupported);
        }
        unsafe {
            hvip::set_vseip();
        }
        self.regs.virtual_hs_csrs.hvip |= hvip::read().bits();
        Ok(())
    }

    /// Sets the guest return value register.
    pub fn set_return_value(&mut self, val: usize) {
        self.set_gpr_from_gpr_index(GprIndex::A0, val);
    }
}

impl<H: RiscvHostOps> RiscvVcpu<H> {
    /// Capture any virtual pending interrupt bits that were raised after the
    /// last `unbind()` so the next `bind()` does not overwrite them with stale
    /// saved state.
    pub fn latch_hvip_from_hw(&mut self) {
        self.regs.virtual_hs_csrs.hvip |= hvip::read().bits();
    }

    /// Attempts to decode the current guest-page-fault trap as an MMIO access.
    pub fn decode_mmio_fault(
        &mut self,
        _fault_addr: RiscvGuestPhysAddr,
        access_flags: RiscvAccessFlags,
    ) -> Option<RiscvVmExit> {
        let writing = access_flags.contains(RiscvAccessFlags::WRITE);
        match self.handle_guest_page_fault(writing).ok()? {
            exit @ (RiscvVmExit::MmioRead { .. } | RiscvVmExit::MmioWrite { .. }) => Some(exit),
            _ => None,
        }
    }
}

impl<H: RiscvHostOps> RiscvVcpu<H> {
    #[inline]
    fn program_guest_timer(&mut self, deadline: usize) {
        #[cfg(feature = "sstc")]
        {
            self.regs.vs_csrs.vstimecmp = deadline;
        }
        sbi_rt::set_timer(deadline as u64);
        unsafe {
            // The guest has consumed the current VS timer event and programmed
            // a new deadline, so clear the injected VS timer pending bit and
            // re-arm HS timer delivery for the next expiration.
            hvip::clear_vstip();
            #[cfg(feature = "sstc")]
            vstimecmp::write(deadline);
            sie::set_stimer();
        }
    }

    /// Gets one of the vCPU's general purpose registers.
    pub fn get_gpr(&self, index: GprIndex) -> usize {
        self.regs.guest_regs.gprs.reg(index)
    }

    /// Set one of the vCPU's general purpose register.
    pub fn set_gpr_from_gpr_index(&mut self, index: GprIndex, val: usize) {
        self.regs.guest_regs.gprs.set_reg(index, val);
    }

    /// Advance guest pc by `instr_len` bytes
    pub fn advance_pc(&mut self, instr_len: usize) {
        self.regs.guest_regs.sepc += instr_len
    }

    /// Gets the vCPU's registers.
    pub fn regs(&mut self) -> &mut VmCpuRegisters {
        &mut self.regs
    }
}

impl<H: RiscvHostOps> RiscvVcpu<H> {
    /// Inject a synchronous VS exception so the guest handles a fault that happened during
    /// hypervisor-side instruction emulation.
    fn inject_guest_exception(&mut self, exception: Exception, fault_addr: RiscvGuestVirtAddr) {
        let mut vsstatus = vsstatus::read();
        let hstatus = hstatus::Hstatus::from_bits(self.regs.guest_regs.hstatus);
        let vstvec = vstvec::read().bits();
        let trap_pc = vstvec & !0b11;

        vsstatus.set_spie(vsstatus.sie());
        vsstatus.set_sie(false);
        vsstatus.set_spp(hstatus.spvp());

        self.regs.vs_csrs.vstvec = vstvec;
        self.regs.vs_csrs.vsepc = self.regs.guest_regs.sepc;
        self.regs.vs_csrs.vscause = exception as usize;
        self.regs.vs_csrs.vstval = fault_addr.as_usize();
        self.regs.vs_csrs.vsstatus = vsstatus.bits();
        self.regs.guest_regs.sepc = trap_pc;

        // `run_vcpu()` may re-enter the same bound vCPU without reloading the
        // cached VS CSR block, so keep the live CSRs in sync with the cache.
        unsafe {
            vsstatus.write();
            vscause::Vscause::from_bits(self.regs.vs_csrs.vscause).write();
            vstval::write(self.regs.vs_csrs.vstval);
            vsepc::write(self.regs.vs_csrs.vsepc);
        }
    }

    fn handle_guest_instruction_fetch_fault(
        &mut self,
        fault: guest_mem::GuestInstructionFetchFault,
    ) -> RiscvVcpuResult<RiscvVmExit> {
        match fault {
            // HLVX reports load-class faults, but the emulated operation is a
            // guest instruction fetch. Convert them before injecting to VS mode.
            guest_mem::GuestInstructionFetchFault::PageFault { addr } => {
                self.inject_guest_exception(Exception::InstructionPageFault, addr);
                Ok(RiscvVmExit::Nothing)
            }
            guest_mem::GuestInstructionFetchFault::AccessFault { addr } => {
                self.inject_guest_exception(Exception::InstructionFault, addr);
                Ok(RiscvVmExit::Nothing)
            }
            guest_mem::GuestInstructionFetchFault::Misaligned { addr } => {
                self.inject_guest_exception(Exception::InstructionMisaligned, addr);
                Ok(RiscvVmExit::Nothing)
            }
            guest_mem::GuestInstructionFetchFault::GuestPageFault { addr } => {
                // G-stage faults must stay visible to AxVM so it can populate or
                // reject the nested mapping.
                Ok(RiscvVmExit::NestedPageFault {
                    addr,
                    access_flags: RiscvAccessFlags::EXECUTE,
                })
            }
            guest_mem::GuestInstructionFetchFault::Unhandled {
                scause,
                stval,
                htval,
            } => {
                warn!(
                    "unhandled riscv HLVX fault while fetching guest instruction: \
                     scause={scause:#x}, stval={stval:#x}, htval={htval:#x}"
                );
                Err(RiscvVcpuError::GuestMemoryFault)
            }
        }
    }

    fn vmexit_handler(&mut self) -> RiscvVcpuResult<RiscvVmExit> {
        self.regs.trap_csrs.load_from_hw();

        let scause = scause::read();
        use riscv::interrupt::{Interrupt, Trap};

        trace!(
            "vmexit_handler: {:?}, sepc: {:#x}, stval: {:#x}",
            scause.cause(),
            self.regs.guest_regs.sepc,
            self.regs.trap_csrs.stval
        );

        // Try to convert the raw trap cause to a standard RISC-V trap cause.
        let trap: Trap<Interrupt, Exception> = scause.cause().try_into().map_err(|_| {
            error!("Unknown trap cause: scause={:#x}", scause.bits());
            RiscvVcpuError::InvalidTrap
        })?;

        match trap {
            Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
                let a = self.regs.guest_regs.gprs.a_regs();
                let param = [a[0], a[1], a[2], a[3], a[4], a[5]];
                let extension_id = a[7];
                let function_id = a[6];

                trace!(
                    "sbi_call: eid {:#x} ('{}') fid {:#x} param {:?}",
                    extension_id,
                    alloc::string::String::from_utf8_lossy(&(extension_id as u32).to_be_bytes()),
                    function_id,
                    param
                );
                match extension_id {
                    // Compatibility with Legacy Extensions.
                    legacy::LEGACY_SET_TIMER..=legacy::LEGACY_SHUTDOWN => match extension_id {
                        legacy::LEGACY_SET_TIMER => {
                            // info!("set timer: {}", param[0]);
                            self.sbi.pmu.record_set_timer();
                            self.program_guest_timer(param[0]);

                            self.set_gpr_from_gpr_index(GprIndex::A0, 0);
                        }
                        legacy::LEGACY_CONSOLE_PUTCHAR => {
                            sbi_call_legacy_1(legacy::LEGACY_CONSOLE_PUTCHAR, param[0]);
                        }
                        legacy::LEGACY_CONSOLE_GETCHAR => {
                            let c = sbi_call_legacy_0(legacy::LEGACY_CONSOLE_GETCHAR);
                            self.set_gpr_from_gpr_index(GprIndex::A0, c);
                        }
                        legacy::LEGACY_SHUTDOWN => {
                            // sbi_call_legacy_0(LEGACY_SHUTDOWN)
                            return Ok(RiscvVmExit::SystemDown);
                        }
                        _ => {
                            warn!(
                                "Unsupported SBI legacy extension id {extension_id:#x} function \
                                 id {function_id:#x}"
                            );
                        }
                    },
                    EID_TIME => match function_id {
                        FID_SET_TIMER => {
                            self.sbi.pmu.record_set_timer();
                            self.program_guest_timer(param[0]);
                            self.sbi_return(RET_SUCCESS, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                        _ => {
                            self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                    },
                    // Handle HSM extension
                    hsm::EID_HSM => match function_id {
                        hsm::HART_START => {
                            let hartid = a[0];
                            let start_addr = a[1];
                            let opaque = a[2];
                            self.advance_pc(4);
                            return Ok(RiscvVmExit::CpuUp {
                                target_cpu: hartid as _,
                                entry_point: RiscvGuestPhysAddr::from(start_addr),
                                arg: opaque as _,
                            });
                        }
                        hsm::HART_STOP => {
                            return Ok(RiscvVmExit::CpuDown { state: 0 });
                        }
                        hsm::HART_SUSPEND => {
                            // These parameters are reserved for a future suspend-state model.
                            let _suspend_type = a[0];
                            let _resume_addr = a[1];
                            let _opaque = a[2];
                            return Ok(RiscvVmExit::Halt);
                        }
                        _ => {
                            self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                    },
                    // Handle hypercall
                    EID_HVC => {
                        self.advance_pc(4);
                        return Ok(RiscvVmExit::Hypercall {
                            nr: function_id as _,
                            args: [
                                param[0] as _,
                                param[1] as _,
                                param[2] as _,
                                param[3] as _,
                                param[4] as _,
                                param[5] as _,
                            ],
                        });
                    }
                    // Debug Console Extension
                    EID_DBCN => match function_id {
                        // Write from memory region to debug console.
                        FID_CONSOLE_WRITE => {
                            let num_bytes = param[0];
                            let gpa = join_u64(param[1], param[2]);

                            if num_bytes == 0 {
                                self.sbi_return(RET_SUCCESS, 0);
                                return Ok(RiscvVmExit::Nothing);
                            }

                            let mut buf = alloc::vec![0u8; num_bytes];
                            let copied = guest_mem::copy_from_guest(
                                &mut buf,
                                RiscvGuestPhysAddr::from(gpa as usize),
                            );

                            if copied == buf.len() {
                                let ret = console_write::<H>(&buf);
                                self.sbi_return(ret.error, ret.value);
                            } else {
                                self.sbi_return(RET_ERR_FAILED, 0);
                            }

                            return Ok(RiscvVmExit::Nothing);
                        }
                        // Read to memory region from debug console.
                        FID_CONSOLE_READ => {
                            let num_bytes = param[0];
                            let gpa = join_u64(param[1], param[2]);

                            if num_bytes == 0 {
                                self.sbi_return(RET_SUCCESS, 0);
                                return Ok(RiscvVmExit::Nothing);
                            }

                            let mut buf = alloc::vec![0u8; num_bytes];
                            let ret = console_read::<H>(&mut buf);

                            if ret.is_ok() && ret.value <= buf.len() {
                                let copied = guest_mem::copy_to_guest(
                                    &buf[..ret.value],
                                    RiscvGuestPhysAddr::from(gpa as usize),
                                );
                                if copied == ret.value {
                                    self.sbi_return(RET_SUCCESS, ret.value);
                                } else {
                                    self.sbi_return(RET_ERR_FAILED, 0);
                                }
                            } else {
                                self.sbi_return(ret.error, ret.value);
                            }

                            return Ok(RiscvVmExit::Nothing);
                        }
                        // Write a single byte to debug console.
                        FID_CONSOLE_WRITE_BYTE => {
                            let byte = (param[0] & 0xff) as u8;
                            print_byte(byte);
                            self.sbi_return(RET_SUCCESS, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                        // Unknown FID.
                        _ => {
                            self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                    },
                    srst::EID_SRST => match function_id {
                        srst::SYSTEM_RESET => {
                            let reset_type = param[0];
                            if reset_type == srst::RESET_TYPE_SHUTDOWN as _ {
                                // Shutdown the system.
                                return Ok(RiscvVmExit::SystemDown);
                            } else {
                                self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                                return Ok(RiscvVmExit::Nothing);
                            }
                        }
                        _ => {
                            self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                    },
                    pmu::EID_PMU => {
                        let ret = self.sbi.handle_ecall(extension_id, function_id, param);
                        self.set_gpr_from_gpr_index(GprIndex::A0, ret.error);
                        self.set_gpr_from_gpr_index(GprIndex::A1, ret.value);
                    }
                    rfnc::EID_RFNC => {
                        match function_id {
                            rfnc::REMOTE_FENCE_I => self.sbi.pmu.record_fence_i_sent(),
                            rfnc::REMOTE_SFENCE_VMA => self.sbi.pmu.record_sfence_vma_sent(),
                            rfnc::REMOTE_SFENCE_VMA_ASID => {
                                self.sbi.pmu.record_sfence_vma_asid_sent();
                            }
                            rfnc::REMOTE_HFENCE_GVMA => self.sbi.pmu.record_hfence_gvma_sent(),
                            rfnc::REMOTE_HFENCE_GVMA_VMID => {
                                self.sbi.pmu.record_hfence_gvma_vmid_sent();
                            }
                            rfnc::REMOTE_HFENCE_VVMA => self.sbi.pmu.record_hfence_vvma_sent(),
                            rfnc::REMOTE_HFENCE_VVMA_ASID => {
                                self.sbi.pmu.record_hfence_vvma_asid_sent();
                            }
                            _ => {}
                        }
                        let ret = self.sbi.handle_ecall(extension_id, function_id, param);
                        self.set_gpr_from_gpr_index(GprIndex::A0, ret.error);
                        self.set_gpr_from_gpr_index(GprIndex::A1, ret.value);
                    }
                    // By default, forward the SBI call to the RustSBI implementation.
                    // See [`RISCVVCpuSbi`].
                    _ => {
                        let ret = self.sbi.handle_ecall(extension_id, function_id, param);
                        if ret.is_err() {
                            warn!(
                                "forward ecall eid {:#x} fid {:#x} param {:#x?} err {:#x} value \
                                 {:#x}",
                                extension_id, function_id, param, ret.error, ret.value
                            );
                        }
                        self.set_gpr_from_gpr_index(GprIndex::A0, ret.error);
                        self.set_gpr_from_gpr_index(GprIndex::A1, ret.value);
                    }
                };

                self.advance_pc(4);
                Ok(RiscvVmExit::Nothing)
            }
            Trap::Exception(Exception::VirtualInstruction) => self.handle_virtual_instruction(),
            Trap::Interrupt(Interrupt::SupervisorTimer) => {
                // Forward the elapsed timer to VS and stop taking the same HS
                // timer interrupt repeatedly until software programs a new one.
                unsafe {
                    hvip::set_vstip();
                    sie::clear_stimer();
                }

                Ok(RiscvVmExit::Nothing)
            }
            Trap::Interrupt(Interrupt::SupervisorExternal) => {
                // 9 == Interrupt::SupervisorExternal
                //
                // It's a great fault in the `riscv` crate that `Interrupt` and `Exception` are not
                // explicitly numbered, and they provide no way to convert them to a number. Also,
                // `as usize` will give use a wrong value.
                Ok(RiscvVmExit::ExternalInterrupt { vector: S_EXT as _ })
            }
            Trap::Exception(
                gpf @ (Exception::LoadGuestPageFault | Exception::StoreGuestPageFault),
            ) => self.handle_guest_page_fault(gpf == Exception::StoreGuestPageFault),
            _ => {
                error!(
                    "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}, htval: {:#x}, htinst: \
                     {:#x}, vsepc: {:#x}, vstval: {:#x}, vsatp: {:#x}, hgatp: {:#x}, a0-a3: \
                     [{:#x}, {:#x}, {:#x}, {:#x}]",
                    scause.cause(),
                    self.regs.guest_regs.sepc,
                    self.regs.trap_csrs.stval,
                    self.regs.trap_csrs.htval,
                    self.regs.trap_csrs.htinst,
                    self.regs.vs_csrs.vsepc,
                    self.regs.vs_csrs.vstval,
                    self.regs.vs_csrs.vsatp,
                    self.regs.virtual_hs_csrs.hgatp,
                    self.regs.guest_regs.gprs.reg(GprIndex::A0),
                    self.regs.guest_regs.gprs.reg(GprIndex::A1),
                    self.regs.guest_regs.gprs.reg(GprIndex::A2),
                    self.regs.guest_regs.gprs.reg(GprIndex::A3)
                );
                Err(RiscvVcpuError::Unsupported)
            }
        }
    }

    #[inline]
    fn sbi_return(&mut self, a0: usize, a1: usize) {
        self.set_gpr_from_gpr_index(GprIndex::A0, a0);
        self.set_gpr_from_gpr_index(GprIndex::A1, a1);
        self.advance_pc(4);
    }

    #[cfg(feature = "sstc")]
    fn handle_virtual_instruction(&mut self) -> RiscvVcpuResult<RiscvVmExit> {
        let instr = match self.read_virtual_instruction()? {
            VirtualInstructionRead::Instruction(instr) => instr,
            VirtualInstructionRead::Handled(exit_reason) => return Ok(exit_reason),
        };
        let csr = ((instr >> 20) & 0xfff) as u16;

        if csr != CSR_STIMECMP {
            self.sbi.pmu.record_illegal_insn();
            warn!(
                "Unhandled virtual instruction csr={csr:#x}, sepc: {:#x}, stval: {:#x}, htval: \
                 {:#x}, htinst: {:#x}",
                self.regs.guest_regs.sepc,
                self.regs.trap_csrs.stval,
                self.regs.trap_csrs.htval,
                self.regs.trap_csrs.htinst,
            );
            return Err(RiscvVcpuError::Unsupported);
        }

        let funct3 = ((instr >> 12) & 0x7) as u8;
        let rd = ((instr >> 7) & 0x1f) as u8;
        let rs1 = ((instr >> 15) & 0x1f) as u8;
        let old_value = self.regs.vs_csrs.vstimecmp;
        let rs1_value = self.read_gpr_raw(rs1);
        let zimm = rs1 as usize;

        let new_value = match funct3 {
            0b001 => Some(rs1_value),
            0b010 => {
                if rs1 == 0 {
                    None
                } else {
                    Some(old_value | rs1_value)
                }
            }
            0b011 => {
                if rs1 == 0 {
                    None
                } else {
                    Some(old_value & !rs1_value)
                }
            }
            0b101 => Some(zimm),
            0b110 => {
                if zimm == 0 {
                    None
                } else {
                    Some(old_value | zimm)
                }
            }
            0b111 => {
                if zimm == 0 {
                    None
                } else {
                    Some(old_value & !zimm)
                }
            }
            _ => {
                self.sbi.pmu.record_illegal_insn();
                warn!(
                    "Unhandled virtual instruction funct3={funct3:#x} for csr={csr:#x}, sepc: \
                     {:#x}",
                    self.regs.guest_regs.sepc,
                );
                return Err(RiscvVcpuError::Unsupported);
            }
        };

        if rd != 0 {
            self.write_gpr_raw(rd, old_value);
        }

        if let Some(new_value) = new_value {
            // Linux is using the advertised `sstc` path (`csrw stimecmp,...`).
            // We currently emulate that CSR access rather than exposing direct
            // hardware STCE, so this path must also program the underlying HS
            // timer instead of only updating saved VS state.
            self.program_guest_timer(new_value);
        }

        self.advance_pc(4);
        Ok(RiscvVmExit::Nothing)
    }

    #[cfg(not(feature = "sstc"))]
    fn handle_virtual_instruction(&mut self) -> RiscvVcpuResult<RiscvVmExit> {
        self.sbi.pmu.record_illegal_insn();
        warn!(
            "Unhandled virtual instruction without `sstc` feature, sepc: {:#x}, stval: {:#x}, \
             htval: {:#x}, htinst: {:#x}",
            self.regs.guest_regs.sepc,
            self.regs.trap_csrs.stval,
            self.regs.trap_csrs.htval,
            self.regs.trap_csrs.htinst,
        );
        Err(RiscvVcpuError::Unsupported)
    }

    #[cfg(feature = "sstc")]
    fn read_virtual_instruction(&mut self) -> RiscvVcpuResult<VirtualInstructionRead> {
        let instr = self.regs.trap_csrs.stval as u32;
        if instr & 0x7f == SYSTEM_OPCODE {
            return Ok(VirtualInstructionRead::Instruction(instr));
        }

        let guest_pc = RiscvGuestVirtAddr::from(self.regs.guest_regs.sepc);
        match guest_mem::fetch_guest_instruction(guest_pc) {
            Ok(instr) => Ok(VirtualInstructionRead::Instruction(instr)),
            Err(fault) => self
                .handle_guest_instruction_fetch_fault(fault)
                .map(VirtualInstructionRead::Handled),
        }
    }

    #[cfg(feature = "sstc")]
    fn read_gpr_raw(&self, index: u8) -> usize {
        GprIndex::from_raw(index as u32)
            .map(|gpr| self.get_gpr(gpr))
            .unwrap_or(0)
    }

    #[cfg(feature = "sstc")]
    fn write_gpr_raw(&mut self, index: u8, value: usize) {
        if let Some(gpr) = GprIndex::from_raw(index as u32) {
            self.set_gpr_from_gpr_index(gpr, value);
        }
    }

    /// Decode the instruction at the given virtual address. Return the decoded instruction and its
    /// length in bytes, or an exit reason already produced while fetching it.
    fn decode_instr_at(&mut self, vaddr: RiscvGuestVirtAddr) -> RiscvVcpuResult<InstructionDecode> {
        // The htinst CSR contains "transformed instruction" that caused the page fault. We
        // can use it but we use the sepc to fetch the original instruction instead for now.
        let mut instr = riscv_h::register::htinst::read();
        let instr_len;
        if instr == 0 {
            // Read the instruction from guest memory.
            instr = match guest_mem::fetch_guest_instruction(vaddr) {
                Ok(instr) => instr as _,
                Err(fault) => {
                    return self
                        .handle_guest_instruction_fetch_fault(fault)
                        .map(InstructionDecode::Handled);
                }
            };
            instr_len = riscv_decode::instruction_length(instr as u16);
            instr = match instr_len {
                2 => instr & 0xffff,
                4 => instr,
                _ => return Err(RiscvVcpuError::DecodeFailed),
            };
        } else if instr_is_pseudo(instr as u32) {
            error!("fault on 1st stage page table walk");
            return Err(RiscvVcpuError::Unsupported);
        } else {
            // Transform htinst value to standard instruction.
            // According to RISC-V Spec:
            //      Bits 1:0 of a transformed standard instruction will be binary 01 if
            //      the trapping instruction is compressed and 11 if not.
            instr_len = match (instr as u16) & 0x3 {
                0x1 => 2,
                0x3 => 4,
                _ => return Err(RiscvVcpuError::DecodeFailed),
            };
            instr |= 0x2;
        }

        riscv_decode::decode(instr as u32)
            .map_err(|_| RiscvVcpuError::DecodeFailed)
            .map(|instr| InstructionDecode::Decoded(instr, instr_len))
    }

    /// Handle a guest page fault. Return an exit reason.
    fn handle_guest_page_fault(&mut self, _writing: bool) -> RiscvVcpuResult<RiscvVmExit> {
        let fault_addr = self.regs.trap_csrs.gpt_page_fault_addr();
        let sepc = self.regs.guest_regs.sepc;
        let sepc_vaddr = RiscvGuestVirtAddr::from(sepc);

        /// Temporary enum to represent the decoded operation.
        enum DecodedOp {
            Read {
                i: IType,
                width: RiscvAccessWidth,
                signed_ext: bool,
            },
            Write {
                s: SType,
                width: RiscvAccessWidth,
            },
        }

        use DecodedOp::*;

        let (decoded_instr, instr_len) = match self.decode_instr_at(sepc_vaddr)? {
            InstructionDecode::Decoded(instr, instr_len) => (instr, instr_len),
            InstructionDecode::Handled(exit_reason) => return Ok(exit_reason),
        };
        let op = match decoded_instr {
            Instruction::Lb(i) => Read {
                i,
                width: RiscvAccessWidth::Byte,
                signed_ext: true,
            },
            Instruction::Lh(i) => Read {
                i,
                width: RiscvAccessWidth::Word,
                signed_ext: true,
            },
            Instruction::Lw(i) => Read {
                i,
                width: RiscvAccessWidth::Dword,
                signed_ext: true,
            },
            Instruction::Ld(i) => Read {
                i,
                width: RiscvAccessWidth::Qword,
                signed_ext: true,
            },
            Instruction::Lbu(i) => Read {
                i,
                width: RiscvAccessWidth::Byte,
                signed_ext: false,
            },
            Instruction::Lhu(i) => Read {
                i,
                width: RiscvAccessWidth::Word,
                signed_ext: false,
            },
            Instruction::Lwu(i) => Read {
                i,
                width: RiscvAccessWidth::Dword,
                signed_ext: false,
            },
            Instruction::Sb(s) => Write {
                s,
                width: RiscvAccessWidth::Byte,
            },
            Instruction::Sh(s) => Write {
                s,
                width: RiscvAccessWidth::Word,
            },
            Instruction::Sw(s) => Write {
                s,
                width: RiscvAccessWidth::Dword,
            },
            Instruction::Sd(s) => Write {
                s,
                width: RiscvAccessWidth::Qword,
            },
            _ => {
                // Not a load or store instruction, so we cannot handle it here, return a nested page fault.
                return Ok(RiscvVmExit::NestedPageFault {
                    addr: fault_addr,
                    access_flags: RiscvAccessFlags::empty(),
                });
            }
        };

        // WARN: This is a temporary place to add the instruction length to the guest's sepc.
        self.advance_pc(instr_len);

        Ok(match op {
            Read {
                i,
                width,
                signed_ext,
            } => {
                self.sbi.pmu.record_access_load();
                RiscvVmExit::MmioRead {
                    addr: fault_addr,
                    width,
                    reg: i.rd() as _,
                    reg_width: RiscvAccessWidth::Qword,
                    signed_ext,
                }
            }
            Write { s, width } => {
                self.sbi.pmu.record_access_store();
                let source_reg = s.rs2();
                let value = self
                    .get_gpr(GprIndex::from_raw(source_reg).ok_or(RiscvVcpuError::DecodeFailed)?);

                RiscvVmExit::MmioWrite {
                    addr: fault_addr,
                    width,
                    data: value as _,
                }
            }
        })
    }
}

#[inline(always)]
fn sbi_call_legacy_0(eid: usize) -> usize {
    let error;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") eid,
            lateout("a0") error,
        );
    }
    error
}

#[inline(always)]
fn sbi_call_legacy_1(eid: usize, arg0: usize) -> usize {
    let error;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") eid,
            inlateout("a0") arg0 => error,
        );
    }
    error
}
