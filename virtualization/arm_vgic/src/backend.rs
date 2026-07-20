//! Checked physical GICv3 and CPU-interface capability boundary.

use alloc::string::String;

use crate::{
    CpuInterfaceState, EventId, GicAffinity, GicVcpuId, IntId, ItsDeviceId, LpiId, PhysicalIrqId,
    VgicError, VgicResult,
};

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
}

impl PhysicalInterruptBinding {
    /// Creates a physical binding. The controller separately validates `guest` as an SPI.
    pub const fn new(
        guest: IntId,
        host: PhysicalIrqId,
        target: GicVcpuId,
        affinity: GicAffinity,
    ) -> Self {
        Self {
            guest,
            host,
            target,
            affinity,
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
}

/// Explicit VM-owned physical ITS translation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalMsiBinding {
    device: ItsDeviceId,
    event: EventId,
    lpi: LpiId,
    target: GicVcpuId,
    affinity: GicAffinity,
}

impl PhysicalMsiBinding {
    /// Creates a complete physical ITS ownership record.
    pub const fn new(
        device: ItsDeviceId,
        event: EventId,
        lpi: LpiId,
        target: GicVcpuId,
        affinity: GicAffinity,
    ) -> Self {
        Self {
            device,
            event,
            lpi,
            target,
            affinity,
        }
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
