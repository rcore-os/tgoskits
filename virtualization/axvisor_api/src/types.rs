//! Common Axvisor API types shared across host and core-facing interfaces.

/// Virtual machine identifier type.
///
/// Each virtual machine is assigned a unique identifier that can be used to
/// reference it in API calls.
pub type VMId = usize;

/// Virtual CPU identifier type.
///
/// Each vCPU within a VM is assigned a unique identifier, usually 0-indexed
/// within the VM.
pub type VCpuId = usize;

/// Interrupt vector type.
///
/// Represents the interrupt vector number to be injected into a guest.
pub type InterruptVector = u8;

/// The maximum number of virtual CPUs supported in a virtual machine.
pub const MAX_VCPU_NUM: usize = 64;

/// A set of virtual CPUs.
pub type VCpuSet = ax_cpumask::CpuMask<MAX_VCPU_NUM>;
