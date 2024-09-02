use aarch64_cpu::registers::{CNTHCTL_EL2, HCR_EL2, SPSR_EL1, VTCR_EL2};
use tock_registers::interfaces::ReadWriteable;

use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use axerrno::AxResult;
use axvcpu::AxVCpuExitReason;

use crate::context_frame::VmContext;
use crate::exception::{handle_exception_irq, handle_exception_sync, TrapKind};
use crate::exception_utils::exception_class_value;
use crate::TrapFrame;

core::arch::global_asm!(include_str!("entry.S"));

/// (v)CPU register state that must be saved or restored when entering/exiting a VM or switching
/// between VMs.
#[repr(C)]
#[derive(Clone, Debug, Copy, Default)]
pub struct VmCpuRegisters {
    /// guest trap context
    pub trap_context_regs: TrapFrame,
    /// virtual machine system regs setting
    pub vm_system_regs: VmContext,
}

impl VmCpuRegisters {
    /// create a default VmCpuRegisters
    pub fn default() -> VmCpuRegisters {
        VmCpuRegisters {
            trap_context_regs: TrapFrame::default(),
            vm_system_regs: VmContext::default(),
        }
    }
}

/// A virtual CPU within a guest
#[repr(C)]
#[derive(Clone, Debug)]
pub struct Aarch64VCpu {
    // DO NOT modify `guest_regs` and `host_stack_top` and their order unless you do know what you are doing!
    // DO NOT add anything before or between them unless you do know what you are doing!
    ctx: TrapFrame,
    host_stack_top: u64,
    system_regs: VmContext,
    vcpu_id: usize,
}

/// Indicates the parameter type used for creating a vCPU, currently using `VmCpuRegisters` directly.
pub type AxArchVCpuConfig = VmCpuRegisters;

impl axvcpu::AxArchVCpu for Aarch64VCpu {
    type CreateConfig = ();

    type SetupConfig = ();

    fn new(_config: Self::CreateConfig) -> AxResult<Self> {
        Ok(Self {
            ctx: TrapFrame::default(),
            host_stack_top: 0,
            system_regs: VmContext::default(),
            vcpu_id: 0, // need to pass a parameter!!!!
        })
    }

    fn setup(&mut self, _config: Self::SetupConfig) -> AxResult {
        // do_register_lower_aarch64_synchronous_handler()?;
        // do_register_lower_aarch64_irq_handler()?;
        self.init_hv();
        Ok(())
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        debug!("set vcpu entry:{:?}", entry);
        self.set_elr(entry.as_usize());
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        debug!("set vcpu ept root:{:#x}", ept_root);
        self.system_regs.vttbr_el2 = ept_root.as_usize() as u64;
        Ok(())
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        self.restore_vm_system_regs();
        let exit_reason = TrapKind::try_from(self.run_guest() as u8).expect("Invalid TrapKind");
        self.vmexit_handler(exit_reason)
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
}

// Private function
impl Aarch64VCpu {
    #[inline(never)]
    fn run_guest(&mut self) -> usize {
        let mut ret;
        unsafe {
            core::arch::asm!(
                save_regs_to_stack!(),  // Save host context.
                "mov x9, sp",
                "mov x10, {0}",
                "str x9, [x10]",    // Save current host stack top in the `Aarch64VCpu` struct.
                "mov x0, {0}",
                "b context_vm_entry",
                in(reg) &self.host_stack_top as *const _ as usize,
                out("x0") ret,
                options(nostack)
            );
        }
        ret
    }

    fn restore_vm_system_regs(&mut self) {
        unsafe {
            // load system regs
            core::arch::asm!(
                "
                mov x3, xzr           // Trap nothing from EL1 to El2.
                msr cptr_el2, x3"
            );
            self.system_regs.ext_regs_restore();
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

    fn vmexit_handler(&mut self, exit_reason: TrapKind) -> AxResult<AxVCpuExitReason> {
        trace!(
            "Aarch64VCpu vmexit_handler() esr:{:#x} ctx:{:#x?}",
            exception_class_value(),
            self.ctx
        );
        // restore system regs
        self.system_regs.ext_regs_store();

        let ctx = &mut self.ctx;
        match exit_reason {
            TrapKind::Synchronous => handle_exception_sync(ctx),
            TrapKind::Irq => handle_exception_irq(ctx),
            _ => panic!("Unhandled exception {:?}", exit_reason),
        }
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
        CNTHCTL_EL2.modify(CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET);
        self.system_regs.cntvoff_el2 = 0;
        self.system_regs.cntkctl_el1 = 0;

        self.system_regs.sctlr_el1 = 0x30C50830;
        self.system_regs.pmcr_el0 = 0;
        self.system_regs.vtcr_el2 = (VTCR_EL2::PS::PA_40B_1TB
            + VTCR_EL2::TG0::Granule4KB
            + VTCR_EL2::SH0::Inner
            + VTCR_EL2::ORGN0::NormalWBRAWA
            + VTCR_EL2::IRGN0::NormalWBRAWA
            + VTCR_EL2::SL0.val(0b01)
            + VTCR_EL2::T0SZ.val(64 - 39))
        .into();
        self.system_regs.hcr_el2 = (HCR_EL2::VM::Enable + HCR_EL2::RW::EL1IsAarch64).into();
        // self.system_regs.hcr_el2 |= 1<<27;
        // + HCR_EL2::IMO::EnableVirtualIRQ).into();
        // trap el1 smc to el2
        // self.system_regs.hcr_el2 |= HCR_TSC_TRAP as u64;

        let mut vmpidr = 0;
        vmpidr |= 1 << 31;
        vmpidr |= self.vcpu_id;
        self.system_regs.vmpidr_el2 = vmpidr as u64;
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
