//! DTB (Device Tree Blob) related functionality.
use core::{ptr::NonNull, slice};

use ax_lazyinit::{LazyLock, OnceLock};
use fdt_parser::Fdt;

static BOOTARG: OnceLock<usize> = OnceLock::new();

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

/// Returns the initial ramdisk range advertised by `/chosen` as a read-only
/// byte slice.
///
/// The bootloader or hypervisor owns the backing memory.  Callers must not
/// mutate it, and the platform must keep the advertised range mapped for the
/// lifetime of the kernel.
pub fn get_initrd() -> Option<&'static [u8]> {
    static CACHED_INITRD: LazyLock<Option<&'static [u8]>> = LazyLock::new(|| {
        let fdt = get_fdt()?;
        let chosen = fdt.find_nodes("/chosen").next()?;
        let address = |name| {
            let property = chosen.find_property(name)?;
            match property.raw_value().len() {
                4 => Some(u64::from(property.u32())),
                8 => Some(property.u64()),
                _ => None,
            }
        };
        let start = usize::try_from(address("linux,initrd-start")?).ok()?;
        let end = usize::try_from(address("linux,initrd-end")?).ok()?;
        let len = end.checked_sub(start).filter(|len| *len != 0)?;
        let ptr = NonNull::new(crate::mem::phys_to_virt(start.into()).as_mut_ptr())?;

        // SAFETY: `/chosen` describes a boot-provided physical range.  The
        // direct physical mapping remains valid for the kernel lifetime, and
        // the checked subtraction above validates the slice bounds.
        Some(unsafe { slice::from_raw_parts(ptr.as_ptr(), len) })
    });

    *CACHED_INITRD
}
