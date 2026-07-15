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

use aarch64_cpu::registers::*;

use crate::{
    ArmGuestPhysAddr, ArmHostOps, ArmNestedPagingConfig, ArmSysRegAddr, ArmVcpuResult, ArmVmExit,
    TrapFrame,
    context_frame::GuestSystemRegisters,
    exception::{TrapKind, handle_exception_sync},
    exception_utils::exception_class_value,
};

/// Size of the guest trap frame used by the EL2 entry/exit assembly.
pub const ARM_VCPU_TRAP_FRAME_SIZE: usize = core::mem::size_of::<TrapFrame>();
/// Offset of `HostRuntimeContext::stack_top` within [`ArmVcpu`].
pub const ARM_VCPU_HOST_STACK_TOP_OFFSET: usize = ARM_VCPU_TRAP_FRAME_SIZE;
/// Offset of `HostRuntimeContext::sp_el0` within [`ArmVcpu`].
pub const ARM_VCPU_HOST_SP_EL0_OFFSET: usize =
    ARM_VCPU_HOST_STACK_TOP_OFFSET + core::mem::size_of::<u64>();

/// (v)CPU register state that must be saved or restored when entering/exiting a VM or switching
/// between VMs.
#[repr(C)]
#[derive(Clone, Debug, Copy, Default)]
#[allow(dead_code)]
pub struct VmCpuRegisters {
    /// guest trap context
    pub trap_context_regs: TrapFrame,
    /// virtual machine system regs setting
    pub vm_system_regs: GuestSystemRegisters,
}

/// Host-only state used by one guest entry/exit round.
#[repr(C)]
#[derive(Debug, Default)]
struct HostRuntimeContext {
    stack_top: u64,
    sp_el0: u64,
}

/// A virtual CPU within a guest.
#[repr(C)]
#[derive(Debug)]
pub struct ArmVcpu<H: ArmHostOps> {
    // The first two fields are consumed by exception.S and vmexit_trampoline.
    // Keep `ctx` first and `host` immediately after it.
    ctx: TrapFrame,
    host: HostRuntimeContext,
    guest_system_regs: GuestSystemRegisters,
    /// The MPIDR_EL1 value for the vCPU.
    mpidr: u64,
    _host: PhantomData<fn() -> H>,
}

/// Configuration for creating a new [`ArmVcpu`].
#[derive(Clone, Debug, Default)]
pub struct ArmVcpuCreateConfig {
    /// The MPIDR_EL1 value for the new vCPU,
    /// which is used to identify the CPU in a multiprocessor system.
    /// Note: mind CPU cluster.
    // FIXME: Handle its interaction with the virtual GIC.
    pub mpidr_el1: u64,
    /// The address of the device tree blob.
    pub dtb_addr: usize,
}

/// Configuration for setting up a new [`ArmVcpu`].
#[derive(Clone, Debug, Default)]
pub struct ArmVcpuSetupConfig {
    /// Should the hypervisor passthrough interrupts to the guest?
    pub passthrough_interrupt: bool,
    /// Should the hypervisor passthrough timers to the guest?
    pub passthrough_timer: bool,
}

impl<H: ArmHostOps> ArmVcpu<H> {
    /// Creates a new architecture-specific vCPU.
    pub fn new(_vm_id: usize, _vcpu_id: usize, config: ArmVcpuCreateConfig) -> ArmVcpuResult<Self> {
        let mut ctx = TrapFrame::default();
        ctx.set_argument(config.dtb_addr);

        Ok(Self {
            ctx,
            host: HostRuntimeContext::default(),
            guest_system_regs: GuestSystemRegisters::default(),
            mpidr: config.mpidr_el1,
            _host: PhantomData,
        })
    }

    /// Completes architecture-specific setup.
    pub fn setup(&mut self, config: ArmVcpuSetupConfig) -> ArmVcpuResult {
        self.init_hv(config);
        Ok(())
    }

    /// Sets the guest entry point.
    pub fn set_entry(&mut self, entry: ArmGuestPhysAddr) -> ArmVcpuResult {
        debug!("set vcpu entry:{entry:?}");
        self.set_elr(entry.as_usize());
        Ok(())
    }

    /// Sets the nested page table selected by the embedding VMM.
    pub fn set_nested_page_table(&mut self, config: ArmNestedPagingConfig) -> ArmVcpuResult {
        debug!("set vcpu stage-2 root:{:#x}", config.root_paddr);
        self.guest_system_regs.vttbr_el2 = config.root_paddr as u64;
        let pa_bits = if config.mode == 0 {
            pa_bits()
        } else {
            config.mode
        };
        self.guest_system_regs.vtcr_el2 = vtcr_for_config(config.levels, config.gpa_bits, pa_bits);
        Ok(())
    }

    /// Runs the vCPU until a VM exit.
    pub fn run(&mut self) -> ArmVcpuResult<ArmVmExit> {
        let host_daif: u64;
        // SAFETY: reading DAIF and masking the local IRQ bit changes only the
        // current CPU's exception mask. The saved value is restored below.
        unsafe {
            core::arch::asm!(
                "mrs {host_daif}, daif",
                "msr daifset, #2",
                host_daif = out(reg) host_daif,
            );
        }

        let exit_reason = unsafe {
            self.restore_vm_system_regs();
            self.run_guest()
        };

        let trap_kind = TrapKind::try_from(exit_reason as u8).expect("Invalid TrapKind");
        let result = self.vmexit_handler(trap_kind);

        // SAFETY: `host_daif` was captured on this CPU immediately before the
        // guest entry and restores the caller's complete exception mask.
        unsafe {
            core::arch::asm!("msr daif, {host_daif}", host_daif = in(reg) host_daif);
        }

        result
    }

    /// Binds this vCPU to the current physical CPU.
    pub fn bind(&mut self) -> ArmVcpuResult {
        Ok(())
    }

    /// Unbinds this vCPU from the current physical CPU.
    pub fn unbind(&mut self) -> ArmVcpuResult {
        Ok(())
    }

    /// Sets a general-purpose register.
    pub fn set_gpr(&mut self, idx: usize, val: usize) {
        self.ctx.set_gpr(idx, val);
    }

    /// Sets the guest return value.
    pub fn set_return_value(&mut self, val: usize) {
        // Return value is stored in x0.
        self.ctx.set_argument(val);
    }
}

// Private function
impl<H: ArmHostOps> ArmVcpu<H> {
    fn init_hv(&mut self, config: ArmVcpuSetupConfig) {
        self.ctx.spsr = (SPSR_EL1::M::EL1h
            + SPSR_EL1::I::Masked
            + SPSR_EL1::F::Masked
            + SPSR_EL1::A::Masked
            + SPSR_EL1::D::Masked)
            .value;
        self.init_vm_context(config);
    }

    /// Init guest context. Also set some el2 register value.
    fn init_vm_context(&mut self, config: ArmVcpuSetupConfig) {
        // CNTHCTL_EL2.modify(CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET);
        // Set CNTVOFF_EL2 to the current physical counter so the guest's
        // virtual counter (CNTVCT_EL0 = CNTPCT_EL0 - CNTVOFF_EL2) starts near zero.
        let cntpct: u64;
        unsafe { core::arch::asm!("mrs {0}, CNTPCT_EL0", out(reg) cntpct) };
        self.guest_system_regs.cntvoff_el2 = cntpct;
        self.guest_system_regs.cntkctl_el1 = 0;
        self.guest_system_regs.cnthctl_el2 = if config.passthrough_timer {
            (CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET).into()
        } else {
            (CNTHCTL_EL2::EL1PCEN::CLEAR + CNTHCTL_EL2::EL1PCTEN::CLEAR).into()
        };

        self.guest_system_regs.sctlr_el1 = 0x30C50830;
        self.guest_system_regs.pmcr_el0 = 0;

        if self.guest_system_regs.vtcr_el2 == 0 {
            let pa_bits = pa_bits();
            let levels = max_gpt_level(pa_bits);
            let gpa_bits = if levels == 3 { 39 } else { 48 };
            self.guest_system_regs.vtcr_el2 = vtcr_for_config(levels, gpa_bits, pa_bits);
        }

        let mut hcr_el2 =
            HCR_EL2::VM::Enable + HCR_EL2::TSC::EnableTrapEl1SmcToEl2 + HCR_EL2::RW::EL1IsAarch64;

        if !config.passthrough_interrupt {
            // Set HCR_EL2.IMO will trap IRQs to EL2 while enabling virtual IRQs.
            //
            // We must choose one of the two:
            // - Enable virtual IRQs and trap physical IRQs to EL2.
            // - Disable virtual IRQs and pass through physical IRQs to EL1.
            hcr_el2 += HCR_EL2::IMO::EnableVirtualIRQ + HCR_EL2::FMO::EnableVirtualFIQ;
        }

        self.guest_system_regs.hcr_el2 = hcr_el2.into();

        // Set VMPIDR_EL2, which provides the value of the Virtualization Multiprocessor ID.
        // This is the value returned by Non-secure EL1 reads of MPIDR.
        let mut vmpidr = 1 << 31;
        // Note: mind CPU cluster here.
        vmpidr |= self.mpidr;
        self.guest_system_regs.vmpidr_el2 = vmpidr;
    }

    /// Set exception return pc
    fn set_elr(&mut self, elr: usize) {
        self.ctx.set_exception_pc(elr);
    }

    /// Get general purpose register
    #[allow(unused)]
    fn get_gpr(&self, idx: usize) {
        self.ctx.gpr(idx);
    }
}

/// Private functions related to vcpu runtime control flow.
impl<H: ArmHostOps> ArmVcpu<H> {
    /// Save host context and run guest.
    ///
    /// When a VM-Exit happens when guest's vCpu is running,
    /// the control flow will be redirected to this function through `return_run_guest`.
    #[unsafe(naked)]
    unsafe extern "C" fn run_guest(&mut self) -> usize {
        // Fixes: https://github.com/arceos-hypervisor/arm_vcpu/issues/22
        //
        // The original issue seems to be caused by an unexpected compiler optimization that takes
        // the dummy return value `0` of `run_guest` as the actual return value. By replacing the
        // original `run_guest` with the current naked one, we eliminate the dummy code path of the
        // original version, and ensure that the compiler does not perform any unexpected return
        // value optimization.
        core::arch::naked_asm!(
            // Save host context.
            save_regs_to_stack!(),
            // Save the host stack top and SP_EL0 to `self.host`.
            //
            // 'extern "C"' here specifies the aapcs64 calling convention, according to which
            // the first and only parameter, the pointer of self, should be in x0.
            "mov x9, sp",
            "add x10, x0, {host_stack_top_offset}",
            "str x9, [x10]",
            "mrs x9, sp_el0",
            "str x9, [x10, #8]",
            // Go to `context_vm_entry` with x0 pointing to `self.host.stack_top`.
            "mov x0, x10",
            "b context_vm_entry",
            // Panic if the control flow comes back here, which should never happen.
            "b {run_guest_panic}",
            host_stack_top_offset = const ARM_VCPU_HOST_STACK_TOP_OFFSET,
            run_guest_panic = sym Self::run_guest_panic,
        );
    }

    /// This function is called when the control flow comes back to `run_guest`. To provide a error
    /// message for debugging purposes.
    ///
    /// This function may fail as the stack may have been corrupted when this function is called.
    /// But we won't handle it here for now.
    unsafe fn run_guest_panic() -> ! {
        panic!("run_guest_panic");
    }

    /// Restores guest system control registers.
    unsafe fn restore_vm_system_regs(&mut self) {
        unsafe {
            // load system regs
            core::arch::asm!(
                "
                mov x3, xzr           // Trap nothing from EL1 to El2.
                msr cptr_el2, x3"
            );
            self.guest_system_regs.restore();
            core::arch::asm!(
                "
                ic  iallu
                tlbi	alle2
                tlbi	alle1         // Flush tlb
                dsb	nsh
                isb"
            );
        }
    }

    /// Handle VM-Exits.
    ///
    /// Parameters:
    /// - `exit_reason`: The reason why the VM-Exit happened in [`TrapKind`].
    ///
    /// Returns:
    /// - [`ArmVmExit`]: a wrappered VM-Exit reason needed to be handled by the hypervisor.
    ///
    /// This function may panic for unhandled exceptions.
    fn vmexit_handler(&mut self, exit_reason: TrapKind) -> ArmVcpuResult<ArmVmExit> {
        trace!(
            "ArmVcpu vmexit_handler() esr:{:#x} ctx:{:#x?}",
            exception_class_value(),
            self.ctx
        );

        unsafe {
            // Store guest system regs. Guest SP_EL0 was already saved into `self.ctx`
            // by the EL2 assembly before host SP_EL0 was restored.
            self.guest_system_regs.store();
        }

        let result = match exit_reason {
            TrapKind::Synchronous => handle_exception_sync(&mut self.ctx),
            TrapKind::Irq => Ok(ArmVmExit::ExternalInterrupt),
            _ => panic!("Unhandled exception {:?}", exit_reason),
        };

        match result {
            Ok(ArmVmExit::SysRegRead { addr, reg }) => {
                if let Some(exit_reason) =
                    self.builtin_sysreg_access_handler(addr, false, 0, reg)?
                {
                    return Ok(exit_reason);
                }

                result
            }
            Ok(ArmVmExit::SysRegWrite { addr, value }) => {
                if let Some(exit_reason) =
                    self.builtin_sysreg_access_handler(addr, true, value, 0)?
                {
                    return Ok(exit_reason);
                }

                result
            }
            r => r,
        }
    }

    /// Handle system register access that can and should be handled by the VCpu itself.
    ///
    /// Return `Ok(None)` if the system register access is not handled by the VCpu itself,
    fn builtin_sysreg_access_handler(
        &mut self,
        addr: ArmSysRegAddr,
        write: bool,
        value: u64,
        reg: usize,
    ) -> ArmVcpuResult<Option<ArmVmExit>> {
        const SYSREG_ICC_SGI1R_EL1: ArmSysRegAddr = ArmSysRegAddr::new(0x3A_3016); // ICC_SGI1R_EL1

        match (addr, write) {
            (SYSREG_ICC_SGI1R_EL1, true) => {
                debug!("arm_vcpu ICC_SGI1R_EL1 write: {value:#x}");
                Ok(Some(ArmVmExit::SendIPI { value }))
            }
            (SYSREG_ICC_SGI1R_EL1, false) => {
                // ICC_SGI1R_EL1 is WO, we take it as RAZ.
                self.set_gpr(reg, 0);
                Ok(Some(ArmVmExit::Nothing))
            }
            _ => {
                // If the system register access is not handled by the VCpu itself,
                // we return None to let the hypervisor handle it.
                Ok(None)
            }
        }
    }
}

pub(crate) fn pa_bits() -> usize {
    match ID_AA64MMFR0_EL1.read_as_enum(ID_AA64MMFR0_EL1::PARange) {
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_32) => 32,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_36) => 36,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_40) => 40,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_42) => 42,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_44) => 44,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_48) => 48,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_52) => 52,
        _ => 32,
    }
}

#[allow(dead_code)]
pub(crate) fn current_gpt_level() -> usize {
    let t0sz = VTCR_EL2.read(VTCR_EL2::T0SZ) as usize;
    match t0sz {
        16..=25 => 4,
        26..=35 => 3,
        _ => 2,
    }
}

pub(crate) fn max_gpt_level(pa_bits: usize) -> usize {
    match pa_bits {
        44.. => 4,
        _ => 3,
    }
}

fn vtcr_for_config(levels: usize, gpa_bits: usize, pa_bits: usize) -> u64 {
    let mut val = match levels {
        4 => VTCR_EL2::SL0::Granule4KBLevel0 + VTCR_EL2::T0SZ.val((64 - gpa_bits) as u64),
        _ => VTCR_EL2::SL0::Granule4KBLevel1 + VTCR_EL2::T0SZ.val((64 - gpa_bits) as u64),
    };

    match pa_bits {
        52..=64 => val += VTCR_EL2::PS::PA_52B_4PB,
        48..=51 => val += VTCR_EL2::PS::PA_48B_256TB,
        44..=47 => val += VTCR_EL2::PS::PA_44B_16TB,
        42..=43 => val += VTCR_EL2::PS::PA_42B_4TB,
        40..=41 => val += VTCR_EL2::PS::PA_40B_1TB,
        36..=39 => val += VTCR_EL2::PS::PA_36B_64GB,
        _ => val += VTCR_EL2::PS::PA_32B_4GB,
    }

    val += VTCR_EL2::TG0::Granule4KB
        + VTCR_EL2::SH0::Inner
        + VTCR_EL2::ORGN0::NormalWBRAWA
        + VTCR_EL2::IRGN0::NormalWBRAWA;

    val.value
}
