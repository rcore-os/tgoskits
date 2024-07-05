// Copyright (c) 2023 Beihang University, Huawei Technologies Co.,Ltd. All rights reserved.
// Rust-Shyper is licensed under Mulan PSL v2.
// You can use this software according to the terms and conditions of the Mulan PSL v2.
// You may obtain a copy of Mulan PSL v2 at:
//          http://license.coscl.org.cn/MulanPSL2
// THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY KIND,
// EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO NON-INFRINGEMENT,
// MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
// See the Mulan PSL v2 for more details.

use alloc::vec::Vec;
use core::mem::size_of;
use core::arch::global_asm;
use spin::Mutex;
use core::marker::PhantomData;

// type ContextFrame = crate::arch::contextFrame::Aarch64ContextFrame;
use cortex_a::registers::*;
use tock_registers::interfaces::*;
 
use super::ContextFrame;
use super::VmContext;
use crate::AxvmHal;

core::arch::global_asm!(include_str!("entry.S"));
// use crate::arch::hvc::run_guest_by_trap2el2;

// TSC, bit [19]
const HCR_TSC_TRAP: usize = 1 << 19;

/// Vcpu State
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VcpuState {
    /// Invalid
    Inv = 0,
    /// Runnable
    Runnable = 1,
    /// Running
    Running = 2,
    /// Blocked
    Blocked = 3,
}


/// (v)CPU register state that must be saved or restored when entering/exiting a VM or switching
/// between VMs.
#[repr(C)]
#[derive(Clone, Debug)]
pub struct VmCpuRegisters {
    /// guest trap context
    pub guest_trap_context_regs: ContextFrame,
    /// virtual machine system regs setting
    pub vm_system_regs: VmContext,
}

impl VmCpuRegisters {
    /// create a default VmCpuRegisters
    pub fn default() -> VmCpuRegisters {
        VmCpuRegisters {
            guest_trap_context_regs: ContextFrame::default(),
            vm_system_regs: VmContext::default(),
        }
    }
}

/// A virtual CPU within a guest
#[derive(Clone, Debug)]
pub struct VCpu<H:AxvmHal> {
    /// Vcpu id
    pub vcpu_id: usize,
    /// vm id
    pub vm_id: usize,
    /// pcpu id
    pub pcpu_id: usize,
    /// Vcpu context
    pub regs: VmCpuRegisters,
    /// Vcpu state
    pub state: VcpuState,

    marker: PhantomData<H>,
}

extern "C" {
    fn context_vm_entry(ctx: usize) -> !;
}

impl <H:AxvmHal> VCpu<H> {
    /// Create a new vCPU
    pub fn new(vm_id:usize, id: usize, pcpu_id: usize) -> Self {
        Self {
            vcpu_id: id,
            vm_id: vm_id,
            pcpu_id: pcpu_id,
            regs: VmCpuRegisters::default(),
            state: VcpuState::Inv,
            marker: PhantomData,
        }
    }

    /// Init Vcpu registers
    pub fn init(&mut self, kernel_entry_point: usize, device_tree_ipa: usize) {
        self.vcpu_arch_init(kernel_entry_point, device_tree_ipa);
        self.init_vm_context();
    }

    /// Get vcpu id
    pub fn vcpu_id(&self) -> usize {
        self.vcpu_id
    }

    /// Run this vcpu
    pub fn run(&self, vttbr_token: usize) {
        init_hv(vttbr_token, self.vcpu_ctx_addr());
        unsafe {
            context_vm_entry(self.vcpu_trap_ctx_addr(true));
        }
        // loop {  // because of elr_el2, it will not return to this?
            // _ = run_guest_by_trap2el2(vttbr_token, self.vcpu_ctx_addr());
        // }
    }
    
    /// Get vcpu whole context address
    pub fn vcpu_ctx_addr(&self) -> usize {
        &(self.regs) as *const _ as usize
    }
    
    /// Get vcpu trap context for guest or arceos
    pub fn vcpu_trap_ctx_addr(&self, if_guest: bool) -> usize {
        if if_guest {
            &(self.regs.guest_trap_context_regs) as *const _ as usize
        }else {
            &(self.regs.save_for_os_context_regs) as *const _ as usize
        }
    }

    /// Set exception return pc
    pub fn set_elr(&mut self, elr: usize) {
        self.regs.guest_trap_context_regs.set_exception_pc(elr);
    }

    /// Get general purpose register
    pub fn get_gpr(&mut self, idx: usize) {
        self.regs.guest_trap_context_regs.gpr(idx);
    }

    /// Set general purpose register
    pub fn set_gpr(&mut self, idx: usize, val: usize) {
        self.regs.guest_trap_context_regs.set_gpr(idx, val);
    }

    /// Init guest context. Also set some el2 register value.
    fn init_vm_context(&mut self) {
        CNTHCTL_EL2.modify(CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET);
        self.regs.vm_system_regs.cntvoff_el2 = 0;
        self.regs.vm_system_regs.cntkctl_el1 = 0;

        self.regs.vm_system_regs.sctlr_el1 = 0x30C50830;
        self.regs.vm_system_regs.pmcr_el0 = 0;
        // self.regs.vm_system_regs.vtcr_el2 = 0x8001355c;
        self.regs.vm_system_regs.vtcr_el2 = (VTCR_EL2::PS::PA_40B_1TB   // 40bit PA, 1TB
                                          + VTCR_EL2::TG0::Granule4KB
                                          + VTCR_EL2::SH0::Inner
                                          + VTCR_EL2::ORGN0::NormalWBRAWA
                                          + VTCR_EL2::IRGN0::NormalWBRAWA
                                          + VTCR_EL2::SL0.val(0b01)
                                          + VTCR_EL2::T0SZ.val(64 - 40)).into();
        self.regs.vm_system_regs.hcr_el2 = (HCR_EL2::VM::Enable
                                         + HCR_EL2::RW::EL1IsAarch64 // ).into();
                                         + HCR_EL2::IMO::EnableVirtualIRQ).into();
        // trap el1 smc to el2
        self.regs.vm_system_regs.hcr_el2 |= HCR_TSC_TRAP as u64;

        let mut vmpidr = 0;
        vmpidr |= 1 << 31;
        vmpidr |= self.vcpu_id;
        self.regs.vm_system_regs.vmpidr_el2 = vmpidr as u64;
        // self.gic_ctx_reset(); // because of passthrough gic, do not need gic context anymore?
    }

    /// Init guest contextFrame
    fn vcpu_arch_init(&mut self, kernel_entry_point: usize, device_tree_ipa: usize) {
        self.set_gpr(0, device_tree_ipa);
        self.set_elr(kernel_entry_point);
        self.regs.guest_trap_context_regs.spsr =( SPSR_EL1::M::EL1h + 
                                            SPSR_EL1::I::Masked + 
                                            SPSR_EL1::F::Masked + 
                                            SPSR_EL1::A::Masked + 
                                            SPSR_EL1::D::Masked )
                                            .value;
    }

}

#[inline(never)]
#[no_mangle]
/// hvc handler for initial hv
/// x0: root_paddr, x1: vm regs context addr
fn init_hv(root_paddr: usize, vm_ctx_addr: usize) {
    unsafe {
        core::arch::asm!("
            mov x3, xzr           // Trap nothing from EL1 to El2.
            msr cptr_el2, x3"
        );
    }
    let regs: &VmCpuRegisters = unsafe{core::mem::transmute(vm_ctx_addr)};
    // set vm system related register
    msr!(VTTBR_EL2, root_paddr);
    regs.vm_system_regs.ext_regs_restore();

    unsafe {
        cache_invalidate(0<<1);
        cache_invalidate(1<<1);
        core::arch::asm!("
            ic  iallu
            tlbi	alle2
            tlbi	alle1         // Flush tlb
            dsb	nsh
            isb"
        );
    }   
}

unsafe fn cache_invalidate(cache_level: usize) {
    core::arch::asm!(
        r#"
        msr csselr_el1, {0}
        mrs x4, ccsidr_el1 // read cache size id.
        and x0, x4, #0x7
        add x0, x0, #0x4 // x0 = cache line size.
        ldr x3, =0x7fff
        and x2, x3, x4, lsr #13 // x2 = cache set number – 1.
        ldr x3, =0x3ff
        and x3, x3, x4, lsr #3 // x3 = cache associativity number – 1.
        clz w4, w3 // x4 = way position in the cisw instruction.
        mov x5, #0 // x5 = way counter way_loop.
    // way_loop:
    1:
        mov x6, #0 // x6 = set counter set_loop.
    // set_loop:
    2:
        lsl x7, x5, x4
        orr x7, {0}, x7 // set way.
        lsl x8, x6, x0
        orr x7, x7, x8 // set set.
        dc csw, x7 // clean and invalidate cache line.
        add x6, x6, #1 // increment set counter.
        cmp x6, x2 // last set reached yet?
        ble 2b // if not, iterate set_loop,
        add x5, x5, #1 // else, next way.
        cmp x5, x3 // last way reached yet?
        ble 1b // if not, iterate way_loop
        "#,
        in(reg) cache_level,
        options(nostack)
    );
}
