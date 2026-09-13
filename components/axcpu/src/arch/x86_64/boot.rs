// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// Early control-register setup and stack handoff migrated from someboot (周睿).
//! Helper functions to initialize the CPU states on systems bootstrapping.

pub use x86_64::{
    PrivilegeLevel, VirtAddr as DescriptorAddress,
    addr::VirtAddrNotValid,
    registers::segmentation::SegmentSelector,
    structures::{
        gdt::{Descriptor, Entry as GdtEntry, GlobalDescriptorTable},
        tss::{InvalidIoMap, TaskStateSegment},
    },
};

pub use super::gdt::{TrapStorage, TrapStorageProvider, trap_storage_provider};

/// Initializes trap handling on the current CPU.
///
/// In detail, it initializes the GDT, IDT on x86_64 platforms. If the `uspace`
/// feature is enabled, it also initializes relevant model-specific registers to
/// configure the handler for `syscall` instruction.
///
/// # Notes
/// Before calling this function, the platform entry path must have installed
/// and verified the current CPU area. Architecture trap initialization is not
/// a second per-CPU binder.
pub fn init_trap() {
    super::gdt::init();
    super::idt::init();
    #[cfg(feature = "uspace")]
    super::uspace::init_syscall();
}

pub use super::entry::boot::{
    BootVectorTable, current_vector_table, install as install_boot_vectors,
};

/// Control-register state installed before a CPU enters the kernel runtime.
///
/// This matches Linux's x86 `CR0_STATE`: paging and protected mode are active,
/// supervisor writes honor read-only PTEs, alignment checking is available,
/// and reset-time cache-disable state is not inherited by secondary CPUs.
pub const KERNEL_CR0_STATE: usize = x86::controlregs::Cr0::CR0_ENABLE_PAGING.bits()
    | x86::controlregs::Cr0::CR0_ALIGNMENT_MASK.bits()
    | x86::controlregs::Cr0::CR0_WRITE_PROTECT.bits()
    | x86::controlregs::Cr0::CR0_NUMERIC_ERROR.bits()
    | x86::controlregs::Cr0::CR0_EXTENSION_TYPE.bits()
    | x86::controlregs::Cr0::CR0_MONITOR_COPROCESSOR.bits()
    | x86::controlregs::Cr0::CR0_PROTECTED_MODE.bits();

/// Checks the complete early kernel CR0 contract on this CPU.
///
/// # Safety
/// Execute at CPL0 before running tasks or enabling interrupts.
pub unsafe fn assert_kernel_cr0_state() {
    // SAFETY: the boot owner executes this check at CPL0.
    let current = unsafe { x86::controlregs::cr0() };
    assert_eq!(
        current.bits(),
        KERNEL_CR0_STATE,
        "invalid x86_64 kernel CR0 state on this CPU"
    );
}

/// Enables architectural execute-disable page permissions.
///
/// # Safety
/// Execute at CPL0 on a CPU with NX support, before installing NX descriptors.
pub unsafe fn enable_execute_disable() {
    // SAFETY: the boot owner has established long mode with NX-capable hardware.
    unsafe {
        let value = x86::msr::rdmsr(x86::msr::IA32_EFER);
        x86::msr::wrmsr(x86::msr::IA32_EFER, value | (1 << 11));
    }
}

/// Installs the kernel CR0 state and enables global page translations.
///
/// # Safety
/// Execute at CPL0 with valid long-mode page tables and no active tasks.
/// Mappings must already have coherent cache attributes; this is boot setup,
/// not a live cache-mode transition.
pub unsafe fn configure_paging() {
    use x86::controlregs::{self, Cr0, Cr4};
    // SAFETY: the caller owns boot control-register state and installed mappings.
    unsafe {
        controlregs::cr0_write(Cr0::from_bits_truncate(KERNEL_CR0_STATE));
        controlregs::cr4_write(controlregs::cr4() | Cr4::CR4_ENABLE_GLOBAL_PAGES);
        assert_kernel_cr0_state();
    }
}

/// Enables supported x87, SSE and AVX components in the boot XCR0 policy.
///
/// # Safety
/// Execute at CPL0 before any task owns extended register state. All subsequent
/// save areas and context switches must support the enabled components.
pub unsafe fn enable_xsave_features() {
    use x86::{controlregs, cpuid::CpuId};
    let Some(info) = CpuId::new().get_feature_info() else {
        return;
    };
    if !info.has_xsave() {
        return;
    }
    // SAFETY: CPUID establishes XSAVE support. OSXSAVE precedes XSETBV,
    // mandatory x87/SSE components precede the optional AVX component.
    unsafe {
        controlregs::cr4_write(controlregs::cr4() | controlregs::Cr4::CR4_ENABLE_OS_XSAVE);
        let mut bits = controlregs::Xcr0::XCR0_FPU_MMX_STATE | controlregs::Xcr0::XCR0_SSE_STATE;
        if info.has_avx() {
            bits |= controlregs::Xcr0::XCR0_AVX_STATE;
        }
        controlregs::xcr0_write(bits);
    }
}

/// Transfers to a Rust/C entry on a new stack with a terminating return slot.
///
/// # Safety
/// `stack` must be a mapped, writable, 16-byte-aligned stack top with sufficient
/// capacity. `entry` must be valid executable code in the active address space
/// and must never return. No references to the abandoned stack may remain live.
#[unsafe(naked)]
pub unsafe extern "C" fn jump_to(_entry: usize, _stack: usize) -> ! {
    core::arch::naked_asm!("mov rsp, rsi", "push 0", "xor ebp, ebp", "jmp rdi",);
}
