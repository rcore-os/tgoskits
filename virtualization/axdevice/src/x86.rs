//! AxVM-facing adapters for OS-neutral x86 virtual interrupt-controller devices.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{
    any::Any,
    marker::PhantomData,
    sync::atomic::{AtomicBool, Ordering},
};

use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceError, IrqLine, IrqResult, Resource,
};
use x86_vlapic::{
    EmulatedIoApic, EmulatedPit, EmulatedSerialPort, IoApicEoi, IoApicInterrupt, X86AccessWidth,
    X86GuestPhysAddr, X86GuestPhysAddrRange, X86Port, X86PortRange, X86SerialBackend,
    X86VlapicHostOps,
};

use crate::DeviceManagerResult;

/// Type-specific IOAPIC capability used by the x86 interrupt runtime.
pub trait X86IoApicDeviceOps: Send + Sync {
    /// Return the guest interrupt vector programmed for a GSI.
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8>;

    /// Assert an IOAPIC GSI and return an interrupt to inject if one is unmasked.
    fn assert_gsi(&self, gsi: usize) -> Option<IoApicInterrupt>;

    /// Broadcast a local APIC EOI to the IOAPIC.
    fn end_of_interrupt(&self, vector: u8) -> Option<IoApicEoi>;
}

/// Runtime delivery side of the IOAPIC-to-local-APIC connection.
pub trait X86IoApicRuntimeOps: Send + Sync {
    /// Returns the guest vector programmed for a GSI when it is deliverable.
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8>;

    /// Signals one GSI and queues any resulting local-APIC message.
    fn signal_gsi(&self, gsi: usize) -> DeviceManagerResult<bool>;

    /// Processes an EOI and queues any level-triggered redelivery.
    fn end_of_interrupt(&self, vector: u8) -> DeviceManagerResult<Option<IoApicEoi>>;
}

/// Type-specific PIT capability used by the x86 interrupt runtime.
pub trait X86PitDeviceOps: Send + Sync {
    /// Delivers a pending PIT IRQ0 edge when the deadline is due.
    fn service_irq0(&self, now_ns: u64) -> IrqResult<bool>;
}

/// Type-specific COM1 capability used by the x86 interrupt runtime.
pub trait X86SerialDeviceOps: Send + Sync {
    /// Polls host input and updates the configured COM1 interrupt line.
    fn service_irq(&self) -> IrqResult<bool>;
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
    irq: Option<IrqLine>,
    name: String,
    resources: Box<[Resource]>,
    _host: PhantomData<fn() -> H>,
}

impl<H: X86VlapicHostOps> X86PitDevice<H> {
    /// Creates a PIT adapter.
    pub fn new() -> Self {
        let inner = EmulatedPit::<H>::new();
        let resources = port_resources(inner.port_ranges());
        Self {
            inner,
            irq: None,
            name: String::from("x86-pit"),
            resources,
            _host: PhantomData,
        }
    }

    /// Returns the wrapped OS-neutral PIT core.
    pub const fn inner(&self) -> &EmulatedPit<H> {
        &self.inner
    }

    /// Creates a PIT adapter connected to its IOAPIC input line.
    pub fn new_with_irq(irq: IrqLine) -> Self {
        let mut device = Self::new();
        device.irq = Some(irq);
        device
    }
}

impl<H: X86VlapicHostOps> Default for X86PitDevice<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: X86VlapicHostOps> X86PitDeviceOps for X86PitDevice<H> {
    fn service_irq0(&self, now_ns: u64) -> IrqResult<bool> {
        if !self.inner.consume_irq0_if_due(now_ns) {
            return Ok(false);
        }
        if let Some(irq) = &self.irq {
            irq.pulse()?;
        }
        Ok(true)
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
pub struct X86SerialPortDevice {
    inner: EmulatedSerialPort,
    irq: Option<IrqLine>,
    edge_asserted: AtomicBool,
    name: String,
    resources: Box<[Resource]>,
}

impl X86SerialPortDevice {
    /// Creates a COM1 adapter with a per-instance byte-stream backend.
    pub fn new_with_backend(backend: Arc<dyn X86SerialBackend>) -> Self {
        let inner = EmulatedSerialPort::new(backend);
        Self::from_inner(inner)
    }

    fn from_inner(inner: EmulatedSerialPort) -> Self {
        let resources = port_resources([inner.address_range()]);
        Self {
            inner,
            irq: None,
            edge_asserted: AtomicBool::new(false),
            name: String::from("x86-serial-com1"),
            resources,
        }
    }

    /// Returns the wrapped OS-neutral COM1 core.
    pub const fn inner(&self) -> &EmulatedSerialPort {
        &self.inner
    }

    /// Creates a COM1 adapter connected to an IRQ and a per-instance backend.
    pub fn new_with_irq_and_backend(irq: IrqLine, backend: Arc<dyn X86SerialBackend>) -> Self {
        let mut device = Self::new_with_backend(backend);
        device.irq = Some(irq);
        device
    }
}

impl X86SerialDeviceOps for X86SerialPortDevice {
    fn service_irq(&self) -> IrqResult<bool> {
        let asserted = self.inner.poll_irq();
        if let Some(irq) = &self.irq {
            match irq.trigger() {
                axvm_types::InterruptTriggerMode::LevelTriggered => {
                    if asserted {
                        irq.raise()?;
                    } else {
                        irq.lower()?;
                    }
                }
                axvm_types::InterruptTriggerMode::EdgeTriggered => {
                    self.update_edge_irq(irq, asserted)?;
                }
            }
        }
        Ok(asserted)
    }
}

impl X86SerialPortDevice {
    fn update_edge_irq(&self, irq: &IrqLine, asserted: bool) -> IrqResult {
        if !asserted {
            self.edge_asserted.store(false, Ordering::Release);
            return Ok(());
        }
        if self
            .edge_asserted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        if let Err(error) = irq.pulse() {
            self.edge_asserted.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }
}

impl Device for X86SerialPortDevice {
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
        let response = if access.is_read {
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
        }?;
        self.service_irq().map_err(|_| DeviceError::Internal)?;
        Ok(response)
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

fn port_resources<const N: usize>(ranges: [X86PortRange; N]) -> Box<[Resource]> {
    ranges
        .into_iter()
        .map(|range| {
            let base = range.start.number();
            let size = range
                .end
                .number()
                .saturating_sub(range.start.number())
                .saturating_add(1);
            Resource::PortRange { base, size }
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
}
