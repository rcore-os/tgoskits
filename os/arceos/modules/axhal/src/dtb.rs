//! DTB (Device Tree Blob) related functionality.
use core::ptr::NonNull;

use fdt_parser::Fdt;
use spin::{LazyLock, Once};

static BOOTARG: Once<usize> = Once::new();

/// Returns the physical address to probe for DTB.
fn dtb_paddr_from_boot_context() -> Option<usize> {
    let arg = get_bootarg();
    if arg != 0 {
        return Some(arg);
    }

    None
}

/// Initializes the boot argument.
pub fn init(arg: usize) {
    BOOTARG.call_once(|| arg);
}

/// Returns the boot argument.
/// This is typically the device tree blob address passed from the bootloader.
pub fn get_bootarg() -> usize {
    BOOTARG
        .get()
        .copied()
        .expect("Boot argument not initialized")
}

/// Get the FDT.
pub fn get_fdt() -> Option<&'static Fdt<'static>> {
    static CACHED_FDT: LazyLock<Option<Fdt<'static>>> = LazyLock::new(|| {
        let fdt_paddr = dtb_paddr_from_boot_context()?;
        let fdt_ptr = NonNull::new(crate::mem::phys_to_virt(fdt_paddr.into()).as_mut_ptr())?;
        Fdt::from_ptr(fdt_ptr).ok()
    });

    CACHED_FDT.as_ref()
}

/// Get the bootargs chosen from the device tree.
pub fn get_chosen_bootargs() -> Option<&'static str> {
    static CACHED_BOOTARGS: LazyLock<Option<&'static str>> = LazyLock::new(|| {
        let fdt = get_fdt()?;
        fdt.chosen()?.bootargs()
    });

    *CACHED_BOOTARGS
}
