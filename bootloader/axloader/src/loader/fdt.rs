//! Firmware device-tree publication owned until application return or EBS.

use core::{ffi::c_void, ptr::NonNull};

use axloader::fdt_image::{
    DtbError, HEADER_BYTES, MAX_DTB_BYTES, ValidatedFdt, efi_memory_covers, header_total_size,
};
use uefi::{
    Guid, Status, StatusExt,
    boot::{self, AllocateType, MemoryType, ScopedProtocol},
    mem::memory_map::MemoryMap,
    proto::{
        loaded_image::LoadedImage,
        media::{
            file::{File, FileAttribute, FileInfo, FileMode, FileType},
            fs::SimpleFileSystem,
        },
        unsafe_protocol,
    },
};

const FDT_GUID: Guid = uefi::guid!("b1b621d5-f19c-41a5-830b-d9152c69aae0");
const PAGE_BYTES: usize = 4096;
const FIXUP_ATTEMPTS: usize = 4;

/// Retains an owned DTB while firmware and the receiving kernel use it.
pub(super) struct Fdt {
    pages: Option<Pages>,
    previous_address: Option<*const c_void>,
    installed: bool,
}

impl Fdt {
    pub(super) fn prepare() -> uefi::Result<Self> {
        let map = boot::memory_map(MemoryType::LOADER_DATA)?;
        let previous_address = installed_address();
        let Some((mut pages, file_bytes)) = read_external_tree()? else {
            let address = previous_address.ok_or(Status::NOT_FOUND)?;
            validate_firmware_tree(address, &map)?;
            return Ok(Self {
                pages: None,
                previous_address: None,
                installed: false,
            });
        };

        // The snapshot precedes our ACPI allocation: newly allocated pages
        // cannot turn an unprotected reservation into apparently protected RAM.
        validate_tree(&pages.bytes()[..file_bytes], &map)?;
        let fixup = Fixup::open()?;
        pages = apply_fixups(pages, &fixup)?;
        validate_tree(pages.bytes(), &map)?;
        let owner = Self::publish(pages, previous_address)?;
        crate::logln!("loader_fdt_installed: source=EFI/BOOT/board.dtb");
        Ok(owner)
    }

    fn publish(pages: Pages, previous_address: Option<*const c_void>) -> uefi::Result<Self> {
        let mut owner = Self {
            pages: Some(pages),
            previous_address,
            installed: false,
        };
        let pointer = owner.pages.as_ref().expect("FDT pages owned").pointer;
        install_table(pointer.as_ptr().cast())?;
        owner.installed = true;
        Ok(owner)
    }
}

impl Drop for Fdt {
    fn drop(&mut self) {
        let Some(pages) = self.pages.as_ref() else {
            return;
        };
        if !self.installed || !boot_services_active() {
            return;
        }
        if installed_address() != Some(pages.pointer.as_ptr().cast_const().cast()) {
            // Another owner replaced the table. It may still retain our tree;
            // do not remove its entry or reclaim a potentially borrowed buffer.
            core::mem::forget(self.pages.take());
            return;
        }
        let previous = self.previous_address.unwrap_or_default();
        if let Err(error) = install_table(previous.cast_mut()) {
            crate::logln!("loader_fdt_restore_failed: {error:?}; retaining pages");
            core::mem::forget(self.pages.take());
        }
    }
}

fn read_external_tree() -> uefi::Result<Option<(Pages, usize)>> {
    let image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())?;
    let Some(device) = image.device() else {
        return Ok(None);
    };
    drop(image);
    let mut filesystem = match boot::open_protocol_exclusive::<SimpleFileSystem>(device) {
        Ok(filesystem) => filesystem,
        Err(error) if error.status() == Status::UNSUPPORTED => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut root = filesystem.open_volume()?;
    let file = match root.open(
        uefi::cstr16!("\\EFI\\BOOT\\board.dtb"),
        FileMode::Read,
        FileAttribute::empty(),
    ) {
        Ok(file) => file,
        Err(error) if error.status() == Status::NOT_FOUND => return Ok(None),
        Err(error) => return Err(error),
    };
    let FileType::Regular(mut file) = file.into_type()? else {
        return Err(Status::UNSUPPORTED.into());
    };
    let file_bytes = usize::try_from(file.get_boxed_info::<FileInfo>()?.file_size())
        .ok()
        .filter(|length| (HEADER_BYTES..=MAX_DTB_BYTES).contains(length))
        .ok_or(Status::BAD_BUFFER_SIZE)?;
    let mut pages = Pages::allocate(file_bytes)?;
    let mut read = 0;
    while read < file_bytes {
        let count = file.read(&mut pages.bytes_mut()[read..file_bytes])?;
        if count == 0 {
            return Err(Status::END_OF_FILE.into());
        }
        read += count;
    }
    let mut extra = [0; 1];
    if file.read(&mut extra)? != 0 {
        return Err(Status::BAD_BUFFER_SIZE.into());
    }
    drop(file);
    drop(root);
    drop(filesystem);
    Ok(Some((pages, file_bytes)))
}

struct Pages {
    pointer: NonNull<u8>,
    count: usize,
}

impl Pages {
    fn allocate(bytes: usize) -> uefi::Result<Self> {
        let capacity = bytes
            .checked_next_multiple_of(PAGE_BYTES)
            .filter(|capacity| *capacity != 0 && *capacity <= MAX_DTB_BYTES)
            .ok_or(Status::BAD_BUFFER_SIZE)?;
        let count = capacity / PAGE_BYTES;
        let pointer =
            boot::allocate_pages(AllocateType::AnyPages, MemoryType::ACPI_RECLAIM, count)?;
        // SAFETY: the allocation uniquely owns count consecutive writable
        // pages. Initialize every byte before exposing a Rust slice or fixup.
        unsafe { pointer.as_ptr().write_bytes(0, capacity) };
        Ok(Self { pointer, count })
    }

    fn capacity(&self) -> usize {
        self.count * PAGE_BYTES
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: these initialized consecutive pages are live for self's
        // lifetime. Firmware fixup cannot run while this borrow is live.
        unsafe { core::slice::from_raw_parts(self.pointer.as_ptr(), self.capacity()) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: before publication, self uniquely owns these initialized
        // pages. The returned borrow excludes fixup or other page access.
        unsafe { core::slice::from_raw_parts_mut(self.pointer.as_ptr(), self.capacity()) }
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        if boot_services_active() {
            // SAFETY: the sole owner has removed every published table entry
            // before this destructor; no borrow or fixup retains these pages.
            if let Err(error) = unsafe { boot::free_pages(self.pointer, self.count) } {
                crate::logln!("loader_fdt_free_failed: {error:?}");
            }
        }
    }
}

#[derive(Debug)]
#[repr(C)]
#[unsafe_protocol("60ed6ba9-dfef-4799-ac7b-75e0f833456c")]
struct DtFixup {
    revision: u64,
    fixup: unsafe extern "efiapi" fn(*const Self, *mut c_void, *mut usize) -> Status,
}

#[derive(Debug)]
#[repr(C)]
#[unsafe_protocol("e617d64c-fe08-46da-f4dc-bbd5870c7300")]
struct LegacyDtFixup {
    revision: u64,
    fixup: unsafe extern "efiapi" fn(*const Self, *mut c_void, *mut usize, u32) -> Status,
}

enum Fixup {
    Current(ScopedProtocol<DtFixup>),
    Legacy(ScopedProtocol<LegacyDtFixup>),
}

impl Fixup {
    fn open() -> uefi::Result<Self> {
        match boot::get_handle_for_protocol::<DtFixup>() {
            Ok(handle) => {
                let protocol = boot::open_protocol_exclusive::<DtFixup>(handle)?;
                if protocol.revision < 0x10000 {
                    return Err(Status::UNSUPPORTED.into());
                }
                Ok(Self::Current(protocol))
            }
            Err(error) if error.status() == Status::NOT_FOUND => {
                let handle = boot::get_handle_for_protocol::<LegacyDtFixup>()?;
                let protocol = boot::open_protocol_exclusive::<LegacyDtFixup>(handle)?;
                if protocol.revision < 0x10000 {
                    return Err(Status::UNSUPPORTED.into());
                }
                Ok(Self::Legacy(protocol))
            }
            Err(error) => Err(error),
        }
    }

    fn apply(&self, pages: &mut Pages, capacity: &mut usize) -> Status {
        // SAFETY: each GUID fixes this exact FFI signature. The protocol guard
        // stays live, pages uniquely own a validated DTB with writable capacity,
        // and no Rust slice exists across this synchronous firmware call.
        unsafe {
            match self {
                Self::Current(protocol) => {
                    (protocol.fixup)(&**protocol, pages.pointer.as_ptr().cast(), capacity)
                }
                // APPLY_FIXUPS only; memory reservation and publication belong
                // to this owner and must not be delegated to legacy extensions.
                Self::Legacy(protocol) => {
                    (protocol.fixup)(&**protocol, pages.pointer.as_ptr().cast(), capacity, 1)
                }
            }
        }
    }
}

fn apply_fixups(mut pages: Pages, fixup: &Fixup) -> uefi::Result<Pages> {
    for _ in 0..FIXUP_ATTEMPTS {
        let mut capacity = pages.capacity();
        let status = fixup.apply(&mut pages, &mut capacity);
        if status == Status::SUCCESS {
            let total = header_total_size(pages.bytes()).map_err(|_| Status::COMPROMISED_DATA)?;
            if capacity > pages.capacity() || capacity < total {
                return Err(Status::BAD_BUFFER_SIZE.into());
            }
            return Ok(pages);
        }
        if status != Status::BUFFER_TOO_SMALL {
            return Err(status.into());
        }
        if capacity <= pages.capacity() || capacity > MAX_DTB_BYTES {
            return Err(Status::BAD_BUFFER_SIZE.into());
        }
        // BUFFER_TOO_SMALL guarantees the input is unchanged. Validate it
        // again before copying into the strictly larger allocation.
        let tree = parse_tree(pages.bytes())?;
        let mut expanded = Pages::allocate(capacity)?;
        expanded.bytes_mut()[..tree.total_size()].copy_from_slice(tree.as_bytes());
        pages = expanded;
    }
    Err(Status::BAD_BUFFER_SIZE.into())
}

fn validate_firmware_tree(address: *const c_void, map: &impl MemoryMap) -> uefi::Result<()> {
    let start = address as u64;
    if start == 0
        || !start.is_multiple_of(8)
        || !covered(map, start, HEADER_BYTES as u64, |kind| {
            kind == MemoryType::ACPI_RECLAIM
        })
    {
        return Err(Status::COMPROMISED_DATA.into());
    }
    // SAFETY: the EFI map proves the aligned configuration-table pointer's
    // header lies in retained ACPI RAM; firmware owns its initialized bytes.
    let header = unsafe { core::slice::from_raw_parts(address.cast::<u8>(), HEADER_BYTES) };
    let length = header_total_size(header).map_err(|_| Status::COMPROMISED_DATA)?;
    if !covered(map, start, length as u64, |kind| {
        kind == MemoryType::ACPI_RECLAIM
    }) {
        return Err(Status::COMPROMISED_DATA.into());
    }
    // SAFETY: the checked length is bounded to 16 MiB and wholly covered by
    // firmware ACPI RAM. No mutation occurs while validation borrows the blob.
    let bytes = unsafe { core::slice::from_raw_parts(address.cast::<u8>(), length) };
    validate_tree(bytes, map)
}

fn validate_tree(bytes: &[u8], map: &impl MemoryMap) -> uefi::Result<()> {
    let tree = parse_tree(bytes)?;
    tree.validate_uefi_reservations(map.entries())
        .map_err(|error| {
            if let DtbError::UnprotectedReservation(reservation) = error {
                crate::logln!(
                    "loader_fdt_unprotected_reservation: start={:#x} bytes={:#x} no_map={}",
                    reservation.address,
                    reservation.size,
                    reservation.no_map
                );
            }
            Status::UNSUPPORTED.into()
        })
}

fn parse_tree(bytes: &[u8]) -> uefi::Result<ValidatedFdt<'_>> {
    ValidatedFdt::parse(bytes).map_err(|error| {
        crate::logln!("loader_fdt_invalid: {error}");
        Status::COMPROMISED_DATA.into()
    })
}

fn covered(
    map: &impl MemoryMap,
    start: u64,
    length: u64,
    accepts: impl Fn(MemoryType) -> bool,
) -> bool {
    efi_memory_covers(map.entries(), start, length, accepts)
}

fn installed_address() -> Option<*const c_void> {
    uefi::system::with_config_table(|tables| {
        tables
            .iter()
            .find(|entry| entry.guid == FDT_GUID)
            .map(|entry| entry.address)
    })
}

fn boot_services_active() -> bool {
    uefi::table::system_table_raw().is_some_and(|table| {
        // SAFETY: the initialized firmware table outlives this EFI application;
        // the handoff guard rejects a returning child after Boot Services exit.
        !unsafe { table.as_ref() }.boot_services.is_null()
    })
}

fn install_table(pointer: *mut c_void) -> uefi::Result<()> {
    let system = uefi::table::system_table_raw().ok_or(Status::NOT_READY)?;
    // SAFETY: the caller owns initialized ACPI_RECLAIM DTB pages retained by
    // Fdt, is restoring the original firmware-owned table, or is removing its
    // own entry with NULL while Boot Services are live.
    // The raw service is needed because uefi-rs restricts its safe wrapper's
    // contract to runtime pool allocations, whereas UEFI requires ACPI pages.
    let status = unsafe {
        let services = system
            .as_ref()
            .boot_services
            .as_ref()
            .ok_or(Status::NOT_READY)?;
        (services.install_configuration_table)(&FDT_GUID, pointer)
    };
    status.to_result()
}
