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

/// Describes how external interrupts reach a guest interrupt controller.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InterruptDelivery {
    /// Mediate physical sources and software sources through a VM-local
    /// interrupt controller.
    #[default]
    Mediated,
    /// Deliver only assigned physical interrupt sources directly.
    Direct,
}

impl InterruptDelivery {
    /// Converts the passthrough configuration flag into a validated delivery
    /// policy.
    pub const fn from_passthrough_flag(interrupts_passthrough: bool) -> Self {
        if interrupts_passthrough {
            Self::Direct
        } else {
            Self::Mediated
        }
    }

    /// Returns whether this policy requires physical direct delivery.
    pub const fn is_direct(self) -> bool {
        matches!(self, Self::Direct)
    }
}
