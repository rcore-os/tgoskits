use core::arch::global_asm;
use core::marker::PhantomData;
use spin::Mutex;

use axvcpu::AxArchVCpuExitReason;
// use cortex_a::registers::*;
use aarch64_cpu::registers::*;
use tock_registers::interfaces::*;

use super::context_frame::VmContext;
use super::exception_utils::*;
use super::sync::{data_abort_handler, hvc_handler};
use super::ContextFrame;
use super::{do_register_lower_aarch64_irq_handler, do_register_lower_aarch64_synchronous_handler};
use axerrno::{AxError, AxResult};

use crate::AxVMHal;
use axaddrspace::{GuestPhysAddr, HostPhysAddr};

global_asm!(include_str!("entry.S"));

// TSC, bit [19]
const HCR_TSC_TRAP: usize = 1 << 19;

/// (v)CPU register state that must be saved or restored when entering/exiting a VM or switching
/// between VMs.
#[repr(C)]
#[derive(Clone, Debug, Copy, Default)]
pub struct VmCpuRegisters {
    /// guest trap context
    pub trap_context_regs: ContextFrame,
    /// virtual machine system regs setting
    pub vm_system_regs: VmContext,
}

impl VmCpuRegisters {
    /// create a default VmCpuRegisters
    pub fn default() -> VmCpuRegisters {
        VmCpuRegisters {
            trap_context_regs: ContextFrame::default(),
            vm_system_regs: VmContext::default(),
        }
    }
}

/// A virtual CPU within a guest
#[derive(Clone, Debug)]
pub struct VCpu<H: AxVMHal> {
    /// Vcpu context
    ctx: ContextFrame,
    host_stack_top: u64,
    system_regs: VmContext,
    vcpu_id: usize,

    marker: PhantomData<H>,
}

extern "C" {
    fn context_vm_entry(ctx: usize);
}

pub type AxArchVCpuConfig = VmCpuRegisters;

impl<H: AxVMHal> axvcpu::AxArchVCpu for VCpu<H> {
    type CreateConfig = ();

    type SetupConfig = ();

    fn new(_config: Self::CreateConfig) -> AxResult<Self> {
        Ok(Self {
            ctx: ContextFrame::default(),
            host_stack_top: 0,
            system_regs: VmContext::default(),
            vcpu_id: 0, // need to pass a parameter!!!!
            marker: PhantomData,
        })
    }

    fn setup(&mut self, _config: Self::SetupConfig) -> AxResult {
        do_register_lower_aarch64_synchronous_handler()?;
        do_register_lower_aarch64_irq_handler()?;
        self.init_hv();
        Ok(())
    }

    fn set_entry(&mut self, entry: usize) -> AxResult {
        debug!("set vcpu entry:{:#x}", entry);
        self.set_elr(entry);
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        debug!("set vcpu ept root:{:#x}", ept_root);
        self.system_regs.vttbr_el2 = ept_root.as_usize() as u64;
        Ok(())
    }

    fn run(&mut self) -> AxResult<AxArchVCpuExitReason> {
        self.restore_vm_system_regs();
        self.run_guest();
        self.vmexit_handler()
    }

    fn bind(&mut self) -> AxResult {
        Ok(())
    }

    fn unbind(&mut self) -> AxResult {
        Ok(())
    }
}

// Private function
impl<H: AxVMHal> VCpu<H> {
    #[inline(never)]
    fn run_guest(&mut self) {
        unsafe {
            core::arch::asm!(
                save_regs_to_stack!(),  // save host context
                "mov x9, sp",
                "mov x10, {0}",
                "str x9, [x10]",    // save host stack top in the vcpu struct
                "mov x0, {0}",
                "b context_vm_entry",
                in(reg) &self.host_stack_top as *const _ as usize,
                options(nostack)
            );
            // context_vm_entry(&self.host_stack_top as *const _ as usize);
        }
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

    fn vmexit_handler(&mut self) -> AxResult<AxArchVCpuExitReason> {
        debug!(
            "enter lower_aarch64_synchronous esr:{:#x} ctx:{:#x?}",
            exception_class_value(),
            self.ctx
        );
        // save system regs
        self.system_regs.ext_regs_store();

        let ctx = &mut self.ctx;
        match exception_class() {
            Some(ESR_EL2::EC::Value::DataAbortLowerEL) => return data_abort_handler(ctx),
            Some(ESR_EL2::EC::Value::HVC64) => return hvc_handler(ctx),
            _ => {
                panic!(
                    "handler not presents for EC_{} @ipa 0x{:x}, @pc 0x{:x}, @esr 0x{:x}, @sctlr_el1 0x{:x}, @vttbr_el2 0x{:x}, @vtcr_el2: {:#x} hcr: {:#x} ctx:{}",
                    exception_class_value(),
                    exception_fault_addr(),
                    (*ctx).exception_pc(),
                    exception_esr(),
                    SCTLR_EL1.get() as usize,
                    VTTBR_EL2.get() as usize,
                    VTCR_EL2.get() as usize,
                    HCR_EL2.get() as usize,
                    ctx
                );
            }
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
    fn get_gpr(&mut self, idx: usize) {
        self.ctx.gpr(idx);
    }

    /// Set general purpose register
    fn set_gpr(&mut self, idx: usize, val: usize) {
        self.ctx.set_gpr(idx, val);
    }
}

#[naked]
pub unsafe extern "C" fn vmexit_aarch64_handler() {
    // save guest context
    core::arch::asm!(
        "add sp, sp, 34 * 8", // skip the exception frame
        "mov x9, sp",
        "ldr x10, [x9]",
        "mov sp, x10",              // move sp to the host stack top value
        restore_regs_from_stack!(), // restore host context
        "ret",
        options(noreturn),
    )
}
