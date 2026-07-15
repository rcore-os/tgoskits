// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Guest physical address layout planning.

use alloc::{format, vec::Vec};
use core::cmp::{max, min};

use ax_memory_addr::{PAGE_SIZE_4K, align_down_4k};
use axdevice_base::Resource;
use axvm_types::{
    AddressSpacePolicy, GuestPhysAddr, HostPhysAddr, MappingFlags, PassThroughAddressConfig,
    PassThroughDeviceConfig,
};

use crate::{AxVmResult, ax_err_type};

/// The ownership class of a guest physical range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmRegionKind {
    /// The range is identity-passthrough to host physical memory.
    Passthrough,
    /// The range is backed by VM-owned guest memory.
    Memory,
    /// The range is occupied by the guest boot description.
    BootDescription,
    /// The range is owned by an emulated device and must fault into device
    /// emulation instead of being stage-2 mapped as passthrough.
    EmulatedDevice,
    /// The range is reserved from passthrough mapping.
    Reserved,
}

/// A final stage-2 linear mapping planned for the VM.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VmStage2Mapping {
    /// Guest physical base address.
    pub gpa: GuestPhysAddr,
    /// Host physical base address.
    pub hpa: HostPhysAddr,
    /// Mapping length in bytes.
    pub size: usize,
    /// Stage-2 mapping flags.
    pub flags: MappingFlags,
    /// Ownership class of this mapping.
    pub kind: VmRegionKind,
}

/// A VM-owned address range that reserves guest physical space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmAddressRegion {
    /// Guest physical base address.
    pub gpa: GuestPhysAddr,
    /// Range length in bytes.
    pub size: usize,
    /// Ownership class of this range.
    pub kind: VmRegionKind,
}

impl VmAddressRegion {
    fn new(base: usize, size: usize, kind: VmRegionKind) -> Self {
        Self {
            gpa: GuestPhysAddr::from(base),
            size,
            kind,
        }
    }
}

impl VmStage2Mapping {
    fn new(
        base_gpa: usize,
        base_hpa: usize,
        size: usize,
        flags: MappingFlags,
        kind: VmRegionKind,
    ) -> Self {
        Self {
            gpa: GuestPhysAddr::from(base_gpa),
            hpa: HostPhysAddr::from(base_hpa),
            size,
            flags,
            kind,
        }
    }

    fn gpa_end(&self) -> usize {
        self.gpa.as_usize() + self.size
    }

    fn hpa_end(&self) -> usize {
        self.hpa.as_usize() + self.size
    }

    fn overlaps_gpa(&self, base: usize, size: usize) -> bool {
        ranges_overlap(self.gpa.as_usize(), self.size, base, size)
    }

    fn can_merge(&self, next: &Self) -> bool {
        self.kind == next.kind
            && self.flags == next.flags
            && self.gpa_end() == next.gpa.as_usize()
            && self.hpa_end() == next.hpa.as_usize()
    }
}

/// A VM-owned range that should reserve guest physical address space from
/// passthrough mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuestOwnedRegion {
    pub(crate) base: usize,
    pub(crate) length: usize,
    pub(crate) kind: VmRegionKind,
}

impl GuestOwnedRegion {
    pub(crate) const fn new(base: usize, length: usize, kind: VmRegionKind) -> Self {
        Self { base, length, kind }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlannedRegion {
    base: usize,
    size: usize,
    kind: VmRegionKind,
}

impl PlannedRegion {
    fn end(&self) -> usize {
        self.base + self.size
    }

    fn contains(&self, other: &Self) -> bool {
        self.base <= other.base && other.end() <= self.end()
    }

    fn overlaps(&self, other: &Self) -> bool {
        ranges_overlap(self.base, self.size, other.base, other.size)
    }

    fn to_address_region(self) -> VmAddressRegion {
        VmAddressRegion::new(self.base, self.size, self.kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PassthroughWindow {
    base: usize,
    size: usize,
}

impl PassthroughWindow {
    fn end(&self) -> usize {
        self.base + self.size
    }

    fn to_mapping(self) -> VmStage2Mapping {
        VmStage2Mapping::new(
            self.base,
            self.base,
            self.size,
            device_mapping_flags(),
            VmRegionKind::Passthrough,
        )
    }
}

/// Final guest physical address layout owned by [`crate::AxVMResources`].
#[derive(Debug, Default, Clone)]
pub struct VmAddressLayout {
    mappings: Vec<VmStage2Mapping>,
    owned_regions: Vec<PlannedRegion>,
}

impl VmAddressLayout {
    fn new(mappings: Vec<VmStage2Mapping>, owned_regions: Vec<PlannedRegion>) -> Self {
        Self {
            mappings,
            owned_regions,
        }
    }

    /// Returns all final stage-2 passthrough mappings.
    pub fn mappings(&self) -> &[VmStage2Mapping] {
        &self.mappings
    }

    /// Returns all VM-owned regions that punched holes in passthrough space.
    pub fn owned_regions(&self) -> impl Iterator<Item = VmAddressRegion> + '_ {
        self.owned_regions
            .iter()
            .copied()
            .map(PlannedRegion::to_address_region)
    }
}

/// Planner for guest physical address mappings.
pub struct GuestRegionPlanner {
    policy: AddressSpacePolicy,
    guest_base: usize,
    guest_end: usize,
    windows: Vec<PassthroughWindow>,
    explicit_mappings: Vec<VmStage2Mapping>,
    owned_regions: Vec<PlannedRegion>,
}

impl GuestRegionPlanner {
    /// Creates a planner for a guest physical address space.
    pub fn new(
        policy: AddressSpacePolicy,
        guest_base: usize,
        guest_size: usize,
    ) -> AxVmResult<Self> {
        let guest_end = checked_end("guest address space", guest_base, guest_size)?;
        let windows = match policy {
            AddressSpacePolicy::Virtualized => Vec::new(),
            AddressSpacePolicy::Passthrough => alloc::vec![PassthroughWindow {
                base: guest_base,
                size: guest_size,
            }],
        };
        Ok(Self {
            policy,
            guest_base,
            guest_end,
            windows,
            explicit_mappings: Vec::new(),
            owned_regions: Vec::new(),
        })
    }

    /// Reserves a VM-owned range and punches it out of passthrough windows.
    pub fn reserve(&mut self, base: usize, length: usize, kind: VmRegionKind) -> AxVmResult {
        let (base, size) = normalize_guest_range(kind.name(), base, length)?;
        self.ensure_guest_range(kind.name(), base, size)?;
        let region = PlannedRegion { base, size, kind };

        for mapping in &self.explicit_mappings {
            if mapping.overlaps_gpa(base, size) {
                return Err(ax_err_type!(
                    InvalidInput,
                    format!(
                        "{} range [{:#x}, {:#x}) conflicts with passthrough mapping [{:#x}, {:#x})",
                        kind.name(),
                        base,
                        base + size,
                        mapping.gpa.as_usize(),
                        mapping.gpa_end()
                    )
                ));
            }
        }

        for existing in &self.owned_regions {
            if existing.overlaps(&region) && !owned_overlap_allowed(existing, &region) {
                return Err(ax_err_type!(
                    InvalidInput,
                    format!(
                        "{} range [{:#x}, {:#x}) conflicts with {} range [{:#x}, {:#x})",
                        kind.name(),
                        base,
                        base + size,
                        existing.kind.name(),
                        existing.base,
                        existing.end()
                    )
                ));
            }
        }

        self.punch_hole(base, size);
        self.owned_regions.push(region);
        Ok(())
    }

    /// Adds an explicit passthrough mapping.
    pub fn add_passthrough_mapping(
        &mut self,
        base_gpa: usize,
        base_hpa: usize,
        length: usize,
    ) -> AxVmResult {
        let (base_gpa, base_hpa, size) =
            normalize_linear_range("passthrough", base_gpa, base_hpa, length)?;
        self.ensure_guest_range("passthrough", base_gpa, size)?;

        for region in &self.owned_regions {
            if ranges_overlap(base_gpa, size, region.base, region.size) {
                return Err(ax_err_type!(
                    InvalidInput,
                    format!(
                        "passthrough range [{:#x}, {:#x}) conflicts with {} range [{:#x}, {:#x})",
                        base_gpa,
                        base_gpa + size,
                        region.kind.name(),
                        region.base,
                        region.end()
                    )
                ));
            }
        }

        let mut mapping = VmStage2Mapping::new(
            base_gpa,
            base_hpa,
            size,
            device_mapping_flags(),
            VmRegionKind::Passthrough,
        );
        let mut index = 0;
        while index < self.explicit_mappings.len() {
            let existing = self.explicit_mappings[index];
            if !existing.overlaps_gpa(mapping.gpa.as_usize(), mapping.size) {
                index += 1;
                continue;
            }

            if same_linear_mapping(&existing, &mapping) {
                mapping = merge_linear_mappings(existing, mapping)?;
                self.explicit_mappings.remove(index);
            } else {
                return Err(ax_err_type!(
                    InvalidInput,
                    format!(
                        "passthrough range [{:#x}, {:#x}) conflicts with passthrough mapping \
                         [{:#x}, {:#x})",
                        mapping.gpa.as_usize(),
                        mapping.gpa_end(),
                        existing.gpa.as_usize(),
                        existing.gpa_end()
                    )
                ));
            }
        }

        if self.policy == AddressSpacePolicy::Passthrough {
            self.punch_hole(base_gpa, size);
        }
        self.explicit_mappings.push(mapping);
        Ok(())
    }

    /// Adds an explicit identity passthrough mapping.
    pub fn add_identity_passthrough(&mut self, base_gpa: usize, length: usize) -> AxVmResult {
        self.add_passthrough_mapping(base_gpa, base_gpa, length)
    }

    /// Finishes the layout and returns final stage-2 mappings.
    pub fn finish(mut self) -> AxVmResult<VmAddressLayout> {
        let mut mappings: Vec<_> = self
            .windows
            .drain(..)
            .map(PassthroughWindow::to_mapping)
            .chain(self.explicit_mappings)
            .collect();
        mappings.sort_by_key(|mapping| mapping.gpa.as_usize());

        let mut merged = Vec::<VmStage2Mapping>::new();
        for mapping in mappings {
            if mapping.size == 0 {
                continue;
            }
            if let Some(last) = merged.last_mut() {
                if last.overlaps_gpa(mapping.gpa.as_usize(), mapping.size) {
                    return Err(ax_err_type!(
                        InvalidInput,
                        format!(
                            "stage-2 mapping [{:#x}, {:#x}) conflicts with [{:#x}, {:#x})",
                            mapping.gpa.as_usize(),
                            mapping.gpa_end(),
                            last.gpa.as_usize(),
                            last.gpa_end()
                        )
                    ));
                }
                if last.can_merge(&mapping) {
                    last.size += mapping.size;
                    continue;
                }
            }
            merged.push(mapping);
        }

        Ok(VmAddressLayout::new(merged, self.owned_regions))
    }

    fn punch_hole(&mut self, base: usize, size: usize) {
        if self.policy == AddressSpacePolicy::Virtualized {
            return;
        }

        let end = base + size;
        let mut next_windows = Vec::with_capacity(self.windows.len() + 1);
        for window in self.windows.drain(..) {
            if !ranges_overlap(window.base, window.size, base, size) {
                next_windows.push(window);
                continue;
            }

            if window.base < base {
                next_windows.push(PassthroughWindow {
                    base: window.base,
                    size: base - window.base,
                });
            }
            if end < window.end() {
                next_windows.push(PassthroughWindow {
                    base: end,
                    size: window.end() - end,
                });
            }
        }
        self.windows = next_windows;
    }

    fn ensure_guest_range(&self, name: &str, base: usize, size: usize) -> AxVmResult {
        let end = checked_end(name, base, size)?;
        if base < self.guest_base || end > self.guest_end {
            return Err(ax_err_type!(
                InvalidInput,
                format!(
                    "{} range [{:#x}, {:#x}) is outside guest address space [{:#x}, {:#x})",
                    name, base, end, self.guest_base, self.guest_end
                )
            ));
        }
        Ok(())
    }
}

/// Plans all non-memory stage-2 mappings for a VM.
pub(crate) fn build_address_layout(
    policy: AddressSpacePolicy,
    guest_base: usize,
    guest_size: usize,
    passthrough_devices: &[PassThroughDeviceConfig],
    passthrough_addresses: &[PassThroughAddressConfig],
    owned_regions: &[GuestOwnedRegion],
    emulated_resources: &[Resource],
) -> AxVmResult<VmAddressLayout> {
    let mut planner = GuestRegionPlanner::new(policy, guest_base, guest_size)?;

    for region in owned_regions {
        planner.reserve(region.base, region.length, region.kind)?;
    }

    for resource in emulated_resources {
        if let Resource::MmioRange { base, size } = *resource {
            let base = usize::try_from(base).map_err(|_| {
                ax_err_type!(
                    InvalidInput,
                    format!("emulated MMIO base exceeds usize: {base:#x}")
                )
            })?;
            let size = usize::try_from(size).map_err(|_| {
                ax_err_type!(
                    InvalidInput,
                    format!("emulated MMIO size exceeds usize: {size:#x}")
                )
            })?;
            planner.reserve(base, size, VmRegionKind::EmulatedDevice)?;
        }
    }

    for device in passthrough_devices {
        planner.add_passthrough_mapping(device.base_gpa, device.base_hpa, device.length)?;
    }

    for address in passthrough_addresses {
        planner.add_identity_passthrough(address.base_gpa, address.length)?;
    }

    planner.finish()
}

fn device_mapping_flags() -> MappingFlags {
    MappingFlags::DEVICE | MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER
}

fn normalize_guest_range(name: &str, base: usize, length: usize) -> AxVmResult<(usize, usize)> {
    let end = checked_end(name, base, length)?;
    let aligned_base = align_down_4k(base);
    let aligned_end = align_up_checked(end).ok_or_else(|| {
        ax_err_type!(
            InvalidInput,
            format!("{name} range [{base:#x}, {end:#x}) overflows when aligned")
        )
    })?;
    Ok((aligned_base, aligned_end - aligned_base))
}

fn normalize_linear_range(
    name: &str,
    base_gpa: usize,
    base_hpa: usize,
    length: usize,
) -> AxVmResult<(usize, usize, usize)> {
    let end_gpa = checked_end(name, base_gpa, length)?;
    checked_end(name, base_hpa, length)?;

    let gpa_offset = base_gpa - align_down_4k(base_gpa);
    let hpa_offset = base_hpa - align_down_4k(base_hpa);
    if gpa_offset != hpa_offset {
        return Err(ax_err_type!(
            InvalidInput,
            format!(
                "{name} range has different GPA/HPA page offsets: gpa={base_gpa:#x}, \
                 hpa={base_hpa:#x}"
            )
        ));
    }

    let aligned_gpa = align_down_4k(base_gpa);
    let aligned_hpa = align_down_4k(base_hpa);
    let aligned_end = align_up_checked(end_gpa).ok_or_else(|| {
        ax_err_type!(
            InvalidInput,
            format!("{name} range [{base_gpa:#x}, {end_gpa:#x}) overflows when aligned")
        )
    })?;
    let aligned_size = aligned_end - aligned_gpa;
    aligned_hpa.checked_add(aligned_size).ok_or_else(|| {
        ax_err_type!(
            InvalidInput,
            format!(
                "{name} host range overflows when aligned: hpa={base_hpa:#x}, length={length:#x}"
            )
        )
    })?;
    Ok((aligned_gpa, aligned_hpa, aligned_size))
}

fn checked_end(name: &str, base: usize, length: usize) -> AxVmResult<usize> {
    if length == 0 {
        return Err(ax_err_type!(
            InvalidInput,
            format!("{name} range has zero length")
        ));
    }
    base.checked_add(length).ok_or_else(|| {
        ax_err_type!(
            InvalidInput,
            format!("{name} range overflows: base={base:#x}, length={length:#x}")
        )
    })
}

fn align_up_checked(value: usize) -> Option<usize> {
    value.checked_add(PAGE_SIZE_4K - 1).map(align_down_4k)
}

fn ranges_overlap(base_a: usize, size_a: usize, base_b: usize, size_b: usize) -> bool {
    let end_a = base_a + size_a;
    let end_b = base_b + size_b;
    base_a < end_b && base_b < end_a
}

fn linear_delta(mapping: &VmStage2Mapping) -> i128 {
    mapping.gpa.as_usize() as i128 - mapping.hpa.as_usize() as i128
}

fn same_linear_mapping(left: &VmStage2Mapping, right: &VmStage2Mapping) -> bool {
    left.kind == right.kind
        && left.flags == right.flags
        && linear_delta(left) == linear_delta(right)
}

fn merge_linear_mappings(
    left: VmStage2Mapping,
    right: VmStage2Mapping,
) -> AxVmResult<VmStage2Mapping> {
    debug_assert!(same_linear_mapping(&left, &right));
    let base_gpa = min(left.gpa.as_usize(), right.gpa.as_usize());
    let end_gpa = max(left.gpa_end(), right.gpa_end());
    let delta = linear_delta(&left);
    let base_hpa = usize::try_from(base_gpa as i128 - delta).map_err(|_| {
        ax_err_type!(
            InvalidInput,
            format!(
                "merged passthrough mapping underflows host address: gpa={base_gpa:#x}, \
                 delta={delta:#x}"
            )
        )
    })?;
    Ok(VmStage2Mapping::new(
        base_gpa,
        base_hpa,
        end_gpa - base_gpa,
        left.flags,
        left.kind,
    ))
}

fn owned_overlap_allowed(existing: &PlannedRegion, new: &PlannedRegion) -> bool {
    matches!(
        (existing.kind, new.kind),
        (VmRegionKind::Memory, VmRegionKind::BootDescription)
            | (VmRegionKind::BootDescription, VmRegionKind::Memory)
    ) && (existing.contains(new) || new.contains(existing))
}

impl VmRegionKind {
    fn name(self) -> &'static str {
        match self {
            Self::Passthrough => "passthrough",
            Self::Memory => "memory",
            Self::BootDescription => "boot description",
            Self::EmulatedDevice => "emulated device",
            Self::Reserved => "reserved",
        }
    }
}

#[cfg(test)]
mod tests {
    use axvm_types::{PassThroughAddressConfig, PassThroughDeviceConfig};

    use super::*;

    const GUEST_BASE: usize = 0;
    const GUEST_SIZE: usize = 0x1_0000;

    #[test]
    fn virtualized_policy_only_maps_explicit_passthrough() {
        let layout = build_address_layout(
            AddressSpacePolicy::Virtualized,
            GUEST_BASE,
            GUEST_SIZE,
            &[],
            &[],
            &[],
            &[],
        )
        .unwrap();
        assert!(layout.mappings().is_empty());

        let device = PassThroughDeviceConfig {
            name: alloc::string::String::from("uart"),
            base_gpa: 0x2000,
            base_hpa: 0x9000,
            length: 0x1000,
            irq_id: 0,
        };
        let layout = build_address_layout(
            AddressSpacePolicy::Virtualized,
            GUEST_BASE,
            GUEST_SIZE,
            &[device],
            &[],
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(layout.mappings().len(), 1);
        assert_eq!(layout.mappings()[0].gpa.as_usize(), 0x2000);
        assert_eq!(layout.mappings()[0].hpa.as_usize(), 0x9000);
        assert_eq!(layout.mappings()[0].size, 0x1000);
    }

    #[test]
    fn passthrough_policy_punches_memory_and_emulated_mmio_holes() {
        let owned = [GuestOwnedRegion::new(0x2000, 0x2000, VmRegionKind::Memory)];
        let emu = [Resource::MmioRange {
            base: 0x8000,
            size: 0x1000,
        }];

        let layout = build_address_layout(
            AddressSpacePolicy::Passthrough,
            GUEST_BASE,
            GUEST_SIZE,
            &[],
            &[],
            &owned,
            &emu,
        )
        .unwrap();

        let mappings = layout.mappings();
        assert_eq!(mappings.len(), 3);
        assert_eq!((mappings[0].gpa.as_usize(), mappings[0].size), (0, 0x2000));
        assert_eq!(
            (mappings[1].gpa.as_usize(), mappings[1].size),
            (0x4000, 0x4000)
        );
        assert_eq!(
            (mappings[2].gpa.as_usize(), mappings[2].size),
            (0x9000, 0x7000)
        );
        let owned_regions = layout.owned_regions().collect::<Vec<_>>();
        assert_eq!(owned_regions.len(), 2);
        assert_eq!(
            (owned_regions[0].gpa.as_usize(), owned_regions[0].size),
            (0x2000, 0x2000)
        );
        assert_eq!(owned_regions[0].kind, VmRegionKind::Memory);
        assert_eq!(
            (owned_regions[1].gpa.as_usize(), owned_regions[1].size),
            (0x8000, 0x1000)
        );
        assert_eq!(owned_regions[1].kind, VmRegionKind::EmulatedDevice);
    }

    #[test]
    fn passthrough_policy_punches_reserved_regions() {
        let owned = [GuestOwnedRegion::new(
            0x3000,
            0x2000,
            VmRegionKind::Reserved,
        )];

        let layout = build_address_layout(
            AddressSpacePolicy::Passthrough,
            GUEST_BASE,
            GUEST_SIZE,
            &[],
            &[],
            &owned,
            &[],
        )
        .unwrap();

        let mappings = layout.mappings();
        assert_eq!(mappings.len(), 2);
        assert_eq!((mappings[0].gpa.as_usize(), mappings[0].size), (0, 0x3000));
        assert_eq!(
            (mappings[1].gpa.as_usize(), mappings[1].size),
            (0x5000, 0xb000)
        );
        assert!(mappings.iter().all(|mapping| {
            !ranges_overlap(mapping.gpa.as_usize(), mapping.size, 0x3000, 0x2000)
        }));
        let owned_regions = layout.owned_regions().collect::<Vec<_>>();
        assert_eq!(owned_regions.len(), 1);
        assert_eq!(owned_regions[0].kind, VmRegionKind::Reserved);
    }

    #[test]
    fn passthrough_device_uses_base_hpa_and_keeps_non_contiguous_mappings_split() {
        let devices = [
            PassThroughDeviceConfig {
                name: alloc::string::String::from("dev0"),
                base_gpa: 0x1000,
                base_hpa: 0x9000,
                length: 0x1000,
                irq_id: 0,
            },
            PassThroughDeviceConfig {
                name: alloc::string::String::from("dev1"),
                base_gpa: 0x2000,
                base_hpa: 0xb000,
                length: 0x1000,
                irq_id: 0,
            },
        ];

        let layout = build_address_layout(
            AddressSpacePolicy::Virtualized,
            GUEST_BASE,
            GUEST_SIZE,
            &devices,
            &[],
            &[],
            &[],
        )
        .unwrap();

        assert_eq!(layout.mappings().len(), 2);
        assert_eq!(layout.mappings()[0].hpa.as_usize(), 0x9000);
        assert_eq!(layout.mappings()[1].hpa.as_usize(), 0xb000);
    }

    #[test]
    fn duplicate_explicit_passthrough_ranges_are_merged_when_linear_mapping_matches() {
        let devices = [
            PassThroughDeviceConfig {
                name: alloc::string::String::from("dev0"),
                base_gpa: 0x1000,
                base_hpa: 0x9000,
                length: 0x2000,
                irq_id: 0,
            },
            PassThroughDeviceConfig {
                name: alloc::string::String::from("dev1"),
                base_gpa: 0x2000,
                base_hpa: 0xa000,
                length: 0x2000,
                irq_id: 0,
            },
        ];

        let layout = build_address_layout(
            AddressSpacePolicy::Virtualized,
            GUEST_BASE,
            GUEST_SIZE,
            &devices,
            &[],
            &[],
            &[],
        )
        .unwrap();

        assert_eq!(layout.mappings().len(), 1);
        assert_eq!(layout.mappings()[0].gpa.as_usize(), 0x1000);
        assert_eq!(layout.mappings()[0].hpa.as_usize(), 0x9000);
        assert_eq!(layout.mappings()[0].size, 0x3000);
    }

    #[test]
    fn passthrough_address_is_identity_and_unaligned_ranges_are_expanded() {
        let addresses = [PassThroughAddressConfig {
            base_gpa: 0x1803,
            length: 0x20,
        }];

        let layout = build_address_layout(
            AddressSpacePolicy::Virtualized,
            GUEST_BASE,
            GUEST_SIZE,
            &[],
            &addresses,
            &[],
            &[],
        )
        .unwrap();

        assert_eq!(layout.mappings().len(), 1);
        assert_eq!(layout.mappings()[0].gpa.as_usize(), 0x1000);
        assert_eq!(layout.mappings()[0].hpa.as_usize(), 0x1000);
        assert_eq!(layout.mappings()[0].size, 0x1000);
    }

    #[test]
    fn invalid_and_conflicting_ranges_are_rejected() {
        let zero = [PassThroughAddressConfig {
            base_gpa: 0x1000,
            length: 0,
        }];
        assert!(
            build_address_layout(
                AddressSpacePolicy::Virtualized,
                GUEST_BASE,
                GUEST_SIZE,
                &[],
                &zero,
                &[],
                &[],
            )
            .is_err()
        );

        let owned = [GuestOwnedRegion::new(0x2000, 0x1000, VmRegionKind::Memory)];
        let conflict = [PassThroughAddressConfig {
            base_gpa: 0x2000,
            length: 0x1000,
        }];
        assert!(
            build_address_layout(
                AddressSpacePolicy::Virtualized,
                GUEST_BASE,
                GUEST_SIZE,
                &[],
                &conflict,
                &owned,
                &[],
            )
            .is_err()
        );

        let conflicting_hpa = [
            PassThroughDeviceConfig {
                name: alloc::string::String::from("dev0"),
                base_gpa: 0x3000,
                base_hpa: 0x9000,
                length: 0x1000,
                irq_id: 0,
            },
            PassThroughDeviceConfig {
                name: alloc::string::String::from("dev1"),
                base_gpa: 0x3000,
                base_hpa: 0xa000,
                length: 0x1000,
                irq_id: 0,
            },
        ];
        assert!(
            build_address_layout(
                AddressSpacePolicy::Virtualized,
                GUEST_BASE,
                GUEST_SIZE,
                &conflicting_hpa,
                &[],
                &[],
                &[],
            )
            .is_err()
        );
    }
}
