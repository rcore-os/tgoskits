//! Checked physical GICv3 and CPU-interface capability boundary.

use alloc::string::String;

use axdevice_base::{InterruptTrigger, ItsId};

use crate::{
    CpuInterfaceState, EventId, GicAffinity, GicVcpuId, IntId, ItsDeviceId, LpiId, PhysicalIrqId,
    VgicError, VgicResult,
};

/// Host virtual CPU-interface architecture implemented by a backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostGicVersion {
    /// GICv2 GICH/GICV virtualization extensions.
    V2,
    /// GICv3 ICH/ICC virtualization extensions.
    V3,
}

/// Normalized host virtual interrupt-interface capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VgicBackendCapabilities {
    host_version: HostGicVersion,
    list_register_count: usize,
    priority_bits: u8,
    v2_compatibility: bool,
}

impl VgicBackendCapabilities {
    /// Creates a validated capability descriptor.
    pub const fn new(
        host_version: HostGicVersion,
        list_register_count: usize,
        priority_bits: u8,
        v2_compatibility: bool,
    ) -> Self {
        Self {
            host_version,
            list_register_count,
            priority_bits,
            v2_compatibility,
        }
    }

    /// Returns the host GIC version.
    pub const fn host_version(self) -> HostGicVersion {
        self.host_version
    }

    /// Returns the available LR count.
    pub const fn list_register_count(self) -> usize {
        self.list_register_count
    }

    /// Returns the implemented priority width.
    pub const fn priority_bits(self) -> u8 {
        self.priority_bits
    }

    /// Returns whether a GICv3 host exposes the GICv2 compatibility interface.
    pub const fn v2_compatibility(self) -> bool {
        self.v2_compatibility
    }
}

/// Backend-specific failure without leaking a platform error type.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("backend operation {operation} failed: {detail}")]
pub struct GicV3BackendError {
    operation: &'static str,
    detail: String,
}

impl GicV3BackendError {
    /// Creates a backend failure at an adapter boundary.
    pub fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }

    /// Returns the failed operation.
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    /// Returns backend-provided detail.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl From<GicV3BackendError> for VgicError {
    fn from(error: GicV3BackendError) -> Self {
        Self::Backend {
            operation: error.operation,
            detail: error.detail,
        }
    }
}

/// Explicit guest-to-physical SPI ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalInterruptBinding {
    guest: IntId,
    host: PhysicalIrqId,
    target: GicVcpuId,
    affinity: GicAffinity,
    trigger: InterruptTrigger,
}

impl PhysicalInterruptBinding {
    /// Creates a physical binding. The controller separately validates `guest` as an SPI.
    pub const fn new(
        guest: IntId,
        host: PhysicalIrqId,
        target: GicVcpuId,
        affinity: GicAffinity,
        trigger: InterruptTrigger,
    ) -> Self {
        Self {
            guest,
            host,
            target,
            affinity,
            trigger,
        }
    }

    /// Returns the guest INTID.
    pub const fn guest(self) -> IntId {
        self.guest
    }

    /// Returns the host interrupt identifier.
    pub const fn host(self) -> PhysicalIrqId {
        self.host
    }

    /// Returns the fixed target vCPU.
    pub const fn target(self) -> GicVcpuId {
        self.target
    }

    /// Returns the fixed physical affinity.
    pub const fn affinity(self) -> GicAffinity {
        self.affinity
    }

    /// Returns the immutable physical trigger configured before VM creation.
    pub const fn trigger(self) -> InterruptTrigger {
        self.trigger
    }
}

/// Explicit VM-owned physical ITS translation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalMsiBinding {
    its: ItsId,
    device: ItsDeviceId,
    event: EventId,
    lpi: LpiId,
    target: GicVcpuId,
    affinity: GicAffinity,
}

impl PhysicalMsiBinding {
    /// Creates a complete physical ITS ownership record.
    pub const fn new(
        its: ItsId,
        device: ItsDeviceId,
        event: EventId,
        lpi: LpiId,
        target: GicVcpuId,
        affinity: GicAffinity,
    ) -> Self {
        Self {
            its,
            device,
            event,
            lpi,
            target,
            affinity,
        }
    }

    /// Returns the VM-local ITS instance.
    pub const fn its(self) -> ItsId {
        self.its
    }

    /// Returns the VM-owned ITS device identifier.
    pub const fn device(self) -> ItsDeviceId {
        self.device
    }

    /// Returns the VM-owned event identifier.
    pub const fn event(self) -> EventId {
        self.event
    }

    /// Returns the assigned physical LPI.
    pub const fn lpi(self) -> LpiId {
        self.lpi
    }

    /// Returns the fixed target vCPU.
    pub const fn target(self) -> GicVcpuId {
        self.target
    }

    /// Returns the fixed physical affinity.
    pub const fn affinity(self) -> GicAffinity {
        self.affinity
    }
}

/// Platform operations required by a GICv3 controller.
pub trait GicV3Backend: Send + Sync {
    /// Returns normalized immutable host capabilities.
    fn capabilities(&self) -> VgicBackendCapabilities {
        VgicBackendCapabilities::new(HostGicVersion::V3, 16, 5, false)
    }

    /// Loads saved ICH state before guest entry.
    fn load_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        state: &CpuInterfaceState,
    ) -> Result<(), GicV3BackendError>;

    /// Saves current ICH state after guest exit.
    fn save_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        state: &mut CpuInterfaceState,
    ) -> Result<(), GicV3BackendError>;

    /// Notifies the platform after a software LR no longer owns an interrupt.
    ///
    /// Mediated host-line adapters use this boundary to unmask a physical
    /// level interrupt only after the guest has retired its virtual delivery.
    fn retire_emulated_interrupt(
        &self,
        _vcpu: GicVcpuId,
        _intid: IntId,
    ) -> Result<(), GicV3BackendError> {
        Ok(())
    }

    /// Completes an owned physical interrupt after a trapped guest DIR.
    ///
    /// A normally retired HW-backed LR is already deactivated by the physical
    /// GIC and does not call this method. When DIR traps before hardware can
    /// perform that transition, the backend must issue physical DIR even for
    /// a level source. That deactivation is the architectural resample point:
    /// if the line remains asserted, the host GIC produces a new
    /// acknowledgement through the normal physical-SPI ingress path.
    fn complete_physical_interrupt(
        &self,
        vcpu: GicVcpuId,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        self.deactivate_physical_interrupt(vcpu, binding)
    }

    /// Forcibly deactivates an owned physical interrupt.
    ///
    /// A trapped guest DIR uses [`Self::complete_physical_interrupt`].
    /// This operation is reserved for teardown and rollback, where the source
    /// must be made quiescent regardless of its sampled level.
    fn deactivate_physical_interrupt(
        &self,
        _vcpu: GicVcpuId,
        _binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "deactivate physical interrupt",
            "the backend does not support trapped physical interrupt deactivation",
        ))
    }

    /// Claims one physical interrupt for hardware-backed delivery.
    fn bind_physical_interrupt(
        &self,
        _binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "bind physical interrupt",
            "the backend does not support physical interrupt ownership",
        ))
    }

    /// Enables or disables one owned hardware-backed interrupt.
    fn set_physical_interrupt_enabled(
        &self,
        _binding: PhysicalInterruptBinding,
        _enabled: bool,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "set physical interrupt enable state",
            "the backend does not support physical interrupt ownership",
        ))
    }

    /// Installs one VM-owned physical ITS translation.
    fn bind_physical_msi(&self, _binding: PhysicalMsiBinding) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "bind physical MSI",
            "the backend does not support physical ITS ownership",
        ))
    }

    /// Signals one previously installed physical ITS translation.
    fn signal_physical_msi(&self, _binding: PhysicalMsiBinding) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "signal physical MSI",
            "the backend does not support physical ITS delivery",
        ))
    }

    /// Releases one hardware-backed physical interrupt.
    fn unbind_physical_interrupt(
        &self,
        _binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        Ok(())
    }

    /// Releases one VM-owned physical ITS translation.
    fn unbind_physical_msi(&self, _binding: PhysicalMsiBinding) -> Result<(), GicV3BackendError> {
        Ok(())
    }
}

/// Version-neutral name used by the unified VGIC core.
pub use GicV3Backend as VgicBackend;

/// Backend for software-only tests and architecture-neutral emulation.
#[derive(Debug, Default)]
pub struct SoftwareGicV3Backend;

impl GicV3Backend for SoftwareGicV3Backend {
    fn load_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        _state: &CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        Ok(())
    }

    fn save_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        _state: &mut CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        Ok(())
    }
}

pub(crate) fn backend_result<T>(result: Result<T, GicV3BackendError>) -> VgicResult<T> {
    result.map_err(Into::into)
}
