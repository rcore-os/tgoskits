//! Current x86 virtualization hardware capabilities.

mod memory;
pub(crate) mod percpu;

pub use percpu::{Backend, PerCpu, authorize_vmx};

mod vmx;
pub use vmx::*;

mod svm;
pub use svm::*;

mod bitmaps;

/// Architecture-specific control leases supplied by an x86 host allocator.
pub enum VcpuControlMemory<M: crate::virtualization::ControlMemory> {
    /// Intel VMCS and interception pages.
    Vmx(VmxControlMemory<M>),
    /// AMD guest/host VMCBs and interception pages.
    Svm(SvmControlMemory<M>),
}

pub(crate) mod xstate;
pub use xstate::{CpuidResult, GuestXstate, XstateLayout};

mod vcpu;
pub use vcpu::{ExecutionMode, Exit, RunError, Vcpu};
