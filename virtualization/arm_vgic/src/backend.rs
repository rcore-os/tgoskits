//! Checked physical GICv3 and CPU-interface capability boundary.

use alloc::string::String;

use crate::{
    CpuInterfaceState, EventId, GicAffinity, GicVcpuId, IntId, ItsDeviceId, LpiId, PhysicalIrqId,
    Priority, PrivateInterruptMask, PrivateInterruptState, TriggerMode, VgicError, VgicResult,
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

/// Guest-controlled state of one assigned physical SPI, excluding its fixed route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalInterruptConfiguration {
    pending: bool,
    active: bool,
    priority: Priority,
    trigger: TriggerMode,
}

impl PhysicalInterruptConfiguration {
    /// Creates a checked physical SPI configuration.
    pub const fn new(
        pending: bool,
        active: bool,
        priority: Priority,
        trigger: TriggerMode,
    ) -> Self {
        Self {
            pending,
            active,
            priority,
            trigger,
        }
    }

    /// Returns whether the SPI is pending.
    pub const fn pending(self) -> bool {
        self.pending
    }

    /// Returns whether the SPI is active.
    pub const fn active(self) -> bool {
        self.active
    }

    /// Returns its guest priority.
    pub const fn priority(self) -> Priority {
        self.priority
    }

    /// Returns its trigger mode.
    pub const fn trigger(self) -> TriggerMode {
        self.trigger
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

    /// Saves host SGI/PPI state and installs one passthrough vCPU's private state.
    ///
    /// Implementations must disable non-owned private interrupts while the
    /// guest is active and restore the host state before returning an error.
    fn load_physical_private_interrupts(
        &self,
        _vcpu: GicVcpuId,
        _owned: PrivateInterruptMask,
        _guest: &PrivateInterruptState,
    ) -> Result<PrivateInterruptState, GicV3BackendError> {
        Err(GicV3BackendError::new(
            "load physical private interrupts",
            "the backend does not support private interrupt context switching",
        ))
    }

    /// Captures guest SGI/PPI state and restores the saved host state.
    fn save_physical_private_interrupts(
        &self,
        _vcpu: GicVcpuId,
        _owned: PrivateInterruptMask,
        _guest: &mut PrivateInterruptState,
        _host: &PrivateInterruptState,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "save physical private interrupts",
            "the backend does not support private interrupt context switching",
        ))
    }

    /// Refreshes a loaded guest snapshot without restoring the host context.
    fn synchronize_physical_private_interrupts(
        &self,
        _vcpu: GicVcpuId,
        _owned: PrivateInterruptMask,
        _guest: &mut PrivateInterruptState,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "synchronize physical private interrupts",
            "the backend does not support private interrupt context switching",
        ))
    }

    /// Applies a guest MMIO update while its physical Redistributor is loaded.
    fn update_physical_private_interrupts(
        &self,
        _vcpu: GicVcpuId,
        _owned: PrivateInterruptMask,
        _guest: &PrivateInterruptState,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "update physical private interrupts",
            "the backend does not support private interrupt context switching",
        ))
    }

    /// Notifies the platform after a software LR no longer owns an interrupt.
    ///
    /// Emulated host-line adapters use this boundary to unmask a physical
    /// level interrupt only after the guest has retired its virtual delivery.
    fn retire_emulated_interrupt(
        &self,
        _vcpu: GicVcpuId,
        _intid: IntId,
    ) -> Result<(), GicV3BackendError> {
        Ok(())
    }

    /// Claims and configures one physical interrupt for direct delivery.
    fn bind_physical_interrupt(
        &self,
        _binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "bind physical interrupt",
            "the backend does not support passthrough interrupts",
        ))
    }

    /// Enables or disables one owned interrupt for direct guest delivery.
    fn set_physical_interrupt_enabled(
        &self,
        _binding: PhysicalInterruptBinding,
        _enabled: bool,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "set physical interrupt enable state",
            "the backend does not support passthrough interrupts",
        ))
    }

    /// Applies guest-controlled state to one owned physical SPI.
    fn configure_physical_interrupt(
        &self,
        _binding: PhysicalInterruptBinding,
        _configuration: PhysicalInterruptConfiguration,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "configure physical interrupt",
            "the backend does not support passthrough interrupt configuration",
        ))
    }

    /// Updates a directly delivered physical level input.
    fn set_physical_interrupt_level(
        &self,
        _binding: PhysicalInterruptBinding,
        _asserted: bool,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "set physical interrupt level",
            "the backend does not support passthrough interrupts",
        ))
    }

    /// Pulses a directly delivered physical input.
    fn pulse_physical_interrupt(
        &self,
        _binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "pulse physical interrupt",
            "the backend does not support passthrough interrupts",
        ))
    }

    /// Sends a physical SGI in passthrough mode.
    fn send_physical_sgi(
        &self,
        _source: GicVcpuId,
        _sgi: crate::SgiId,
        _targets: &[GicAffinity],
    ) -> Result<(), GicV3BackendError> {
        Err(GicV3BackendError::new(
            "send physical SGI",
            "the backend does not support passthrough SGIs",
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

    /// Releases one directly delivered physical interrupt.
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
