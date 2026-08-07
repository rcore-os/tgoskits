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
use aarch64_sysreg::SystemRegType;

use super::{
    TrapFrame,
    context_frame::GuestSystemRegisters,
    exception::{TrapKind, handle_exception_sync},
    exception_utils::exception_class_value,
    host::{ArmHostIrqConfig, ArmHostIrqGuard, ArmHostOps},
};
use crate::{
    ArmGuestPhysAddr, ArmNestedPagingConfig, ArmSysRegAddr, ArmTimerKind, ArmTimerSnapshot,
    ArmTimerVmConfig, ArmVcpuResult, ArmVcpuTimer, ArmVmExit,
};

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
    tpidr_el0: u64,
    irq_interface: u64,
    irq_cpu_interface_base: usize,
    pending_irq_ack: u32,
    _reserved: u32,
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
    timer: ArmVcpuTimer,
    /// The MPIDR_EL1 value for the vCPU.
    mpidr: u64,
    _host: PhantomData<fn() -> H>,
}

struct AssemblyLayoutHost;

impl ArmHostOps for AssemblyLayoutHost {
    fn inject_virtual_interrupt(_vector: u32) -> ArmVcpuResult {
        Err(crate::ArmVcpuError::BadState)
    }

    fn finish_pending_host_irq(_raw_ack: u32) -> Option<usize> {
        None
    }

    fn handle_current_host_irq() {}
}

type AssemblyArmVcpu = ArmVcpu<AssemblyLayoutHost>;

/// Size of the guest trap frame used by the EL2 entry/exit assembly.
pub const ARM_VCPU_TRAP_FRAME_SIZE: usize = core::mem::size_of::<TrapFrame>();
/// Offset of [`HostRuntimeContext::stack_top`] within [`ArmVcpu`].
pub const ARM_VCPU_HOST_STACK_TOP_OFFSET: usize = core::mem::offset_of!(AssemblyArmVcpu, host)
    + core::mem::offset_of!(HostRuntimeContext, stack_top);
/// Offset of [`HostRuntimeContext::sp_el0`] within [`ArmVcpu`].
pub const ARM_VCPU_HOST_SP_EL0_OFFSET: usize = core::mem::offset_of!(AssemblyArmVcpu, host)
    + core::mem::offset_of!(HostRuntimeContext, sp_el0);
/// Offset of the host task's `TPIDR_EL0` slot within [`ArmVcpu`].
pub(crate) const ARM_VCPU_HOST_TPIDR_EL0_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, host)
        + core::mem::offset_of!(HostRuntimeContext, tpidr_el0);
pub(crate) const ARM_VCPU_HOST_IRQ_INTERFACE_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, host)
        + core::mem::offset_of!(HostRuntimeContext, irq_interface);
pub(crate) const ARM_VCPU_HOST_IRQ_CPU_INTERFACE_BASE_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, host)
        + core::mem::offset_of!(HostRuntimeContext, irq_cpu_interface_base);
pub(crate) const ARM_VCPU_HOST_PENDING_IRQ_ACK_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, host)
        + core::mem::offset_of!(HostRuntimeContext, pending_irq_ack);
/// Offset of the guest-owned `TPIDR_EL0` slot within [`ArmVcpu`].
pub(crate) const ARM_VCPU_GUEST_TPIDR_EL0_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, guest_system_regs)
        + super::context_frame::GUEST_TPIDR_EL0_OFFSET;
pub(crate) const ARM_VCPU_TIMER_VIRTUAL_OFFSET_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, timer) + crate::timer::TIMER_VIRTUAL_OFFSET_OFFSET;
pub(crate) const ARM_VCPU_TIMER_VIRTUAL_COMPARE_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, timer) + crate::timer::TIMER_VIRTUAL_COMPARE_OFFSET;
pub(crate) const ARM_VCPU_TIMER_VIRTUAL_CONTROL_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, timer) + crate::timer::TIMER_VIRTUAL_CONTROL_OFFSET;
pub(crate) const ARM_VCPU_TIMER_GUEST_HYPERVISOR_CONTROL_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, timer)
        + crate::timer::TIMER_GUEST_HYPERVISOR_CONTROL_OFFSET;
pub(crate) const ARM_VCPU_TIMER_GUEST_KERNEL_CONTROL_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, timer) + crate::timer::TIMER_GUEST_KERNEL_CONTROL_OFFSET;
pub(crate) const ARM_VCPU_TIMER_HOST_HYPERVISOR_CONTROL_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, timer)
        + crate::timer::TIMER_HOST_HYPERVISOR_CONTROL_OFFSET;
pub(crate) const ARM_VCPU_TIMER_HOST_KERNEL_CONTROL_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, timer) + crate::timer::TIMER_HOST_KERNEL_CONTROL_OFFSET;
pub(crate) const ARM_VCPU_TIMER_LOADED_OFFSET: usize =
    core::mem::offset_of!(AssemblyArmVcpu, timer) + crate::timer::TIMER_LOADED_OFFSET;

const _: () = {
    assert!(core::mem::offset_of!(AssemblyArmVcpu, ctx) == 0);
    assert!(ARM_VCPU_HOST_STACK_TOP_OFFSET == ARM_VCPU_TRAP_FRAME_SIZE);
    assert!(
        ARM_VCPU_HOST_SP_EL0_OFFSET == ARM_VCPU_HOST_STACK_TOP_OFFSET + core::mem::size_of::<u64>()
    );
    assert!(
        ARM_VCPU_HOST_TPIDR_EL0_OFFSET == ARM_VCPU_HOST_SP_EL0_OFFSET + core::mem::size_of::<u64>()
    );
    assert!(ARM_VCPU_HOST_IRQ_INTERFACE_OFFSET.is_multiple_of(core::mem::align_of::<u64>()));
    assert!(
        ARM_VCPU_HOST_IRQ_CPU_INTERFACE_BASE_OFFSET.is_multiple_of(core::mem::align_of::<usize>())
    );
    assert!(ARM_VCPU_HOST_PENDING_IRQ_ACK_OFFSET.is_multiple_of(core::mem::align_of::<u32>()));
    assert!(
        ARM_VCPU_GUEST_TPIDR_EL0_OFFSET
            >= ARM_VCPU_HOST_TPIDR_EL0_OFFSET + core::mem::size_of::<u64>()
    );
    assert!(ARM_VCPU_TIMER_VIRTUAL_OFFSET_OFFSET.is_multiple_of(core::mem::align_of::<u64>()));
    assert!(ARM_VCPU_TIMER_VIRTUAL_COMPARE_OFFSET.is_multiple_of(core::mem::align_of::<u64>()));
    assert!(ARM_VCPU_TIMER_VIRTUAL_CONTROL_OFFSET.is_multiple_of(core::mem::align_of::<u32>()));
};

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

/// Fixed EL2 setup policy for a new [`ArmVcpu`].
///
/// Physical interrupts and timers are always trapped. A physical device may
/// back a virtual interrupt, but it must still pass through the VM-owned
/// virtual interrupt controller rather than bypassing vCPU state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmVcpuSetupConfig {
    timer: ArmTimerVmConfig,
    host_irq: ArmHostIrqConfig,
}

impl ArmVcpuSetupConfig {
    /// Creates setup state with the VM-wide timer configuration and immutable
    /// host interrupt-controller interface.
    ///
    /// Every vCPU in one VM must receive the same immutable configuration.
    pub const fn new(timer: ArmTimerVmConfig, host_irq: ArmHostIrqConfig) -> Self {
        Self { timer, host_irq }
    }

    /// Returns the VM-wide timer configuration.
    pub const fn timer(self) -> ArmTimerVmConfig {
        self.timer
    }

    /// Returns the host IRQ interface consumed by the world-switch assembly.
    pub const fn host_irq(self) -> ArmHostIrqConfig {
        self.host_irq
    }
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
            timer: ArmVcpuTimer::unconfigured(),
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

    /// Returns the architectural timer state saved at the last VM exit.
    pub fn timer_snapshot(&self) -> ArmVcpuResult<ArmTimerSnapshot> {
        self.timer.snapshot()
    }

    /// Runs the vCPU until a VM exit while the caller holds the host IRQ mask.
    ///
    /// Requiring the guard keeps architecture-external VGIC load/save hooks in
    /// the same IRQ-atomic transaction as guest execution.
    pub fn run(&mut self, _host_irq_guard: &ArmHostIrqGuard) -> ArmVcpuResult<ArmVmExit> {
        let exit_reason = unsafe {
            if !self.timer.is_configured() || self.timer.is_loaded() {
                return Err(crate::ArmVcpuError::BadState);
            }
            self.restore_vm_system_regs();
            self.run_guest()
        };

        if self.timer.is_loaded() {
            return Err(crate::ArmVcpuError::BadState);
        }
        let trap_kind = TrapKind::try_from(exit_reason as u8).expect("Invalid TrapKind");
        self.vmexit_handler(trap_kind)
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

    /// Injects an interrupt into the guest vCPU.
    pub fn inject_interrupt(&mut self, vector: usize) -> ArmVcpuResult {
        let vector = u32::try_from(vector).map_err(|_| crate::ArmVcpuError::InvalidInput)?;
        H::inject_virtual_interrupt(vector)
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
        let guest_hypervisor_control =
            (CNTHCTL_EL2::EL1PCEN::CLEAR + CNTHCTL_EL2::EL1PCTEN::CLEAR).into();
        self.timer = ArmVcpuTimer::new(config.timer(), guest_hypervisor_control);
        self.host.irq_interface = config.host_irq().interface();
        self.host.irq_cpu_interface_base = config.host_irq().cpu_interface_base();
        self.host.pending_irq_ack = u32::MAX;

        self.guest_system_regs.sctlr_el1 = 0x30C50830;
        self.guest_system_regs.pmcr_el0 = 0;

        if self.guest_system_regs.vtcr_el2 == 0 {
            let pa_bits = pa_bits();
            let levels = max_gpt_level(pa_bits);
            let gpa_bits = if levels == 3 { 39 } else { 48 };
            self.guest_system_regs.vtcr_el2 = vtcr_for_config(levels, gpa_bits, pa_bits);
        }

        let hcr_el2 = HCR_EL2::VM::Enable
            + HCR_EL2::TSC::EnableTrapEl1SmcToEl2
            + HCR_EL2::TWI::SET
            + HCR_EL2::RW::EL1IsAarch64
            + HCR_EL2::IMO::EnableVirtualIRQ
            + HCR_EL2::FMO::EnableVirtualFIQ;

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
            // Save host task TLS before the final assembly window installs the
            // guest value. No Rust executes with guest TPIDR_EL0 live.
            "mrs x9, tpidr_el0",
            "str x9, [x10, {host_tpidr_el0_delta}]",
            // Go to `context_vm_entry` with x0 pointing to `self.host.stack_top`.
            "mov x0, x10",
            "b context_vm_entry",
            // Panic if the control flow comes back here, which should never happen.
            "b {run_guest_panic}",
            host_stack_top_offset = const ARM_VCPU_HOST_STACK_TOP_OFFSET,
            host_tpidr_el0_delta = const ARM_VCPU_HOST_TPIDR_EL0_OFFSET
                - ARM_VCPU_HOST_STACK_TOP_OFFSET,
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
            TrapKind::Irq => {
                let raw_ack = core::mem::replace(&mut self.host.pending_irq_ack, u32::MAX);
                Ok(ArmVmExit::ExternalInterrupt {
                    token: (raw_ack != u32::MAX)
                        .then(|| H::finish_pending_host_irq(raw_ack))
                        .flatten(),
                })
            }
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
        const SYSREG_ICC_PMR_EL1: ArmSysRegAddr = ArmSysRegAddr::new(0x30_100c);
        const SYSREG_ICC_SGI1R_EL1: ArmSysRegAddr = ArmSysRegAddr::new(0x3a_3016);
        const SYSREG_ICC_DIR_EL1: ArmSysRegAddr = ArmSysRegAddr::new(0x32_3016);
        const SYSREG_ICC_RPR_EL1: ArmSysRegAddr = ArmSysRegAddr::new(0x36_3016);
        const SYSREG_ICC_CTLR_EL1: ArmSysRegAddr = ArmSysRegAddr::new(0x38_3018);
        const SYSREG_CNTFRQ_EL0: ArmSysRegAddr =
            ArmSysRegAddr::new(SystemRegType::CNTFRQ_EL0 as usize);
        const SYSREG_CNTPCT_EL0: ArmSysRegAddr =
            ArmSysRegAddr::new(SystemRegType::CNTPCT_EL0 as usize);
        const SYSREG_CNTP_TVAL_EL0: ArmSysRegAddr =
            ArmSysRegAddr::new(SystemRegType::CNTP_TVAL_EL0 as usize);
        const SYSREG_CNTP_CTL_EL0: ArmSysRegAddr =
            ArmSysRegAddr::new(SystemRegType::CNTP_CTL_EL0 as usize);
        const SYSREG_CNTP_CVAL_EL0: ArmSysRegAddr =
            ArmSysRegAddr::new(SystemRegType::CNTP_CVAL_EL0 as usize);

        match (addr, write) {
            (SYSREG_CNTFRQ_EL0, false) => {
                self.set_gpr(reg, self.timer.config().frequency() as usize);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTPCT_EL0, false) => {
                let counter = self
                    .timer
                    .guest_counter(ArmTimerKind::Physical, physical_counter())?;
                self.set_gpr(reg, counter as usize);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTP_TVAL_EL0, false) => {
                let value = self
                    .timer
                    .read_tval(ArmTimerKind::Physical, physical_counter())?;
                self.set_gpr(reg, value as usize);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTP_CTL_EL0, false) => {
                let value = self
                    .timer
                    .read_control(ArmTimerKind::Physical, physical_counter())?;
                self.set_gpr(reg, value as usize);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTP_CVAL_EL0, false) => {
                let value = self.timer.read_compare(ArmTimerKind::Physical)?;
                self.set_gpr(reg, value as usize);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTP_TVAL_EL0, true) => {
                self.timer
                    .write_tval(ArmTimerKind::Physical, physical_counter(), value as u32)?;
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTP_CTL_EL0, true) => {
                self.timer
                    .write_control(ArmTimerKind::Physical, value as u32)?;
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTP_CVAL_EL0, true) => {
                self.timer.write_compare(ArmTimerKind::Physical, value)?;
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTFRQ_EL0 | SYSREG_CNTPCT_EL0, true) => Err(crate::ArmVcpuError::InvalidInput),
            (SYSREG_ICC_SGI1R_EL1, true) => {
                debug!("arm_vcpu ICC_SGI1R_EL1 write: {value:#x}");
                Ok(Some(ArmVmExit::SendIPI { value }))
            }
            (SYSREG_ICC_SGI1R_EL1, false) => {
                // ICC_SGI1R_EL1 is WO, we take it as RAZ.
                self.set_gpr(reg, 0);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_ICC_DIR_EL1, true) => Ok(Some(ArmVmExit::DeactivateInterrupt {
                intid: value as u32 & 0x00ff_ffff,
            })),
            (SYSREG_ICC_DIR_EL1, false) => {
                self.set_gpr(reg, 0);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_ICC_CTLR_EL1, false) => Ok(Some(ArmVmExit::GicCpuInterfaceRead {
                register: crate::ArmGicCpuInterfaceRegister::Control,
                destination: reg,
            })),
            (SYSREG_ICC_CTLR_EL1, true) => Ok(Some(ArmVmExit::GicCpuInterfaceWrite {
                register: crate::ArmGicCpuInterfaceRegister::Control,
                value,
            })),
            (SYSREG_ICC_PMR_EL1, false) => Ok(Some(ArmVmExit::GicCpuInterfaceRead {
                register: crate::ArmGicCpuInterfaceRegister::PriorityMask,
                destination: reg,
            })),
            (SYSREG_ICC_PMR_EL1, true) => Ok(Some(ArmVmExit::GicCpuInterfaceWrite {
                register: crate::ArmGicCpuInterfaceRegister::PriorityMask,
                value,
            })),
            (SYSREG_ICC_RPR_EL1, false) => Ok(Some(ArmVmExit::GicCpuInterfaceRead {
                register: crate::ArmGicCpuInterfaceRegister::RunningPriority,
                destination: reg,
            })),
            (SYSREG_ICC_RPR_EL1, true) => Ok(Some(ArmVmExit::GicCpuInterfaceWrite {
                register: crate::ArmGicCpuInterfaceRegister::RunningPriority,
                value,
            })),
            _ => {
                // If the system register access is not handled by the VCpu itself,
                // we return None to let the hypervisor handle it.
                Ok(None)
            }
        }
    }
}

fn physical_counter() -> u64 {
    let counter: u64;
    unsafe {
        core::arch::asm!("mrs {counter}, CNTPCT_EL0", counter = out(reg) counter);
    }
    counter
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
