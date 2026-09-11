//! Scoped guest-memory operations used by VirtIO queues.

use ax_memory_addr::PhysAddr;
use axaddrspace::GuestMemoryAccessor;
use axdevice_base::{DeviceContext, DmaGrant};
use axvm_types::GuestPhysAddr;

use crate::{VirtioError, VirtioResult};

/// The minimum guest-memory capability required by a VirtIO queue operation.
pub trait GuestMemory {
    /// Reads bytes starting at `guest_addr`.
    fn read(&mut self, guest_addr: GuestPhysAddr, data: &mut [u8]) -> VirtioResult<()>;

    /// Writes bytes starting at `guest_addr`.
    fn write(&mut self, guest_addr: GuestPhysAddr, data: &[u8]) -> VirtioResult<()>;
}

/// Placeholder accessor for runtimes that provide guest memory only through
/// a scoped [`GuestMemory`] capability at queue-processing time.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGuestMemoryAccessor;

impl GuestMemoryAccessor for NoGuestMemoryAccessor {
    fn translate_and_get_limit(&self, _guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
        None
    }
}

/// Adapter for existing address-space accessors used by host tests and
/// standalone device-model users.
pub struct AddressSpaceMemory<'a, T> {
    accessor: &'a T,
}

impl<'a, T> AddressSpaceMemory<'a, T> {
    /// Wraps one address-space accessor for a scoped queue operation.
    pub const fn new(accessor: &'a T) -> Self {
        Self { accessor }
    }
}

impl<T: GuestMemoryAccessor> GuestMemory for AddressSpaceMemory<'_, T> {
    fn read(&mut self, guest_addr: GuestPhysAddr, data: &mut [u8]) -> VirtioResult<()> {
        self.accessor
            .read_buffer(guest_addr, data)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    fn write(&mut self, guest_addr: GuestPhysAddr, data: &[u8]) -> VirtioResult<()> {
        self.accessor
            .write_buffer(guest_addr, data)
            .map_err(|_| VirtioError::InvalidAddress)
    }
}

/// Guest-memory capability adapter for a routed PCI endpoint callback.
///
/// The endpoint supplies its registration-time [`DmaGrant`]; the runtime still
/// validates the grant and the current BME snapshot on every operation through
/// [`DeviceContext`].
pub struct DeviceContextMemory<'a> {
    context: &'a mut dyn DeviceContext,
    grant: &'a DmaGrant,
}

impl<'a> DeviceContextMemory<'a> {
    /// Creates a scoped memory adapter for one endpoint callback.
    pub fn new(context: &'a mut dyn DeviceContext, grant: &'a DmaGrant) -> Self {
        Self { context, grant }
    }
}

impl GuestMemory for DeviceContextMemory<'_> {
    fn read(&mut self, guest_addr: GuestPhysAddr, data: &mut [u8]) -> VirtioResult<()> {
        self.context
            .read_guest_memory(self.grant, guest_addr, data)
            .map_err(|_| VirtioError::MemoryError)
    }

    fn write(&mut self, guest_addr: GuestPhysAddr, data: &[u8]) -> VirtioResult<()> {
        self.context
            .write_guest_memory(self.grant, guest_addr, data)
            .map_err(|_| VirtioError::MemoryError)
    }
}
