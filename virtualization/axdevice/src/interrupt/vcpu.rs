//! Controller-to-vCPU attachment capabilities.

use alloc::sync::Arc;

use crate::DeviceManagerResult;

/// Identifies a vCPU at an interrupt-controller boundary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct VcpuInterruptId(usize);

impl VcpuInterruptId {
    /// Creates a VM-local vCPU interrupt identifier.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the VM-local vCPU number.
    pub const fn value(self) -> usize {
        self.0
    }
}

/// Identifies one guest-visible interrupt at a CPU-interface boundary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct GuestInterruptId(u32);

impl GuestInterruptId {
    /// Creates an identifier from an architecture-decoded guest INTID.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the guest-visible numeric identifier.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Architecture-defined interrupt affinity associated with a vCPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct VcpuInterruptAffinity(u64);

impl VcpuInterruptAffinity {
    /// Creates an affinity from its architecture-defined packed value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the architecture-defined packed value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Wakes a vCPU after its controller has made an interrupt deliverable.
pub trait VcpuInterruptWake: Send + Sync {
    /// Requests that the runtime schedule or kick the vCPU.
    fn wake(&self) -> DeviceManagerResult;
}

/// A vCPU endpoint supplied to a registered interrupt controller.
#[derive(Clone)]
pub struct VcpuInterruptPort {
    id: VcpuInterruptId,
    affinity: VcpuInterruptAffinity,
    wake: Arc<dyn VcpuInterruptWake>,
}

impl VcpuInterruptPort {
    /// Creates a vCPU interrupt port.
    pub fn new(
        id: VcpuInterruptId,
        affinity: VcpuInterruptAffinity,
        wake: Arc<dyn VcpuInterruptWake>,
    ) -> Self {
        Self { id, affinity, wake }
    }

    /// Returns the VM-local vCPU identifier.
    pub const fn id(&self) -> VcpuInterruptId {
        self.id
    }

    /// Returns the architecture-defined affinity.
    pub const fn affinity(&self) -> VcpuInterruptAffinity {
        self.affinity
    }

    /// Requests that the runtime schedule or kick this vCPU.
    pub fn wake(&self) -> DeviceManagerResult {
        self.wake.wake()
    }
}

impl core::fmt::Debug for VcpuInterruptPort {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("VcpuInterruptPort")
            .field("id", &self.id)
            .field("affinity", &self.affinity)
            .finish_non_exhaustive()
    }
}

/// Per-vCPU controller state synchronized around guest execution.
pub trait VcpuInterruptBinding: Send + Sync {
    /// Restores controller state before the vCPU enters the guest.
    fn load(&self) -> DeviceManagerResult;

    /// Saves controller state after the vCPU leaves the guest.
    fn save(&self) -> DeviceManagerResult;

    /// Reconciles completed deliveries and makes pending work deliverable.
    fn synchronize(&self) -> DeviceManagerResult;
}

/// Optional CPU-interface capability for separately trapped deactivation.
///
/// Architectures such as GICv3 can split priority drop from interrupt
/// deactivation. This capability routes that architectural operation back to
/// the controller that owns the vCPU interface without exposing an injection
/// API to devices or the VM runtime.
pub trait VcpuInterruptDeactivation: Send + Sync {
    /// Deactivates one guest interrupt for the currently loaded vCPU.
    fn deactivate(&self, vcpu: VcpuInterruptId, intid: GuestInterruptId) -> DeviceManagerResult;
}

/// Associates an interrupt controller with vCPU ports.
pub trait VcpuInterruptController: Send + Sync {
    /// Attaches one vCPU and returns its lifecycle binding.
    fn attach_vcpu(
        &self,
        port: VcpuInterruptPort,
    ) -> DeviceManagerResult<Arc<dyn VcpuInterruptBinding>>;
}
