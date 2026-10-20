//! Pre-EBS setup and post-EBS transaction adapter. Contains no application policy.
use core::{
    ffi::c_void,
    mem::size_of,
    sync::atomic::{AtomicUsize, Ordering},
};

use uefi::{
    Handle, Status,
    boot::{self, MemoryType},
    proto::loaded_image::LoadedImage,
};

use super::{normalize_system_table, physical_alias};
use crate::{
    ArchTrait,
    arch::Arch,
    boot_resources::{self, Allocation, PhysicalRange},
    handoff::{
        ExecutionSnapshot, HandoffError, HandoffSpec,
        protocol::{HandoffRecord, memory_kind::RECLAIMABLE},
        received::loongarch64 as inherited,
        transaction::Transaction,
    },
};

static TABLE: AtomicUsize = AtomicUsize::new(0);
static SYSTEM_TABLE: AtomicUsize = AtomicUsize::new(0);
static IMAGE_START: AtomicUsize = AtomicUsize::new(0);
static IMAGE_END: AtomicUsize = AtomicUsize::new(0);

pub(super) fn prepare_context(system_table: *const c_void, inherited_handoff: bool) {
    SYSTEM_TABLE.store(system_table as usize, Ordering::Release);
    let (table, start, end) = if inherited_handoff {
        let payload = inherited::inherited_payload().expect("missing inherited handoff");
        (
            Arch::virt_to_phys(inherited::inherited_table().unwrap() as *const u8),
            payload.image.start as usize,
            payload.image.end().unwrap() as usize,
        )
    } else {
        let loaded = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
            .expect("missing EFI image");
        let (base, len) = loaded.info();
        let start = physical_alias(base as usize).expect("invalid EFI image physical alias");
        let end = start
            .checked_add(usize::try_from(len).expect("image size overflows"))
            .expect("image range overflows");
        drop(loaded);
        let records = boot_resources::snapshots();
        assert!(
            !records.iter().any(|r| r
                .range
                .overlaps(PhysicalRange::new(start as u64, (end - start) as u64))),
            "boot resource overlaps current image"
        );
        let allocation = boot::allocate_pool(
            MemoryType::RUNTIME_SERVICES_DATA,
            size_of::<HandoffRecord>(),
        )
        .expect("failed to allocate EFI handoff record");
        let table =
            physical_alias(allocation.as_ptr() as usize).expect("invalid EFI handoff alias");
        // SAFETY: exclusive pool allocation; configuration table stays allocated until reset.
        unsafe {
            map(table)
                .cast::<HandoffRecord>()
                .write(HandoffRecord::empty());
            if let Err(error) =
                boot::install_configuration_table(&inherited::HANDOFF_GUID, table as *mut c_void)
            {
                boot::free_pool(allocation).expect("failed to release unpublished handoff table");
                panic!("failed to install EFI handoff table: {error:?}");
            }
        }
        (table, start, end)
    };
    TABLE.store(table, Ordering::Release);
    IMAGE_START.store(start, Ordering::Release);
    IMAGE_END.store(end, Ordering::Release);
}

fn map(physical: usize) -> *mut u8 {
    if Arch::is_mmu_enabled() {
        Arch::_va(physical)
    } else {
        physical as *mut u8
    }
}
pub fn current_image() -> Option<PhysicalRange> {
    let start = IMAGE_START.load(Ordering::Acquire);
    let end = IMAGE_END.load(Ordering::Acquire);
    (start < end).then_some(PhysicalRange::new(start as u64, (end - start) as u64))
}
pub fn flush_instruction_cache() {
    ax_cpu::cache::flush_icache_all();
}

/// Owns a published transaction and the optional archive reservation. The caller
/// separately retains the target image allocation until the invocation completes.
pub struct PreparedHandoff {
    record: alloc::boxed::Box<HandoffRecord>,
    published: bool,
    _archive: Option<Allocation>,
    _transaction: Transaction,
}
impl PreparedHandoff {
    pub fn prepare(
        spec: HandoffSpec<'_>,
        archive: Option<Allocation>,
    ) -> Result<Self, HandoffError> {
        spec.validate()?;
        let current = current_image().ok_or(HandoffError::Unavailable)?;
        if current.overlaps(spec.image) {
            return Err(HandoffError::InvalidTarget);
        }
        let table = TABLE.load(Ordering::Acquire);
        if table == 0 {
            return Err(HandoffError::Unavailable);
        }
        let transaction = Transaction::begin()?;
        // Rebind after the transaction so error paths also release the archive
        // before relinquishing exclusive publication ownership.
        let archive = archive;
        let next = Self::build_record(spec, archive.as_ref())?;
        Ok(Self {
            record: allocate_record(next)?,
            published: false,
            _archive: archive,
            _transaction: transaction,
        })
    }

    /// Publishes the final snapshot after all runtime producers have stopped.
    /// Does not allocate or call firmware services.
    ///
    /// # Safety
    /// The caller must stop all other CPUs, DMA, tasks and IRQs. `context` must
    /// cover every table frame and stack/TLS backing still reachable by entry.
    /// Only the target's irreversible context takeover permits their release.
    pub unsafe fn seal(
        mut self,
        context: &ExecutionSnapshot,
    ) -> Result<SealedHandoff, HandoffError> {
        let mut map = inherited::export_map()?;
        for range in context.ranges() {
            map.assign(*range, RECLAIMABLE)?;
        }
        self.record.memory_map = map.regions;
        self.record.memory_map_len = map.len as u32;
        self.record.validate()?;
        // SAFETY: the unique transaction and terminal runtime exclusion prevent
        // concurrent access. The receiver copies this before reclaiming anything.
        unsafe {
            map_table().write(*self.record);
        }
        self.published = true;
        Ok(SealedHandoff(self))
    }
    fn build_record(
        spec: HandoffSpec<'_>,
        archive: Option<&Allocation>,
    ) -> Result<HandoffRecord, HandoffError> {
        let mut next = HandoffRecord::empty();
        next.image = spec.image;
        next.caller_image = current_image().ok_or(HandoffError::Unavailable)?;
        if let Some(cmdline) = spec.cmdline {
            next.set_cmdline(cmdline)?;
        }
        if let Some(metadata) = spec.metadata {
            next.set_metadata(metadata)?;
        }
        next.resources = boot_resources::snapshots();
        next.resource_count = next.resources.iter().take_while(|r| r.id != 0).count() as u32;
        if let Some(bytes) = spec.initramfs.filter(|bytes| !bytes.is_empty()) {
            let range = archive.ok_or(HandoffError::Unavailable)?.range();
            if range.len < bytes.len() as u64 || range.overlaps(spec.image) {
                return Err(HandoffError::InvalidTarget);
            }
            let source =
                physical_alias(bytes.as_ptr() as usize).ok_or(HandoffError::InvalidTarget)?;
            if range.overlaps(PhysicalRange::new(source as u64, bytes.len() as u64)) {
                return Err(HandoffError::InvalidTarget);
            }
            // SAFETY: archive is an exclusive live allocation, checked above, and
            // the source is a separate borrowed upload. No firmware call occurs.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    map(range.start as usize),
                    bytes.len(),
                );
            }
            next.archive = PhysicalRange::new(range.start, bytes.len() as u64);
        }
        let live = boot_resources::live_snapshots();
        for range in core::iter::once(next.image)
            .chain((next.archive.len != 0).then_some(next.archive))
            .chain(spec.preserve.iter().copied())
        {
            let allocation = *live
                .iter()
                .find(|a| a.resource_id != 0 && a.range.contains(range))
                .ok_or(HandoffError::InvalidTarget)?;
            if !next.allocations[..next.allocation_count as usize].contains(&allocation) {
                let index = next.allocation_count as usize;
                if index == next.allocations.len() {
                    return Err(HandoffError::InvalidProtocol);
                }
                // The full allocation, including target alignment padding, stays live.
                next.allocations[index] = allocation;
                next.allocation_count += 1;
            }
        }
        Ok(next)
    }
}

/// A published transaction which can enter the validated target.
pub struct SealedHandoff(PreparedHandoff);
impl SealedHandoff {
    /// Calls the target's EFI entry with the inherited firmware context.
    ///
    /// # Safety
    /// `entry` must identify executable code with the EFI ABI in the validated
    /// target image. Its allocation must remain live throughout this call. The
    /// caller must quiesce other CPUs, resource allocations and network/device
    /// services, and finish persistent writes before transferring execution.
    /// A returning entry must preserve the caller's stack, MMU mappings,
    /// CPU-local state and interrupt mask. A kernel which has replaced that
    /// execution context must not return through this EFI call frame.
    pub unsafe fn enter(&self, entry: u64) -> Result<(), HandoffError> {
        let address = usize::try_from(entry).map_err(|_| HandoffError::InvalidTarget)?;
        let physical = physical_alias(address).ok_or(HandoffError::InvalidTarget)? as u64;
        if !self
            .0
            .record
            .image
            .contains(PhysicalRange::new(physical, 1))
        {
            return Err(HandoffError::InvalidTarget);
        }
        let handle = super::super::saved_image_handle().ok_or(HandoffError::Unavailable)?;
        let system = SYSTEM_TABLE.load(Ordering::Acquire);
        if system == 0 {
            return Err(HandoffError::Unavailable);
        }
        flush_instruction_cache();
        // SAFETY: the caller guarantees executable EFI code and its lifetime.
        let entry: extern "efiapi" fn(Handle, *const c_void) -> Status =
            unsafe { core::mem::transmute(address) };
        let status = entry(handle, normalize_system_table(system as *const c_void));
        if status == Status::SUCCESS {
            Ok(())
        } else {
            Err(HandoffError::Firmware(status.0))
        }
    }
}
impl Drop for PreparedHandoff {
    fn drop(&mut self) {
        if self.published {
            // SAFETY: the returning entry retained the caller's context. The
            // transaction still owns the table and no receiver runs concurrently.
            unsafe {
                map_table().write(HandoffRecord::empty());
            }
        }
    }
}

fn map_table() -> *mut HandoffRecord {
    map(TABLE.load(Ordering::Acquire)).cast()
}

fn allocate_record(
    record: HandoffRecord,
) -> Result<alloc::boxed::Box<HandoffRecord>, HandoffError> {
    let layout = core::alloc::Layout::new::<HandoffRecord>();
    // SAFETY: nonzero layout for one record. Null is checked before writing;
    // Box receives the same allocation/layout and owns its eventual release.
    let pointer = unsafe { alloc::alloc::alloc(layout) }.cast::<HandoffRecord>();
    if pointer.is_null() {
        return Err(crate::boot_resources::ResourceError::Capacity.into());
    }
    // SAFETY: the allocation is exclusive, suitably aligned and large enough.
    unsafe {
        pointer.write(record);
        Ok(alloc::boxed::Box::from_raw(pointer))
    }
}
