//! Guest-visible serial resources selected by a machine profile.

use alloc::{string::String, vec::Vec};

use axdevice_base::AccessWidth;

use super::GuestMmioRegion;

/// Guest-visible serial register model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestSerialModel {
    /// 16550-compatible UART.
    Uart16550,
    /// Arm PrimeCell PL011 UART.
    Pl011,
}

/// Guest-visible serial register transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestSerialTransport {
    /// x86 port I/O range.
    Port { base: u16, length: u16 },
    /// Memory-mapped register range.
    Mmio {
        base: usize,
        length: usize,
        /// Address stride expressed as a power-of-two register shift.
        register_shift: u8,
        /// Bus width used to access one register.
        register_width: AccessWidth,
    },
}

/// Machine-owned serial resources selected for one guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestSerialProfile {
    /// Guest-visible UART model.
    pub model: GuestSerialModel,
    /// Register transport and address range.
    pub transport: GuestSerialTransport,
    /// Virtual interrupt-controller input used by the UART.
    pub irq: usize,
    /// UART reference clock in hertz.
    pub clock_hz: u32,
}

/// Firmware identity retained when a host UART is replaced by a virtual UART.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestClockReference {
    /// Firmware phandle of the clock provider.
    pub provider_phandle: u32,
    /// Provider-specific clock specifier cells.
    pub specifier: Vec<u32>,
    /// Physical register windows owned by this provider.
    pub provider_regions: Vec<GuestMmioRegion>,
}

/// Firmware identity retained when a host UART is replaced by a virtual UART.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestSerialFdtIdentity {
    /// Absolute path of the firmware-selected UART node.
    pub node_path: String,
    /// UART node phandle, when supplied by firmware.
    pub node_phandle: Option<u32>,
    /// Effective interrupt-controller phandle.
    pub interrupt_parent: u32,
    /// Raw firmware interrupt specifier.
    pub interrupt_specifier: Vec<u32>,
    /// Original `stdout-path` selector, including any line settings.
    pub stdout_path: String,
    /// Host clock dependencies that must remain protected after replacement.
    pub clock_references: Vec<GuestClockReference>,
}

/// Interrupt encoding used when the common FDT pipeline describes a UART.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestSerialFdtInterrupt {
    /// Arm GIC SPI tuple.
    GicSpi,
    /// RISC-V PLIC source number.
    PlicSource,
}
