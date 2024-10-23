use aarch64_cpu::registers::{CNTHCTL_EL2, HCR_EL2, SPSR_EL1, SP_EL0, VTCR_EL2};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use axerrno::AxResult;
use axvcpu::AxVCpuExitReason;

use crate::context_frame::GuestSystemRegisters;
use crate::exception::{handle_exception_irq, handle_exception_sync, TrapKind};
use crate::exception_utils::exception_class_value;
use crate::TrapFrame;

core::arch::global_asm!(include_str!("entry.S"));

#[percpu::def_percpu]
static HOST_SP_EL0: u64 = 0;

/// Save host's `SP_EL0` to the current percpu region.
unsafe fn save_host_sp_el0() {
    HOST_SP_EL0.write_current_raw(SP_EL0.get())
}

/// Restore host's `SP_EL0` from the current percpu region.
unsafe fn restore_host_sp_el0() {
    SP_EL0.set(HOST_SP_EL0.read_current_raw());
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
#[derive(Clone, Debug)]
pub struct Aarch64VCpu {
    // DO NOT modify `guest_regs` and `host_stack_top` and their order unless you do know what you are doing!
    // DO NOT add anything before or between them unless you do know what you are doing!
    ctx: TrapFrame,
    host_stack_top: u64,
    guest_system_regs: GuestSystemRegisters,
    /// The MPIDR_EL1 value for the vCPU.
    mpidr: u64,
}

/// Configuration for creating a new `Aarch64VCpu`
#[derive(Clone, Debug, Default)]
pub struct Aarch64VCpuCreateConfig {
    /// The MPIDR_EL1 value for the new vCPU,
    /// which is used to identify the CPU in a multiprocessor system.
    /// Note: mind CPU cluster.
    pub mpidr_el1: u64,
}

impl axvcpu::AxArchVCpu for Aarch64VCpu {
    type CreateConfig = Aarch64VCpuCreateConfig;

    type SetupConfig = ();

    fn new(config: Self::CreateConfig) -> AxResult<Self> {
        Ok(Self {
            ctx: TrapFrame::default(),
            host_stack_top: 0,
            guest_system_regs: GuestSystemRegisters::default(),
            mpidr: config.mpidr_el1,
        })
    }

    fn setup(&mut self, _config: Self::SetupConfig) -> AxResult {
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
}

// Private function
impl Aarch64VCpu {
    #[inline(never)]
    unsafe fn run_guest(&mut self) -> usize {
        // Save function call context.
        core::arch::asm!(
            save_regs_to_stack!(),  // Save host context.
            "mov x9, sp",
            "mov x10, x11",
            "str x9, [x10]",    // Save current host stack top in the `Aarch64VCpu` struct.
            "mov x0, x11",
            "b context_vm_entry",
            // in(reg) here is dangerous, because the compiler may use the register we want to use, creating a conflict.
            in("x11") &self.host_stack_top as *const _ as usize,
            options(nostack)
        );

        // the dummy return value, the real return value is in x0 when `vmexit_trampoline` returns
        0
    }

    unsafe fn restore_vm_system_regs(&mut self) {
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

    fn vmexit_handler(&mut self, exit_reason: TrapKind) -> AxResult<AxVCpuExitReason> {
        trace!(
            "Aarch64VCpu vmexit_handler() esr:{:#x} ctx:{:#x?}",
            exception_class_value(),
            self.ctx
        );

        unsafe {
            // Store guest system regs
            self.guest_system_regs.store();
            // Restore host SP_EL0.
            // This has to be done after guest's SP_EL0 is stored by `ext_regs_store`.
            restore_host_sp_el0();
        }

        match exit_reason {
            TrapKind::Synchronous => handle_exception_sync(&mut self.ctx),
            TrapKind::Irq => handle_exception_irq(&mut self.ctx),
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
        self.guest_system_regs.cntvoff_el2 = 0;
        self.guest_system_regs.cntkctl_el1 = 0;

        self.guest_system_regs.sctlr_el1 = 0x30C50830;
        self.guest_system_regs.pmcr_el0 = 0;
        self.guest_system_regs.vtcr_el2 = (VTCR_EL2::PS::PA_40B_1TB
            + VTCR_EL2::TG0::Granule4KB
            + VTCR_EL2::SH0::Inner
            + VTCR_EL2::ORGN0::NormalWBRAWA
            + VTCR_EL2::IRGN0::NormalWBRAWA
            + VTCR_EL2::SL0.val(0b01)
            + VTCR_EL2::T0SZ.val(64 - 39))
        .into();
        self.guest_system_regs.hcr_el2 = (HCR_EL2::VM::Enable + HCR_EL2::RW::EL1IsAarch64).into();
        // self.system_regs.hcr_el2 |= 1<<27;
        // + HCR_EL2::IMO::EnableVirtualIRQ).into();
        // trap el1 smc to el2
        // self.system_regs.hcr_el2 |= HCR_TSC_TRAP as u64;

        // Set VMPIDR_EL2, which provides the value of the Virtualization Multiprocessor ID.
        // This is the value returned by Non-secure EL1 reads of MPIDR.
        let mut vmpidr = 1 << 31;
        // Note: mind CPU cluster here.
        vmpidr |= self.mpidr;
        self.guest_system_regs.vmpidr_el2 = vmpidr as u64;
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
