use core::sync::atomic::{AtomicBool, Ordering};

use percpu_macros::percpu_symbol_vma;

static IS_INIT: AtomicBool = AtomicBool::new(false);

const fn align_up_64(val: usize) -> usize {
    const SIZE_64BIT: usize = 0x40;
    (val + SIZE_64BIT - 1) & !(SIZE_64BIT - 1)
}

#[cfg(not(target_os = "none"))]
static PERCPU_AREA_BASE: spin::once::Once<usize> = spin::once::Once::new();

extern "C" {
    fn _percpu_start();
    fn _percpu_end();
    // WARNING: `_percpu_load_start`/`_percpu_load_end` (i.e. symbols in the
    // `.percpu` section) must be used with `percpu_symbol_vma!` macro to get
    // their VMA addresses. Casting them directly to `usize` may lead to
    // unexpected results, including:
    // - Rust assuming they are valid pointers and optimizing code based on that
    //   assumption (they are non-zero), causing unexpected runtime errors;
    // - Link-time errors because they are too far away from the program counter
    //.  (when Rust uses PC-relative addressing).
    //
    // See https://github.com/arceos-org/percpu/issues/18 for more details.
    fn _percpu_load_start();
    fn _percpu_load_end();
}

/// Returns the number of per-CPU data areas reserved.
pub fn percpu_area_num() -> usize {
    (_percpu_end as *const () as usize - _percpu_start as *const () as usize)
        / align_up_64(percpu_area_size())
}

/// Returns the per-CPU data area size for one CPU.
pub fn percpu_area_size() -> usize {
    percpu_symbol_vma!(_percpu_load_end) - percpu_symbol_vma!(_percpu_load_start)
}

/// Returns the base address of the per-CPU data area on the given CPU.
///
/// if `cpu_id` is 0, it returns the base address of all per-CPU data areas.
pub fn percpu_area_base(cpu_id: usize) -> usize {
    cfg_if::cfg_if! {
        if #[cfg(target_os = "none")] {
            let base = _percpu_start as *const () as usize;
        } else {
            let base = *PERCPU_AREA_BASE.get().unwrap();
        }
    }
    base + cpu_id * align_up_64(percpu_area_size())
}

/// Initialize all per-CPU data areas.
///
/// The number of areas is determined by the following formula:
///
/// ```text
/// (percpu_section_size / align_up(percpu_area_size, 64)
/// ```
///
/// Returns the number of areas initialized. If this function has been called
/// before, it does nothing and returns 0.
pub fn init() -> usize {
    // avoid re-initialization.
    if IS_INIT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return 0;
    }

    #[cfg(not(feature = "non-zero-vma"))]
    {
        // `_percpu_load_start as *const () as usize` cannot be used here because
        // rust will assume a `*const ()` is a valid pointer and will not be 0,
        // causing unexpected `0 != 0` assertion failure.
        assert_eq!(
            percpu_symbol_vma!(_percpu_load_start), 0,
            "The `.percpu` section must be loaded at VMA address 0 when feature \"non-zero-vma\" is disabled"
        )
    }

    #[cfg(target_os = "linux")]
    {
        // we not load the percpu section in ELF, allocate them here.
        let total_size = _percpu_end as *const () as usize - _percpu_start as *const () as usize;
        let layout = std::alloc::Layout::from_size_align(total_size, 0x1000).unwrap();
        PERCPU_AREA_BASE.call_once(|| unsafe { std::alloc::alloc(layout) as usize });
    }

    let base = percpu_area_base(0);
    let size = percpu_area_size();
    let num = percpu_area_num();
    for i in 1..num {
        let secondary_base = percpu_area_base(i);
        #[cfg(target_os = "none")]
        assert!(secondary_base + size <= _percpu_end as *const () as usize);
        // copy per-cpu data of the primary CPU to other CPUs.
        unsafe {
            core::ptr::copy_nonoverlapping(base as *const u8, secondary_base as *mut u8, size);
        }
    }
    num
}

/// Reads the architecture-specific per-CPU data register.
///
/// This register is used to hold the per-CPU data base on each CPU.
pub fn read_percpu_reg() -> usize {
    let tp: usize;
    unsafe {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                tp = if cfg!(target_os = "linux") {
                    SELF_PTR.read_current_raw()
                } else if cfg!(target_os = "none") {
                    x86::msr::rdmsr(x86::msr::IA32_GS_BASE) as usize
                } else {
                    unimplemented!()
                };
            } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
                core::arch::asm!("mv {}, gp", out(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", not(feature = "arm-el2")))] {
                core::arch::asm!("mrs {}, TPIDR_EL1", out(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", feature = "arm-el2"))] {
                core::arch::asm!("mrs {}, TPIDR_EL2", out(reg) tp)
            } else if #[cfg(target_arch = "loongarch64")] {
                // Register Convention
                // https://docs.kernel.org/arch/loongarch/introduction.html#gprs
                core::arch::asm!("move {}, $r21", out(reg) tp)
            } else if #[cfg(target_arch = "arm")] {
                core::arch::asm!("mrc p15, 0, {}, c13, c0, 3", out(reg) tp)
            }
        }
    }
    cfg_if::cfg_if! {
        if #[cfg(feature = "non-zero-vma")] {
            tp + percpu_symbol_vma!(_percpu_load_start)
        } else {
            tp
        }
    }
}

/// Writes the architecture-specific per-CPU data register.
///
/// This register is used to hold the per-CPU data base on each CPU.
///
/// # Safety
///
/// This function is unsafe because it writes the low-level register directly.
pub unsafe fn write_percpu_reg(tp: usize) {
    cfg_if::cfg_if! {
        if #[cfg(feature = "non-zero-vma")] {
            let tp = tp - percpu_symbol_vma!(_percpu_load_start);
        }
    };

    unsafe {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                if cfg!(target_os = "linux") {
                    const ARCH_SET_GS: u32 = 0x1001;
                    const SYS_ARCH_PRCTL: u32 = 158;
                    core::arch::asm!(
                        "syscall",
                        in("eax") SYS_ARCH_PRCTL,
                        in("edi") ARCH_SET_GS,
                        in("rsi") tp,
                    );
                } else if cfg!(target_os = "none") {
                    x86::msr::wrmsr(x86::msr::IA32_GS_BASE, tp as u64);
                } else {
                    unimplemented!()
                }
                SELF_PTR.write_current_raw(tp);
            } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
                core::arch::asm!("mv gp, {}", in(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", not(feature = "arm-el2")))] {
                core::arch::asm!("msr TPIDR_EL1, {}", in(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", feature = "arm-el2"))] {
                core::arch::asm!("msr TPIDR_EL2, {}", in(reg) tp)
            } else if #[cfg(target_arch = "loongarch64")] {
                core::arch::asm!("move $r21, {}", in(reg) tp)
            } else if #[cfg(target_arch = "arm")] {
                core::arch::asm!("mcr p15, 0, {}, c13, c0, 3", in(reg) tp)
            }
        }
    }
}

/// Initializes the per-CPU data register.
///
/// It is equivalent to `write_percpu_reg(percpu_area_base(cpu_id))`, which set
/// the architecture-specific per-CPU data register to the base address of the
/// corresponding per-CPU data area.
///
/// `cpu_id` indicates which per-CPU data area to use.
pub fn init_percpu_reg(cpu_id: usize) {
    let tp = percpu_area_base(cpu_id);
    unsafe { write_percpu_reg(tp) }
}

/// To use `percpu::__priv::NoPreemptGuard::new()` and `percpu::percpu_area_base()` in macro expansion.
#[allow(unused_imports)]
use crate as percpu;

/// On x86, we use `gs:SELF_PTR` to store the address of the per-CPU data area base.
#[cfg(target_arch = "x86_64")]
#[no_mangle]
#[percpu_macros::def_percpu]
static SELF_PTR: usize = 0;
