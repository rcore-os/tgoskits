//! Revision-bound guest copies and explicit backing ownership.

use std::{
    alloc::Layout,
    sync::{Arc, Mutex},
    vec::Vec,
};

use axdevice_base::{DeviceError, DeviceResult, GuestMemoryAccess};
use axvm_types::{GuestPhysAddr, HostPhysAddr, HostVirtAddr, MappingFlags};

use crate::{AxVmError, AxVmResult, RunId, sync::MutexExt};

/// The translation revision installed for one run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MemoryRevision {
    pub(crate) run: RunId,
    pub(crate) sequence: u64,
}

/// A validated half-open guest range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GuestRange {
    pub(crate) start: GuestPhysAddr,
    pub(crate) length: usize,
}

impl GuestRange {
    pub(crate) fn new(start: GuestPhysAddr, length: usize) -> AxVmResult<Self> {
        if length == 0 || start.as_usize().checked_add(length).is_none() {
            return Err(AxVmError::invalid_config("invalid guest mapping range"));
        }
        Ok(Self { start, length })
    }
}

/// Retains either an allocation or a host resource's existing lifetime owner.
pub(crate) struct MemoryBacking {
    virtual_start: usize,
    length: usize,
    allocation: Option<Layout>,
    _reservation: Option<Arc<dyn Send + Sync>>,
}

impl MemoryBacking {
    pub(crate) fn allocate(layout: Layout) -> AxVmResult<Arc<Self>> {
        if layout.size() == 0 {
            return Err(AxVmError::invalid_config("empty guest backing allocation"));
        }
        // SAFETY: the nonzero layout is retained unchanged until the final
        // backing lease is dropped. A null allocation is never published.
        let address = unsafe { std::alloc::alloc_zeroed(layout) };
        if address.is_null() {
            return Err(AxVmError::OutOfMemory {
                operation: "allocate guest backing",
            });
        }
        Ok(Arc::new(Self {
            virtual_start: address as usize,
            length: layout.size(),
            allocation: Some(layout),
            _reservation: None,
        }))
    }

    /// Retains a mapping authorized by an existing host-resource owner.
    ///
    /// # Safety
    ///
    /// `owner` must preserve a RAM mapping of `length` bytes at `address`,
    /// including CPU access permission, until its final strong reference ends.
    /// This constructor must not be used for MMIO or an unreserved host page.
    pub(crate) unsafe fn reserved(
        address: HostVirtAddr,
        length: usize,
        owner: Arc<dyn Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            virtual_start: address.as_usize(),
            length,
            allocation: None,
            _reservation: Some(owner),
        })
    }

    pub(crate) fn address(&self) -> HostVirtAddr {
        self.virtual_start.into()
    }
}

impl Drop for MemoryBacking {
    fn drop(&mut self) {
        if let Some(layout) = self.allocation {
            // SAFETY: this is the final backing owner. Mapping retirement and
            // access leases retain strong references through their last use.
            unsafe { std::alloc::dealloc(self.virtual_start as *mut u8, layout) };
        }
    }
}

/// A mapping and the ownership needed to keep its physical backing valid.
#[derive(Clone)]
pub(crate) struct MappingLease {
    pub(crate) range: GuestRange,
    pub(crate) host: HostPhysAddr,
    pub(crate) flags: MappingFlags,
    backing: Arc<MemoryBacking>,
    backing_offset: usize,
}

impl MappingLease {
    pub(crate) fn new(
        range: GuestRange,
        host: HostPhysAddr,
        flags: MappingFlags,
        backing: Arc<MemoryBacking>,
        backing_offset: usize,
    ) -> AxVmResult<Self> {
        if backing_offset
            .checked_add(range.length)
            .is_none_or(|end| end > backing.length)
            || host.as_usize().checked_add(range.length).is_none()
            || flags.contains(MappingFlags::DEVICE)
        {
            return Err(AxVmError::invalid_config(
                "mapping exceeds its RAM backing lease",
            ));
        }
        Ok(Self {
            range,
            host,
            flags,
            backing,
            backing_offset,
        })
    }
}

pub(crate) enum MemoryUpdate {
    Map(MappingLease),
    Unmap(GuestRange),
}

struct MemorySnapshot {
    revision: MemoryRevision,
    mappings: Vec<MappingLease>,
}

/// Immutable RAM view prepared before hardware binding for instruction decode.
///
/// This view owns its backing leases. It has no publication mutex or callback;
/// the control owner replaces it only after the corresponding vCPU parks.
pub(crate) struct DecodeMemory {
    mappings: Vec<MappingLease>,
}

impl DecodeMemory {
    pub(crate) fn new(mappings: Vec<MappingLease>) -> Self {
        Self { mappings }
    }

    pub(crate) fn read_byte(&self, address: GuestPhysAddr) -> DeviceResult<u8> {
        let current = address.as_usize();
        let mapping = self
            .mappings
            .iter()
            .find(|mapping| {
                current >= mapping.range.start.as_usize()
                    && current - mapping.range.start.as_usize() < mapping.range.length
            })
            .ok_or(DeviceError::NotFound)?;
        if !mapping.flags.contains(MappingFlags::READ) {
            return Err(DeviceError::WriteOnly);
        }
        let offset = current - mapping.range.start.as_usize();
        let pointer =
            (mapping.backing.virtual_start + mapping.backing_offset + offset) as *const u8;
        // SAFETY: MappingLease validated this RAM range against its backing;
        // this immutable view retains that backing through the byte load. No
        // guest Rust reference is constructed, including under concurrent guest
        // writes or DMA.
        Ok(unsafe { pointer.read_volatile() })
    }
}

struct Publication {
    snapshot: Arc<MemorySnapshot>,
    open: bool,
    accesses: usize,
}

struct MemoryState {
    publication: Mutex<Publication>,
    notify_idle: Box<dyn Fn() + Send + Sync>,
}

/// Copies guest bytes while retaining one complete mapping revision.
#[derive(Clone)]
pub(crate) struct GuestMemoryPort {
    state: Arc<MemoryState>,
}

impl GuestMemoryPort {
    pub(crate) fn new(
        revision: MemoryRevision,
        mappings: Vec<MappingLease>,
        notify_idle: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Arc::new(MemoryState {
                publication: Mutex::new(Publication {
                    snapshot: Arc::new(MemorySnapshot { revision, mappings }),
                    open: true,
                    accesses: 0,
                }),
                notify_idle: Box::new(notify_idle),
            }),
        }
    }

    pub(crate) fn close(&self) {
        self.state.publication.lock_unpoisoned().open = false;
    }

    /// Holds one revision and access admission for an entire scoped DMA call.
    pub(crate) fn with_access<T>(
        &self,
        operation: impl FnOnce(&mut dyn GuestMemoryAccess) -> T,
    ) -> DeviceResult<T> {
        let mut access = self.acquire()?;
        Ok(operation(&mut access))
    }

    pub(crate) fn quiescent(&self) -> bool {
        self.state.publication.lock_unpoisoned().accesses == 0
    }

    pub(crate) fn publish(
        &self,
        revision: MemoryRevision,
        mappings: Vec<MappingLease>,
    ) -> AxVmResult<()> {
        let retired = {
            let mut state = self.state.publication.lock_unpoisoned();
            if state.open || state.accesses != 0 {
                return Err(AxVmError::resource_unavailable(
                    "publish guest memory",
                    "access admission is not quiescent",
                ));
            }
            std::mem::replace(
                &mut state.snapshot,
                Arc::new(MemorySnapshot { revision, mappings }),
            )
        };
        drop(retired);
        Ok(())
    }

    pub(crate) fn reopen(&self, expected: MemoryRevision) -> AxVmResult<()> {
        let mut state = self.state.publication.lock_unpoisoned();
        if state.snapshot.revision != expected {
            return Err(AxVmError::resource_unavailable(
                "open guest memory",
                "unexpected memory revision",
            ));
        }
        state.open = true;
        Ok(())
    }

    fn acquire(&self) -> DeviceResult<AccessLease> {
        let mut publication = self.state.publication.lock_unpoisoned();
        if !publication.open {
            return Err(memory_closed());
        }
        publication.accesses = publication
            .accesses
            .checked_add(1)
            .ok_or_else(memory_closed)?;
        Ok(AccessLease {
            state: self.state.clone(),
            snapshot: publication.snapshot.clone(),
        })
    }
}

struct AccessLease {
    state: Arc<MemoryState>,
    snapshot: Arc<MemorySnapshot>,
}

impl AccessLease {
    fn copy(
        &self,
        start: GuestPhysAddr,
        length: usize,
        write: bool,
        mut byte: impl FnMut(usize, *mut u8),
    ) -> DeviceResult {
        start
            .as_usize()
            .checked_add(length)
            .ok_or(DeviceError::OutOfRange {
                addr: start.as_usize() as u64,
            })?;
        let mut copied = 0;
        while copied < length {
            let current = start.as_usize() + copied;
            let mapping = self
                .snapshot
                .mappings
                .iter()
                .find(|mapping| {
                    let begin = mapping.range.start.as_usize();
                    current >= begin && current - begin < mapping.range.length
                })
                .ok_or(DeviceError::NotFound)?;
            let required = if write {
                MappingFlags::WRITE
            } else {
                MappingFlags::READ
            };
            if !mapping.flags.contains(required) {
                return Err(if write {
                    DeviceError::ReadOnly
                } else {
                    DeviceError::WriteOnly
                });
            }
            let offset = current - mapping.range.start.as_usize();
            let count = (mapping.range.length - offset).min(length - copied);
            let address = mapping.backing.virtual_start + mapping.backing_offset + offset;
            for index in 0..count {
                byte(copied + index, (address + index) as *mut u8);
            }
            if write {
                crate::arch::current::make_guest_memory_visible(address.into(), count);
            }
            copied += count;
        }
        Ok(())
    }
}

impl Drop for AccessLease {
    fn drop(&mut self) {
        let idle = {
            let mut publication = self.state.publication.lock_unpoisoned();
            publication.accesses -= 1;
            publication.accesses == 0 && !publication.open
        };
        if idle {
            (self.state.notify_idle)();
        }
    }
}

impl GuestMemoryAccess for AccessLease {
    fn read(&mut self, addr: GuestPhysAddr, out: &mut [u8]) -> DeviceResult {
        self.copy(addr, out.len(), false, |index, pointer| {
            // SAFETY: the access lease validated this RAM byte and retains its
            // backing. Volatile loads expose no Rust reference to guest bytes.
            out[index] = unsafe { pointer.read_volatile() };
        })
    }

    fn write(&mut self, addr: GuestPhysAddr, data: &[u8]) -> DeviceResult {
        self.copy(addr, data.len(), true, |index, pointer| {
            // SAFETY: the access lease validated write permission and retains
            // the backing. The only Rust slice is the caller-owned input.
            unsafe { pointer.write_volatile(data[index]) };
        })
    }
}

impl GuestMemoryAccess for GuestMemoryPort {
    fn read(&mut self, addr: GuestPhysAddr, output: &mut [u8]) -> DeviceResult {
        self.with_access(|access| access.read(addr, output))?
    }

    fn write(&mut self, addr: GuestPhysAddr, input: &[u8]) -> DeviceResult {
        self.with_access(|access| access.write(addr, input))?
    }
}

fn memory_closed() -> DeviceError {
    DeviceError::InvalidState {
        operation: "access guest memory",
        detail: "memory access admission is closed".into(),
    }
}
