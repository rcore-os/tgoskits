//! Reusable x86 device package for OS-neutral x86 virtual devices.
//!
//! This module intentionally lives outside the architecture-neutral runtime
//! core: it is compiled only for x86_64 targets and exposes narrow typed
//! services consumed by AxVM's x86 architecture layer.

use alloc::{boxed::Box, string::String};
use core::marker::PhantomData;

use axdevice_base::*;
use x86_vlapic::*;

use crate::{ServiceCardinality, ServiceKey};

#[path = "x86/acpi_pm_timer.rs"]
mod acpi_pm_timer;
pub use acpi_pm_timer::{X86AcpiPmTimerDevice, X86MonotonicNanos};
#[path = "x86/cmos.rs"]
mod cmos;
pub use cmos::X86CmosDevice;
#[path = "x86/pci_config.rs"]
mod pci_config;
pub use pci_config::X86PciConfigDevice;
#[path = "x86/pic.rs"]
mod pic;
pub use pic::X86PicDevice;

/// Type-specific IOAPIC capability used by the x86 interrupt runtime.
pub trait X86IoApicDeviceOps: Send + Sync {
    /// Return the guest interrupt vector programmed for a GSI.
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8>;

    /// Assert an IOAPIC GSI and return an interrupt to inject if one is unmasked.
    fn assert_gsi(&self, gsi: usize) -> Option<IoApicInterrupt>;

    /// Updates the electrical level of an IOAPIC GSI.
    fn set_gsi_level(&self, gsi: usize, asserted: bool) -> Option<IoApicInterrupt>;

    /// Broadcast a local APIC EOI to the IOAPIC.
    fn end_of_interrupt(&self, vector: u8) -> Option<IoApicEoi>;
}

/// Type-specific PIT capability used by the x86 interrupt runtime.
pub trait X86PitDeviceOps: Send + Sync {
    /// Consume a pending PIT IRQ0 tick if the deadline is due.
    fn consume_irq0_if_due(&self, now_ns: u64) -> bool;
}

/// Type-specific legacy PIC capability used by the x86 timer path.
pub trait X86PicDeviceOps: Send + Sync {
    /// Latch one legacy IRQ edge and return a vector when it is deliverable.
    fn pulse_irq(&self, irq: u8) -> Option<u8>;
}

/// x86 interrupt-controller operations needed by the VM interrupt runtime.
///
/// This is an adapter boundary rather than the IOAPIC device type itself:
/// synthetic and forwarded sources only need to resolve a GSI, assert it, and
/// process guest EOIs.
pub trait X86InterruptDomainOps: Send + Sync {
    /// Returns the guest vector currently programmed for a GSI.
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8>;

    /// Asserts a GSI and returns an interrupt to inject when it is unmasked.
    fn assert_gsi(&self, gsi: usize) -> Option<IoApicInterrupt>;

    /// Processes a guest local-APIC EOI.
    fn end_of_interrupt(&self, vector: u8) -> Option<IoApicEoi>;
}

/// Typed service key for the VM's x86 virtual I/O APIC.
pub struct X86IoApicServiceKey;

impl ServiceKey for X86IoApicServiceKey {
    type Service = dyn X86IoApicDeviceOps;

    const NAME: &'static str = "x86-ioapic";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// Typed service key for the VM's x86 interrupt-domain adapter.
pub struct X86InterruptDomainKey;

impl ServiceKey for X86InterruptDomainKey {
    type Service = dyn X86InterruptDomainOps;

    const NAME: &'static str = "x86-interrupt-domain";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// Typed service key for the VM's x86 virtual PIT.
pub struct X86PitServiceKey;

impl ServiceKey for X86PitServiceKey {
    type Service = dyn X86PitDeviceOps;

    const NAME: &'static str = "x86-pit";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// Typed service key for the VM's guest-owned legacy PIC pair.
pub struct X86PicServiceKey;

impl ServiceKey for X86PicServiceKey {
    type Service = dyn X86PicDeviceOps;

    const NAME: &'static str = "x86-pic";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
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

    fn set_gsi_level(&self, gsi: usize, asserted: bool) -> Option<IoApicInterrupt> {
        self.inner.set_gsi_level(gsi, asserted)
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

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn axdevice_base::DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
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
        Self::new_for_vcpu(0, 0)
    }

    /// Creates a PIT adapter whose IRQ0 targets one VM vCPU.
    pub fn new_for_vcpu(vm_id: usize, vcpu_id: usize) -> Self {
        let inner = EmulatedPit::<H>::new_for_vcpu(vm_id, vcpu_id);
        let resources = EmulatedPit::<H>::port_ranges()
            .map(port_resource)
            .to_vec()
            .into_boxed_slice();
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

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn axdevice_base::DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
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

fn port_resource(range: X86PortRange) -> Resource {
    let base = range.start.number();
    let size = range
        .end
        .number()
        .saturating_sub(range.start.number())
        .saturating_add(1);
    Resource::PortRange { base, size }
}
