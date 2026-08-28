//! Deterministic Type-0 function and memory-BAR resolution.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::ToString,
    vec::Vec,
};
use core::{fmt, ops::Range};

use super::{
    FOUR_GIB, PciBarIndex, PciBdf, PciEndpointIdentity, PciError, PciFunctionSpec, PciResult,
    bar::ResolvedBarPlan, config::PowerOnConfig, placement::resolve_bar_addresses,
};
use crate::{DeviceNodeId, ResourceRequest};

const DEVICE_COUNT: u8 = 32;

/// Internal PCI topology declaration used while resolving a device graph.
pub(crate) struct PciTopologyBuilder {
    functions: BTreeMap<DeviceNodeId, PciFunctionSpec>,
    reservations: BTreeSet<PciBdf>,
}

impl PciTopologyBuilder {
    /// Creates an empty topology.
    pub(crate) const fn new() -> Self {
        Self {
            functions: BTreeMap::new(),
            reservations: BTreeSet::new(),
        }
    }

    /// Adds one function declaration.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::DuplicateFunction`] if the stable identity is
    /// already present.
    pub(crate) fn add_function(&mut self, function: PciFunctionSpec) -> PciResult {
        let id = function.id.clone();
        if self.functions.contains_key(&id) {
            return Err(PciError::DuplicateFunction {
                function: id.to_string(),
            });
        }
        self.functions.insert(id, function);
        Ok(())
    }

    /// Reserves a BDF against automatic and fixed endpoint placement.
    ///
    /// Duplicate reservations are accepted so composition code can merge
    /// platform policies without order sensitivity.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidAddress`] for a segment or bus outside the
    /// currently supported segment-zero, bus-zero topology.
    pub(crate) fn reserve_bdf(&mut self, bdf: PciBdf) -> PciResult {
        validate_supported_bdf(bdf)?;
        self.reservations.insert(bdf);
        Ok(())
    }

    /// Resolves all fixed requests, performs deterministic automatic
    /// placement, and validates the complete topology before graph ownership
    /// is assigned.
    ///
    /// The resolved functions carry placeholder `owner`/`host` identities
    /// (each pointing at itself) until graph resolution assigns real
    /// ownership. This is an internal intermediate step; callers must use
    /// [`DeclaredDeviceGraph::resolve`](crate::DeclaredDeviceGraph::resolve)
    /// for the sealed topology published to firmware and runtime.
    ///
    /// # Errors
    ///
    /// Returns a typed [`PciError`] for malformed host apertures, BDF
    /// conflicts, invalid identities, or BAR placement failures. No partial
    /// topology is returned.
    pub(crate) fn resolve(self, memory_aperture: Range<u64>) -> PciResult<ResolvedPciTopology> {
        validate_memory_aperture(&memory_aperture)?;
        let bdfs = resolve_bdfs(&self.functions, &self.reservations)?;
        let bar_addresses = resolve_bar_addresses(&memory_aperture, &self.functions)?;
        let mut functions = Vec::with_capacity(self.functions.len());
        for (id, spec) in self.functions {
            let bdf = bdfs[&id];
            let bars = spec
                .bars
                .iter()
                .map(|bar| ResolvedBarPlan {
                    index: bar.index(),
                    size: bar.size(),
                    address: bar_addresses[&(id.clone(), bar.index())],
                })
                .collect::<Vec<_>>();
            let power_on = PowerOnConfig::build(spec.identity, &bars, &spec.config_bytes)?;
            functions.push(ResolvedPciFunction {
                owner: id.clone(),
                host: id.clone(),
                id,
                identity: spec.identity,
                bdf,
                bars,
                power_on,
            });
        }
        functions.sort_by_key(|function| function.bdf);
        Ok(ResolvedPciTopology {
            memory_aperture,
            functions,
        })
    }
}

impl Default for PciTopologyBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PciTopologyBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PciTopologyBuilder")
            .field("function_count", &self.functions.len())
            .field("reservation_count", &self.reservations.len())
            .finish()
    }
}

/// One immutable, resolved PCI function.
pub struct ResolvedPciFunction {
    id: DeviceNodeId,
    owner: DeviceNodeId,
    host: DeviceNodeId,
    identity: PciEndpointIdentity,
    bdf: PciBdf,
    bars: Vec<ResolvedBarPlan>,
    pub(crate) power_on: PowerOnConfig,
}

impl ResolvedPciFunction {
    /// Returns the stable function identity.
    pub const fn id(&self) -> &DeviceNodeId {
        &self.id
    }

    /// Returns the graph node owning this function's state.
    pub const fn owner(&self) -> &DeviceNodeId {
        &self.owner
    }

    /// Returns the graph node owning this function's PCI host.
    pub const fn host(&self) -> &DeviceNodeId {
        &self.host
    }

    /// Returns the PCI identity fields.
    pub const fn identity(&self) -> PciEndpointIdentity {
        self.identity
    }

    /// Returns the resolved BDF.
    pub const fn bdf(&self) -> PciBdf {
        self.bdf
    }

    /// Returns one resolved BAR descriptor.
    pub fn bar(&self, index: PciBarIndex) -> Option<ResolvedPciBar> {
        self.bars
            .iter()
            .find(|bar| bar.index == index)
            .copied()
            .map(ResolvedPciBar)
    }

    pub(crate) fn bars(&self) -> &[ResolvedBarPlan] {
        &self.bars
    }
}

impl fmt::Debug for ResolvedPciFunction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedPciFunction")
            .field("id", &self.id)
            .field("owner", &self.owner)
            .field("host", &self.host)
            .field("identity", &self.identity)
            .field("bdf", &self.bdf)
            .field("bars", &self.bars)
            .finish()
    }
}

/// Public immutable view of one resolved 32-bit memory BAR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedPciBar(ResolvedBarPlan);

impl ResolvedPciBar {
    /// Returns the BAR slot.
    pub const fn index(self) -> PciBarIndex {
        self.0.index
    }

    /// Returns the assigned guest-physical address.
    pub const fn address(self) -> u64 {
        self.0.address
    }

    /// Returns the fixed BAR size.
    pub const fn size(self) -> u64 {
        self.0.size
    }
}

/// Immutable PCI topology shared by later firmware and runtime integration.
pub struct ResolvedPciTopology {
    memory_aperture: Range<u64>,
    functions: Vec<ResolvedPciFunction>,
}

impl ResolvedPciTopology {
    /// Returns the PCI memory aperture used for BAR placement and decode.
    pub const fn memory_aperture(&self) -> &Range<u64> {
        &self.memory_aperture
    }

    /// Returns functions in BDF order.
    pub fn functions(&self) -> impl Iterator<Item = &ResolvedPciFunction> {
        self.functions.iter()
    }

    /// Finds one function by stable identity.
    pub fn function(&self, id: &DeviceNodeId) -> Option<&ResolvedPciFunction> {
        self.functions.iter().find(|function| function.id() == id)
    }

    pub(crate) fn function_plans(&self) -> &[ResolvedPciFunction] {
        &self.functions
    }

    pub(crate) fn assign_graph_ownership(
        &mut self,
        host: &DeviceNodeId,
        endpoint_ids: &BTreeSet<DeviceNodeId>,
    ) {
        for function in &mut self.functions {
            function.host = host.clone();
            function.owner = if endpoint_ids.contains(&function.id) {
                function.id.clone()
            } else {
                host.clone()
            };
        }
    }
}

impl fmt::Debug for ResolvedPciTopology {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedPciTopology")
            .field("memory_aperture", &self.memory_aperture)
            .field("functions", &self.functions)
            .finish()
    }
}

fn validate_memory_aperture(memory_aperture: &Range<u64>) -> PciResult {
    if memory_aperture.start >= memory_aperture.end {
        return Err(PciError::InvalidHostAperture {
            detail: "memory aperture is empty or reversed",
        });
    }
    if memory_aperture.end > FOUR_GIB {
        return Err(PciError::InvalidHostAperture {
            detail: "32-bit memory BAR aperture exceeds 4 GiB",
        });
    }
    Ok(())
}

fn resolve_bdfs(
    functions: &BTreeMap<DeviceNodeId, PciFunctionSpec>,
    reservations: &BTreeSet<PciBdf>,
) -> PciResult<BTreeMap<DeviceNodeId, PciBdf>> {
    let mut resolved = BTreeMap::new();
    let mut occupied = BTreeMap::<PciBdf, DeviceNodeId>::new();
    for (id, spec) in functions {
        if let ResourceRequest::Fixed(bdf) = spec.bdf {
            validate_supported_bdf(bdf)?;
            if bdf.function() != 0 {
                return Err(PciError::UnsupportedFunctionPlacement { bdf });
            }
            if reservations.contains(&bdf) {
                return Err(PciError::BdfReserved {
                    bdf,
                    function: id.to_string(),
                });
            }
            if let Some(existing) = occupied.insert(bdf, id.clone()) {
                return Err(PciError::DuplicateBdf {
                    bdf,
                    first: existing.to_string(),
                    second: id.to_string(),
                });
            }
            resolved.insert(id.clone(), bdf);
        }
    }
    for (id, spec) in functions {
        if spec.bdf == ResourceRequest::Auto {
            let bdf = (0..DEVICE_COUNT)
                .map(PciBdf::bus_zero)
                .find(|candidate| {
                    !occupied.contains_key(candidate) && !reservations.contains(candidate)
                })
                .ok_or_else(|| PciError::BdfExhausted {
                    function: id.to_string(),
                })?;
            occupied.insert(bdf, id.clone());
            resolved.insert(id.clone(), bdf);
        }
    }
    Ok(resolved)
}

fn validate_supported_bdf(bdf: PciBdf) -> PciResult {
    if bdf.segment().value() != 0 {
        return Err(PciError::InvalidAddress {
            component: "segment",
            value: u64::from(bdf.segment().value()),
        });
    }
    if bdf.bus() != 0 {
        return Err(PciError::InvalidAddress {
            component: "bus",
            value: u64::from(bdf.bus()),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConfigOffset, PciClass, PciMemoryBar, PciSegment};

    const APERTURE_START: u64 = 0x2000_0000;
    const APERTURE_END: u64 = 0x2040_0000;
    const BAR_SIZE: u64 = 0x1_0000;

    #[test]
    fn rejects_addresses_outside_conventional_pci_ranges() {
        assert!(matches!(
            PciBdf::new(PciSegment::new(0), 0, 32, 0),
            Err(PciError::InvalidAddress {
                component: "device",
                ..
            })
        ));
        assert!(matches!(
            PciBdf::new(PciSegment::new(0), 0, 0, 8),
            Err(PciError::InvalidAddress {
                component: "function",
                ..
            })
        ));
        assert!(matches!(
            PciBarIndex::new(6),
            Err(PciError::InvalidAddress {
                component: "BAR index",
                ..
            })
        ));
        assert!(matches!(
            ConfigOffset::new(0x100),
            Err(PciError::InvalidAddress {
                component: "config offset",
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_memory_bar_sizes() {
        let bar = PciBarIndex::new(2).unwrap();
        assert!(matches!(
            PciMemoryBar::new(bar, 0),
            Err(PciError::InvalidBar { .. })
        ));
        assert!(matches!(
            PciMemoryBar::new(bar, 0x18),
            Err(PciError::InvalidBar { .. })
        ));
        assert!(matches!(
            PciMemoryBar::new(bar, 1_u64 << 33),
            Err(PciError::InvalidBar { .. })
        ));
    }

    #[test]
    fn rejects_absent_vendor_identity_and_invalid_host_apertures() {
        let mut identity = PciTopologyBuilder::new();
        identity
            .add_function(PciFunctionSpec::new(
                node("absent"),
                PciEndpointIdentity::new(u16::MAX, 0x5678, PciClass::new(0x05, 0x00, 0x00)),
            ))
            .unwrap();
        assert!(matches!(
            identity.resolve(APERTURE_START..APERTURE_END),
            Err(PciError::InvalidEndpointIdentity { .. })
        ));

        assert!(matches!(
            PciTopologyBuilder::new().resolve(APERTURE_START..APERTURE_START),
            Err(PciError::InvalidHostAperture { .. })
        ));
        assert!(matches!(
            PciTopologyBuilder::new().resolve(APERTURE_START..(1_u64 << 32) + 1),
            Err(PciError::InvalidHostAperture { .. })
        ));
    }

    #[test]
    fn resolves_auto_bdfs_deterministically_and_skips_reservations() {
        let mut builder = PciTopologyBuilder::new();
        builder.reserve_bdf(bdf(0, 0)).unwrap();
        builder.reserve_bdf(bdf(31, 0)).unwrap();
        builder.add_function(function("beta")).unwrap();
        builder.add_function(function("alpha")).unwrap();

        let topology = builder.resolve(APERTURE_START..APERTURE_END).unwrap();

        assert_eq!(topology.function(&node("alpha")).unwrap().bdf(), bdf(1, 0));
        assert_eq!(topology.function(&node("beta")).unwrap().bdf(), bdf(2, 0));
    }

    #[test]
    fn rejects_fixed_requests_for_nonzero_functions() {
        let mut builder = PciTopologyBuilder::new();
        builder
            .add_function(function("endpoint").with_bdf(ResourceRequest::Fixed(bdf(3, 1))))
            .unwrap();
        assert!(matches!(
            builder.resolve(APERTURE_START..APERTURE_END),
            Err(PciError::UnsupportedFunctionPlacement { .. })
        ));
    }

    #[test]
    fn rejects_auto_bar_larger_than_the_aperture() {
        let bar0 = PciBarIndex::new(0).unwrap();
        let mut builder = PciTopologyBuilder::new();
        builder
            .add_function(
                function("oversized")
                    .with_bar(PciMemoryBar::new(bar0, 0x100_0000).unwrap())
                    .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            builder.resolve(APERTURE_START..APERTURE_END),
            Err(PciError::BarApertureExhausted { .. })
        ));
    }

    #[test]
    fn rejects_fixed_bars_at_size_misaligned_addresses() {
        let bar0 = PciBarIndex::new(0).unwrap();
        let mut builder = PciTopologyBuilder::new();
        builder
            .add_function(
                function("misaligned")
                    .with_bar(
                        PciMemoryBar::new(bar0, BAR_SIZE)
                            .unwrap()
                            .with_address(ResourceRequest::Fixed(APERTURE_START + 8)),
                    )
                    .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            builder.resolve(APERTURE_START..APERTURE_END),
            Err(PciError::InvalidBar { .. })
        ));
    }

    #[test]
    fn rejects_fixed_bdf_conflicts_and_reserved_requests() {
        let mut duplicate = PciTopologyBuilder::new();
        duplicate
            .add_function(function("alpha").with_bdf(ResourceRequest::Fixed(bdf(3, 0))))
            .unwrap();
        duplicate
            .add_function(function("beta").with_bdf(ResourceRequest::Fixed(bdf(3, 0))))
            .unwrap();
        assert!(matches!(
            duplicate.resolve(APERTURE_START..APERTURE_END),
            Err(PciError::DuplicateBdf { .. })
        ));

        let mut reserved = PciTopologyBuilder::new();
        reserved.reserve_bdf(bdf(3, 0)).unwrap();
        reserved
            .add_function(function("endpoint").with_bdf(ResourceRequest::Fixed(bdf(3, 0))))
            .unwrap();
        assert!(matches!(
            reserved.resolve(APERTURE_START..APERTURE_END),
            Err(PciError::BdfReserved { .. })
        ));
    }

    #[test]
    fn places_larger_auto_bars_first_and_preserves_function_order_tiebreaks() {
        let bar0 = PciBarIndex::new(0).unwrap();
        let bar2 = PciBarIndex::new(2).unwrap();
        let mut builder = PciTopologyBuilder::new();
        builder
            .add_function(
                function("beta")
                    .with_bar(PciMemoryBar::new(bar0, 0x1_0000).unwrap())
                    .unwrap(),
            )
            .unwrap();
        builder
            .add_function(
                function("alpha")
                    .with_bar(PciMemoryBar::new(bar2, 0x20_0000).unwrap())
                    .unwrap(),
            )
            .unwrap();

        let topology = builder.resolve(APERTURE_START..APERTURE_END).unwrap();

        assert_eq!(
            topology
                .function(&node("alpha"))
                .unwrap()
                .bar(bar2)
                .unwrap()
                .address(),
            APERTURE_START
        );
        assert_eq!(
            topology
                .function(&node("beta"))
                .unwrap()
                .bar(bar0)
                .unwrap()
                .address(),
            APERTURE_START + 0x20_0000
        );
    }

    #[test]
    fn rejects_fixed_bars_outside_or_overlapping_the_aperture() {
        let bar0 = PciBarIndex::new(0).unwrap();
        let mut outside = PciTopologyBuilder::new();
        outside
            .add_function(
                function("outside")
                    .with_bar(
                        PciMemoryBar::new(bar0, BAR_SIZE)
                            .unwrap()
                            .with_address(ResourceRequest::Fixed(APERTURE_END)),
                    )
                    .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            outside.resolve(APERTURE_START..APERTURE_END),
            Err(PciError::InvalidBar { .. })
        ));

        let mut overlap = PciTopologyBuilder::new();
        for id in ["alpha", "beta"] {
            overlap
                .add_function(
                    function(id)
                        .with_bar(
                            PciMemoryBar::new(bar0, BAR_SIZE)
                                .unwrap()
                                .with_address(ResourceRequest::Fixed(APERTURE_START)),
                        )
                        .unwrap(),
                )
                .unwrap();
        }
        assert!(matches!(
            overlap.resolve(APERTURE_START..APERTURE_END),
            Err(PciError::BarConflict { .. })
        ));
    }

    fn function(id: &str) -> PciFunctionSpec {
        PciFunctionSpec::new(
            node(id),
            PciEndpointIdentity::new(0x1234, 0x5678, PciClass::new(0x05, 0x00, 0x00))
                .with_revision(1),
        )
    }

    fn node(id: &str) -> DeviceNodeId {
        DeviceNodeId::new(id).unwrap()
    }

    fn bdf(device: u8, function: u8) -> PciBdf {
        PciBdf::new(PciSegment::new(0), 0, device, function).unwrap()
    }
}
