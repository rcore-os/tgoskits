pub type EfiMainFn =
    extern "efiapi" fn(image_handle: *mut core::ffi::c_void, system_table: *mut SystemTable) -> u64;

use core::mem::transmute;

use uefi_raw::table::system::SystemTable;

/// Resolve the entry function from an RVA (relative virtual address).
///
/// For PE/COFF, the entry point in the optional header is an RVA, and
/// `object::Object::entry()` returns `image_base + entry_rva`. The RVA must
/// fall inside the loaded image (`size_of_image`); a malformed or tampered
/// image with an out-of-bounds RVA must not divert execution outside the
/// mapping, so it is rejected here instead of being transmuted.
pub fn resolve_entry_func(
    mapping: *const u8,
    entry_rva: u64,
    size_of_image: u64,
) -> Option<EfiMainFn> {
    if entry_rva >= size_of_image {
        return None;
    }
    let func_addr = (mapping as usize).checked_add(entry_rva as usize)? as *const ();
    Some(unsafe { transmute::<*const (), EfiMainFn>(func_addr) })
}
