use core::arch::global_asm;
use core::mem::size_of;

use memoffset::offset_of;
use riscv::register::{htinst, htval, scause, sstatus, stval};
use sbi_rt::{pmu_counter_get_info, pmu_counter_stop};
use tock_registers::LocalRegisterCopy;

use axaddrspace::{GuestPhysAddr, HostPhysAddr, MappingFlags};
use axerrno::AxResult;
use axvcpu::AxVCpuExitReason;

use super::csrs::defs::hstatus;
use super::csrs::{traps, RiscvCsrTrait, CSR};
use super::sbi::{BaseFunction, PmuFunction, RemoteFenceFunction, SbiMessage};

use super::regs::{GeneralPurposeRegisters, GprIndex};

/// Hypervisor GPR and CSR state which must be saved/restored when entering/exiting virtualization.
#[derive(Default)]
#[repr(C)]
struct HypervisorCpuState {
    gprs: GeneralPurposeRegisters,
    sstatus: usize,
    hstatus: usize,
    scounteren: usize,
    stvec: usize,
    sscratch: usize,
}

/// Guest GPR and CSR state which must be saved/restored when exiting/entering virtualization.
#[derive(Default)]
#[repr(C)]
pub struct GuestCpuState {
    pub gprs: GeneralPurposeRegisters,
    pub sstatus: usize,
    pub hstatus: usize,
    pub scounteren: usize,
    pub sepc: usize,
}

/// The CSRs that are only in effect when virtualization is enabled (V=1) and must be saved and
/// restored whenever we switch between VMs.
#[derive(Default)]
#[repr(C)]
pub struct GuestVsCsrs {
    htimedelta: usize,
    vsstatus: usize,
    vsie: usize,
    vstvec: usize,
    vsscratch: usize,
    vsepc: usize,
    vscause: usize,
    vstval: usize,
    vsatp: usize,
    vstimecmp: usize,
}

/// Virtualized HS-level CSRs that are used to emulate (part of) the hypervisor extension for the
/// guest.
#[derive(Default)]
#[repr(C)]
pub struct GuestVirtualHsCsrs {
    hie: usize,
    hgeie: usize,
    hgatp: usize,
}

/// CSRs written on an exit from virtualization that are used by the hypervisor to determine the cause
/// of the trap.
#[derive(Default, Clone)]
#[repr(C)]
pub struct VmCpuTrapState {
    pub scause: usize,
    pub stval: usize,
    pub htval: usize,
    pub htinst: usize,
}

/// (v)CPU register state that must be saved or restored when entering/exiting a VM or switching
/// between VMs.
#[derive(Default)]
#[repr(C)]
pub struct VmCpuRegisters {
    // CPU state that's shared between our's and the guest's execution environment. Saved/restored
    // when entering/exiting a VM.
    hyp_regs: HypervisorCpuState,
    pub guest_regs: GuestCpuState,

    // CPU state that only applies when V=1, e.g. the VS-level CSRs. Saved/restored on activation of
    // the vCPU.
    vs_csrs: GuestVsCsrs,

    // Virtualized HS-level CPU state.
    virtual_hs_csrs: GuestVirtualHsCsrs,

    // Read on VM exit.
    pub trap_csrs: VmCpuTrapState,
}

#[allow(dead_code)]
const fn hyp_gpr_offset(index: GprIndex) -> usize {
    offset_of!(VmCpuRegisters, hyp_regs)
        + offset_of!(HypervisorCpuState, gprs)
        + (index as usize) * size_of::<u64>()
}

#[allow(dead_code)]
const fn guest_gpr_offset(index: GprIndex) -> usize {
    offset_of!(VmCpuRegisters, guest_regs)
        + offset_of!(GuestCpuState, gprs)
        + (index as usize) * size_of::<u64>()
}

#[allow(unused_macros)]
macro_rules! hyp_csr_offset {
    ($reg:tt) => {
        offset_of!(VmCpuRegisters, hyp_regs) + offset_of!(HypervisorCpuState, $reg)
    };
}

#[allow(unused_macros)]
macro_rules! guest_csr_offset {
    ($reg:tt) => {
        offset_of!(VmCpuRegisters, guest_regs) + offset_of!(GuestCpuState, $reg)
    };
}

global_asm!(
    include_str!("guest.S"),
    hyp_ra = const hyp_gpr_offset(GprIndex::RA),
    hyp_gp = const hyp_gpr_offset(GprIndex::GP),
    hyp_tp = const hyp_gpr_offset(GprIndex::TP),
    hyp_s0 = const hyp_gpr_offset(GprIndex::S0),
    hyp_s1 = const hyp_gpr_offset(GprIndex::S1),
    hyp_a1 = const hyp_gpr_offset(GprIndex::A1),
    hyp_a2 = const hyp_gpr_offset(GprIndex::A2),
    hyp_a3 = const hyp_gpr_offset(GprIndex::A3),
    hyp_a4 = const hyp_gpr_offset(GprIndex::A4),
    hyp_a5 = const hyp_gpr_offset(GprIndex::A5),
    hyp_a6 = const hyp_gpr_offset(GprIndex::A6),
    hyp_a7 = const hyp_gpr_offset(GprIndex::A7),
    hyp_s2 = const hyp_gpr_offset(GprIndex::S2),
    hyp_s3 = const hyp_gpr_offset(GprIndex::S3),
    hyp_s4 = const hyp_gpr_offset(GprIndex::S4),
    hyp_s5 = const hyp_gpr_offset(GprIndex::S5),
    hyp_s6 = const hyp_gpr_offset(GprIndex::S6),
    hyp_s7 = const hyp_gpr_offset(GprIndex::S7),
    hyp_s8 = const hyp_gpr_offset(GprIndex::S8),
    hyp_s9 = const hyp_gpr_offset(GprIndex::S9),
    hyp_s10 = const hyp_gpr_offset(GprIndex::S10),
    hyp_s11 = const hyp_gpr_offset(GprIndex::S11),
    hyp_sp = const hyp_gpr_offset(GprIndex::SP),
    hyp_sstatus = const hyp_csr_offset!(sstatus),
    hyp_hstatus = const hyp_csr_offset!(hstatus),
    hyp_scounteren = const hyp_csr_offset!(scounteren),
    hyp_stvec = const hyp_csr_offset!(stvec),
    hyp_sscratch = const hyp_csr_offset!(sscratch),
    guest_ra = const guest_gpr_offset(GprIndex::RA),
    guest_gp = const guest_gpr_offset(GprIndex::GP),
    guest_tp = const guest_gpr_offset(GprIndex::TP),
    guest_s0 = const guest_gpr_offset(GprIndex::S0),
    guest_s1 = const guest_gpr_offset(GprIndex::S1),
    guest_a0 = const guest_gpr_offset(GprIndex::A0),
    guest_a1 = const guest_gpr_offset(GprIndex::A1),
    guest_a2 = const guest_gpr_offset(GprIndex::A2),
    guest_a3 = const guest_gpr_offset(GprIndex::A3),
    guest_a4 = const guest_gpr_offset(GprIndex::A4),
    guest_a5 = const guest_gpr_offset(GprIndex::A5),
    guest_a6 = const guest_gpr_offset(GprIndex::A6),
    guest_a7 = const guest_gpr_offset(GprIndex::A7),
    guest_s2 = const guest_gpr_offset(GprIndex::S2),
    guest_s3 = const guest_gpr_offset(GprIndex::S3),
    guest_s4 = const guest_gpr_offset(GprIndex::S4),
    guest_s5 = const guest_gpr_offset(GprIndex::S5),
    guest_s6 = const guest_gpr_offset(GprIndex::S6),
    guest_s7 = const guest_gpr_offset(GprIndex::S7),
    guest_s8 = const guest_gpr_offset(GprIndex::S8),
    guest_s9 = const guest_gpr_offset(GprIndex::S9),
    guest_s10 = const guest_gpr_offset(GprIndex::S10),
    guest_s11 = const guest_gpr_offset(GprIndex::S11),
    guest_t0 = const guest_gpr_offset(GprIndex::T0),
    guest_t1 = const guest_gpr_offset(GprIndex::T1),
    guest_t2 = const guest_gpr_offset(GprIndex::T2),
    guest_t3 = const guest_gpr_offset(GprIndex::T3),
    guest_t4 = const guest_gpr_offset(GprIndex::T4),
    guest_t5 = const guest_gpr_offset(GprIndex::T5),
    guest_t6 = const guest_gpr_offset(GprIndex::T6),
    guest_sp = const guest_gpr_offset(GprIndex::SP),

    guest_sstatus = const guest_csr_offset!(sstatus),
    guest_hstatus = const guest_csr_offset!(hstatus),
    guest_scounteren = const guest_csr_offset!(scounteren),
    guest_sepc = const guest_csr_offset!(sepc),

);

extern "C" {
    fn _run_guest(state: *mut VmCpuRegisters);
}

/// The architecture dependent configuration of a `AxArchVCpu`.
#[derive(Clone, Copy, Debug, Default)]
pub struct VCpuConfig {}

#[derive(Default)]
/// A virtual CPU within a guest
pub struct RISCVVCpu {
    regs: VmCpuRegisters,
}

impl axvcpu::AxArchVCpu for RISCVVCpu {
    type CreateConfig = ();

    type SetupConfig = ();

    fn new(_config: Self::CreateConfig) -> AxResult<Self> {
        let mut regs = VmCpuRegisters::default();
        // Set hstatus
        let mut hstatus = LocalRegisterCopy::<usize, hstatus::Register>::new(
            riscv::register::hstatus::read().bits(),
        );
        hstatus.modify(hstatus::spv::Supervisor);
        // Set SPVP bit in order to accessing VS-mode memory from HS-mode.
        hstatus.modify(hstatus::spvp::Supervisor);
        CSR.hstatus.write_value(hstatus.get());
        regs.guest_regs.hstatus = hstatus.get();

        // Set sstatus
        let mut sstatus = sstatus::read();
        sstatus.set_spp(sstatus::SPP::Supervisor);
        regs.guest_regs.sstatus = sstatus.bits();

        regs.guest_regs.gprs.set_reg(GprIndex::A0, 0);
        regs.guest_regs.gprs.set_reg(GprIndex::A1, 0x9000_0000);

        Ok(Self { regs })
    }

    fn setup(&mut self, _config: Self::SetupConfig) -> AxResult {
        Ok(())
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        let regs = &mut self.regs;
        regs.guest_regs.sepc = entry.as_usize();
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        self.regs.virtual_hs_csrs.hgatp = 8usize << 60 | usize::from(ept_root) >> 12;
        unsafe {
            core::arch::asm!(
                "csrw hgatp, {hgatp}",
                hgatp = in(reg) self.regs.virtual_hs_csrs.hgatp,
            );
            core::arch::riscv64::hfence_gvma_all();
        }
        Ok(())
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        let regs = &mut self.regs;
        unsafe {
            // Safe to run the guest as it only touches memory assigned to it by being owned
            // by its page table
            _run_guest(regs);
        }
        self.vmexit_handler()
    }

    fn bind(&mut self) -> AxResult {
        // unimplemented!()
        Ok(())
    }

    fn unbind(&mut self) -> AxResult {
        // unimplemented!()
        Ok(())
    }

    /// Set one of the vCPU's general purpose register.
    fn set_gpr(&mut self, index: usize, val: usize) {
        self.set_gpr_from_gpr_index(GprIndex::from_raw(index as u32).unwrap(), val);
    }
}

impl RISCVVCpu {
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

impl RISCVVCpu {
    fn vmexit_handler(&mut self) -> AxResult<AxVCpuExitReason> {
        self.regs.trap_csrs.scause = scause::read().bits();
        self.regs.trap_csrs.stval = stval::read();
        self.regs.trap_csrs.htval = htval::read();
        self.regs.trap_csrs.htinst = htinst::read();

        let scause = scause::read();
        use scause::{Exception, Interrupt, Trap};
        match scause.cause() {
            Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
                let sbi_msg = SbiMessage::from_regs(self.regs.guest_regs.gprs.a_regs()).ok();
                if let Some(sbi_msg) = sbi_msg {
                    match sbi_msg {
                        SbiMessage::Base(base) => {
                            self.handle_base_function(base).unwrap();
                        }
                        SbiMessage::GetChar => {
                            let c = sbi_rt::legacy::console_getchar();
                            self.set_gpr_from_gpr_index(GprIndex::A0, c);
                        }
                        SbiMessage::PutChar(c) => {
                            sbi_rt::legacy::console_putchar(c);
                        }
                        SbiMessage::SetTimer(timer) => {
                            sbi_rt::set_timer(timer as u64);
                            // Clear guest timer interrupt
                            CSR.hvip
                                .read_and_clear_bits(traps::interrupt::VIRTUAL_SUPERVISOR_TIMER);
                            //  Enable host timer interrupt
                            CSR.sie
                                .read_and_set_bits(traps::interrupt::SUPERVISOR_TIMER);
                        }
                        SbiMessage::Reset(_) => {
                            sbi_rt::system_reset(sbi_rt::Shutdown, sbi_rt::SystemFailure);
                        }
                        SbiMessage::RemoteFence(rfnc) => {
                            self.handle_rfnc_function(rfnc).unwrap();
                        }
                        SbiMessage::PMU(pmu) => {
                            self.handle_pmu_function(pmu).unwrap();
                        }
                        _ => todo!(),
                    }
                    self.advance_pc(4);
                    Ok(AxVCpuExitReason::Nothing)
                } else {
                    panic!()
                }
            }
            Trap::Interrupt(Interrupt::SupervisorTimer) => {
                // debug!("timer irq emulation");
                // Enable guest timer interrupt
                CSR.hvip
                    .read_and_set_bits(traps::interrupt::VIRTUAL_SUPERVISOR_TIMER);
                // Clear host timer interrupt
                CSR.sie
                    .read_and_clear_bits(traps::interrupt::SUPERVISOR_TIMER);
                Ok(AxVCpuExitReason::Nothing)
            }
            Trap::Interrupt(Interrupt::SupervisorExternal) => {
                Ok(AxVCpuExitReason::ExternalInterrupt { vector: 0 })
            }
            Trap::Exception(Exception::LoadGuestPageFault)
            | Trap::Exception(Exception::StoreGuestPageFault) => {
                let fault_addr = self.regs.trap_csrs.htval << 2 | self.regs.trap_csrs.stval & 0x3;
                Ok(AxVCpuExitReason::NestedPageFault {
                    addr: GuestPhysAddr::from(fault_addr),
                    access_flags: MappingFlags::empty(),
                })
            }
            _ => {
                panic!(
                    "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}",
                    scause.cause(),
                    self.regs.guest_regs.sepc,
                    self.regs.trap_csrs.stval
                );
            }
        }
    }

    fn handle_base_function(&mut self, base: BaseFunction) -> AxResult<()> {
        match base {
            BaseFunction::GetSepcificationVersion => {
                let version = sbi_rt::get_spec_version();
                self.set_gpr_from_gpr_index(GprIndex::A1, version.major() << 24 | version.minor());
                debug!(
                    "GetSepcificationVersion: {}",
                    version.major() << 24 | version.minor()
                );
            }
            BaseFunction::GetImplementationID => {
                let id = sbi_rt::get_sbi_impl_id();
                self.set_gpr_from_gpr_index(GprIndex::A1, id);
            }
            BaseFunction::GetImplementationVersion => {
                let impl_version = sbi_rt::get_sbi_impl_version();
                self.set_gpr_from_gpr_index(GprIndex::A1, impl_version);
            }
            BaseFunction::ProbeSbiExtension(extension) => {
                let extension = sbi_rt::probe_extension(extension as usize).raw;
                self.set_gpr_from_gpr_index(GprIndex::A1, extension);
            }
            BaseFunction::GetMachineVendorID => {
                let mvendorid = sbi_rt::get_mvendorid();
                self.set_gpr_from_gpr_index(GprIndex::A1, mvendorid);
            }
            BaseFunction::GetMachineArchitectureID => {
                let marchid = sbi_rt::get_marchid();
                self.set_gpr_from_gpr_index(GprIndex::A1, marchid);
            }
            BaseFunction::GetMachineImplementationID => {
                let mimpid = sbi_rt::get_mimpid();
                self.set_gpr_from_gpr_index(GprIndex::A1, mimpid);
            }
        }
        self.set_gpr_from_gpr_index(GprIndex::A0, 0);
        Ok(())
    }

    fn handle_rfnc_function(&mut self, rfnc: RemoteFenceFunction) -> AxResult<()> {
        self.set_gpr_from_gpr_index(GprIndex::A0, 0);
        match rfnc {
            RemoteFenceFunction::FenceI {
                hart_mask,
                hart_mask_base,
            } => {
                let sbi_ret = sbi_rt::remote_fence_i(hart_mask as usize, hart_mask_base as usize);
                self.set_gpr_from_gpr_index(GprIndex::A0, sbi_ret.error);
                self.set_gpr_from_gpr_index(GprIndex::A1, sbi_ret.value);
            }
            RemoteFenceFunction::RemoteSFenceVMA {
                hart_mask,
                hart_mask_base,
                start_addr,
                size,
            } => {
                let sbi_ret = sbi_rt::remote_sfence_vma(
                    hart_mask as usize,
                    hart_mask_base as usize,
                    start_addr as usize,
                    size as usize,
                );
                self.set_gpr_from_gpr_index(GprIndex::A0, sbi_ret.error);
                self.set_gpr_from_gpr_index(GprIndex::A1, sbi_ret.value);
            }
        }
        Ok(())
    }

    fn handle_pmu_function(&mut self, pmu: PmuFunction) -> AxResult<()> {
        self.set_gpr_from_gpr_index(GprIndex::A0, 0);
        match pmu {
            PmuFunction::GetNumCounters => {
                self.set_gpr_from_gpr_index(GprIndex::A1, sbi_rt::pmu_num_counters())
            }
            PmuFunction::GetCounterInfo(counter_index) => {
                let sbi_ret = pmu_counter_get_info(counter_index as usize);
                self.set_gpr_from_gpr_index(GprIndex::A0, sbi_ret.error);
                self.set_gpr_from_gpr_index(GprIndex::A1, sbi_ret.value);
            }
            PmuFunction::StopCounter {
                counter_index,
                counter_mask,
                stop_flags,
            } => {
                let sbi_ret = pmu_counter_stop(
                    counter_index as usize,
                    counter_mask as usize,
                    stop_flags as usize,
                );
                self.set_gpr_from_gpr_index(GprIndex::A0, sbi_ret.error);
                self.set_gpr_from_gpr_index(GprIndex::A1, sbi_ret.value);
            }
        }
        Ok(())
    }
}
