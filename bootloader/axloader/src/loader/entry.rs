use core::{mem, ptr::NonNull};

use uefi::{
    Status, boot,
    mem::memory_map::{MemoryMap, MemoryType},
};

const UEFI_PAGE_SIZE: u64 = 4096;
const OSTOOL_BOOT_INFO_MAGIC: u64 = 0x4f53_544f_4f4c_4249;
const OSTOOL_BOOT_INFO_VERSION: u32 = 1;
const OSTOOL_BOOT_INFO_MAX_RAM_REGIONS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpError {
    EntryAddressTooLarge,
    BootInfoAllocateFailed,
    SystemTableUnavailable,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OstoolRamRegion {
    start: u64,
    size: u64,
}

#[repr(C)]
struct OstoolBootInfo {
    magic: u64,
    version: u32,
    region_count: u32,
    regions: [OstoolRamRegion; OSTOOL_BOOT_INFO_MAX_RAM_REGIONS],
}

impl OstoolBootInfo {
    const fn new() -> Self {
        Self {
            magic: OSTOOL_BOOT_INFO_MAGIC,
            version: OSTOOL_BOOT_INFO_VERSION,
            region_count: 0,
            regions: [OstoolRamRegion { start: 0, size: 0 }; OSTOOL_BOOT_INFO_MAX_RAM_REGIONS],
        }
    }

    fn push_region(&mut self, start: u64, size: u64) {
        if size == 0 || self.region_count as usize >= OSTOOL_BOOT_INFO_MAX_RAM_REGIONS {
            return;
        }

        let index = self.region_count as usize;
        self.regions[index] = OstoolRamRegion { start, size };
        self.region_count += 1;
    }
}

pub fn exit_boot_services_and_jump(entry_point: u64) -> Result<(), JumpError> {
    let entry_point = usize::try_from(entry_point).map_err(|_| JumpError::EntryAddressTooLarge)?;
    let mut boot_info = allocate_boot_info()?;
    let memory_map = unsafe { boot::exit_boot_services(None) };

    let boot_info = unsafe { boot_info.as_mut() };
    for descriptor in memory_map.entries() {
        if descriptor.ty == MemoryType::CONVENTIONAL {
            boot_info.push_region(
                descriptor.phys_start,
                descriptor.page_count.saturating_mul(UEFI_PAGE_SIZE),
            );
        }
    }

    let boot_info_ptr = boot_info as *mut OstoolBootInfo as usize;
    unsafe { call_entry_point(entry_point, boot_info_ptr) }
}

#[cfg(target_arch = "x86_64")]
pub fn jump_to_uefi_entry(entry_point: u64) -> Result<(), JumpError> {
    let entry_point = usize::try_from(entry_point).map_err(|_| JumpError::EntryAddressTooLarge)?;
    let system_table = uefi::table::system_table_raw().ok_or(JumpError::SystemTableUnavailable)?;
    unsafe {
        call_uefi_entry_point(
            entry_point,
            boot::image_handle(),
            system_table.as_ptr().cast(),
        )
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn jump_to_uefi_entry(_entry_point: u64) -> Result<(), JumpError> {
    Err(JumpError::SystemTableUnavailable)
}

fn allocate_boot_info() -> Result<NonNull<OstoolBootInfo>, JumpError> {
    let ptr = boot::allocate_pool(MemoryType::LOADER_DATA, mem::size_of::<OstoolBootInfo>())
        .map_err(|_| JumpError::BootInfoAllocateFailed)?;
    let ptr = ptr.cast::<OstoolBootInfo>();
    unsafe {
        ptr.as_ptr().write(OstoolBootInfo::new());
    }
    Ok(ptr)
}

#[cfg(target_arch = "x86_64")]
unsafe fn call_entry_point(entry_point: usize, boot_info: usize) -> ! {
    // SAFETY: `entry_point` is produced from an ELF image that has already been
    // validated and loaded by the loader. On x86_64 UEFI, converting a machine
    // code address represented as `usize` to an `extern "sysv64"` function
    // pointer is the ABI shape expected by the loaded AxVisor entry. The caller
    // must ensure the target address points to executable code with this
    // signature.
    let entry: extern "sysv64" fn(usize) -> ! = unsafe { core::mem::transmute(entry_point) };
    entry(boot_info)
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn call_entry_point(entry_point: usize, boot_info: usize) -> ! {
    // SAFETY: `entry_point` is produced from an ELF image that has already been
    // validated and loaded by the loader. The target architecture must define a
    // C-compatible boot entry ABI for this handoff. The caller must ensure the
    // target address points to executable code with this signature.
    let entry: extern "C" fn(usize) -> ! = unsafe { core::mem::transmute(entry_point) };
    entry(boot_info)
}

#[cfg(target_arch = "x86_64")]
unsafe fn call_uefi_entry_point(
    entry_point: usize,
    image_handle: uefi::Handle,
    system_table: *const core::ffi::c_void,
) -> Result<(), JumpError> {
    // SAFETY: the address comes from the loaded kernel ELF symbol
    // `__x86_64_efi_pe_entry`, whose ABI matches a UEFI PE entry point.
    let entry: extern "efiapi" fn(uefi::Handle, *const core::ffi::c_void) -> Status =
        unsafe { core::mem::transmute(entry_point) };
    let status = entry(image_handle, system_table);
    crate::logln!("uefi_entry_returned: {status:?}");
    Ok(())
}
