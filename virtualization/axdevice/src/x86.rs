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

use crate::{DeviceManagerError, DeviceManagerResult};

/// Type-specific IOAPIC capability used by the x86 interrupt runtime.
pub trait X86IoApicDeviceOps: Send + Sync {
    /// Return the guest interrupt vector programmed for a GSI.
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8>;

    /// Return the level-triggered GSI currently awaiting this vector's EOI.
    fn in_service_gsi_for_vector(&self, vector: u8) -> Option<usize>;

    /// Assert an IOAPIC GSI and return an interrupt to inject if one is unmasked.
    fn assert_gsi(&self, gsi: usize) -> Option<IoApicInterrupt>;

    /// Broadcast a local APIC EOI to the IOAPIC.
    fn end_of_interrupt(&self, vector: u8) -> Option<IoApicEoi>;
}

/// Runtime delivery side of the IOAPIC-to-local-APIC connection.
pub trait X86IoApicRuntimeOps: Send + Sync {
    /// Returns the guest vector programmed for a GSI when it is deliverable.
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8>;

    /// Returns the level-triggered GSI currently awaiting this vector's EOI.
    fn in_service_gsi_for_vector(&self, vector: u8) -> Option<usize>;

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

/// Protected or physically absent x86 platform MMIO window.
///
/// Reads return the conventional all-ones value for their access width and
/// writes are ignored. The window is always handled in software and never
/// forwards an access to host physical MMIO.
pub struct X86UnassignedMmioDevice {
    base: u64,
    end: u64,
    resources: Box<[Resource]>,
}

impl X86UnassignedMmioDevice {
    /// Creates a checked, non-empty unassigned MMIO window.
    pub fn new(base: u64, size: u64) -> DeviceManagerResult<Self> {
        let end = base
            .checked_add(size)
            .filter(|_| size != 0)
            .ok_or_else(|| DeviceManagerError::InvalidInput {
                operation: "create x86 unassigned MMIO window",
                detail: String::from("range must be non-empty and must not overflow"),
            })?;
        Ok(Self {
            base,
            end,
            resources: alloc::vec![Resource::MmioRange { base, size }].into_boxed_slice(),
        })
    }

    fn contains_access(&self, access: &BusAccess) -> bool {
        access.addr >= self.base
            && access
                .addr
                .checked_add(access.width.size() as u64)
                .is_some_and(|end| end <= self.end)
    }
}

impl Device for X86UnassignedMmioDevice {
    fn name(&self) -> &str {
        "x86-unassigned-mmio"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio || !self.contains_access(access) {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        if !access.is_read {
            return Ok(BusResponse::Write);
        }
        let value = match access.width {
            AccessWidth::Byte => u8::MAX as u64,
            AccessWidth::Word => u16::MAX as u64,
            AccessWidth::Dword => u32::MAX as u64,
            AccessWidth::Qword => u64::MAX,
        };
        Ok(BusResponse::Read { value })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
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

    fn in_service_gsi_for_vector(&self, vector: u8) -> Option<usize> {
        self.inner.in_service_gsi_for_vector(vector)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unassigned_mmio_reads_as_all_ones_and_ignores_writes() {
        let device = X86UnassignedMmioDevice::new(0xfed8_0000, 0x1_0000).unwrap();
        for (width, expected) in [
            (AccessWidth::Byte, u8::MAX as u64),
            (AccessWidth::Word, u16::MAX as u64),
            (AccessWidth::Dword, u32::MAX as u64),
            (AccessWidth::Qword, u64::MAX),
        ] {
            let response = device
                .handle(&BusAccess {
                    kind: BusKind::Mmio,
                    is_read: true,
                    addr: 0xfed8_03c0,
                    width,
                    data: 0,
                })
                .unwrap();
            assert!(matches!(response, BusResponse::Read { value } if value == expected));
        }

        assert!(matches!(
            device
                .handle(&BusAccess {
                    kind: BusKind::Mmio,
                    is_read: false,
                    addr: 0xfed8_03c0,
                    width: AccessWidth::Dword,
                    data: 0,
                })
                .unwrap(),
            BusResponse::Write
        ));
    }

    #[test]
    fn unassigned_mmio_rejects_invalid_ranges_and_out_of_range_accesses() {
        assert!(X86UnassignedMmioDevice::new(0, 0).is_err());
        assert!(X86UnassignedMmioDevice::new(u64::MAX, 2).is_err());

        let device = X86UnassignedMmioDevice::new(0x1000, 4).unwrap();
        assert!(matches!(
            device.handle(&BusAccess {
                kind: BusKind::Mmio,
                is_read: true,
                addr: 0x1002,
                width: AccessWidth::Dword,
                data: 0,
            }),
            Err(DeviceError::OutOfRange { .. })
        ));
    }
}
