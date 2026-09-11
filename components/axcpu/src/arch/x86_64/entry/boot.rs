// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// Early IDT gate construction migrated from someboot (周睿).
//! Early exception gates sharing the native register-save assembly.

use core::{
    hint::spin_loop,
    sync::atomic::{AtomicU8, Ordering},
};

use x86::{
    bits64::segmentation::Descriptor64,
    dtables::{self, DescriptorTablePointer},
    segmentation::{BuildDescriptor, DescriptorBuilder, GateDescriptorBuilder, cs},
};

use super::super::context::TrapFrame;

#[repr(C, align(16))]
/// Owned early exception descriptors for the current code mapping and selector.
/// Keep the value at a stable address while any CPU has it installed.
pub struct BootVectorTable([Descriptor64; 256]);
static mut IDT: BootVectorTable = BootVectorTable([Descriptor64::NULL; 256]);
static STATE: AtomicU8 = AtomicU8::new(0);

core::arch::global_asm!(include_str!("gpr.S"), include_str!("boot.S"),
    ".purgem PUSH_GENERAL_REGS", ".purgem POP_GENERAL_REGS", dispatch = sym dispatch);

unsafe extern "C" {
    fn __ax_cpu_x86_boot_0();
    fn __ax_cpu_x86_boot_1();
    fn __ax_cpu_x86_boot_2();
    fn __ax_cpu_x86_boot_3();
    fn __ax_cpu_x86_boot_4();
    fn __ax_cpu_x86_boot_5();
    fn __ax_cpu_x86_boot_6();
    fn __ax_cpu_x86_boot_7();
    fn __ax_cpu_x86_boot_8();
    fn __ax_cpu_x86_boot_9();
    fn __ax_cpu_x86_boot_10();
    fn __ax_cpu_x86_boot_11();
    fn __ax_cpu_x86_boot_12();
    fn __ax_cpu_x86_boot_13();
    fn __ax_cpu_x86_boot_14();
    fn __ax_cpu_x86_boot_15();
    fn __ax_cpu_x86_boot_16();
    fn __ax_cpu_x86_boot_17();
    fn __ax_cpu_x86_boot_18();
    fn __ax_cpu_x86_boot_19();
    fn __ax_cpu_x86_boot_20();
    fn __ax_cpu_x86_boot_21();
    fn __ax_cpu_x86_boot_22();
    fn __ax_cpu_x86_boot_23();
    fn __ax_cpu_x86_boot_24();
    fn __ax_cpu_x86_boot_25();
    fn __ax_cpu_x86_boot_26();
    fn __ax_cpu_x86_boot_27();
    fn __ax_cpu_x86_boot_28();
    fn __ax_cpu_x86_boot_29();
    fn __ax_cpu_x86_boot_30();
    fn __ax_cpu_x86_boot_31();
}

unsafe extern "C" fn dispatch(frame: *const TrapFrame) {
    // SAFETY: the assembly owns a fully initialized native frame until return.
    let frame = unsafe { &*frame };
    // SAFETY: this callback executes in CPL0; CR2 is copied before boot policy runs.
    let fault = unsafe { x86::controlregs::cr2() };
    crate::trap::boot::boot_trap_handler::handle(&crate::trap::boot::BootException {
        registers: frame.regs,
        pc: frame.rip as usize,
        sp: frame.rsp as usize,
        status: frame.rflags,
        syndrome: frame.error_code,
        vector: frame.vector as u8,
        fault_address: crate::VirtAddr::from_usize(fault as usize),
    });
}

/// Installs the immutable early IDT on the current CPU.
///
/// # Safety
/// Run in CPL0 with interrupts disabled and a valid, sufficiently sized stack.
/// Every participating CPU must use the same long-mode code selector. Boot
/// callbacks must not use runtime TLS or SIMD state, and must not unwind.
/// Keep the IDT and its entry code mapped until replacing it with a runtime IDT.
pub unsafe fn install() {
    if STATE
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Acquire)
        .is_ok()
    {
        // SAFETY: STATE grants exclusive initialization before any IDT load.
        unsafe {
            core::ptr::addr_of_mut!(IDT).write(BootVectorTable::new());
        }
        STATE.store(2, Ordering::Release);
    } else {
        while STATE.load(Ordering::Acquire) != 2 {
            spin_loop();
        }
    }
    // SAFETY: acquire observes complete immutable descriptors; the caller keeps
    // this static table mapped while it is installed on this CPU.
    unsafe {
        dtables::lidt(&DescriptorTablePointer {
            base: core::ptr::addr_of!(IDT.0).cast::<Descriptor64>(),
            limit: (core::mem::size_of::<BootVectorTable>() - 1) as u16,
        });
    }
}

/// Returns the currently installed IDT base.
pub fn current_vector_table() -> usize {
    let mut pointer: DescriptorTablePointer<Descriptor64> = Default::default();
    // SAFETY: SIDT writes exactly the initialized descriptor pointer object.
    unsafe {
        dtables::sidt(&mut pointer);
    }
    pointer.base as usize
}

impl Default for BootVectorTable {
    fn default() -> Self {
        Self::new()
    }
}

impl BootVectorTable {
    /// Builds gates using this invocation's mapped entry addresses and CS.
    pub fn new() -> Self {
        let entries = [
            __ax_cpu_x86_boot_0 as *const () as u64,
            __ax_cpu_x86_boot_1 as *const () as u64,
            __ax_cpu_x86_boot_2 as *const () as u64,
            __ax_cpu_x86_boot_3 as *const () as u64,
            __ax_cpu_x86_boot_4 as *const () as u64,
            __ax_cpu_x86_boot_5 as *const () as u64,
            __ax_cpu_x86_boot_6 as *const () as u64,
            __ax_cpu_x86_boot_7 as *const () as u64,
            __ax_cpu_x86_boot_8 as *const () as u64,
            __ax_cpu_x86_boot_9 as *const () as u64,
            __ax_cpu_x86_boot_10 as *const () as u64,
            __ax_cpu_x86_boot_11 as *const () as u64,
            __ax_cpu_x86_boot_12 as *const () as u64,
            __ax_cpu_x86_boot_13 as *const () as u64,
            __ax_cpu_x86_boot_14 as *const () as u64,
            __ax_cpu_x86_boot_15 as *const () as u64,
            __ax_cpu_x86_boot_16 as *const () as u64,
            __ax_cpu_x86_boot_17 as *const () as u64,
            __ax_cpu_x86_boot_18 as *const () as u64,
            __ax_cpu_x86_boot_19 as *const () as u64,
            __ax_cpu_x86_boot_20 as *const () as u64,
            __ax_cpu_x86_boot_21 as *const () as u64,
            __ax_cpu_x86_boot_22 as *const () as u64,
            __ax_cpu_x86_boot_23 as *const () as u64,
            __ax_cpu_x86_boot_24 as *const () as u64,
            __ax_cpu_x86_boot_25 as *const () as u64,
            __ax_cpu_x86_boot_26 as *const () as u64,
            __ax_cpu_x86_boot_27 as *const () as u64,
            __ax_cpu_x86_boot_28 as *const () as u64,
            __ax_cpu_x86_boot_29 as *const () as u64,
            __ax_cpu_x86_boot_30 as *const () as u64,
            __ax_cpu_x86_boot_31 as *const () as u64,
        ];
        let mut table = Self([Descriptor64::NULL; 256]);
        let selector = cs();
        for (vector, address) in entries.into_iter().enumerate() {
            table.0[vector] = DescriptorBuilder::interrupt_descriptor(selector, address)
                .present()
                .finish();
        }
        table
    }

    /// Installs this table on the current CPU.
    ///
    /// # Safety
    /// The table must remain unmoved and mapped until a different IDT is loaded.
    /// Execute at CPL0 with masked IRQs and the same CS used to construct it.
    /// Callbacks require a valid kernel stack, no SIMD/TLS use and no unwinding.
    pub unsafe fn install(&self) {
        // SAFETY: the caller retains this table and the corresponding code.
        unsafe {
            dtables::lidt(&DescriptorTablePointer {
                base: self.0.as_ptr(),
                limit: (core::mem::size_of::<Self>() - 1) as u16,
            });
        }
    }
}
