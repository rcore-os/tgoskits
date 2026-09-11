//! CPU-owned register cache layout embedded in runtime-provided storage.

/// CPU-owned physical userspace register image.
#[repr(C)]
#[derive(Default)]
pub struct CpuEntryState {
    pub(super) fs_base: usize,
    pub(super) gs_base: usize,
    pub(super) tls_generation: usize,
    pub(super) user_fp_owner: usize,
    pub(super) xsave_config: usize,
}
