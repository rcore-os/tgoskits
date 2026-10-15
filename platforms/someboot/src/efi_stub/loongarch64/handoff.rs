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
    boot_payload::loongarch64 as inherited,
    boot_resources::{self, Allocation, PhysicalRange},
    handoff::{
        HandoffError, HandoffSpec,
        protocol::{HandoffRecord, MAX_REGIONS, MemoryRegion},
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
    image: PhysicalRange,
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
        // SAFETY: the transaction grants exclusive producer ownership; no target runs yet.
        unsafe {
            map(table).cast::<HandoffRecord>().write(next);
        }
        Ok(Self {
            image: spec.image,
            _archive: archive,
            _transaction: transaction,
        })
    }
    fn build_record(
        spec: HandoffSpec<'_>,
        archive: Option<&Allocation>,
    ) -> Result<HandoffRecord, HandoffError> {
        let mut next = HandoffRecord::empty();
        next.image = spec.image;
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
            let owned = next.resources[..next.resource_count as usize]
                .iter()
                .any(|r| PhysicalRange::new(r.range.start, r.used).contains(range));
            if !owned {
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
        let regions = crate::mem::memory_map();
        if regions.len() > MAX_REGIONS {
            return Err(HandoffError::InvalidProtocol);
        }
        for (i, r) in regions.iter().enumerate() {
            next.memory_map[i] = MemoryRegion {
                range: PhysicalRange::new(r.physical_start as u64, r.size_in_bytes as u64),
                kind: match r.memory_type {
                    crate::mem::MemoryType::Free => 0,
                    crate::mem::MemoryType::Ram => 1,
                    crate::mem::MemoryType::KImage => 2,
                    crate::mem::MemoryType::Reserved => 3,
                    crate::mem::MemoryType::Mmio => 4,
                    crate::mem::MemoryType::PerCpuData => 5,
                },
                reserved: 0,
            };
        }
        next.memory_map_len = regions.len() as u32;
        next.validate()?;
        Ok(next)
    }

    /// Calls the target's EFI entry with the inherited firmware context.
    ///
    /// # Safety
    /// `entry` must identify executable code with the EFI ABI in the validated
    /// target image. Its allocation must remain live throughout this call. The
    /// caller must quiesce other CPUs, resource allocations and network/device
    /// services, and finish persistent writes before transferring execution.
    pub unsafe fn enter(&self, entry: u64) -> Result<(), HandoffError> {
        let address = usize::try_from(entry).map_err(|_| HandoffError::InvalidTarget)?;
        let physical = physical_alias(address).ok_or(HandoffError::InvalidTarget)? as u64;
        if !self.image.contains(PhysicalRange::new(physical, 1)) {
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
        // SAFETY: the target has returned; the transaction still owns the table.
        unsafe {
            map(TABLE.load(Ordering::Acquire))
                .cast::<HandoffRecord>()
                .write(HandoffRecord::empty());
        }
    }
}
