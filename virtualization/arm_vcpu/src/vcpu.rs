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
    ArmAccessWidth, ArmDataAbort, ArmDataAccess, ArmDataAccessResult, ArmGuestPhysAddr, ArmHostOps,
    ArmLoadExtension, ArmNestedPagingConfig, ArmSysRegAddr, ArmVcpuError, ArmVcpuResult, ArmVmExit,
    TrapFrame,
    context_frame::GuestSystemRegisters,
    data_abort::access_mask,
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

/// Fixed EL2 setup policy for a new [`ArmVcpu`].
///
/// Physical interrupts and timers are always trapped. The embedding VMM may
/// back a virtual interrupt with a physical source, but it must do so through
/// the virtual CPU interface rather than bypassing vCPU state ownership.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArmVcpuSetupConfig;

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
    pub fn setup(&mut self, _config: ArmVcpuSetupConfig) -> ArmVcpuResult {
        self.init_hv();
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

        let result = decode_trap_kind(exit_reason).and_then(|kind| self.vmexit_handler(kind));

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

    /// Completes one successfully emulated data access and advances its guest PC.
    ///
    /// # Errors
    ///
    /// Returns [`ArmVcpuError::InvalidInput`] when `result` does not match the
    /// decoded access direction, and [`ArmVcpuError::BadState`] when the abort
    /// no longer refers to the current guest instruction.
    pub fn complete_data_abort(
        &mut self,
        abort: ArmDataAbort,
        result: ArmDataAccessResult,
    ) -> ArmVcpuResult {
        self.ensure_current_data_abort(&abort)?;
        match (abort.access(), result) {
            (
                Some(ArmDataAccess::Read {
                    width,
                    register,
                    register_width,
                    extension,
                }),
                ArmDataAccessResult::Read(value),
            ) => {
                let value = loaded_register_value(value, width, register_width, extension)?;
                self.ctx.set_gpr(register.index(), value as usize);
            }
            (Some(ArmDataAccess::Write { .. }), ArmDataAccessResult::Write) => {}
            _ => return Err(ArmVcpuError::InvalidInput),
        }
        self.advance_data_abort_pc(abort.instruction_size())
    }

    /// Injects a synchronous external data abort into guest EL1.
    ///
    /// # Errors
    ///
    /// Returns an error if the abort is stale, its exception origin is not a
    /// supported AArch64 EL0/EL1 mode, or the saved vector state is invalid.
    pub fn inject_external_data_abort(&mut self, abort: ArmDataAbort) -> ArmVcpuResult {
        self.ensure_current_data_abort(&abort)?;
        let fault_address = abort
            .fault_virtual_address()
            .map(crate::ArmGuestVirtAddr::as_u64);
        self.guest_system_regs.inject_external_data_abort(
            &mut self.ctx,
            fault_address,
            abort.instruction_size(),
        )
    }
}

// Private function
impl<H: ArmHostOps> ArmVcpu<H> {
    fn ensure_current_data_abort(&self, abort: &ArmDataAbort) -> ArmVcpuResult {
        if self.ctx.exception_pc() as u64 != abort.instruction_address() {
            return Err(ArmVcpuError::BadState);
        }
        Ok(())
    }

    fn advance_data_abort_pc(&mut self, instruction_size: usize) -> ArmVcpuResult {
        let next_pc = self
            .ctx
            .exception_pc()
            .checked_add(instruction_size)
            .ok_or(ArmVcpuError::BadState)?;
        self.ctx.set_exception_pc(next_pc);
        Ok(())
    }

    fn init_hv(&mut self) {
        self.ctx.spsr = (SPSR_EL1::M::EL1h
            + SPSR_EL1::I::Masked
            + SPSR_EL1::F::Masked
            + SPSR_EL1::A::Masked
            + SPSR_EL1::D::Masked)
            .value;
        self.init_vm_context();
    }

    /// Init guest context. Also set some el2 register value.
    fn init_vm_context(&mut self) {
        // CNTHCTL_EL2.modify(CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET);
        // Set CNTVOFF_EL2 to the current physical counter so the guest's
        // virtual counter (CNTVCT_EL0 = CNTPCT_EL0 - CNTVOFF_EL2) starts near zero.
        let cntpct: u64;
        unsafe { core::arch::asm!("mrs {0}, CNTPCT_EL0", out(reg) cntpct) };
        self.guest_system_regs.cntvoff_el2 = cntpct;
        self.guest_system_regs.cntkctl_el1 = 0;
        self.guest_system_regs.cnthctl_el2 =
            (CNTHCTL_EL2::EL1PCEN::CLEAR + CNTHCTL_EL2::EL1PCTEN::CLEAR).into();

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

fn loaded_register_value(
    value: u64,
    access_width: ArmAccessWidth,
    register_width: ArmAccessWidth,
    extension: ArmLoadExtension,
) -> ArmVcpuResult<u64> {
    if !matches!(
        register_width,
        ArmAccessWidth::Dword | ArmAccessWidth::Qword
    ) {
        return Err(ArmVcpuError::InvalidInput);
    }
    let narrowed = value & access_mask(access_width);
    let extended = match extension {
        ArmLoadExtension::Zero => narrowed,
        ArmLoadExtension::Sign => sign_extend(narrowed, access_width),
    };
    Ok(extended & access_mask(register_width))
}

fn sign_extend(value: u64, width: ArmAccessWidth) -> u64 {
    let bits = width.size() * 8;
    if bits == u64::BITS as usize {
        value
    } else {
        let shift = u64::BITS as usize - bits;
        ((value << shift) as i64 >> shift) as u64
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
    /// Unsupported lower-EL asynchronous exceptions are returned as typed errors.
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
            kind => asynchronous_vmexit(kind),
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

fn decode_trap_kind(exit_reason: usize) -> ArmVcpuResult<TrapKind> {
    let encoding = u8::try_from(exit_reason).map_err(|_| ArmVcpuError::BadState)?;
    TrapKind::try_from(encoding).map_err(|_| ArmVcpuError::BadState)
}

fn asynchronous_vmexit(kind: TrapKind) -> ArmVcpuResult<ArmVmExit> {
    match kind {
        TrapKind::Irq => Ok(ArmVmExit::ExternalInterrupt),
        TrapKind::Fiq | TrapKind::SError => Err(ArmVcpuError::Unsupported),
        TrapKind::Synchronous => Err(ArmVcpuError::BadState),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArmDataAbortSyndrome, data_abort::decode_data_access};

    struct DummyHost;

    impl ArmHostOps for DummyHost {
        fn handle_current_host_irq() {}
    }

    #[test]
    fn invalid_trap_encoding_returns_a_typed_error() {
        assert!(matches!(decode_trap_kind(4), Err(ArmVcpuError::BadState)));
    }

    #[test]
    fn unexpected_asynchronous_exceptions_do_not_panic() {
        assert!(matches!(
            asynchronous_vmexit(TrapKind::Fiq),
            Err(ArmVcpuError::Unsupported)
        ));
        assert!(matches!(
            asynchronous_vmexit(TrapKind::SError),
            Err(ArmVcpuError::Unsupported)
        ));
    }

    #[test]
    fn completed_data_abort_advances_pc_only_after_successful_emulation() {
        let mut vcpu = ArmVcpu::<DummyHost>::new(0, 0, ArmVcpuCreateConfig::default()).unwrap();
        vcpu.ctx.set_exception_pc(0x1000);
        let abort = read_abort(&vcpu.ctx, 0x1000);

        vcpu.complete_data_abort(abort, ArmDataAccessResult::Read(0xffff_ff80))
            .unwrap();

        assert_eq!(vcpu.ctx.exception_pc(), 0x1004);
        assert_eq!(vcpu.ctx.gpr(3), 0xffff_ff80);
    }

    #[test]
    fn stale_data_abort_cannot_advance_a_different_instruction() {
        let mut vcpu = ArmVcpu::<DummyHost>::new(0, 0, ArmVcpuCreateConfig::default()).unwrap();
        vcpu.ctx.set_exception_pc(0x2000);
        let abort = read_abort(&vcpu.ctx, 0x1000);

        assert_eq!(
            vcpu.complete_data_abort(abort, ArmDataAccessResult::Read(0)),
            Err(ArmVcpuError::BadState)
        );
        assert_eq!(vcpu.ctx.exception_pc(), 0x2000);
    }

    #[test]
    fn external_abort_does_not_substitute_ipa_for_invalid_far() {
        const ESR_FNV_BIT: u32 = 1 << 10;

        let mut vcpu = ArmVcpu::<DummyHost>::new(0, 0, ArmVcpuCreateConfig::default()).unwrap();
        vcpu.ctx.set_exception_pc(0x3000);
        let syndrome = ArmDataAbortSyndrome::from_esr((0x24 << 26) | (1 << 25) | ESR_FNV_BIT | 0x7);
        let abort = ArmDataAbort::new(
            Some(crate::ArmFaultIpa::page(ArmGuestPhysAddr::from_usize(
                0x1030_00,
            ))),
            None,
            0x3000,
            syndrome,
            None,
        );

        vcpu.inject_external_data_abort(abort).unwrap();

        let (esr_el1, far_el1) = vcpu.guest_system_regs.injected_fault_state();
        assert_eq!(far_el1, 0);
        assert_ne!(esr_el1 & ESR_FNV_BIT, 0);
    }

    fn read_abort(frame: &TrapFrame, instruction_address: u64) -> ArmDataAbort {
        let esr = (0x24 << 26) | (1 << 25) | (1 << 24) | (2 << 22) | (3 << 16) | (1 << 15) | 7;
        let syndrome = ArmDataAbortSyndrome::from_esr(esr);
        let access =
            decode_data_access(syndrome, |register| frame.gpr(register.index()) as u64).unwrap();
        ArmDataAbort::new(
            Some(crate::ArmFaultIpa::exact(ArmGuestPhysAddr::from_usize(
                0x4000,
            ))),
            Some(crate::ArmGuestVirtAddr::from_u64(0xffff_0000_4000)),
            instruction_address,
            syndrome,
            access,
        )
    }
}
