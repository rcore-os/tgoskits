//! Deterministic placement of memory BARs inside one host aperture.

use alloc::{collections::BTreeMap, string::ToString, vec::Vec};
use core::{cmp::Reverse, ops::Range};

use super::{FOUR_GIB, PciBarIndex, PciError, PciFunctionSpec, PciResult};
use crate::{DeviceNodeId, ResourceRequest};

pub(super) fn resolve_bar_addresses(
    memory_aperture: &Range<u64>,
    functions: &BTreeMap<DeviceNodeId, PciFunctionSpec>,
) -> PciResult<BTreeMap<(DeviceNodeId, PciBarIndex), u64>> {
    let (mut fixed, mut automatic) = collect_placements(functions);
    fixed.sort_by(|left, right| {
        left.function
            .cmp(&right.function)
            .then_with(|| left.index.cmp(&right.index))
    });
    automatic.sort_by(|left, right| {
        Reverse(left.size)
            .cmp(&Reverse(right.size))
            .then_with(|| left.function.cmp(&right.function))
            .then_with(|| left.index.cmp(&right.index))
    });

    let mut occupied = Vec::<Range<u64>>::new();
    let mut resolved = BTreeMap::new();
    place_fixed(memory_aperture, fixed, &mut occupied, &mut resolved)?;
    place_automatic(memory_aperture, automatic, &mut occupied, &mut resolved)?;
    Ok(resolved)
}

fn collect_placements(
    functions: &BTreeMap<DeviceNodeId, PciFunctionSpec>,
) -> (Vec<BarPlacement>, Vec<BarPlacement>) {
    let mut fixed = Vec::new();
    let mut automatic = Vec::new();
    for (id, spec) in functions {
        for bar in &spec.bars {
            let placement = BarPlacement {
                function: id.clone(),
                index: bar.index(),
                size: bar.size(),
                request: bar.address_request(),
            };
            match placement.request {
                ResourceRequest::Fixed(_) => fixed.push(placement),
                ResourceRequest::Auto => automatic.push(placement),
            }
        }
    }
    (fixed, automatic)
}

fn place_fixed(
    memory_aperture: &Range<u64>,
    placements: Vec<BarPlacement>,
    occupied: &mut Vec<Range<u64>>,
    resolved: &mut BTreeMap<(DeviceNodeId, PciBarIndex), u64>,
) -> PciResult {
    for placement in placements {
        let ResourceRequest::Fixed(address) = placement.request else {
            unreachable!("fixed placement list only contains fixed requests");
        };
        let range = checked_bar_range(memory_aperture, &placement, address)?;
        if overlaps_any(occupied, &range) {
            return Err(PciError::BarConflict {
                function: placement.function.to_string(),
                bar: placement.index,
                start: range.start,
                end: range.end,
            });
        }
        occupied.push(range);
        resolved.insert((placement.function, placement.index), address);
    }
    Ok(())
}

fn place_automatic(
    memory_aperture: &Range<u64>,
    placements: Vec<BarPlacement>,
    occupied: &mut Vec<Range<u64>>,
    resolved: &mut BTreeMap<(DeviceNodeId, PciBarIndex), u64>,
) -> PciResult {
    for placement in placements {
        let address = first_fit(memory_aperture, placement.size, occupied).ok_or_else(|| {
            PciError::BarApertureExhausted {
                function: placement.function.to_string(),
                bar: placement.index,
                size: placement.size,
            }
        })?;
        let range = checked_bar_range(memory_aperture, &placement, address)?;
        occupied.push(range);
        resolved.insert((placement.function, placement.index), address);
    }
    Ok(())
}

struct BarPlacement {
    function: DeviceNodeId,
    index: PciBarIndex,
    size: u64,
    request: ResourceRequest<u64>,
}

fn checked_bar_range(
    memory_aperture: &Range<u64>,
    placement: &BarPlacement,
    address: u64,
) -> PciResult<Range<u64>> {
    if address & (placement.size - 1) != 0 {
        return Err(PciError::InvalidBar {
            bar: placement.index,
            detail: "fixed address is not aligned to BAR size".into(),
        });
    }
    let end = address
        .checked_add(placement.size)
        .ok_or_else(|| PciError::InvalidBar {
            bar: placement.index,
            detail: "BAR range overflows u64".into(),
        })?;
    if address < memory_aperture.start || end > memory_aperture.end || end > FOUR_GIB {
        return Err(PciError::InvalidBar {
            bar: placement.index,
            detail: "BAR range lies outside the 32-bit host memory aperture".into(),
        });
    }
    Ok(address..end)
}

fn first_fit(aperture: &Range<u64>, size: u64, occupied: &[Range<u64>]) -> Option<u64> {
    let mut candidate = align_up(aperture.start, size)?;
    loop {
        let end = candidate.checked_add(size)?;
        if end > aperture.end {
            return None;
        }
        if let Some(conflict) = occupied
            .iter()
            .filter(|range| candidate < range.end && range.start < end)
            .min_by_key(|range| range.start)
        {
            candidate = align_up(conflict.end, size)?;
        } else {
            return Some(candidate);
        }
    }
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment - 1)
        .map(|aligned| aligned & !(alignment - 1))
}

fn overlaps_any(occupied: &[Range<u64>], candidate: &Range<u64>) -> bool {
    occupied
        .iter()
        .any(|range| candidate.start < range.end && range.start < candidate.end)
}
