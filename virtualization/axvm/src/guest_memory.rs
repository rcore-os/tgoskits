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
pub struct MemoryRevision {
    pub(crate) run: RunId,
    pub(crate) sequence: u64,
}

/// A validated half-open guest range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestRange {
    pub(crate) start: GuestPhysAddr,
    pub(crate) length: usize,
}

impl GuestRange {
    pub fn new(start: GuestPhysAddr, length: usize) -> AxVmResult<Self> {
        if length == 0 || start.as_usize().checked_add(length).is_none() {
            return Err(AxVmError::invalid_config("invalid guest mapping range"));
        }
        Ok(Self { start, length })
    }
}

impl GuestRange {
    /// Returns the guest physical start of the validated range.
    pub const fn start(self) -> GuestPhysAddr {
        self.start
    }
    /// Returns the byte length of the validated range.
    pub const fn length(self) -> usize {
        self.length
    }
}

impl MemoryRevision {
    /// Returns the execution period whose mappings this revision identifies.
    pub const fn run(self) -> RunId {
        self.run
    }
    /// Returns the monotonically increasing mapping revision in that run.
    pub const fn sequence(self) -> u64 {
        self.sequence
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
pub struct MappingLease {
    pub(crate) range: GuestRange,
    pub(crate) host: HostPhysAddr,
    pub(crate) flags: MappingFlags,
    backing: Arc<MemoryBacking>,
    backing_offset: usize,
}

impl MappingLease {
    /// Allocates page-aligned RAM retained until the mapping and all accesses retire.
    ///
    /// This task-context constructor rejects unaligned ranges and device memory.
    pub fn allocate(range: GuestRange, flags: MappingFlags) -> AxVmResult<Self> {
        let page_size = ax_memory_addr::PAGE_SIZE_4K;
        if !range.start.as_usize().is_multiple_of(page_size)
            || !range.length.is_multiple_of(page_size)
            || flags.contains(MappingFlags::DEVICE)
        {
            return Err(AxVmError::invalid_input(
                "allocate guest mapping",
                "RAM range must be page-aligned",
            ));
        }
        let layout = Layout::from_size_align(range.length, page_size).map_err(|_| {
            AxVmError::invalid_input("allocate guest mapping", "invalid RAM allocation layout")
        })?;
        let backing = MemoryBacking::allocate(layout)?;
        let host = crate::host::paging::virt_to_phys(backing.address());
        Self::new(range, host, flags, backing, 0)
    }

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
            || crate::host::paging::virt_to_phys(backing.address() + backing_offset) != host
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

    pub(crate) fn subrange(&self, range: GuestRange) -> AxVmResult<Self> {
        let offset = range
            .start
            .as_usize()
            .checked_sub(self.range.start.as_usize())
            .ok_or_else(|| AxVmError::invalid_config("mapping subrange starts before backing"))?;
        if offset
            .checked_add(range.length)
            .is_none_or(|end| end > self.range.length)
        {
            return Err(AxVmError::invalid_config(
                "mapping subrange exceeds backing",
            ));
        }
        Self::new(
            range,
            self.host + offset,
            self.flags,
            self.backing.clone(),
            self.backing_offset + offset,
        )
    }
}

/// A control-owner transaction that installs RAM or retires a guest range.
pub enum MemoryUpdate {
    /// Installs a mapping with an owned RAM lifetime lease.
    Map(MappingLease),
    /// Removes a range after access and architecture translation retirement.
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
    #[cfg(any(test, target_arch = "x86_64"))]
    mappings: Vec<MappingLease>,
}

impl DecodeMemory {
    pub(crate) fn new(_mappings: Vec<MappingLease>) -> Self {
        Self {
            #[cfg(any(test, target_arch = "x86_64"))]
            mappings: _mappings,
        }
    }

    #[cfg(any(test, target_arch = "x86_64"))]
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
pub struct GuestMemoryPort {
    state: Arc<MemoryState>,
}

impl GuestMemoryPort {
    /// Copies from one revision while retaining its backing for the whole call.
    /// This task-context operation returns an error while access is closed.
    pub fn read_bytes(&self, address: GuestPhysAddr, output: &mut [u8]) -> AxVmResult {
        self.with_access(|access| access.read(address, output))
            .map_err(|error| AxVmError::device("admit guest memory read", error))?
            .map_err(|error| AxVmError::device("copy guest memory read", error))
    }

    /// Copies into guest RAM without returning a guest reference. Access may
    /// block in task context; it is refused during translation publication.
    pub fn write_bytes(&self, address: GuestPhysAddr, input: &[u8]) -> AxVmResult {
        self.with_access(|access| access.write(address, input))
            .map_err(|error| AxVmError::device("admit guest memory write", error))?
            .map_err(|error| AxVmError::device("copy guest memory write", error))
    }

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

    /// Validates an entire RAM buffer before an external operation consumes it.
    /// The access lease retains this exact revision through the operation.
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn with_access_range<T>(
        &self,
        address: GuestPhysAddr,
        length: usize,
        required: MappingFlags,
        operation: impl FnOnce(&mut dyn GuestMemoryAccess) -> T,
    ) -> DeviceResult<T> {
        let mut access = self.acquire()?;
        access.validate_range(address, length, required)?;
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

    /// Withdraws backing ownership from closed ports after hardware retirement.
    pub(crate) fn retire(&self) -> AxVmResult {
        let revision = self.state.publication.lock_unpoisoned().snapshot.revision;
        self.publish(revision, Vec::new())
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
    #[cfg(target_arch = "riscv64")]
    fn validate_range(
        &self,
        start: GuestPhysAddr,
        length: usize,
        required: MappingFlags,
    ) -> DeviceResult {
        let end = start
            .as_usize()
            .checked_add(length)
            .ok_or(DeviceError::OutOfRange {
                addr: start.as_usize() as u64,
            })?;
        let mut current = start.as_usize();
        while current < end {
            let mapping = self
                .snapshot
                .mappings
                .iter()
                .find(|mapping| {
                    let begin = mapping.range.start.as_usize();
                    current >= begin && current - begin < mapping.range.length
                })
                .ok_or(DeviceError::NotFound)?;
            if !mapping.flags.contains(required) {
                return Err(if required.contains(MappingFlags::WRITE) {
                    DeviceError::ReadOnly
                } else {
                    DeviceError::WriteOnly
                });
            }
            current += (mapping.range.length - (current - mapping.range.start.as_usize()))
                .min(end - current);
        }
        Ok(())
    }

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
