//! Lazy x86 LinuxCurrent userspace register ownership.

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

/// CPU-owned physical userspace register image.
#[repr(C)]
struct CpuUserState {
    fs_base: usize,
    gs_base: usize,
    tls_generation: usize,
    user_fp_owner: usize,
    xsave_config: usize,
}

#[cfg(not(feature = "host-test"))]
const CPU_USER_FS_BASE_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuUserState, fs_base);
#[cfg(not(feature = "host-test"))]
const CPU_USER_GS_BASE_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuUserState, gs_base);
#[cfg(not(feature = "host-test"))]
const CPU_USER_TLS_GENERATION_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuUserState, tls_generation);
#[cfg(not(feature = "host-test"))]
const CPU_USER_FP_OWNER_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuUserState, user_fp_owner);
#[cfg(all(feature = "fp-simd", not(feature = "host-test")))]
const CPU_USER_XSAVE_CONFIG_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuUserState, xsave_config);

#[cfg(feature = "fp-simd")]
const USER_XSAVEOPT_ENABLED: usize = 1 << (usize::BITS - 1);

const _: () = assert!(size_of::<CpuUserState>() <= CPU_AREA_ARCH_STATE_SIZE);

#[cfg(feature = "fp-simd")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UserFpOwnerMatch {
    Unowned,
    Current,
    Foreign,
}

#[cfg(feature = "fp-simd")]
fn classify_user_fp_owner(owner: usize, current: usize) -> UserFpOwnerMatch {
    if owner == 0 {
        UserFpOwnerMatch::Unowned
    } else if owner == current {
        UserFpOwnerMatch::Current
    } else {
        UserFpOwnerMatch::Foreign
    }
}

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

#[cfg(all(feature = "fp-simd", not(feature = "host-test")))]
fn current_cpu_user_fp_owner() -> usize {
    let owner: usize;
    // SAFETY: local IRQs are disabled after CPU-area installation. The owner
    // word is private to this physical CPU and is never accessed remotely.
    unsafe {
        core::arch::asm!(
            "mov {owner}, gs:[{owner_offset}]",
            owner = out(reg) owner,
            owner_offset = const CPU_USER_FP_OWNER_OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
    owner
}

#[cfg(all(feature = "fp-simd", feature = "host-test"))]
fn current_cpu_user_fp_owner() -> usize {
    0
}

#[cfg(not(feature = "host-test"))]
fn publish_current_cpu_user_fp_owner(owner: usize) {
    // SAFETY: the caller retains the same IRQ-disabled CPU ownership used by
    // `current_cpu_user_fp_owner` for the complete hardware-state transition.
    unsafe {
        core::arch::asm!(
            "mov gs:[{owner_offset}], {owner}",
            owner_offset = const CPU_USER_FP_OWNER_OFFSET,
            owner = in(reg) owner,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(feature = "host-test")]
fn publish_current_cpu_user_fp_owner(_owner: usize) {
    // Host tests cannot address the kernel GS CPU area.
}

#[cfg(all(feature = "fp-simd", not(feature = "host-test")))]
pub(super) fn current_cpu_user_xsave_config() -> Option<(u64, bool)> {
    let config: usize;
    // SAFETY: userspace CPU initialization publishes this immutable word
    // before this CPU can schedule a userspace context. It is thereafter read
    // only by the same IRQ-disabled CPU during FP save and restore.
    unsafe {
        core::arch::asm!(
            "mov {config}, gs:[{config_offset}]",
            config = out(reg) config,
            config_offset = const CPU_USER_XSAVE_CONFIG_OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
    let mask = config & !USER_XSAVEOPT_ENABLED;
    (mask != 0).then_some((mask as u64, config & USER_XSAVEOPT_ENABLED != 0))
}

#[cfg(all(feature = "fp-simd", feature = "host-test"))]
pub(super) fn current_cpu_user_xsave_config() -> Option<(u64, bool)> {
    None
}

#[cfg(all(feature = "fp-simd", not(feature = "host-test")))]
fn publish_current_cpu_user_xsave_config(mask: u64, xsaveopt_enabled: bool) {
    let config = mask as usize | usize::from(xsaveopt_enabled) * USER_XSAVEOPT_ENABLED;
    // SAFETY: this runs once for the current CPU after its GS CPU-area base and
    // XCR0 policy are installed, before that CPU can enter userspace.
    unsafe {
        core::arch::asm!(
            "mov gs:[{config_offset}], {config}",
            config_offset = const CPU_USER_XSAVE_CONFIG_OFFSET,
            config = in(reg) config,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(all(feature = "fp-simd", feature = "host-test"))]
fn publish_current_cpu_user_xsave_config(_mask: u64, _xsaveopt_enabled: bool) {}

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
    publish_current_cpu_user_fp_owner(0);
    #[cfg(feature = "fp-simd")]
    {
        #[cfg(not(feature = "host-test"))]
        let config = {
            let cr4 = unsafe { x86::controlregs::cr4() };
            if cr4.contains(x86::controlregs::Cr4::CR4_ENABLE_OS_XSAVE) {
                let mask = unsafe { x86::controlregs::xcr0().bits() };
                let xsaveopt_enabled = core::arch::x86_64::__cpuid_count(0x0d, 1).eax & 1 != 0;
                (mask, xsaveopt_enabled)
            } else {
                (0, false)
            }
        };
        #[cfg(feature = "host-test")]
        let config = (0, false);
        publish_current_cpu_user_xsave_config(config.0, config.1);
    }
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

/// Reports whether `current` owns the physical FPU image that must be saved.
#[cfg(feature = "fp-simd")]
pub(super) fn current_user_fp_is_owner(current: usize) -> bool {
    #[cfg(not(feature = "host-test"))]
    debug_assert!(!super::asm::irqs_enabled());
    match classify_user_fp_owner(current_cpu_user_fp_owner(), current) {
        UserFpOwnerMatch::Unowned => false,
        UserFpOwnerMatch::Current => true,
        UserFpOwnerMatch::Foreign => {
            panic!("x86 user FPU owner does not match the outgoing current context")
        }
    }
}

/// Clears `current` only after its physical FPU image has reached task memory.
#[cfg(feature = "fp-simd")]
pub(super) fn clear_current_user_fp_owner_after_save(_current: usize) {
    #[cfg(not(feature = "host-test"))]
    {
        debug_assert!(!super::asm::irqs_enabled());
        debug_assert_eq!(current_cpu_user_fp_owner(), _current);
    }
    publish_current_cpu_user_fp_owner(0);
}

/// Verifies that a context without a scheduler identity owns no user FPU image.
#[cfg(feature = "fp-simd")]
pub(super) fn assert_current_user_fp_unowned() {
    #[cfg(not(feature = "host-test"))]
    debug_assert!(!super::asm::irqs_enabled());
    assert_eq!(
        current_cpu_user_fp_owner(),
        0,
        "an unbound context cannot own the physical user FPU image",
    );
}

/// Reports whether `current` must restore its user FPU image before user mode.
#[cfg(feature = "fp-simd")]
pub(super) fn current_user_fp_needs_restore(current: usize) -> bool {
    #[cfg(not(feature = "host-test"))]
    debug_assert!(!super::asm::irqs_enabled());
    match classify_user_fp_owner(current_cpu_user_fp_owner(), current) {
        UserFpOwnerMatch::Unowned => true,
        UserFpOwnerMatch::Current => false,
        UserFpOwnerMatch::Foreign => {
            panic!("x86 user FPU owner does not match the return-to-user context")
        }
    }
}

/// Validates that `current` may replace the physical user FPU image.
#[cfg(feature = "fp-simd")]
pub(super) fn assert_current_user_fp_resettable(current: usize) {
    #[cfg(not(feature = "host-test"))]
    debug_assert!(!super::asm::irqs_enabled());
    match classify_user_fp_owner(current_cpu_user_fp_owner(), current) {
        UserFpOwnerMatch::Unowned | UserFpOwnerMatch::Current => {}
        UserFpOwnerMatch::Foreign => {
            panic!("x86 user FPU owner does not match the resetting current context")
        }
    }
}

/// Publishes `current` after its user FPU image has reached hardware.
#[cfg(feature = "fp-simd")]
pub(super) fn publish_current_user_fp_owner(current: usize) {
    #[cfg(not(feature = "host-test"))]
    debug_assert!(!super::asm::irqs_enabled());
    assert_ne!(current, 0, "a user FPU owner requires a context identity");
    publish_current_cpu_user_fp_owner(current);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_user_tls_requires_no_owner_write() {
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
    fn user_owner_transition_writes_only_changed_registers() {
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

    #[test]
    #[cfg(feature = "fp-simd")]
    fn user_fpu_owner_has_one_current_or_unowned_state() {
        assert_eq!(classify_user_fp_owner(0, 0x1000), UserFpOwnerMatch::Unowned);
        assert_eq!(
            classify_user_fp_owner(0x1000, 0x1000),
            UserFpOwnerMatch::Current
        );
        assert_eq!(
            classify_user_fp_owner(0x2000, 0x1000),
            UserFpOwnerMatch::Foreign
        );
    }
}
