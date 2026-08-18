//! DTB (Device Tree Blob) related functionality.
use core::ptr::NonNull;

use ax_lazyinit::{LazyLock, OnceLock};
use fdt_parser::{Fdt, Node};

static BOOTARG: OnceLock<usize> = OnceLock::new();

/// Returns the physical address to probe for DTB, or `None` if no boot argument was
/// installed (e.g. host unit tests, or any path that never called [`init`]). The FDT
/// probe (and thus [`cpu_capacities`]) must degrade to "no device tree" rather than
/// panic in that case, so this reads the boot arg as an `Option` instead of via the
/// strict [`get_bootarg`].
fn dtb_paddr_from_boot_context() -> Option<usize> {
    let arg = BOOTARG.get().copied()?;
    if arg != 0 { Some(arg) } else { None }
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
    let Some(fdt) = get_fdt() else {
        return [DEFAULT_CPU_CAPACITY; MAX_CPU_NUM];
    };
    caps_from_fdt(fdt)
}

/// A `cpu@*` device-tree node the boot path treats as a real, enabled CPU.
///
/// Mirrors someboot's `is_cpu_node_available` (platforms/someboot/src/fdt) so
/// this table's logical indices line up with boot's `.enumerate()` cpu_id
/// mapping: the node is named `cpu@*`, is not firmware-disabled, and either has
/// no `device_type` or declares `device_type = "cpu"` (nodes that reuse a
/// `cpu@` name for something else are excluded).
fn is_cpu_node_available(node: &Node) -> bool {
    node.name().starts_with("cpu@")
        && matches!(
            node.find_property("device_type").map(|p| p.str()),
            None | Some("cpu")
        )
        && matches!(
            node.find_property("status").map(|p| p.str()),
            None | Some("okay") | Some("ok")
        )
}

/// Parse per-logical-CPU normalized compute capacities from `fdt`, indexed by
/// logical `cpu_id`.
///
/// Only the direct `cpu@*` children of `/cpus` are considered, filtered by
/// [`is_cpu_node_available`] and taken in device-tree order (the N-th enabled
/// node is logical CPU N — do NOT key by `reg`); this matches boot's CPU
/// enumeration so a stray `cpu@*` node elsewhere in the tree cannot shift the
/// indices. Each CPU's capacity comes from `capacity-dmips-mhz` (a raw relative
/// value), else a `cortex-a55`/`a76` compatible fallback, else
/// [`DEFAULT_CPU_CAPACITY`].
///
/// `capacity-dmips-mhz` values are normalized so the largest maps to
/// `SCHED_CAPACITY_SCALE` (1024), the same convention Linux uses; the compat and
/// default fallbacks are already on the 1024 scale and are left untouched, so
/// raw DMIPS/MHz numbers can never leak into the table at a different scale.
///
/// Generic over the table size `N` so it can be unit-tested with fixture device
/// trees independent of the build-time [`MAX_CPU_NUM`].
fn caps_from_fdt<const N: usize>(fdt: &Fdt) -> [u16; N] {
    // Pass 1: select the enabled `/cpus` children, recording each CPU's raw
    // `capacity-dmips-mhz` (if any) and its already-1024-scaled compat/default
    // fallback, in logical-cpu_id order.
    let mut raw_dmips = [None::<u32>; N];
    let mut fallback = [DEFAULT_CPU_CAPACITY; N];
    let mut count = 0usize;

    let mut cpus_level: Option<usize> = None;
    for node in fdt.all_nodes() {
        match cpus_level {
            None => {
                if node.name() == "cpus" {
                    cpus_level = Some(node.level);
                }
            }
            Some(cpus_level) => {
                // Left the `/cpus` subtree: no more CPU nodes follow.
                if node.level <= cpus_level {
                    break;
                }
                // Only direct children of `/cpus` are CPUs; skip deeper descendants.
                if node.level == cpus_level + 1 && is_cpu_node_available(&node) {
                    if count >= N {
                        break;
                    }
                    raw_dmips[count] = node
                        .find_property("capacity-dmips-mhz")
                        .map(|p| p.u32())
                        .filter(|&c| c != 0);
                    fallback[count] = node
                        .compatibles()
                        .find_map(|compat| {
                            if compat.contains("cortex-a55") {
                                Some(530)
                            } else if compat.contains("cortex-a76") {
                                Some(1024)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(DEFAULT_CPU_CAPACITY);
                    count += 1;
                }
            }
        }
    }

    // Pass 2: normalize the raw DMIPS/MHz values against the largest so it maps
    // to 1024; leave compat/default fallbacks (already 1024-scaled) as-is.
    let max_dmips = raw_dmips[..count].iter().flatten().copied().max();

    let mut caps = [DEFAULT_CPU_CAPACITY; N];
    for i in 0..count {
        caps[i] = match (raw_dmips[i], max_dmips) {
            // u64 math so a large/garbage `capacity-dmips-mhz` can't overflow the
            // `* 1024` scale; the result is clamped into `[1, 1024]` regardless.
            (Some(dmips), Some(max)) => (dmips as u64 * 1024 / max as u64).clamp(1, 1024) as u16,
            _ => fallback[i],
        };
    }
    log::info!("cpu_capacities: {:?}", &caps[..count.max(1)]);
    caps
}

#[cfg(test)]
mod tests {
    use alloc::{format, vec::Vec};

    use fdt_edit::{Fdt as EditFdt, Node, Property};
    use fdt_parser::Fdt as ParsedFdt;

    use super::{DEFAULT_CPU_CAPACITY, caps_from_fdt};

    /// Fixture description of one `cpu@*` device-tree node.
    struct CpuSpec {
        device_type: Option<&'static str>,
        status: Option<&'static str>,
        compatible: Option<&'static str>,
        dmips: Option<u32>,
    }

    /// A normal enabled CPU node (`device_type = "cpu"`).
    fn cpu(
        status: Option<&'static str>,
        compatible: Option<&'static str>,
        dmips: Option<u32>,
    ) -> CpuSpec {
        CpuSpec {
            device_type: Some("cpu"),
            status,
            compatible,
            dmips,
        }
    }

    fn str_prop(name: &str, value: &str) -> Property {
        let mut data = value.as_bytes().to_vec();
        data.push(0); // device-tree strings are NUL-terminated
        Property::new(name, data)
    }

    fn u32_prop(name: &str, value: u32) -> Property {
        Property::new(name, value.to_be_bytes().to_vec())
    }

    /// Build a device tree with `/cpus` holding `cpus`, optionally adding a stray
    /// `cpu@*` node at the root (outside `/cpus`). Returns the encoded DTB blob.
    fn build_dtb(cpus: &[CpuSpec], stray_root_cpu: Option<&str>) -> Vec<u8> {
        let mut fdt = EditFdt::new();
        let root = fdt.root_id();
        let cpus_node = fdt.add_node(root, Node::new("cpus"));
        fdt.node_mut(cpus_node)
            .unwrap()
            .set_property(u32_prop("#address-cells", 1));
        fdt.node_mut(cpus_node)
            .unwrap()
            .set_property(u32_prop("#size-cells", 0));

        for (i, spec) in cpus.iter().enumerate() {
            let id = fdt.add_node(cpus_node, Node::new(&format!("cpu@{i}")));
            let node = fdt.node_mut(id).unwrap();
            if let Some(device_type) = spec.device_type {
                node.set_property(str_prop("device_type", device_type));
            }
            node.set_property(u32_prop("reg", i as u32));
            if let Some(status) = spec.status {
                node.set_property(str_prop("status", status));
            }
            if let Some(compatible) = spec.compatible {
                node.set_property(str_prop("compatible", compatible));
            }
            if let Some(dmips) = spec.dmips {
                node.set_property(u32_prop("capacity-dmips-mhz", dmips));
            }
        }

        if let Some(name) = stray_root_cpu {
            // A `cpu@*` node directly under the root, not under `/cpus`. A scan
            // that matched `cpu@*` anywhere in the tree would wrongly count it.
            let id = fdt.add_node(root, Node::new(name));
            let node = fdt.node_mut(id).unwrap();
            node.set_property(str_prop("device_type", "cpu"));
            node.set_property(str_prop("compatible", "arm,cortex-a55"));
        }

        fdt.encode().as_ref().to_vec()
    }

    /// Encode `cpus` under `/cpus`, parse through the same `fdt_parser` path
    /// `build_cpu_capacities` uses, and compute the capacity table (fixed
    /// `N = 8`, independent of the build-time `MAX_CPU_NUM`).
    fn caps_of(cpus: &[CpuSpec]) -> [u16; 8] {
        let bytes = build_dtb(cpus, None);
        let parsed = ParsedFdt::from_bytes(&bytes).expect("fixture DTB should parse");
        caps_from_fdt::<8>(&parsed)
    }

    #[test]
    fn compatible_fallback_maps_a76_and_a55() {
        let caps = caps_of(&[
            cpu(None, Some("arm,cortex-a76"), None),
            cpu(None, Some("arm,cortex-a55"), None),
        ]);
        assert_eq!(caps[0], 1024, "cortex-a76 -> 1024");
        assert_eq!(caps[1], 530, "cortex-a55 -> 530");
        // A CPU with no matching node keeps the default.
        assert_eq!(caps[2], DEFAULT_CPU_CAPACITY);
    }

    #[test]
    fn unknown_compatible_uses_default_capacity() {
        let caps = caps_of(&[cpu(None, Some("arm,cortex-unknown"), None)]);
        assert_eq!(caps[0], DEFAULT_CPU_CAPACITY);
    }

    #[test]
    fn dmips_values_are_normalized_to_1024_scale() {
        // Phytium-like raw DMIPS/MHz values: the largest maps to 1024 and the
        // rest scale proportionally, so a raw value never leaks into the table.
        // The normalized DMIPS also takes precedence over the compatible fallback.
        let caps = caps_of(&[
            cpu(None, Some("arm,cortex-a55"), Some(2850)),
            cpu(None, Some("arm,cortex-a76"), Some(5660)),
        ]);
        assert_eq!(
            caps[1], 1024,
            "largest capacity-dmips-mhz normalizes to 1024"
        );
        assert_eq!(caps[0], 515, "2850 * 1024 / 5660 = 515");
    }

    /// Regression for the disabled-CPU skip: a firmware-disabled `cpu@` node
    /// interleaved before enabled ones must not consume a logical index, or every
    /// subsequent capacity is shifted off its `cpu_id`. The pre-fix
    /// implementation counted disabled nodes and fails this.
    #[test]
    fn disabled_cpu_node_does_not_shift_logical_indices() {
        let caps = caps_of(&[
            cpu(Some("disabled"), Some("arm,cortex-a55"), None),
            cpu(Some("okay"), Some("arm,cortex-a76"), None),
            cpu(None, Some("arm,cortex-a55"), None),
        ]);
        // Disabled cpu@0 is skipped: logical CPU 0 is the a76, CPU 1 the a55.
        assert_eq!(
            caps[0], 1024,
            "disabled cpu@0 must not occupy logical index 0"
        );
        assert_eq!(caps[1], 530);
    }

    #[test]
    fn cpu_node_outside_cpus_is_ignored() {
        // Only `/cpus` children are CPUs; a stray `cpu@9` at the root must not
        // become a third logical CPU.
        let bytes = build_dtb(
            &[
                cpu(None, Some("arm,cortex-a76"), None),
                cpu(None, Some("arm,cortex-a55"), None),
            ],
            Some("cpu@9"),
        );
        let parsed = ParsedFdt::from_bytes(&bytes).expect("fixture DTB should parse");
        let caps = caps_from_fdt::<8>(&parsed);
        assert_eq!(caps[0], 1024);
        assert_eq!(caps[1], 530);
        assert_eq!(
            caps[2], DEFAULT_CPU_CAPACITY,
            "a stray cpu@ outside /cpus must not be counted"
        );
    }

    #[test]
    fn non_cpu_device_type_is_ignored() {
        // A `/cpus` child named `cpu@*` but declaring a non-"cpu" device_type is
        // not a CPU (someboot excludes it) and must not consume a logical index.
        let caps = caps_of(&[
            CpuSpec {
                device_type: Some("memory"),
                status: None,
                compatible: Some("arm,cortex-a55"),
                dmips: None,
            },
            cpu(None, Some("arm,cortex-a76"), None),
            cpu(None, Some("arm,cortex-a55"), None),
        ]);
        assert_eq!(
            caps[0], 1024,
            "a cpu@ node with device_type != cpu must not occupy index 0"
        );
        assert_eq!(caps[1], 530);
    }
}
