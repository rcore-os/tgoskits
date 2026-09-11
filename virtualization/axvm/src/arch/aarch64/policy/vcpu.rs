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

use aarch64_cpu::registers::*;
use aarch64_sysreg::SystemRegType;
use ax_cpu::virtualization::HostIrqConfig;

use super::{
    TrapFrame,
    exception::{TrapKind, handle_exception_sync},
    exception_utils::exception_class_value,
    host::ArmHostIrqGuard,
};
use crate::arch::aarch64::policy::{
    ArmGuestPhysAddr, ArmNestedPagingConfig, ArmSysRegAddr, ArmTimerKind, ArmTimerSnapshot,
    ArmTimerVmConfig, ArmVcpuError, ArmVcpuResult, ArmVcpuTimer, ArmVmExit,
};

/// VM policy around the CPU-owned guest machine state.
#[derive(Debug)]
pub struct ArmVcpu {
    machine: ax_cpu::virtualization::Vcpu,
    timer: ArmVcpuTimer,
    mpidr: u64,
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

/// Fixed EL2 setup policy for a new [`ArmVcpu`].
///
/// Physical interrupts and timers are always trapped. A physical device may
/// back a virtual interrupt, but it must still pass through the VM-owned
/// virtual interrupt controller rather than bypassing vCPU state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmVcpuSetupConfig {
    timer: ArmTimerVmConfig,
    host_irq: HostIrqConfig,
}

impl ArmVcpuSetupConfig {
    /// Creates setup state with the VM-wide timer configuration and immutable
    /// host interrupt-controller interface.
    ///
    /// Every vCPU in one VM must receive the same immutable configuration.
    pub const fn new(timer: ArmTimerVmConfig, host_irq: HostIrqConfig) -> Self {
        Self { timer, host_irq }
    }

    /// Returns the VM-wide timer configuration.
    pub const fn timer(self) -> ArmTimerVmConfig {
        self.timer
    }

    /// Returns the host IRQ interface consumed by the world-switch assembly.
    pub const fn host_irq(self) -> HostIrqConfig {
        self.host_irq
    }
}

impl ArmVcpu {
    /// Creates a new architecture-specific vCPU.
    pub fn new(_vm_id: usize, _vcpu_id: usize, config: ArmVcpuCreateConfig) -> ArmVcpuResult<Self> {
        let mut ctx = TrapFrame::default();
        ctx.set_argument(config.dtb_addr);

        let mut machine = ax_cpu::virtualization::Vcpu::default();
        machine.context = ctx;
        Ok(Self {
            machine,
            timer: ArmVcpuTimer::unconfigured(),
            mpidr: config.mpidr_el1,
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
        let pa_bits = if config.mode == 0 {
            pa_bits()
        } else {
            config.mode
        };
        let paging = ax_cpu::virtualization::Stage2Config::new(
            config.root_paddr.into(),
            config.levels,
            config.gpa_bits,
            pa_bits,
        )
        .map_err(|_| ArmVcpuError::InvalidInput)?;
        self.machine.system.vttbr_el2 = paging.table_base(0);
        self.machine.system.vtcr_el2 = paging.control();
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
        self.machine.timer = self.timer.prepare_machine()?;
        // SAFETY: AxVM owns the pinned EL2 backend and retains IRQ exclusion,
        // the CPU-owned vector, guest memory, and the immutable GIC interface.
        let exit = unsafe { ax_cpu::virtualization::enter_guest(&mut self.machine) };
        self.timer.finish_machine(self.machine.timer);
        self.vmexit_handler(exit)
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
        self.machine.context.set_gpr(idx, val);
    }

    /// Sets the guest return value.
    pub fn set_return_value(&mut self, val: usize) {
        // Return value is stored in x0.
        self.machine.context.set_argument(val);
    }
}

// Private function
impl ArmVcpu {
    fn init_hv(&mut self, config: ArmVcpuSetupConfig) {
        self.machine.context.spsr = (SPSR_EL1::M::EL1h
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
        self.machine.set_host_irq_interface(config.host_irq());

        self.machine.system.sctlr_el1 = 0x30C50830;
        self.machine.system.pmcr_el0 = 0;

        if self.machine.system.vtcr_el2 == 0 {
            let pa_bits = pa_bits();
            let levels = max_gpt_level(pa_bits);
            let gpa_bits = if levels == 3 { 39 } else { 48 };
            self.machine.system.vtcr_el2 =
                ax_cpu::virtualization::Stage2Config::new(0usize.into(), levels, gpa_bits, pa_bits)
                    .expect("supported host stage-two geometry")
                    .control();
        }

        let hcr_el2 = HCR_EL2::VM::Enable
            + HCR_EL2::TSC::EnableTrapEl1SmcToEl2
            + HCR_EL2::TWI::SET
            + HCR_EL2::RW::EL1IsAarch64
            + HCR_EL2::IMO::EnableVirtualIRQ
            + HCR_EL2::FMO::EnableVirtualFIQ;

        self.machine.system.hcr_el2 = hcr_el2.into();

        // Set VMPIDR_EL2, which provides the value of the Virtualization Multiprocessor ID.
        // This is the value returned by Non-secure EL1 reads of MPIDR.
        let mut vmpidr = 1 << 31;
        // Note: mind CPU cluster here.
        vmpidr |= self.mpidr;
        self.machine.system.vmpidr_el2 = vmpidr;
    }

    /// Set exception return pc
    fn set_elr(&mut self, elr: usize) {
        self.machine.context.set_exception_pc(elr);
    }

    /// Get general purpose register
    #[allow(unused)]
    fn get_gpr(&self, idx: usize) {
        self.machine.context.gpr(idx);
    }
}

/// Private functions related to vcpu runtime control flow.
impl ArmVcpu {
    /// Handle VM-Exits.
    ///
    /// Parameters:
    /// - `exit_reason`: The reason why the VM-Exit happened in [`TrapKind`].
    ///
    /// Returns:
    /// - [`ArmVmExit`]: a wrappered VM-Exit reason needed to be handled by the hypervisor.
    ///
    /// This function may panic for unhandled exceptions.
    fn vmexit_handler(&mut self, exit: ax_cpu::virtualization::Exit) -> ArmVcpuResult<ArmVmExit> {
        trace!(
            "ArmVcpu vmexit_handler() esr:{:#x} ctx:{:#x?}",
            exception_class_value(&exit),
            self.machine.context
        );

        let result = match exit.kind {
            TrapKind::Synchronous => {
                handle_exception_sync(&mut self.machine.context, &self.machine.system, &exit)
            }
            TrapKind::Irq => {
                let raw_ack = exit.irq_ack;
                Ok(ArmVmExit::ExternalInterrupt {
                    token: (raw_ack != u32::MAX)
                        .then(|| crate::arch::aarch64::gic::finish_pending_host_irq(raw_ack))
                        .flatten(),
                })
            }
            _ => panic!("Unhandled exception {:?}", exit.kind),
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
                    .guest_counter(ArmTimerKind::Physical, ax_cpu::timer::physical_counter())?;
                self.set_gpr(reg, counter as usize);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTP_TVAL_EL0, false) => {
                let value = self
                    .timer
                    .read_tval(ArmTimerKind::Physical, ax_cpu::timer::physical_counter())?;
                self.set_gpr(reg, value as usize);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTP_CTL_EL0, false) => {
                let value = self
                    .timer
                    .read_control(ArmTimerKind::Physical, ax_cpu::timer::physical_counter())?;
                self.set_gpr(reg, value as usize);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTP_CVAL_EL0, false) => {
                let value = self.timer.read_compare(ArmTimerKind::Physical)?;
                self.set_gpr(reg, value as usize);
                Ok(Some(ArmVmExit::Nothing))
            }
            (SYSREG_CNTP_TVAL_EL0, true) => {
                self.timer.write_tval(
                    ArmTimerKind::Physical,
                    ax_cpu::timer::physical_counter(),
                    value as u32,
                )?;
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
            (SYSREG_CNTFRQ_EL0 | SYSREG_CNTPCT_EL0, true) => {
                Err(crate::arch::aarch64::policy::ArmVcpuError::InvalidInput)
            }
            (SYSREG_ICC_SGI1R_EL1, true) => {
                debug!("guest ICC_SGI1R_EL1 write: {value:#x}");
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
                register: crate::arch::aarch64::policy::ArmGicCpuInterfaceRegister::Control,
                destination: reg,
            })),
            (SYSREG_ICC_CTLR_EL1, true) => Ok(Some(ArmVmExit::GicCpuInterfaceWrite {
                register: crate::arch::aarch64::policy::ArmGicCpuInterfaceRegister::Control,
                value,
            })),
            (SYSREG_ICC_PMR_EL1, false) => Ok(Some(ArmVmExit::GicCpuInterfaceRead {
                register: crate::arch::aarch64::policy::ArmGicCpuInterfaceRegister::PriorityMask,
                destination: reg,
            })),
            (SYSREG_ICC_PMR_EL1, true) => Ok(Some(ArmVmExit::GicCpuInterfaceWrite {
                register: crate::arch::aarch64::policy::ArmGicCpuInterfaceRegister::PriorityMask,
                value,
            })),
            (SYSREG_ICC_RPR_EL1, false) => Ok(Some(ArmVmExit::GicCpuInterfaceRead {
                register: crate::arch::aarch64::policy::ArmGicCpuInterfaceRegister::RunningPriority,
                destination: reg,
            })),
            (SYSREG_ICC_RPR_EL1, true) => Ok(Some(ArmVmExit::GicCpuInterfaceWrite {
                register: crate::arch::aarch64::policy::ArmGicCpuInterfaceRegister::RunningPriority,
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

pub(crate) fn pa_bits() -> usize {
    ax_cpu::capability::physical_address_bits()
        .unwrap_or(32)
        .min(48)
}

pub(crate) fn max_gpt_level(pa_bits: usize) -> usize {
    match pa_bits {
        44.. => 4,
        _ => 3,
    }
}
