//! Errors produced while validating and planning a VM machine.

use alloc::string::{String, ToString};

/// Result returned by VM machine planning operations.
pub type MachinePlanResult<T> = Result<T, MachinePlanError>;

/// A deterministic validation or allocation failure in VM machine planning.
#[derive(Debug, thiserror::Error)]
pub enum MachinePlanError {
    /// A typed identifier was empty.
    #[error("{kind} identifier must not be empty")]
    EmptyIdentifier {
        /// Identifier domain being validated.
        kind: &'static str,
    },
    /// An address range was empty or overflowed.
    #[error("invalid address range at {base:#x} with size {size:#x}")]
    InvalidAddressRange {
        /// Range base.
        base: u64,
        /// Range size.
        size: u64,
    },
    /// A port-I/O range was empty or exceeded the 16-bit address space.
    #[error("invalid port-I/O range at {base:#x} with size {size:#x}")]
    InvalidPortRange {
        /// First port.
        base: u16,
        /// Number of ports.
        size: u16,
    },
    /// A device requirement used an invalid alignment.
    #[error("invalid alignment {alignment:#x} for resource size {size:#x}")]
    InvalidAlignment {
        /// Requested resource size.
        size: u64,
        /// Requested alignment.
        alignment: u64,
    },
    /// An interrupt pool was empty.
    #[error("invalid interrupt pool {start}..={end}")]
    InvalidInterruptPool {
        /// First interrupt ID.
        start: u32,
        /// Last interrupt ID.
        end: u32,
    },
    /// A machine request contained no virtual CPU.
    #[error("VM machine planning requires at least one vCPU")]
    InvalidVcpuCount,
    /// Host firmware could not be normalized safely.
    #[error("invalid host firmware: {detail}")]
    InvalidFirmware {
        /// Parser or representability detail.
        detail: String,
    },
    /// Trusted assignment authority contradicted firmware ownership policy.
    #[error(
        "host device '{device}' with ownership {ownership:?} cannot use assignment authority \
         {assignment:?}"
    )]
    InvalidHostDeviceAssignment {
        /// Stable host device identity.
        device: String,
        /// Ownership classification derived from normalized platform data.
        ownership: super::HostDeviceOwnership,
        /// Trusted authority requested by the platform adapter.
        assignment: super::HostDeviceAssignment,
    },
    /// A finalized guest firmware description could not be encoded.
    #[error("failed to encode guest firmware: {detail}")]
    FirmwareEncoding {
        /// Concrete writer or validation failure.
        detail: String,
    },
    /// Two virtual devices used the same stable instance identity.
    #[error("duplicate virtual device instance '{id}'")]
    DuplicateDeviceInstance {
        /// Duplicate instance identifier.
        id: String,
    },
    /// A deny or explicit host selector matched no host device.
    #[error("host selector '{selector}' matched no device")]
    HostSelectorNotFound {
        /// Human-readable selector.
        selector: String,
    },
    /// An explicit host template was already consumed by another virtual device.
    #[error("host device '{device}' is already used as a virtual-device template")]
    HostTemplateAlreadyUsed {
        /// Conflicting host device identifier.
        device: String,
    },
    /// A host template did not contain enough resources for the virtual model.
    #[error("host template '{device}' cannot satisfy {resource} requirement {index}")]
    HostTemplateResourceMismatch {
        /// Host device identifier.
        device: String,
        /// Resource domain.
        resource: &'static str,
        /// Requirement index.
        index: usize,
    },
    /// A host template's electrical trigger semantics differ from the model.
    #[error(
        "host template '{device}' interrupt {index} has trigger {actual:?}, expected {expected:?}"
    )]
    HostTemplateInterruptTriggerMismatch {
        /// Host firmware device identity.
        device: String,
        /// Interrupt resource index within the device.
        index: usize,
        /// Trigger required by the virtual model.
        expected: axvm_types::InterruptTriggerMode,
        /// Trigger described by host firmware.
        actual: axvm_types::InterruptTriggerMode,
    },
    /// Two passthrough devices described one controller input inconsistently.
    #[error(
        "passthrough devices '{first_device}' and '{second_device}' describe controller input \
         {input} differently"
    )]
    ConflictingHostInterrupt {
        /// Shared controller input whose route metadata disagreed.
        input: u32,
        /// First device that described the input.
        first_device: String,
        /// Later device whose description conflicted.
        second_device: String,
    },
    /// An architecture profile omitted a pool required by a configured model.
    #[error("machine profile has no {resource} pool required by virtual device '{device}'")]
    MissingResourcePool {
        /// Resource domain.
        resource: &'static str,
        /// Stable device instance.
        device: String,
    },
    /// A host template was requested for a fully virtual machine.
    #[error("virtual machine device '{device}' cannot select a host template")]
    HostTemplateInVirtualMachine {
        /// Virtual device identifier.
        device: String,
    },
    /// Named resources could not be materialized for a model build.
    #[error("failed to resolve resources for virtual device '{device}': {source}")]
    DeviceResource {
        /// Stable device instance identifier.
        device: String,
        /// Device-framework validation error.
        #[source]
        source: axdevice::DeviceManagerError,
    },
    /// Direct physical interrupt delivery was selected for a software source.
    #[error(
        "virtual device '{device}' requires a software interrupt, which direct delivery cannot \
         provide"
    )]
    SoftwareInterruptWithDirectDelivery {
        /// Virtual device identifier.
        device: String,
    },
    /// Direct delivery was selected for a fully virtual machine.
    #[error("direct interrupt delivery is valid only for a passthrough machine")]
    DirectDeliveryInVirtualMachine,
    /// The live host platform changed after the plan was produced.
    #[error(
        "host platform snapshot changed while claiming devices: planned generation {planned}, \
         current generation {current}"
    )]
    SnapshotGenerationChanged {
        /// Generation recorded in the immutable plan.
        planned: u64,
        /// Generation observed immediately before claiming resources.
        current: u64,
    },
    /// A host device could not be claimed for the VM.
    #[error("failed to claim host device '{device}': {detail}")]
    ClaimRejected {
        /// Stable identity of the rejected device.
        device: String,
        /// Platform-specific rejection detail.
        detail: String,
    },
    /// A resource allocator rejected a reservation or allocation.
    #[error("failed to allocate {resource} for '{owner}': {source}")]
    ResourceAllocation {
        /// Resource domain.
        resource: &'static str,
        /// Resource owner or planning phase.
        owner: String,
        /// Allocator error.
        #[source]
        source: vm_allocator::Error,
    },
}

impl From<vm_fdt::Error> for MachinePlanError {
    fn from(error: vm_fdt::Error) -> Self {
        Self::FirmwareEncoding {
            detail: error.to_string(),
        }
    }
}
