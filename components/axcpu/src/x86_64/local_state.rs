//! Lazy x86 LinuxCurrent user-TLS ownership.

#[cfg(not(feature = "host-test"))]
use core::mem::offset_of;
use core::mem::size_of;

#[cfg(not(feature = "host-test"))]
use cpu_local::CPU_AREA_ARCH_STATE_OFFSET;
use cpu_local::CPU_AREA_ARCH_STATE_SIZE;

const IA32_FS_BASE: u32 = 0xc000_0100;
const IA32_KERNEL_GS_BASE: u32 = 0xc000_0102;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct UserTlsValues {
    fs_base: usize,
    gs_base: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UserTlsWrites {
    fs_base: bool,
    gs_base: bool,
}

/// CPU-owned physical user-TLS image and its publication generation.
#[repr(C)]
struct CpuUserTlsState {
    fs_base: usize,
    gs_base: usize,
    generation: usize,
}

#[cfg(not(feature = "host-test"))]
const CPU_USER_FS_BASE_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuUserTlsState, fs_base);
#[cfg(not(feature = "host-test"))]
const CPU_USER_GS_BASE_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuUserTlsState, gs_base);
#[cfg(not(feature = "host-test"))]
const CPU_USER_TLS_GENERATION_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuUserTlsState, generation);

const _: () = assert!(size_of::<CpuUserTlsState>() <= CPU_AREA_ARCH_STATE_SIZE);

fn changed_user_tls(
    previous: UserTlsValues,
    next: UserTlsValues,
    initialized: bool,
) -> UserTlsWrites {
    UserTlsWrites {
        fs_base: !initialized || previous.fs_base != next.fs_base,
        gs_base: !initialized || previous.gs_base != next.gs_base,
    }
}

fn next_generation(previous: usize) -> usize {
    match previous.wrapping_add(1) {
        0 => 1,
        generation => generation,
    }
}

#[cfg(not(feature = "host-test"))]
fn current_cpu_user_tls() -> (UserTlsValues, usize) {
    let fs_base: usize;
    let gs_base: usize;
    let generation: usize;
    // SAFETY: this runs with local IRQs disabled after the CPU area has been
    // installed. The three fields are owned by this CPU and no remote path
    // reads or writes the architecture reserve.
    unsafe {
        core::arch::asm!(
            "mov {fs_base}, gs:[{fs_offset}]",
            "mov {gs_base}, gs:[{gs_offset}]",
            "mov {generation}, gs:[{generation_offset}]",
            fs_base = out(reg) fs_base,
            gs_base = out(reg) gs_base,
            generation = out(reg) generation,
            fs_offset = const CPU_USER_FS_BASE_OFFSET,
            gs_offset = const CPU_USER_GS_BASE_OFFSET,
            generation_offset = const CPU_USER_TLS_GENERATION_OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
    (UserTlsValues { fs_base, gs_base }, generation)
}

#[cfg(feature = "host-test")]
fn current_cpu_user_tls() -> (UserTlsValues, usize) {
    (UserTlsValues::default(), 0)
}

#[cfg(not(feature = "host-test"))]
fn publish_current_cpu_user_tls(values: UserTlsValues, generation: usize) {
    // SAFETY: the caller retains the same IRQ-disabled CPU ownership used by
    // current_cpu_user_tls. Publishing the generation last makes a future
    // diagnostic reader reject a partially updated cache image.
    unsafe {
        core::arch::asm!(
            "mov gs:[{fs_offset}], {fs_base}",
            "mov gs:[{gs_offset}], {gs_base}",
            "mov gs:[{generation_offset}], {generation}",
            fs_offset = const CPU_USER_FS_BASE_OFFSET,
            gs_offset = const CPU_USER_GS_BASE_OFFSET,
            generation_offset = const CPU_USER_TLS_GENERATION_OFFSET,
            fs_base = in(reg) values.fs_base,
            gs_base = in(reg) values.gs_base,
            generation = in(reg) generation,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(feature = "host-test")]
fn publish_current_cpu_user_tls(_values: UserTlsValues, _generation: usize) {
    // Host tests cannot address the kernel GS CPU area.
}

fn write_changed_user_tls(previous: UserTlsValues, next: UserTlsValues, initialized: bool) {
    let writes = changed_user_tls(previous, next, initialized);
    if writes.fs_base {
        write_user_tls_msr(IA32_FS_BASE, next.fs_base);
    }
    if writes.gs_base {
        write_user_tls_msr(IA32_KERNEL_GS_BASE, next.gs_base);
    }
}

#[cfg(not(feature = "host-test"))]
fn write_user_tls_msr(msr: u32, value: usize) {
    let value = value as u64;
    // SAFETY: LinuxCurrent does not use FS for kernel TLS, and SWAPGS keeps
    // IA32_KERNEL_GS_BASE inactive while Rust executes. The caller holds this
    // CPU with IRQs disabled, so both writes update only its user return image.
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(feature = "host-test")]
fn write_user_tls_msr(_msr: u32, _value: usize) {
    // Host scheduler tests model the decision but cannot execute WRMSR.
}

/// Establishes the physical reset image before this CPU can enter userspace.
pub(super) fn initialize_cpu_user_tls() {
    #[cfg(not(feature = "host-test"))]
    debug_assert!(!super::asm::irqs_enabled());
    let values = UserTlsValues::default();
    write_changed_user_tls(UserTlsValues::default(), values, false);
    publish_current_cpu_user_tls(values, 1);
}

/// Lazily installs a user context without disturbing it in kernel-only tasks.
pub(super) fn install_current_user_tls(fs_base: usize, gs_base: usize) {
    #[cfg(not(feature = "host-test"))]
    debug_assert!(!super::asm::irqs_enabled());
    let (previous, generation) = current_cpu_user_tls();
    let next = UserTlsValues { fs_base, gs_base };
    let initialized = generation != 0;
    let writes = changed_user_tls(previous, next, initialized);
    if !writes.fs_base && !writes.gs_base {
        return;
    }
    write_changed_user_tls(previous, next, initialized);
    publish_current_cpu_user_tls(next, next_generation(generation));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_user_tls_requires_no_msr_write() {
        let values = UserTlsValues {
            fs_base: 0x1000,
            gs_base: 0x2000,
        };
        assert_eq!(
            changed_user_tls(values, values, true),
            UserTlsWrites {
                fs_base: false,
                gs_base: false,
            }
        );
    }

    #[test]
    fn user_tls_transition_writes_only_the_changed_msr() {
        let previous = UserTlsValues {
            fs_base: 0x1000,
            gs_base: 0x2000,
        };
        assert_eq!(
            changed_user_tls(
                previous,
                UserTlsValues {
                    fs_base: 0x3000,
                    gs_base: previous.gs_base,
                },
                true,
            ),
            UserTlsWrites {
                fs_base: true,
                gs_base: false,
            }
        );
        assert_eq!(
            changed_user_tls(
                previous,
                UserTlsValues {
                    fs_base: previous.fs_base,
                    gs_base: 0x4000,
                },
                true,
            ),
            UserTlsWrites {
                fs_base: false,
                gs_base: true,
            }
        );
    }

    #[test]
    fn uninitialized_cpu_image_forces_both_register_writes() {
        assert_eq!(
            changed_user_tls(UserTlsValues::default(), UserTlsValues::default(), false),
            UserTlsWrites {
                fs_base: true,
                gs_base: true,
            }
        );
    }

    #[test]
    fn user_tls_generation_remains_a_nonzero_initialized_marker() {
        assert_eq!(next_generation(usize::MAX), 1);
    }
}
