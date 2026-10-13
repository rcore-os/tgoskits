#[cfg(all(efi, target_arch = "loongarch64"))]
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(all(efi, target_arch = "loongarch64"))]
use crate::mem::KIMAGE_MAP_ALIGN;
#[cfg(efi)]
use crate::mem::{MemoryDescriptor, MemoryType, add_memory_descriptor};

static PUBLISHED: AtomicBool = AtomicBool::new(false);
static START: AtomicUsize = AtomicUsize::new(0);
static END: AtomicUsize = AtomicUsize::new(0);
static RECLAIMABLE: AtomicBool = AtomicBool::new(false);
#[cfg(efi)]
static PENDING_START: AtomicUsize = AtomicUsize::new(0);
#[cfg(efi)]
static PENDING_END: AtomicUsize = AtomicUsize::new(0);

#[cfg(all(efi, target_arch = "loongarch64"))]
/// LoongArch-only v2 payload ABI. This GUID is intentionally distinct from the
/// removed host-boot-abi v1 table, so an old table cannot be interpreted here.
pub(crate) const LOONG_BOOT_PAYLOAD_GUID: uefi::Guid =
    uefi::guid!("5d34b5d2-9a5b-4d0f-8e3c-6bb3dfc6a1fe");
#[cfg(all(efi, target_arch = "loongarch64"))]
const LOONG_BOOT_PAYLOAD_MAGIC: u64 = 0x5447_4f53_494e_4954;
#[cfg(all(efi, target_arch = "loongarch64"))]
const LOONG_BOOT_PAYLOAD_VERSION: u32 = 2;
#[cfg(all(efi, target_arch = "loongarch64"))]
const LOONG_BOOT_PAYLOAD_MAX_CMDLINE: usize = 4095;
#[cfg(all(efi, target_arch = "loongarch64"))]
const LOONG_BOOT_PAYLOAD_MAX_REGIONS: usize = 128;
#[cfg(all(efi, target_arch = "loongarch64"))]
const LOONGARCH_PHYSICAL_ADDRESS_LIMIT: u64 = 1 << 48;
#[cfg(all(efi, target_arch = "loongarch64"))]
pub(crate) const LOONG_IMAGE_POOL_BYTES: u64 = 8192 * 4096;
#[cfg(all(efi, target_arch = "loongarch64"))]
pub(crate) const LOONG_ARCHIVE_POOL_BYTES: u64 = 65536 * 4096;
#[cfg(all(efi, target_arch = "loongarch64"))]
pub(crate) const LOONG_SLOT_POOL_BYTES: u64 = 8192 * 4096;

#[cfg(all(efi, target_arch = "loongarch64"))]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct LoongBootMemoryRegion {
    pub physical_start: u64,
    pub size_in_bytes: u64,
    pub memory_type: u32,
    pub reserved: u32,
}

#[cfg(all(efi, target_arch = "loongarch64"))]
#[repr(C)]
struct LoongBootPayloadHeader {
    magic: u64,
    version: u32,
    size: u32,
    reserved: u32,
}

#[cfg(all(efi, target_arch = "loongarch64"))]
#[repr(C)]
pub(crate) struct LoongBootPayload {
    magic: u64,
    version: u32,
    size: u32,
    reserved: u32,
    pub archive_start: u64,
    pub archive_len: u64,
    pub cmdline_len: u32,
    pub cmdline: [u8; LOONG_BOOT_PAYLOAD_MAX_CMDLINE],
    pub target_image_start: u64,
    pub target_image_len: u64,
    pub image_pool_start: u64,
    pub image_pool_len: u64,
    pub image_pool_cursor: u64,
    pub archive_pool_start: u64,
    pub archive_pool_len: u64,
    pub slot_pool_start: u64,
    pub slot_pool_len: u64,
    pub slot_pool_cursor: u64,
    pub current_image_start: u64,
    pub current_image_len: u64,
    pub loader_slot: u8,
    pub loader_slot_reserved: [u8; 7],
    pub memory_map_len: u32,
    pub memory_map: [LoongBootMemoryRegion; LOONG_BOOT_PAYLOAD_MAX_REGIONS],
}

#[cfg(all(efi, target_arch = "loongarch64"))]
impl LoongBootPayload {
    #[cfg(feature = "efi-loader")]
    pub const fn empty() -> Self {
        Self {
            magic: LOONG_BOOT_PAYLOAD_MAGIC,
            version: LOONG_BOOT_PAYLOAD_VERSION,
            size: size_of::<Self>() as u32,
            reserved: 0,
            archive_start: 0,
            archive_len: 0,
            cmdline_len: 0,
            cmdline: [0; LOONG_BOOT_PAYLOAD_MAX_CMDLINE],
            target_image_start: 0,
            target_image_len: 0,
            image_pool_start: 0,
            image_pool_len: 0,
            image_pool_cursor: 0,
            archive_pool_start: 0,
            archive_pool_len: 0,
            slot_pool_start: 0,
            slot_pool_len: 0,
            slot_pool_cursor: 0,
            current_image_start: 0,
            current_image_len: 0,
            loader_slot: u8::MAX,
            loader_slot_reserved: [0; 7],
            memory_map_len: 0,
            memory_map: [LoongBootMemoryRegion {
                physical_start: 0,
                size_in_bytes: 0,
                memory_type: 3,
                reserved: 0,
            }; LOONG_BOOT_PAYLOAD_MAX_REGIONS],
        }
    }

    #[cfg(feature = "efi-loader")]
    pub fn set_cmdline(&mut self, cmdline: &str) -> Result<(), &'static str> {
        if cmdline.len() > LOONG_BOOT_PAYLOAD_MAX_CMDLINE
            || !cmdline
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic())
        {
            return Err("invalid host command line");
        }
        self.cmdline[..cmdline.len()].copy_from_slice(cmdline.as_bytes());
        self.cmdline_len = cmdline.len() as u32;
        Ok(())
    }

    pub fn cmdline(&self) -> &str {
        core::str::from_utf8(&self.cmdline[..self.cmdline_len as usize])
            .expect("validated LoongArch host command line")
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.magic != LOONG_BOOT_PAYLOAD_MAGIC
            || self.version != LOONG_BOOT_PAYLOAD_VERSION
            || self.size as usize != size_of::<Self>()
            || self.reserved != 0
            || self.cmdline_len as usize > LOONG_BOOT_PAYLOAD_MAX_CMDLINE
            || (self.archive_start == 0) != (self.archive_len == 0)
            || self.archive_start.checked_add(self.archive_len).is_none()
            || (self.target_image_start == 0) != (self.target_image_len == 0)
            || self
                .target_image_start
                .checked_add(self.target_image_len)
                .is_none()
            || !valid_pool(
                self.image_pool_start,
                self.image_pool_len,
                self.image_pool_cursor,
            )
            || !valid_pool(
                self.slot_pool_start,
                self.slot_pool_len,
                self.slot_pool_cursor,
            )
            || !valid_range(self.archive_pool_start, self.archive_pool_len)
            || !valid_range(self.current_image_start, self.current_image_len)
            || !matches!(self.loader_slot, 0 | 1 | u8::MAX)
            || self.loader_slot_reserved != [0; 7]
            || self.memory_map_len as usize > LOONG_BOOT_PAYLOAD_MAX_REGIONS
        {
            return Err("invalid LoongArch EFI payload");
        }
        if !valid_nonempty_pool(
            self.image_pool_start,
            self.image_pool_len,
            self.image_pool_cursor,
        ) || !valid_nonempty_pool(
            self.archive_pool_start,
            self.archive_pool_len,
            self.archive_pool_start,
        ) || !valid_nonempty_pool(
            self.slot_pool_start,
            self.slot_pool_len,
            self.slot_pool_cursor,
        ) {
            return Err("invalid LoongArch EFI pool");
        }
        if self.image_pool_len != LOONG_IMAGE_POOL_BYTES
            || self.archive_pool_len != LOONG_ARCHIVE_POOL_BYTES
            || self.slot_pool_len != LOONG_SLOT_POOL_BYTES
        {
            return Err("unexpected LoongArch EFI pool size");
        }
        if ranges_overlap(
            self.image_pool_start,
            self.image_pool_len,
            self.archive_pool_start,
            self.archive_pool_len,
        ) || ranges_overlap(
            self.image_pool_start,
            self.image_pool_len,
            self.slot_pool_start,
            self.slot_pool_len,
        ) || ranges_overlap(
            self.archive_pool_start,
            self.archive_pool_len,
            self.slot_pool_start,
            self.slot_pool_len,
        ) {
            return Err("overlapping LoongArch EFI pools");
        }
        if self.archive_len != 0
            && !range_contains(
                self.archive_pool_start,
                self.archive_pool_len,
                self.archive_start,
                self.archive_len,
            )
        {
            return Err("EFI archive is outside its pool");
        }
        if self.target_image_len != 0
            && !range_in_any_pool(
                self.target_image_start,
                self.target_image_len,
                self.image_pool_start,
                self.image_pool_len,
                self.slot_pool_start,
                self.slot_pool_len,
            )
        {
            return Err("EFI target image is outside its pool");
        }
        if self.current_image_start == 0 || self.current_image_len == 0 {
            return Err("missing current EFI image");
        }
        if self.current_image_start % 4096 != 0 || self.current_image_len % 4096 != 0 {
            return Err("unaligned current EFI image");
        }
        if self.target_image_len != 0
            && (self.target_image_start % 4096 != 0 || self.target_image_len % 4096 != 0)
        {
            return Err("unaligned target EFI image");
        }
        if self.target_image_len == 0
            && (ranges_overlap(
                self.current_image_start,
                self.current_image_len,
                self.image_pool_start,
                self.image_pool_len,
            ) || ranges_overlap(
                self.current_image_start,
                self.current_image_len,
                self.slot_pool_start,
                self.slot_pool_len,
            ) || ranges_overlap(
                self.current_image_start,
                self.current_image_len,
                self.archive_pool_start,
                self.archive_pool_len,
            ))
        {
            return Err("current EFI image overlaps an EFI pool");
        }
        if self.target_image_len != 0
            && (self.current_image_start != self.target_image_start
                || self.current_image_len != self.target_image_len)
        {
            return Err("current EFI image does not match target image");
        }
        if self.cmdline_len != 0 {
            let cmdline = core::str::from_utf8(&self.cmdline[..self.cmdline_len as usize])
                .map_err(|_| "invalid LoongArch EFI command line")?;
            if !cmdline
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic())
            {
                return Err("invalid LoongArch EFI command line");
            }
        }
        for region in &self.memory_map[..self.memory_map_len as usize] {
            if region.size_in_bytes == 0
                || !valid_memory_range(region.physical_start, region.size_in_bytes)
                || region.physical_start % 4096 != 0
                || region.size_in_bytes % 4096 != 0
                || region.memory_type > 5
                || region.reserved != 0
            {
                return Err("invalid LoongArch EFI memory map");
            }
        }
        for (index, region) in self.memory_map[..self.memory_map_len as usize]
            .iter()
            .enumerate()
        {
            let region_end = region
                .physical_start
                .checked_add(region.size_in_bytes)
                .ok_or("invalid LoongArch EFI memory map")?;
            for other in &self.memory_map[index + 1..self.memory_map_len as usize] {
                let other_end = other
                    .physical_start
                    .checked_add(other.size_in_bytes)
                    .ok_or("invalid LoongArch EFI memory map")?;
                if region.physical_start < other_end && other.physical_start < region_end {
                    return Err("overlapping LoongArch EFI memory map");
                }
            }
        }
        if self.target_image_len != 0 {
            let target_end = self
                .target_image_start
                .checked_add(self.target_image_len)
                .ok_or("invalid LoongArch EFI target image")?;
            if !self.memory_map[..self.memory_map_len as usize]
                .iter()
                .any(|region| {
                    region.physical_start <= self.target_image_start
                        && region
                            .physical_start
                            .checked_add(region.size_in_bytes)
                            .is_some_and(|region_end| target_end <= region_end)
                })
            {
                return Err("EFI target image is absent from the memory map");
            }
        }
        Ok(())
    }
}

#[cfg(all(efi, target_arch = "loongarch64"))]
fn valid_range(start: u64, len: u64) -> bool {
    (start == 0) == (len == 0)
        && start < LOONGARCH_PHYSICAL_ADDRESS_LIMIT
        && start
            .checked_add(len)
            .is_some_and(|end| end <= LOONGARCH_PHYSICAL_ADDRESS_LIMIT)
}

#[cfg(all(efi, target_arch = "loongarch64"))]
fn valid_memory_range(start: u64, len: u64) -> bool {
    len != 0
        && start < LOONGARCH_PHYSICAL_ADDRESS_LIMIT
        && start
            .checked_add(len)
            .is_some_and(|end| end <= LOONGARCH_PHYSICAL_ADDRESS_LIMIT)
}

#[cfg(all(efi, target_arch = "loongarch64"))]
fn valid_pool(start: u64, len: u64, cursor: u64) -> bool {
    valid_range(start, len)
        && (start == 0
            || (start % 4096 == 0
                && cursor >= start
                && cursor <= start.saturating_add(len)
                && cursor % 4096 == 0))
}

#[cfg(all(efi, target_arch = "loongarch64"))]
fn valid_nonempty_pool(start: u64, len: u64, cursor: u64) -> bool {
    valid_pool(start, len, cursor) && len != 0 && len % 4096 == 0
}

#[cfg(all(efi, target_arch = "loongarch64"))]
fn range_contains(outer_start: u64, outer_len: u64, inner_start: u64, inner_len: u64) -> bool {
    valid_range(outer_start, outer_len)
        && valid_range(inner_start, inner_len)
        && (inner_len == 0
            || (inner_start >= outer_start
                && inner_start.checked_add(inner_len).is_some_and(|inner_end| {
                    outer_start
                        .checked_add(outer_len)
                        .is_some_and(|outer_end| inner_end <= outer_end)
                })))
}

#[cfg(all(efi, target_arch = "loongarch64"))]
fn range_in_any_pool(
    start: u64,
    len: u64,
    first_start: u64,
    first_len: u64,
    second_start: u64,
    second_len: u64,
) -> bool {
    range_contains(first_start, first_len, start, len)
        || range_contains(second_start, second_len, start, len)
}

#[cfg(all(efi, target_arch = "loongarch64"))]
fn ranges_overlap(first_start: u64, first_len: u64, second_start: u64, second_len: u64) -> bool {
    first_len != 0
        && second_len != 0
        && first_start < second_start.saturating_add(second_len)
        && second_start < first_start.saturating_add(first_len)
}

#[cfg(all(efi, target_arch = "loongarch64"))]
static INHERITED_PAYLOAD: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", efi, target_arch = "loongarch64"))]
static INHERITED_LOADER_SLOT: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Physical host archive pinned until the runtime finishes unpacking it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitramfsRange {
    pub start: usize,
    pub end: usize,
    pub reclaimable: bool,
}

pub fn initramfs_range() -> Option<InitramfsRange> {
    let end = END.load(Ordering::Acquire);
    (end != 0).then(|| InitramfsRange {
        start: START.load(Ordering::Relaxed),
        end,
        reclaimable: RECLAIMABLE.load(Ordering::Relaxed),
    })
}

/// Transfers the reserved archive to its sole runtime consumer.
///
/// The caller must finish every archive borrow before reclaiming owned pages.
/// Subsequent callers and metadata queries cannot observe the consumed range.
pub fn take_initramfs_range() -> Option<InitramfsRange> {
    let end = END.swap(0, Ordering::AcqRel);
    (end != 0).then(|| InitramfsRange {
        start: START.load(Ordering::Relaxed),
        end,
        reclaimable: RECLAIMABLE.load(Ordering::Relaxed),
    })
}

#[cfg(all(efi, target_arch = "loongarch64"))]
pub(crate) fn remember_inherited_payload(payload: *const LoongBootPayload) {
    assert!(!payload.is_null(), "null inherited LoongArch EFI payload");
    // Read only the fixed header before interpreting the complete table. The
    // producer publishes the exact allocation size so a truncated or older
    // configuration table cannot make the consumer read past its boundary.
    let header = unsafe { &*(payload.cast::<LoongBootPayloadHeader>()) };
    if header.magic != LOONG_BOOT_PAYLOAD_MAGIC
        || header.version != LOONG_BOOT_PAYLOAD_VERSION
        || header.size as usize != size_of::<LoongBootPayload>()
        || header.reserved != 0
    {
        panic!("invalid LoongArch EFI payload header");
    }
    // SAFETY: the validated header comes from the trusted EFI configuration
    // table and keeps the complete allocation live for the inherited handoff.
    let payload_ref = unsafe { &*payload };
    payload_ref
        .validate()
        .expect("invalid inherited LoongArch EFI payload");
    INHERITED_PAYLOAD.store(payload as usize, Ordering::Release);
    #[cfg(feature = "efi-loader")]
    let slot = payload_ref.loader_slot;
    #[cfg(feature = "efi-loader")]
    INHERITED_LOADER_SLOT.store(slot as usize, Ordering::Release);
}

#[cfg(all(feature = "efi-loader", efi, target_arch = "loongarch64"))]
pub(crate) fn inherited_payload() -> Option<&'static LoongBootPayload> {
    let address = INHERITED_PAYLOAD.load(Ordering::Acquire);
    (address != 0).then(|| unsafe { &*(address as *const LoongBootPayload) })
}

#[cfg(all(feature = "efi-loader", efi, target_arch = "loongarch64"))]
pub fn inherited_loader_slot() -> Option<u8> {
    let slot = INHERITED_LOADER_SLOT.load(Ordering::Acquire);
    (slot <= 1).then_some(slot as u8)
}

#[cfg(all(efi, target_arch = "loongarch64"))]
pub(crate) fn restore_inherited_memory_map() {
    let address = INHERITED_PAYLOAD.swap(0, Ordering::AcqRel);
    if address == 0 {
        return;
    }
    // SAFETY: the first loader retains this runtime configuration-table allocation
    // for the entire inherited target boot.
    let payload = unsafe { &*(address as *const LoongBootPayload) };
    payload
        .validate()
        .expect("invalid inherited LoongArch EFI payload");
    let target = if payload.target_image_len == 0 {
        None
    } else {
        let start =
            usize::try_from(payload.target_image_start).expect("target image address overflows");
        let end = usize::try_from(
            payload
                .target_image_start
                .checked_add(payload.target_image_len)
                .expect("target image range overflows"),
        )
        .expect("target image end overflows")
        .checked_add(KIMAGE_MAP_ALIGN - 1)
        .expect("target image alignment overflows")
            / KIMAGE_MAP_ALIGN
            * KIMAGE_MAP_ALIGN;
        let aligned_len = end
            .checked_sub(start)
            .expect("target image alignment underflows");
        if !range_in_any_pool(
            payload.target_image_start,
            aligned_len as u64,
            payload.image_pool_start,
            payload.image_pool_len,
            payload.slot_pool_start,
            payload.slot_pool_len,
        ) {
            panic!("aligned target image exceeds its EFI pool");
        }
        Some((start, end))
    };
    for region in &payload.memory_map[..payload.memory_map_len as usize] {
        let start = usize::try_from(region.physical_start).expect("memory address overflows");
        let end = usize::try_from(
            region
                .physical_start
                .checked_add(region.size_in_bytes)
                .expect("memory range overflows"),
        )
        .expect("memory end overflows");
        let memory_type = match region.memory_type {
            0 => MemoryType::Free,
            1 => MemoryType::Ram,
            2 => MemoryType::KImage,
            3 => MemoryType::Reserved,
            4 => MemoryType::Mmio,
            5 => MemoryType::PerCpuData,
            _ => unreachable!("validated inherited memory type"),
        };
        let ranges = if let Some((image_start, image_end)) = target
            && start < image_end
            && end > image_start
        {
            [(start, end.min(image_start)), (start.max(image_end), end)]
        } else {
            [(start, end), (0, 0)]
        };
        for (range_start, range_end) in ranges {
            if range_start < range_end {
                add_memory_descriptor(MemoryDescriptor::new_with_range(
                    range_start..range_end,
                    memory_type,
                ))
                .unwrap_or_else(|error| {
                    panic!("failed to restore inherited memory map: {error:?}")
                });
            }
        }
    }
}

pub(crate) fn publish(start: usize, end: usize, reclaimable: bool) {
    // Only the boot CPU publishes, before secondary CPUs or archive consumers
    // can run. AArch64 exclusive atomics cannot be used before MMU enablement.
    assert!(start < end && !PUBLISHED.load(Ordering::Relaxed));
    PUBLISHED.store(true, Ordering::Relaxed);
    RECLAIMABLE.store(reclaimable, Ordering::Relaxed);
    START.store(start, Ordering::Relaxed);
    END.store(end, Ordering::Release);
}

/// Registers a UEFI loader allocation before ExitBootServices. It remains
/// invisible to the runtime until the final firmware map has reserved it.
#[cfg(efi)]
pub(crate) fn stage_uefi(start: usize, end: usize) {
    assert!(start < end && PENDING_END.load(Ordering::Relaxed) == 0);
    PENDING_START.store(start, Ordering::Relaxed);
    PENDING_END.store(end, Ordering::Release);
}

#[cfg(efi)]
pub(crate) fn reserve_staged_uefi() {
    let end = PENDING_END.load(Ordering::Acquire);
    if end == 0 {
        return;
    }
    let start = PENDING_START.load(Ordering::Relaxed);
    assert!(end.checked_add(crate::consts::PAGE_SIZE - 1).is_some());
    let reservation = MemoryDescriptor::new_aligned(
        start,
        end - start,
        MemoryType::Reserved,
        crate::consts::PAGE_SIZE,
    );
    add_memory_descriptor(reservation)
        .unwrap_or_else(|error| panic!("failed to reserve UEFI host initramfs: {error:?}"));
    publish(start, end, true);
}

#[cfg(efi)]
pub(crate) fn staged_uefi() -> Option<(usize, usize)> {
    let end = PENDING_END.load(Ordering::Acquire);
    (end != 0).then(|| (PENDING_START.load(Ordering::Relaxed), end))
}
