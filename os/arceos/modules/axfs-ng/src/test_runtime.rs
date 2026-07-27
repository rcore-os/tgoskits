//! Kernel-guard symbols owned by the ax-fs-ng unit-test binary.

struct FsTestKernelGuard;

#[ax_crate_interface::impl_interface]
impl ax_kernel_guard::KernelGuardIf for FsTestKernelGuard {
    fn disable_preempt() {}

    fn enable_preempt() {}
}
