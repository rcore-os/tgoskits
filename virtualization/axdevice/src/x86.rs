//! AxVM-facing adapters for OS-neutral x86 virtual interrupt-controller devices.

use alloc::{boxed::Box, string::String};
use core::{any::Any, marker::PhantomData};

use axdevice_base::{AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceError, Resource};
use x86_vlapic::{
    EmulatedIoApic, EmulatedPit, EmulatedSerialPort, IoApicEoi, IoApicInterrupt, X86AccessWidth,
    X86GuestPhysAddr, X86GuestPhysAddrRange, X86Port, X86PortRange, X86VlapicHostOps,
};

/// Type-specific IOAPIC capability used by the x86 interrupt runtime.
pub trait X86IoApicDeviceOps: Send + Sync {
    /// Return the guest interrupt vector programmed for a GSI.
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8>;

    /// Assert an IOAPIC GSI and return an interrupt to inject if one is unmasked.
    fn assert_gsi(&self, gsi: usize) -> Option<IoApicInterrupt>;

    /// Broadcast a local APIC EOI to the IOAPIC.
    fn end_of_interrupt(&self, vector: u8) -> Option<IoApicEoi>;
}

/// Type-specific PIT capability used by the x86 interrupt runtime.
pub trait X86PitDeviceOps: Send + Sync {
    /// Consume a pending PIT IRQ0 tick if the deadline is due.
    fn consume_irq0_if_due(&self, now_ns: u64) -> bool;
}

/// Type-specific COM1 capability used by the x86 interrupt runtime.
pub trait X86SerialDeviceOps: Send + Sync {
    /// Poll host input and return whether COM1 should assert an IRQ.
    fn poll_irq(&self) -> bool;
}

/// Unified-device adapter for [`EmulatedIoApic`].
pub struct X86IoApicDevice {
    inner: EmulatedIoApic,
    name: String,
    resources: Box<[Resource]>,
}

impl X86IoApicDevice {
    /// Creates an IOAPIC adapter with the given guest MMIO range.
    pub fn new(base: X86GuestPhysAddr, size: Option<usize>) -> Self {
        let inner = EmulatedIoApic::new(base, size);
        let resources = mmio_resources(inner.address_range());
        Self {
            inner,
            name: String::from("x86-ioapic"),
            resources,
        }
    }

    /// Returns the wrapped OS-neutral IOAPIC core.
    pub const fn inner(&self) -> &EmulatedIoApic {
        &self.inner
    }
}

impl X86IoApicDeviceOps for X86IoApicDevice {
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8> {
        self.inner.vector_for_gsi(gsi)
    }

    fn assert_gsi(&self, gsi: usize) -> Option<IoApicInterrupt> {
        self.inner.assert_gsi(gsi)
    }

    fn end_of_interrupt(&self, vector: u8) -> Option<IoApicEoi> {
        self.inner.end_of_interrupt(vector)
    }
}

impl Device for X86IoApicDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let addr = X86GuestPhysAddr::from_usize(access.addr as usize);
        let width = x86_access_width(access.width);
        if access.is_read {
            self.inner
                .handle_read(addr, width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
                .map_err(|_| DeviceError::Internal)
        } else {
            self.inner
                .handle_write(addr, width, access.data as usize)
                .map(|_| BusResponse::Write)
                .map_err(|_| DeviceError::Internal)
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Unified-device adapter for [`EmulatedPit`].
pub struct X86PitDevice<H: X86VlapicHostOps> {
    inner: EmulatedPit<H>,
    name: String,
    resources: Box<[Resource]>,
    _host: PhantomData<fn() -> H>,
}

impl<H: X86VlapicHostOps> X86PitDevice<H> {
    /// Creates a PIT adapter.
    pub fn new() -> Self {
        let inner = EmulatedPit::<H>::new();
        let resources = port_resources(inner.address_range());
        Self {
            inner,
            name: String::from("x86-pit"),
            resources,
            _host: PhantomData,
        }
    }

    /// Returns the wrapped OS-neutral PIT core.
    pub const fn inner(&self) -> &EmulatedPit<H> {
        &self.inner
    }
}

impl<H: X86VlapicHostOps> Default for X86PitDevice<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: X86VlapicHostOps> X86PitDeviceOps for X86PitDevice<H> {
    fn consume_irq0_if_due(&self, now_ns: u64) -> bool {
        self.inner.consume_irq0_if_due(now_ns)
    }
}

impl<H: X86VlapicHostOps + 'static> Device for X86PitDevice<H> {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Port {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let port = X86Port::new(
            u16::try_from(access.addr)
                .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?,
        );
        let width = x86_access_width(access.width);
        if access.is_read {
            self.inner
                .handle_read(port, width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
                .map_err(|_| DeviceError::Internal)
        } else {
            self.inner
                .handle_write(port, width, access.data as usize)
                .map(|_| BusResponse::Write)
                .map_err(|_| DeviceError::Internal)
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Unified-device adapter for [`EmulatedSerialPort`].
pub struct X86SerialPortDevice<H: X86VlapicHostOps> {
    inner: EmulatedSerialPort<H>,
    name: String,
    resources: Box<[Resource]>,
    _host: PhantomData<fn() -> H>,
}

impl<H: X86VlapicHostOps> X86SerialPortDevice<H> {
    /// Creates a COM1 adapter.
    pub fn new() -> Self {
        let inner = EmulatedSerialPort::<H>::new();
        let resources = port_resources(inner.address_range());
        Self {
            inner,
            name: String::from("x86-serial-com1"),
            resources,
            _host: PhantomData,
        }
    }

    /// Returns the wrapped OS-neutral COM1 core.
    pub const fn inner(&self) -> &EmulatedSerialPort<H> {
        &self.inner
    }
}

impl<H: X86VlapicHostOps> Default for X86SerialPortDevice<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: X86VlapicHostOps> X86SerialDeviceOps for X86SerialPortDevice<H> {
    fn poll_irq(&self) -> bool {
        self.inner.poll_irq()
    }
}

impl<H: X86VlapicHostOps + 'static> Device for X86SerialPortDevice<H> {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Port {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let port = X86Port::new(
            u16::try_from(access.addr)
                .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?,
        );
        let width = x86_access_width(access.width);
        if access.is_read {
            self.inner
                .handle_read(port, width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
                .map_err(|_| DeviceError::Internal)
        } else {
            self.inner
                .handle_write(port, width, access.data as usize)
                .map(|_| BusResponse::Write)
                .map_err(|_| DeviceError::Internal)
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn x86_access_width(width: AccessWidth) -> X86AccessWidth {
    match width {
        AccessWidth::Byte => X86AccessWidth::Byte,
        AccessWidth::Word => X86AccessWidth::Word,
        AccessWidth::Dword => X86AccessWidth::Dword,
        AccessWidth::Qword => X86AccessWidth::Qword,
    }
}

fn mmio_resources(range: X86GuestPhysAddrRange) -> Box<[Resource]> {
    let base = range.start.as_usize() as u64;
    let size = range.end.as_usize().saturating_sub(range.start.as_usize()) as u64;
    alloc::vec![Resource::MmioRange { base, size }].into_boxed_slice()
}

fn port_resources(range: X86PortRange) -> Box<[Resource]> {
    let base = range.start.number();
    let size = range
        .end
        .number()
        .saturating_sub(range.start.number())
        .saturating_add(1);
    alloc::vec![Resource::PortRange { base, size }].into_boxed_slice()
}
