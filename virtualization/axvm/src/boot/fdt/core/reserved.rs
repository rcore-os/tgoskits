//! Reservations describe guest memory; they never authorize host mappings.

use std::vec::Vec;

use axvmconfig::{GuestConfig, VmMemMappingType};
use fdt_edit::Fdt;

use super::device::node_enabled;
use crate::{AxVmResult, VMMemoryRegion, ax_err_type};

pub(super) fn validate_configured(fdt: &Fdt, config: &GuestConfig) -> AxVmResult {
    let regions = config
        .kernel
        .memory_regions
        .iter()
        .filter(|region| region.map_type != VmMemMappingType::MapIdentical)
        .map(|region| checked_range(region.gpa as u64, region.size as u64))
        .collect::<AxVmResult<Vec<_>>>()?;
    // Identity allocations acquire their GPA from the allocator. Only the
    // runtime check can decide coverage, but malformed reservations fail now.
    let deferred = config
        .kernel
        .memory_regions
        .iter()
        .any(|region| region.map_type == VmMemMappingType::MapIdentical);
    validate(fdt, &regions, deferred)
}

pub(super) fn validate_allocated(fdt: &Fdt, memory: &[VMMemoryRegion]) -> AxVmResult {
    let regions = memory
        .iter()
        .map(|region| checked_range(region.gpa.as_usize() as u64, region.size() as u64))
        .collect::<AxVmResult<Vec<_>>>()?;
    validate(fdt, &regions, false)
}

fn checked_range(base: u64, size: u64) -> AxVmResult<(u64, u64)> {
    base.checked_add(size)
        .map(|end| (base, end))
        .ok_or_else(|| ax_err_type!(InvalidData, "firmware memory range overflows"))
}

fn validate(fdt: &Fdt, regions: &[(u64, u64)], deferred: bool) -> AxVmResult {
    let Some(reserved) = fdt.get_by_path_id("/reserved-memory") else {
        return Ok(());
    };
    if !node_enabled(fdt, reserved) {
        return Ok(());
    }
    let parent = fdt.node(reserved).unwrap();
    if parent
        .get_property("ranges")
        .is_some_and(|p| !p.data.is_empty())
    {
        return Err(ax_err_type!(
            Unsupported,
            "reserved-memory address translation is unsupported"
        ));
    }
    let count = |name| -> AxVmResult<usize> {
        parent
            .get_property(name)
            .filter(|p| p.data.len() == 4)
            .and_then(|p| p.get_u32())
            .filter(|n| matches!(n, 1 | 2))
            .map(|n| n as usize)
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    "reserved-memory requires valid address/size cells"
                )
            })
    };
    let address_cells = count("#address-cells")?;
    let size_cells = count("#size-cells")?;
    let mut regions = regions.to_vec();
    regions.sort_unstable();
    for &id in parent.children() {
        if !node_enabled(fdt, id) {
            continue;
        }
        let node = fdt.node(id).unwrap();
        let Some(reg) = node.get_property("reg") else {
            // Dynamic reservations are allocated by the guest from its own RAM.
            continue;
        };
        let stride = (address_cells + size_cells) * 4;
        if reg.data.is_empty() || !reg.data.len().is_multiple_of(stride) {
            return Err(ax_err_type!(
                InvalidData,
                std::format!("malformed reserved-memory reg at {}", fdt.path_of(id))
            ));
        }
        let cells = reg.get_u32_iter().collect::<Vec<_>>();
        for tuple in cells.chunks_exact(address_cells + size_cells) {
            let number =
                |cells: &[u32]| cells.iter().fold(0u64, |n, &cell| (n << 32) | cell as u64);
            let (start, end) = checked_range(
                number(&tuple[..address_cells]),
                number(&tuple[address_cells..]),
            )?;
            let mut covered = start;
            for &(base, limit) in &regions {
                if base <= covered && covered < limit {
                    covered = limit;
                }
            }
            if covered < end && !deferred {
                // Memory records are not the complete stage-2 layout: the
                // passthrough policy may already cover this firmware region.
                // Keep the declaration without turning it into a new grant.
                warn!(
                    "{} [{start:#x}, {end:#x}) is outside recorded guest memory; retaining the \
                     firmware reservation without adding a mapping",
                    fdt.path_of(id)
                );
            }
        }
    }
    Ok(())
}
