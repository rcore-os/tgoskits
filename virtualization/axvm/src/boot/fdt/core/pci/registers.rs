//! Checked translation of register windows into the root address space.

use core::ops::Range;

use fdt_edit::{Fdt, Node, NodeId, Property};

use crate::{AxVmError, AxVmResult};

/// Returns CPU-addressable register windows, not bus-local identifiers.
/// A missing `ranges` property means that a non-root bus has no CPU mapping;
/// an empty property is an identity mapping. Every ancestor must be traversed.
pub(super) fn cpu_registers(fdt: &Fdt, node_id: NodeId) -> AxVmResult<Vec<Range<u64>>> {
    let node = fdt.node(node_id).expect("node originates from this FDT");
    let Some(reg) = node.get_property("reg") else {
        return Ok(Vec::new());
    };
    let Some(parent_id) = fdt.parent_of(node_id) else {
        return Ok(Vec::new());
    };
    let parent = fdt.node(parent_id).expect("FDT parent exists");
    let size_cells = parent.size_cells().unwrap_or(1);
    if size_cells == 0 {
        // CPU, I2C, and similar bus-local identifiers are not MMIO windows.
        return Ok(Vec::new());
    }
    let cells = [parent.address_cells().unwrap_or(2), size_cells];
    let mut registers = Vec::new();
    for [address, size] in entries(reg, cells)? {
        if size == 0 {
            continue;
        }
        let range = checked_range(address, size)?;
        if let Some(range) = translate(fdt, parent_id, range)? {
            registers.push(range);
        }
    }
    Ok(registers)
}

fn translate(
    fdt: &Fdt,
    mut bus_id: NodeId,
    mut range: Range<u64>,
) -> AxVmResult<Option<Range<u64>>> {
    while let Some(parent_id) = fdt.parent_of(bus_id) {
        let bus = fdt.node(bus_id).expect("FDT bus exists");
        let parent = fdt.node(parent_id).expect("FDT parent exists");
        let Some(ranges) = bus.get_property("ranges") else {
            return Ok(None);
        };
        if !ranges.data.is_empty() {
            range = translate_bus(bus, parent, ranges, range)?;
        }
        bus_id = parent_id;
    }
    Ok(Some(range))
}

fn translate_bus(
    bus: &Node,
    parent: &Node,
    ranges: &Property,
    range: Range<u64>,
) -> AxVmResult<Range<u64>> {
    let cells = [
        bus.address_cells().unwrap_or(2),
        parent.address_cells().unwrap_or(2),
        bus.size_cells().unwrap_or(1),
    ];
    let mut translated = None;
    for [child, parent, length] in entries(ranges, cells)? {
        let window = checked_range(child, length)?;
        checked_range(parent, length)?;
        if window.start <= range.start && range.end <= window.end {
            let start = parent.checked_add(range.start - child).ok_or_else(|| {
                AxVmError::invalid_config("FDT translated register address overflows")
            })?;
            let candidate = checked_range(start, range.end - range.start)?;
            if translated.replace(candidate).is_some() {
                return Err(AxVmError::invalid_config("ambiguous FDT ranges mapping"));
            }
        }
    }
    translated.ok_or_else(|| {
        AxVmError::invalid_config("FDT register window is not contained in one ranges mapping")
    })
}

fn checked_range(start: u64, size: u64) -> AxVmResult<Range<u64>> {
    let end = start
        .checked_add(size)
        .ok_or_else(|| AxVmError::invalid_config("FDT register or ranges window overflows"))?;
    Ok(start..end)
}

/// Validate geometry and complete tuples before reading so malformed properties
/// cannot silently truncate the conflict check or overflow a cell accumulator.
fn entries<const N: usize>(property: &Property, cells: [u32; N]) -> AxVmResult<Vec<[u64; N]>> {
    if cells.iter().any(|count| !matches!(count, 1 | 2)) {
        return Err(AxVmError::invalid_config(format!(
            "unsupported FDT cell geometry for {}",
            property.name()
        )));
    }
    let stride = cells.iter().map(|count| *count as usize * 4).sum::<usize>();
    if !property.data.len().is_multiple_of(stride) {
        return Err(AxVmError::invalid_config(format!(
            "incomplete FDT {} tuple",
            property.name()
        )));
    }
    let mut reader = property.as_reader();
    let mut result = Vec::new();
    for _ in 0..property.data.len() / stride {
        result.push(cells.map(|count| {
            reader
                .read_cells(count as usize)
                .expect("complete validated tuple")
        }));
    }
    Ok(result)
}
