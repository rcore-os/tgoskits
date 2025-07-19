use core::marker::PhantomData;

use aarch64_cpu::registers::*;
use axaddrspace::{GuestPhysAddr, HostPhysAddr, device::SysRegAddr};
use axerrno::AxResult;
use axvcpu::{AxArchVCpu, AxVCpuExitReason, AxVCpuHal};

use crate::TrapFrame;
use crate::context_frame::GuestSystemRegisters;
use crate::exception::{TrapKind, handle_exception_sync};
use crate::exception_utils::exception_class_value;

#[percpu::def_percpu]
static HOST_SP_EL0: u64 = 0;

/// Save host's `SP_EL0` to the current percpu region.
unsafe fn save_host_sp_el0() {
    unsafe { HOST_SP_EL0.write_current_raw(SP_EL0.get()) }
}

/// Restore host's `SP_EL0` from the current percpu region.
unsafe fn restore_host_sp_el0() {
    SP_EL0.set(unsafe { HOST_SP_EL0.read_current_raw() });
}

/// (v)CPU register state that must be saved or restored when entering/exiting a VM or switching
/// between VMs.
#[repr(C)]
#[derive(Clone, Debug, Copy, Default)]
pub struct VmCpuRegisters {
    /// guest trap context
    pub trap_context_regs: TrapFrame,
    /// virtual machine system regs setting
    pub vm_system_regs: GuestSystemRegisters,
}

/// A virtual CPU within a guest
#[repr(C)]
#[derive(Debug)]
pub struct Aarch64VCpu<H: AxVCpuHal> {
    // DO NOT modify `guest_regs` and `host_stack_top` and their order unless you do know what you are doing!
    // DO NOT add anything before or between them unless you do know what you are doing!
    ctx: TrapFrame,
    host_stack_top: u64,
    guest_system_regs: GuestSystemRegisters,
    /// The MPIDR_EL1 value for the vCPU.
    mpidr: u64,
    _phantom: PhantomData<H>,
}

/// Configuration for creating a new `Aarch64VCpu`
#[derive(Clone, Debug, Default)]
pub struct Aarch64VCpuCreateConfig {
    /// The MPIDR_EL1 value for the new vCPU,
    /// which is used to identify the CPU in a multiprocessor system.
    /// Note: mind CPU cluster.
    // FIXME: Handle its interaction with the virtual GIC.
    pub mpidr_el1: u64,
    /// The address of the device tree blob.
    pub dtb_addr: usize,
}

/// Configuration for setting up a new `Aarch64VCpu`
#[derive(Clone, Debug, Default)]
pub struct Aarch64VCpuSetupConfig {
    /// Should the hypervisor passthrough interrupts to the guest?
    pub passthrough_interrupt: bool,
    /// Should the hypervisor passthrough timers to the guest?
    pub passthrough_timer: bool,
}

impl<H: AxVCpuHal> axvcpu::AxArchVCpu for Aarch64VCpu<H> {
    type CreateConfig = Aarch64VCpuCreateConfig;

    type SetupConfig = Aarch64VCpuSetupConfig;

    fn new(_vm_id: usize, _vcpu_id: usize, config: Self::CreateConfig) -> AxResult<Self> {
        let mut ctx = TrapFrame::default();
        ctx.set_argument(config.dtb_addr);

        Ok(Self {
            ctx,
            host_stack_top: 0,
            guest_system_regs: GuestSystemRegisters::default(),
            mpidr: config.mpidr_el1,
            _phantom: PhantomData,
        })
    }

    fn setup(&mut self, config: Self::SetupConfig) -> AxResult {
        self.init_hv(config);
        Ok(())
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        debug!("set vcpu entry:{entry:?}");
        self.set_elr(entry.as_usize());
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        debug!("set vcpu ept root:{ept_root:#x}");
        self.guest_system_regs.vttbr_el2 = ept_root.as_usize() as u64;
        Ok(())
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        // Run guest.
        let exit_reson = unsafe {
            // Save host SP_EL0 to the ctx becase it's used as current task ptr.
            // This has to be done before vm system regs are restored.
            save_host_sp_el0();
            self.restore_vm_system_regs();
            self.run_guest()
        };

        let trap_kind = TrapKind::try_from(exit_reson as u8).expect("Invalid TrapKind");
        self.vmexit_handler(trap_kind)
    }

    fn bind(&mut self) -> AxResult {
        Ok(())
    }

    fn unbind(&mut self) -> AxResult {
        Ok(())
    }

    fn set_gpr(&mut self, idx: usize, val: usize) {
        self.ctx.set_gpr(idx, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> AxResult {
        axvisor_api::arch::hardware_inject_virtual_interrupt(vector as u8);
        Ok(())
    }

    fn set_return_value(&mut self, val: usize) {
        // Return value is stored in x0.
        self.ctx.set_argument(val);
    }
}

// Private function
impl<H: AxVCpuHal> Aarch64VCpu<H> {
    fn init_hv(&mut self, config: Aarch64VCpuSetupConfig) {
        self.ctx.spsr = (SPSR_EL1::M::EL1h
            + SPSR_EL1::I::Masked
            + SPSR_EL1::F::Masked
            + SPSR_EL1::A::Masked
            + SPSR_EL1::D::Masked)
            .value;
        self.init_vm_context(config);
    }

    /// Init guest context. Also set some el2 register value.
    fn init_vm_context(&mut self, config: Aarch64VCpuSetupConfig) {
        // CNTHCTL_EL2.modify(CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET);
        self.guest_system_regs.cntvoff_el2 = 0;
        self.guest_system_regs.cntkctl_el1 = 0;
        self.guest_system_regs.cnthctl_el2 = if config.passthrough_timer {
            (CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET).into()
        } else {
            (CNTHCTL_EL2::EL1PCEN::CLEAR + CNTHCTL_EL2::EL1PCTEN::CLEAR).into()
        };

        self.guest_system_regs.sctlr_el1 = 0x30C50830;
        self.guest_system_regs.pmcr_el0 = 0;

        // use 3 level ept paging
        // - 4KiB granule (TG0)
        // - 39-bit address space (T0_SZ)
        // - start at level 1 (SL0)
        // self.guest_system_regs.vtcr_el2 = (VTCR_EL2::PS::PA_40B_1TB
        //     + VTCR_EL2::TG0::Granule4KB
        //     + VTCR_EL2::SH0::Inner
        //     + VTCR_EL2::ORGN0::NormalWBRAWA
        //     + VTCR_EL2::IRGN0::NormalWBRAWA
        //     + VTCR_EL2::SL0.val(0b01)
        //     + VTCR_EL2::T0SZ.val(64 - 39))
        // .into();

        // use 4 level ept paging
        // - 4KiB granule (TG0)
        // - 48-bit address space (T0_SZ)
        // - start at level 0 (SL0)
        self.guest_system_regs.vtcr_el2 = (VTCR_EL2::PS::PA_48B_256TB
            + VTCR_EL2::TG0::Granule4KB
            + VTCR_EL2::SH0::Inner
            + VTCR_EL2::ORGN0::NormalWBRAWA
            + VTCR_EL2::IRGN0::NormalWBRAWA
            + VTCR_EL2::SL0.val(0b10) // 0b10 means start at level 0
            + VTCR_EL2::T0SZ.val(64 - 48))
        .into();

        let mut hcr_el2 = HCR_EL2::VM::Enable
            + HCR_EL2::RW::EL1IsAarch64
            + HCR_EL2::FMO::EnableVirtualFIQ
            + HCR_EL2::TSC::EnableTrapEl1SmcToEl2
            + HCR_EL2::RW::EL1IsAarch64;

        if !config.passthrough_interrupt {
            // Set HCR_EL2.IMO will trap IRQs to EL2 while enabling virtual IRQs.
            //
            // We must choose one of the two:
            // - Enable virtual IRQs and trap physical IRQs to EL2.
            // - Disable virtual IRQs and pass through physical IRQs to EL1.
            hcr_el2 += HCR_EL2::IMO::EnableVirtualIRQ;
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
impl<H: AxVCpuHal> Aarch64VCpu<H> {
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
            // Save current host stack top to `self.host_stack_top`.
            //
            // 'extern "C"' here specifies the aapcs64 calling convention, according to which
            // the first and only parameter, the pointer of self, should be in x0:
            "mov x9, sp",
            "add x0, x0, {host_stack_top_offset}",
            "str x9, [x0]",
            // Go to `context_vm_entry`.
            "b context_vm_entry",
            // Panic if the control flow comes back here, which should never happen.
            "b {run_guest_panic}",
            host_stack_top_offset = const core::mem::size_of::<TrapFrame>(),
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
    /// - [`AxVCpuExitReason`]: a wrappered VM-Exit reason needed to be handled by the hypervisor.
    ///
    /// This function may panic for unhandled exceptions.
    fn vmexit_handler(&mut self, exit_reason: TrapKind) -> AxResult<AxVCpuExitReason> {
        trace!(
            "Aarch64VCpu vmexit_handler() esr:{:#x} ctx:{:#x?}",
            exception_class_value(),
            self.ctx
        );

        unsafe {
            // Store guest system regs
            self.guest_system_regs.store();

            // Store guest `SP_EL0` into the `Aarch64VCpu` struct,
            // which will be restored when the guest is resumed in `exception_return_el2`.
            self.ctx.sp_el0 = self.guest_system_regs.sp_el0;

            // Restore host `SP_EL0`.
            // This has to be done after guest's SP_EL0 is stored by `ext_regs_store`.
            restore_host_sp_el0();
        }

        let result = match exit_reason {
            TrapKind::Synchronous => handle_exception_sync(&mut self.ctx),
            TrapKind::Irq => Ok(AxVCpuExitReason::ExternalInterrupt {
                vector: H::irq_fetch() as _,
            }),
            _ => panic!("Unhandled exception {:?}", exit_reason),
        };

        match result {
            Ok(AxVCpuExitReason::SysRegRead { addr, reg }) => {
                if let Some(exit_reason) =
                    self.builtin_sysreg_access_handler(addr, false, 0, reg)?
                {
                    return Ok(exit_reason);
                }

                result
            }
            Ok(AxVCpuExitReason::SysRegWrite { addr, value }) => {
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
        addr: SysRegAddr,
        write: bool,
        value: u64,
        reg: usize,
    ) -> AxResult<Option<AxVCpuExitReason>> {
        const SYSREG_ICC_SGI1R_EL1: SysRegAddr = SysRegAddr::new(0x3A_3016); // ICC_SGI1R_EL1

        match (addr, write) {
            (SYSREG_ICC_SGI1R_EL1, true) => {
                debug!("arm_vcpu ICC_SGI1R_EL1 write: {value:#x}");

                // TODO: support RangeSelector

                let intid = (value >> 24) & 0b1111;
                let irm = ((value >> 40) & 0b1) != 0;

                // IRM == 1 => send to all except self
                if irm {
                    debug!("arm_vcpu ICC_SGI1R_EL1 write: irm == 1, send to all except self");

                    return Ok(Some(AxVCpuExitReason::SendIPI {
                        target_cpu: 0,
                        target_cpu_aux: 0,
                        send_to_all: true,
                        send_to_self: false,
                        vector: intid,
                    }));
                }

                let aff3 = (value >> 48) & 0xff;
                let aff2 = (value >> 32) & 0xff;
                let aff1 = (value >> 16) & 0xff;
                let target_list = value & 0xffff;

                debug!(
                    "arm_vcpu ICC_SGI1R_EL1 write: aff3:{aff3:#x} aff2:{aff2:#x} aff1:{aff1:#x} intid:{intid:#x} target_list:{target_list:#x}"
                );

                Ok(Some(AxVCpuExitReason::SendIPI {
                    target_cpu: (aff3 << 24) | (aff2 << 16) | (aff1 << 8),
                    target_cpu_aux: target_list,
                    send_to_all: false,
                    send_to_self: false,
                    vector: intid,
                }))
            }
            (SYSREG_ICC_SGI1R_EL1, false) => {
                // ICC_SGI1R_EL1 is WO, we take it as RAZ.
                self.set_gpr(reg, 0);
                Ok(Some(AxVCpuExitReason::Nothing))
            }
            _ => {
                // If the system register access is not handled by the VCpu itself,
                // we return None to let the hypervisor handle it.
                Ok(None)
            }
        }
    }
}
