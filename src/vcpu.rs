use core::arch::global_asm;
use core::mem::size_of;

use memoffset::offset_of;
use riscv::register::{htinst, htval, scause, sstatus, stval};
use rustsbi::{Forward, RustSBI, Timer};
use sbi_spec::{hsm, legacy};

use axaddrspace::{GuestPhysAddr, HostPhysAddr, HostVirtAddr, MappingFlags};
use axerrno::AxResult;
use axvcpu::AxVCpuExitReason;

use super::csrs::{traps, RiscvCsrTrait, CSR};
use super::regs::{GeneralPurposeRegisters, GprIndex};
use crate::{RISCVVCpuCreateConfig, EID_HVC};

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
    sbi: RISCVVCpuSbi,
}

#[derive(RustSBI)]
struct RISCVVCpuSbi {
    timer: RISCVVCpuSbiTimer,
    #[rustsbi(console, pmu, fence, reset, info, hsm)]
    forward: Forward,
}

impl Default for RISCVVCpuSbi {
    #[inline]
    fn default() -> Self {
        Self {
            timer: RISCVVCpuSbiTimer,
            forward: Forward,
        }
    }
}

struct RISCVVCpuSbiTimer;

impl rustsbi::Timer for RISCVVCpuSbiTimer {
    #[inline]
    fn set_timer(&self, stime_value: u64) {
        sbi_rt::set_timer(stime_value);
        // Clear guest timer interrupt
        CSR.hvip
            .read_and_clear_bits(traps::interrupt::VIRTUAL_SUPERVISOR_TIMER);
        //  Enable host timer interrupt
        CSR.sie
            .read_and_set_bits(traps::interrupt::SUPERVISOR_TIMER);
    }
}

impl axvcpu::AxArchVCpu for RISCVVCpu {
    type CreateConfig = RISCVVCpuCreateConfig;

    type SetupConfig = ();

    fn new(config: Self::CreateConfig) -> AxResult<Self> {
        let mut regs = VmCpuRegisters::default();
        // Setup the guest's general purpose registers.
        // `a0` is the hartid
        regs.guest_regs.gprs.set_reg(GprIndex::A0, config.hart_id);
        // `a1` is the address of the device tree blob.
        regs.guest_regs
            .gprs
            .set_reg(GprIndex::A1, config.dtb_addr.as_usize());

        Ok(Self {
            regs: VmCpuRegisters::default(),
            sbi: RISCVVCpuSbi::default(),
        })
    }

    fn setup(&mut self, _config: Self::SetupConfig) -> AxResult {
        // Set sstatus.
        let mut sstatus = sstatus::read();
        sstatus.set_spp(sstatus::SPP::Supervisor);
        self.regs.guest_regs.sstatus = sstatus.bits();

        // Set hstatus.
        self.regs.guest_regs.hstatus = riscv::register::hstatus::read().bits();
        Ok(())
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        self.regs.guest_regs.sepc = entry.as_usize();
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        self.regs.virtual_hs_csrs.hgatp = 8usize << 60 | usize::from(ept_root) >> 12;
        Ok(())
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        unsafe {
            // Safe to run the guest as it only touches memory assigned to it by being owned
            // by its page table
            _run_guest(&mut self.regs);
        }
        self.vmexit_handler()
    }

    fn bind(&mut self) -> AxResult {
        unsafe {
            core::arch::asm!(
                "csrw hgatp, {hgatp}",
                hgatp = in(reg) self.regs.virtual_hs_csrs.hgatp,
            );
            core::arch::riscv64::hfence_gvma_all();
        }
        Ok(())
    }

    fn unbind(&mut self) -> AxResult {
        Ok(())
    }

    /// Set one of the vCPU's general purpose register.
    fn set_gpr(&mut self, index: usize, val: usize) {
        match index {
            0..=7 => {
                self.set_gpr_from_gpr_index(GprIndex::from_raw(index as u32 + 10).unwrap(), val);
            }
            _ => {
                warn!(
                    "RISCVVCpu: Unsupported general purpose register index: {}",
                    index
                );
            }
        }
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

        trace!(
            "vmexit_handler: {:?}, sepc: {:#x}, stval: {:#x}",
            scause.cause(),
            self.regs.guest_regs.sepc,
            self.regs.trap_csrs.stval
        );

        match scause.cause() {
            Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
                let a = self.regs.guest_regs.gprs.a_regs();
                let param = [a[0], a[1], a[2], a[3], a[4], a[5]];
                let extension_id = a[7];
                let function_id = a[6];

                match extension_id {
                    // Compatibility with Legacy Extensions.
                    legacy::LEGACY_SET_TIMER..=legacy::LEGACY_SHUTDOWN => match extension_id {
                        legacy::LEGACY_SET_TIMER => {
                            // sbi_call_legacy_1(LEGACY_SET_TIMER, time_value)
                            self.sbi.timer.set_timer(param[0] as _);
                            self.set_gpr_from_gpr_index(GprIndex::A0, 0);
                        }
                        legacy::LEGACY_CONSOLE_PUTCHAR => {
                            // sbi_call_legacy_1(LEGACY_CONSOLE_PUTCHAR, c)
                            let ret = sbi_rt::console_write_byte(param[0] as _);
                            self.set_gpr_from_gpr_index(GprIndex::A0, ret.error);
                            // Note:
                            // The RustSBI implementation does not return a value for this call,
                            // we do not set `a1` here, because guest VM may not expect a modification to `a1`.
                        }
                        legacy::LEGACY_CONSOLE_GETCHAR => {
                            // sbi_call_legacy_0(LEGACY_CONSOLE_GETCHAR)
                            let c: isize = -1;
                            let ret = sbi_rt::console_read(sbi_rt::Physical::new(
                                1,
                                crate_interface::call_interface!(crate::HalIf::virt_to_phys(
                                    HostVirtAddr::from_ptr_of(core::ptr::addr_of!(c))
                                ))
                                .as_usize(),
                                0,
                            ));
                            if ret.is_ok() {
                                self.set_gpr_from_gpr_index(GprIndex::A0, c as _);
                            } else {
                                warn!(
                                "LEGACY_CONSOLE_GETCHAR c {:#x} param {:#x?} err {:#x} value {:#x}",
                                c, param, ret.error, ret.value
                            );
                                self.set_gpr_from_gpr_index(GprIndex::A0, ret.error);
                            }
                        }
                        legacy::LEGACY_SHUTDOWN => {
                            // sbi_call_legacy_0(LEGACY_SHUTDOWN)
                            return Ok(AxVCpuExitReason::SystemDown);
                        }
                        _ => {
                            warn!(
                                "Unsupported SBI legacy extension id {:#x} function id {:#x}",
                                extension_id, function_id
                            );
                        }
                    },
                    // Handle HSM extension
                    hsm::EID_HSM => match function_id {
                        hsm::HART_START => {
                            let hartid = a[0];
                            let start_addr = a[1];
                            let opaque = a[2];
                            self.advance_pc(4);
                            return Ok(AxVCpuExitReason::CpuUp {
                                target_cpu: hartid as _,
                                entry_point: GuestPhysAddr::from(start_addr),
                                arg: opaque as _,
                            });
                        }
                        hsm::HART_STOP => {
                            return Ok(AxVCpuExitReason::CpuDown { _state: 0 });
                        }
                        hsm::HART_SUSPEND => {
                            // Todo: support these parameters.
                            let _suspend_type = a[0];
                            let _resume_addr = a[1];
                            let _opaque = a[2];
                            return Ok(AxVCpuExitReason::Halt);
                        }
                        _ => todo!(),
                    },
                    // Handle hypercall
                    EID_HVC => {
                        self.advance_pc(4);
                        return Ok(AxVCpuExitReason::Hypercall {
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
                    // By default, forward the SBI call to the RustSBI implementation.
                    // See [`RISCVVCpuSbi`].
                    _ => {
                        let ret = self.sbi.handle_ecall(extension_id, function_id, param);
                        if ret.is_err() {
                            warn!(
                                "forward ecall eid {:#x} fid {:#x} param {:#x?} err {:#x} value {:#x}",
                                extension_id, function_id, param, ret.error, ret.value
                            );
                        }
                        self.set_gpr_from_gpr_index(GprIndex::A0, ret.error);
                        self.set_gpr_from_gpr_index(GprIndex::A1, ret.value);
                    }
                };

                self.advance_pc(4);
                Ok(AxVCpuExitReason::Nothing)
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
}
