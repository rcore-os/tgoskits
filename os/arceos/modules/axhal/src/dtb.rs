//! DTB (Device Tree Blob) related functionality.
use core::ptr::NonNull;

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

/// Upper bound on the number of logical CPUs, sizing the [`cpu_capacities`]
/// table. Mirrors the build-time bound [`crate::cpu_num`] uses (the `SMP`
/// build-env-configured max under the `smp` feature); with `smp` disabled
/// there is always exactly one CPU.
#[cfg(feature = "smp")]
const MAX_CPU_NUM: usize = crate::build_info::CPU_CAPACITY;
#[cfg(not(feature = "smp"))]
const MAX_CPU_NUM: usize = 1;

/// Fallback capacity when the DT gives no hint (all-equal => homogeneous/QEMU
/// degrades to plain load-spreading).
const DEFAULT_CPU_CAPACITY: u16 = 1024;

/// Per-logical-CPU normalized compute capacity (A76 ~ 1024 / A55 ~ 530), indexed
/// by logical `cpu_id`. Built once from the cached FDT.
pub fn cpu_capacities() -> &'static [u16; MAX_CPU_NUM] {
    static CACHED_CAPS: LazyLock<[u16; MAX_CPU_NUM]> = LazyLock::new(build_cpu_capacities);
    &CACHED_CAPS
}

fn build_cpu_capacities() -> [u16; MAX_CPU_NUM] {
    let mut caps = [DEFAULT_CPU_CAPACITY; MAX_CPU_NUM];
    let Some(fdt) = get_fdt() else {
        return caps;
    };
    // `all_nodes()` yields device-tree order; the N-th `cpu@*` node is logical
    // CPU N (matches boot's `.enumerate()` cpu_id mapping). Do NOT key by `reg`.
    let mut idx = 0usize;
    for node in fdt.all_nodes() {
        if idx >= caps.len() {
            break;
        }
        if !node.name().starts_with("cpu@") {
            continue;
        }
        // Skip firmware-disabled CPUs (status != okay/ok) so `idx` stays aligned
        // with boot's `.enumerate()` cpu_id mapping, which also skips them.
        // Mirrors someboot's is_cpu_node_available (platforms/someboot/src/fdt).
        let available = node
            .find_property("status")
            .map(|p| matches!(p.str(), "okay" | "ok"))
            .unwrap_or(true);
        if !available {
            continue;
        }
        let cap = node
            .find_property("capacity-dmips-mhz")
            .map(|p| p.u32() as u16)
            .filter(|&c| c != 0)
            .or_else(|| {
                node.compatibles().find_map(|compat| {
                    if compat.contains("cortex-a55") {
                        Some(530)
                    } else if compat.contains("cortex-a76") {
                        Some(1024)
                    } else {
                        None
                    }
                })
            })
            .unwrap_or(DEFAULT_CPU_CAPACITY);
        caps[idx] = cap;
        idx += 1;
    }
    log::info!("cpu_capacities: {:?}", &caps[..idx.max(1)]);
    caps
}
