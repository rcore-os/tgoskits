//! Nofault kernel copy: an inaccessible range reports a fault instead of
//! entering the page-fault path.

use ax_cpu::kernel_access::{KernelAccessError, copy_from_kernel_nofault};

/// Contents the destination holds when a copy must not touch it.
const KEPT: u8 = 0xa5;

/// An address whose bits 63:57 are not the sign extension of bit 56. No
/// translation covers it at any width the architectures here use, so reaching
/// it says nothing about the page tables; on AArch64 and LoongArch the fault it
/// raises is not a page fault at all.
const UNTRANSLATABLE: *mut u8 = 0xfe00_0000_0000_0000 as *mut u8;

/// Probes an address from both sides of the copy.
fn probe_unreachable(address: *mut u8) {
    // A source that faults before the first byte is read leaves the destination
    // untouched.
    let mut kept = [KEPT; 8];
    // SAFETY: the destination is this frame's own array; the source is the
    // unreachable address this case means to probe.
    let faulted = unsafe { copy_from_kernel_nofault(kept.as_mut_ptr(), address, kept.len()) };
    assert_eq!(faulted, Err(KernelAccessError::Fault));
    assert_eq!(kept, [KEPT; 8]);

    // The store side reports the same way, with the source readable.
    let source = [7u8; 8];
    // SAFETY: the source is this frame's own array; the destination is the
    // unreachable address this case means to probe.
    let faulted = unsafe { copy_from_kernel_nofault(address, source.as_ptr(), source.len()) };
    assert_eq!(faulted, Err(KernelAccessError::Fault));
}

pub fn run() {
    // A mapped source and destination copy whole.
    let source = [1u8, 2, 3, 4, 5, 6, 7, 8];
    let mut destination = [0u8; 8];
    // SAFETY: both ranges are this frame's own arrays.
    let copied = unsafe {
        copy_from_kernel_nofault(destination.as_mut_ptr(), source.as_ptr(), source.len())
    };
    assert!(copied.is_ok());
    assert_eq!(destination, source);

    probe_unreachable(UNTRANSLATABLE);

    // A user address without a translation fails differently, and emptying the
    // user range is the only way to reach that class on demand. AArch64 is the
    // architecture here that can empty it.
    #[cfg(target_arch = "aarch64")]
    {
        let _window = super::empty_user_table::EmptyUserTable::install();
        probe_unreachable(core::ptr::null_mut());
    }

    std::println!("CPU_KERNEL_ACCESS_OK");
}
