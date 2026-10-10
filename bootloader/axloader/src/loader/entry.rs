use alloc::vec::Vec;
use core::{mem, ptr, ptr::NonNull};

use uefi::{
    Status, boot,
    mem::memory_map::{MemoryMap, MemoryType},
    proto::loaded_image::LoadedImage,
};

const UEFI_PAGE_SIZE: u64 = 4096;
const OSTOOL_BOOT_INFO_MAGIC: u64 = 0x4f53_544f_4f4c_4249;
const OSTOOL_BOOT_INFO_VERSION: u32 = 1;
const OSTOOL_BOOT_INFO_MAX_RAM_REGIONS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpError {
    EntryAddressTooLarge,
    BootInfoAllocateFailed,
    LoadOptionsAllocateFailed,
    SystemTableUnavailable,
    InvalidLoadOptions,
    LoadedImageProtocol(Status),
}

#[derive(Debug)]
pub struct PreparedLoadOptions {
    words: Option<Vec<u16>>,
}

impl PreparedLoadOptions {
    pub fn new(cmdline: Option<&str>) -> Result<Self, JumpError> {
        let words = cmdline
            .filter(|cmdline| !cmdline.is_empty())
            .map(|cmdline| {
                if !httpboot_protocol::valid_host_cmdline(cmdline) {
                    return Err(JumpError::InvalidLoadOptions);
                }
                let mut words = Vec::new();
                words
                    .try_reserve_exact(cmdline.len() + 1)
                    .map_err(|_| JumpError::LoadOptionsAllocateFailed)?;
                words.extend(cmdline.bytes().map(u16::from));
                words.push(0);
                Ok(words)
            })
            .transpose()?;
        Ok(Self { words })
    }
}

struct InstalledLoadOptions {
    words: Option<Vec<u16>>,
    previous_ptr: *const u8,
    previous_size: u32,
}

impl InstalledLoadOptions {
    fn install(prepared: PreparedLoadOptions) -> Result<Self, JumpError> {
        let mut image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
            .map_err(|error| JumpError::LoadedImageProtocol(error.status()))?;
        let (previous_ptr, previous_size) = image
            .load_options_as_bytes()
            .map(|bytes| {
                (
                    bytes.as_ptr(),
                    u32::try_from(bytes.len()).expect("UEFI load options size is u32"),
                )
            })
            .unwrap_or((ptr::null(), 0));
        let (options, size) = prepared.words.as_ref().map_or((ptr::null(), 0), |words| {
            (
                words.as_ptr().cast::<u8>(),
                u32::try_from(words.len() * size_of::<u16>())
                    .expect("validated command line fits UEFI LoadOptionsSize"),
            )
        });
        // SAFETY: `prepared.words` moves into the returned guard, so the
        // aligned UCS-2 buffer remains allocated until the EFI entry returns.
        // A missing command line deliberately installs a null, zero-size value.
        unsafe { image.set_load_options(options, size) };
        drop(image);
        Ok(Self {
            words: prepared.words,
            previous_ptr,
            previous_size,
        })
    }

    fn restore(mut self) -> Result<(), JumpError> {
        let mut image = match boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()) {
            Ok(image) => image,
            Err(error) => {
                // The image handle still points at this buffer. Keep it alive
                // rather than leave firmware with a dangling LoadOptions pointer.
                if let Some(words) = self.words.take() {
                    mem::forget(words);
                }
                return Err(JumpError::LoadedImageProtocol(error.status()));
            }
        };
        // SAFETY: the previous pointer and size were copied from this same
        // LoadedImage protocol before replacement. Its original owner remains
        // responsible for that allocation throughout the child invocation.
        unsafe { image.set_load_options(self.previous_ptr, self.previous_size) };
        Ok(())
    }
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
pub fn jump_to_uefi_entry(
    entry_point: u64,
    load_options: PreparedLoadOptions,
) -> Result<(), JumpError> {
    let entry_point = usize::try_from(entry_point).map_err(|_| JumpError::EntryAddressTooLarge)?;
    let system_table = uefi::table::system_table_raw().ok_or(JumpError::SystemTableUnavailable)?;
    let installed = InstalledLoadOptions::install(load_options)?;
    let result = unsafe {
        call_uefi_entry_point(
            entry_point,
            boot::image_handle(),
            system_table.as_ptr().cast(),
        )
    };
    installed.restore()?;
    result
}

#[cfg(not(target_arch = "x86_64"))]
pub fn jump_to_uefi_entry(
    _entry_point: u64,
    _load_options: PreparedLoadOptions,
) -> Result<(), JumpError> {
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

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{JumpError, PreparedLoadOptions};

    #[test]
    fn prepares_optional_nul_terminated_ucs2_load_options() {
        assert!(PreparedLoadOptions::new(None).unwrap().words.is_none());
        assert_eq!(
            PreparedLoadOptions::new(Some("console=ttyS0"))
                .unwrap()
                .words
                .unwrap(),
            "console=ttyS0"
                .bytes()
                .map(u16::from)
                .chain([0])
                .collect::<Vec<_>>()
        );
        assert!(PreparedLoadOptions::new(Some("")).unwrap().words.is_none());
    }

    #[test]
    fn rejects_invalid_load_options_before_installing_protocol_pointer() {
        assert_eq!(
            PreparedLoadOptions::new(Some("console=ttyS0\nreset")).unwrap_err(),
            JumpError::InvalidLoadOptions
        );
        assert_eq!(
            PreparedLoadOptions::new(Some(&"x".repeat(4096))).unwrap_err(),
            JumpError::InvalidLoadOptions
        );
    }
}
