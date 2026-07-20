//! VM machine-policy value types shared by configuration and runtime crates.

/// Determines whether a VM derives devices from the host platform or builds a
/// fully virtual platform.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VmMachineMode {
    /// Build a new guest platform without exposing host I/O resources.
    #[default]
    Virtual,
    /// Derive the guest platform from assignable host resources.
    Passthrough,
}

/// Selects the firmware description emitted for a guest.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GuestFirmwareKind {
    /// Let the architecture and boot protocol select the firmware format.
    #[default]
    Auto,
    /// Emit a flattened device tree.
    Fdt,
    /// Emit ACPI tables.
    Acpi,
}

/// Selects how assigned physical interrupt sources are forwarded to a guest.
///
/// This policy does not apply to software interrupt sources. Virtual devices
/// always connect to VM-local controller inputs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PhysicalInterruptPolicy {
    /// Convert an assigned host IRQ into a software-backed controller input.
    #[default]
    Mediated,
    /// Forward an owned host IRQ through a hardware-backed virtual interrupt.
    HardwareForwarded,
}

impl PhysicalInterruptPolicy {
    /// Converts the passthrough configuration flag into a validated delivery
    /// policy.
    pub const fn from_passthrough_flag(interrupts_passthrough: bool) -> Self {
        if interrupts_passthrough {
            Self::HardwareForwarded
        } else {
            Self::Mediated
        }
    }

    /// Returns whether assigned physical sources require hardware forwarding.
    pub const fn uses_hardware_forwarding(self) -> bool {
        matches!(self, Self::HardwareForwarded)
    }
}
