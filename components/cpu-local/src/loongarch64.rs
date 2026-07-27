//! LoongArch host scratch-register assignments shared by trap and vCPU code.

/// Kernel stack pointer scratch slot used by exception entry.
pub const KSAVE_KSP: usize = 0;
/// First temporary-register scratch slot used by exception entry.
pub const KSAVE_T0: usize = 1;
/// Second temporary-register scratch slot used by exception entry.
pub const KSAVE_T1: usize = 2;
/// CPU-local runtime area-base shadow restored by exception entry.
pub const KSAVE_PERCPU: usize = 3;

/// Host CPU-local runtime area-base shadow.
pub const HOST_PERCPU_KS: usize = KSAVE_PERCPU;
/// Host stack scratch reserved for vCPU entry and exit.
pub const HOST_VCPU_KS: usize = 4;
/// Temporary scratch reserved for vCPU entry and exit.
pub const HOST_VCPU_TMP_KS: usize = 5;
