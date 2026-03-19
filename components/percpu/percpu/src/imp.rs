use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use crate::InitError;
use percpu_macros::percpu_symbol_vma;

/// This atomic tracks whether the per-CPU data areas is being initialized.
/// It is cleared after initialization to enable re-initialization.
static IS_INIT: AtomicBool = AtomicBool::new(false);

const SIZE_64BIT: usize = 64;
const PERCPU_AREA_ALIGN: usize = SIZE_64BIT;

const fn align_up_64(val: usize) -> usize {
    (val + SIZE_64BIT - 1) & !(SIZE_64BIT - 1)
}

/// The per-CPU data area base address.
/// Set by `init()` or `init_in_place()` during initialization.
static PERCPU_AREA_BASE: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

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

/// Returns the number of per-CPU data areas reserved in the `.percpu` section.
///
/// This is calculated based on the size of the `.percpu` section and the size
/// of one per-CPU data area. The section size should be reserved in the linker
/// script with enough space for all CPUs.
pub fn percpu_area_num() -> usize {
    (_percpu_end as *mut u8 as usize - _percpu_start as *mut u8 as usize)
        / align_up_64(percpu_area_size())
}

/// Returns the per-CPU data area size for one CPU.
///
/// This is the size of the `.percpu` section content (all per-CPU static variables),
/// rounded up to 64-byte alignment.
pub fn percpu_area_size() -> usize {
    percpu_symbol_vma!(_percpu_load_end) - percpu_symbol_vma!(_percpu_load_start)
}

/// Returns the expected layout for the per-CPU data area for the given number
/// of CPUs.
///
/// # Arguments
///
/// - `cpu_count`: Number of CPUs.
///
/// # Returns
///
/// The expected layout for the per-CPU data area.
pub fn percpu_area_layout_expected(cpu_count: usize) -> core::alloc::Layout {
    let size = cpu_count * align_up_64(percpu_area_size());
    core::alloc::Layout::from_size_align(size, PERCPU_AREA_ALIGN).unwrap()
}

fn percpu_area_base_nolock(cpu_id: usize) -> usize {
    let base = PERCPU_AREA_BASE.load(Ordering::Relaxed);

    if base.is_null() {
        panic!("PerCPU area base address not set");
    }

    base as usize + cpu_id * align_up_64(percpu_area_size())
}

/// Returns the base address of the per-CPU data area for the given CPU.
///
/// # Panics
///
/// Panics if the per-CPU area base address has not been set (i.e., `init()`
/// or `init_in_place()` has not been called).
///
/// # Concurrency
///
/// This function spins until initialization is complete if called during
/// the initialization process.
pub fn percpu_area_base(cpu_id: usize) -> usize {
    while IS_INIT.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }

    percpu_area_base_nolock(cpu_id)
}

/// Check if the `.percpu` section is loaded at VMA address 0 when feature "non-zero-vma" is disabled.
fn validate_percpu_vma() {
    // `_percpu_load_start as *mut u8 as usize` cannot be used here because
    // rust will assume a `*mut u8` is a valid pointer and will not be 0,
    // causing unexpected `0 != 0` assertion failure.
    #[cfg(not(feature = "non-zero-vma"))]
    {
        assert_eq!(
            percpu_symbol_vma!(_percpu_load_start), 0,
            "The `.percpu` section must be loaded at VMA address 0 when feature \"non-zero-vma\" is disabled"
        )
    }
}

/// Copies the per-CPU data from the source to the per-CPU data areas of the
/// given CPUs.
fn copy_percpu_region<T: Iterator<Item = usize>>(source: *mut u8, dest_ids: T) {
    let size = percpu_area_size();

    for dest_id in dest_ids {
        let dest_base = percpu_area_base_nolock(dest_id);
        unsafe {
            core::ptr::copy_nonoverlapping(source as *const u8, dest_base as *mut u8, size);
        }
    }
}

fn validate_percpu_area_base(base: *mut u8) -> Result<(), InitError> {
    if base.is_null() {
        return Err(InitError::InvalidBase);
    }
    if (base as usize) % PERCPU_AREA_ALIGN != 0 {
        return Err(InitError::UnalignedBase);
    }
    Ok(())
}

fn init_inner(
    base: *mut u8,
    cpu_count: usize,
    do_not_copy_to_primary: bool,
) -> Result<usize, InitError> {
    validate_percpu_area_base(base)?;

    // Avoid re-initialization.
    if IS_INIT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(0);
    }

    // Validate the VMA of the `.percpu` section.
    validate_percpu_vma();

    // Set the base address of the per-CPU data areas.
    PERCPU_AREA_BASE.store(base as _, Ordering::Relaxed);

    // Copy the per-CPU data from the `.percpu` section to the per-CPU areas of
    // all CPUs.
    copy_percpu_region(
        _percpu_start as *mut u8,
        if do_not_copy_to_primary {
            1..cpu_count
        } else {
            0..cpu_count
        },
    );

    // Enable re-initialization.
    IS_INIT.store(false, Ordering::Release);

    Ok(cpu_count)
}

/// Initialize per-CPU data areas using the `.percpu` section.
///
/// This function uses `_percpu_start` as the base address. The per-CPU data
/// areas are statically allocated in the `.percpu` section by the linker script.
/// The primary CPU's data is already in place, so only data for secondary CPUs
/// (1 to cpu_count-1) is copied.
///
/// This function can be called repeatedly for re-initialization. However,
/// re-initialization will overwrite existing per-CPU data, so per-CPU variables
/// should be reset manually after re-initialization.
///
/// # Returns
///
/// The number of per-CPU areas initialized (i.e., `percpu_area_num()`) on
/// success.
///
/// Returns [`InitError::InvalidBase`] if `_percpu_start` is null, or
/// [`InitError::UnalignedBase`] if `_percpu_start` is not 64-byte aligned.
pub fn init_in_place() -> Result<usize, InitError> {
    let base = _percpu_start as *mut u8;
    init_inner(base, percpu_area_num(), true)
}

/// Initialize per-CPU data areas with user-provided memory.
///
/// The caller is responsible for allocating memory for the per-CPU data areas.
/// Use [`percpu_area_layout_expected()`] to calculate the required memory size.
/// The allocated memory should be aligned to at least 64 bytes (cache line size),
/// and preferably to 4KiB (page size) for better performance.
///
/// This function copies the `.percpu` section content to the user-provided memory
/// for all CPUs (0 to cpu_count-1).
///
/// This function can be called repeatedly for re-initialization. However,
/// re-initialization will overwrite existing per-CPU data, so per-CPU variables
/// should be reset manually after re-initialization.
///
/// # Arguments
///
/// - `base`: Base address of the user-allocated memory.
/// - `cpu_count`: Number of CPUs.
///
/// # Returns
///
/// The number of per-CPU areas initialized (i.e., `cpu_count`) on success.
///
/// Returns [`InitError::InvalidBase`] if `base` is null, or
/// [`InitError::UnalignedBase`] if `base` is not 64-byte aligned.
///
/// # Example
///
/// ```rust,no_run
/// let cpu_count = 4;
/// let layout = percpu::percpu_area_layout_expected(cpu_count);
/// let base = unsafe { std::alloc::alloc(layout) as usize };
/// percpu::init(base as *mut u8, cpu_count).unwrap();
/// ```
pub fn init(base: *mut u8, cpu_count: usize) -> Result<usize, InitError> {
    init_inner(base, cpu_count, false)
}

/// Reads the architecture-specific per-CPU data register.
///
/// Returns the value stored in the per-CPU register, which is the base address
/// of the current CPU's per-CPU data area.
///
/// # Architecture-specific registers
///
/// | Architecture | Register |
/// |--------------|----------|
/// | x86_64 | `GS_BASE` MSR |
/// | RISC-V | `gp` |
/// | AArch64 | `TPIDR_EL1` or `TPIDR_EL2` (with `arm-el2` feature) |
/// | LoongArch | `$r21` |
/// | ARM (32-bit) | `TPIDRPRW` (CP15 c13) |
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
                core::arch::asm!("mrc p15, 0, {}, c13, c0, 4", out(reg) tp)
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
/// Sets the per-CPU register to the given value, which should be the base address
/// of the current CPU's per-CPU data area.
///
/// # Safety
///
/// This function is unsafe because it directly writes to a low-level register.
/// Setting an invalid address may cause undefined behavior.
///
/// # Architecture-specific registers
///
/// See [`read_percpu_reg()`] for the list of registers used per architecture.
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
                core::arch::asm!("mcr p15, 0, {}, c13, c0, 4", in(reg) tp)
            }
        }
    }
}

/// Initializes the per-CPU data register for the current CPU.
///
/// This function sets the architecture-specific per-CPU register to point to
/// the base address of the per-CPU data area for the given CPU ID.
///
/// This should be called on each CPU during boot, after `init()` or `init_in_place()`
/// has been called.
///
/// # Arguments
///
/// - `cpu_id`: The CPU ID to use (0-based index).
///
/// # Panics
///
/// Panics if the per-CPU area base address has not been set.
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
