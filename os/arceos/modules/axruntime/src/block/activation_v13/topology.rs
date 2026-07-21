//! Transactional fixed-CPU ownership for staged hardware domains.

use alloc::{boxed::Box, vec, vec::Vec};

use ax_driver::block::BlockDeviceBinding;
use ax_hal::irq::IrqId;
use rdif_block::{ActivationPlan, ControlDomainActivation, IrqSourceId, OwnershipDomainId};

use super::V13ActivationError;
use crate::block::controller::IrqLineOwnershipReservation;

/// One portable source resolved by identity rather than binding-array index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedDomainIrq {
    source: IrqSourceId,
    irq: IrqId,
}

/// Immutable owner selection for one portable hardware ownership domain.
#[derive(Debug)]
pub struct FixedDomainOwner {
    domain: OwnershipDomainId,
    owner_cpu: usize,
    sources: Box<[ResolvedDomainIrq]>,
}

impl FixedDomainOwner {
    pub const fn domain(&self) -> OwnershipDomainId {
        self.domain
    }

    pub const fn owner_cpu(&self) -> usize {
        self.owner_cpu
    }

    pub fn irq_for_source(&self, source: IrqSourceId) -> Option<IrqId> {
        self.sources
            .iter()
            .find(|resolved| resolved.source == source)
            .map(|resolved| resolved.irq)
    }
}

/// Controller-lifetime CPU and physical-line reservations.
///
/// Components are reserved before this value is returned. Dropping any failed
/// construction path releases all earlier reservations in reverse ownership
/// order, so a rejected controller cannot leave a partial global affinity.
pub struct FixedOwnershipTopology {
    domains: Box<[FixedDomainOwner]>,
    _line_reservations: Box<[IrqLineOwnershipReservation]>,
}

impl FixedOwnershipTopology {
    /// Resolves every selected source from immutable platform binding facts and
    /// reserves one CPU for each connected physical IRQ-line component.
    pub fn reserve(
        binding: &BlockDeviceBinding,
        plan: &ActivationPlan,
        online_cpu_count: usize,
    ) -> Result<Self, V13ActivationError> {
        let unresolved = selected_domains(plan, |source| {
            let binding = binding
                .irq_for_source(source.get())
                .cloned()
                .ok_or(V13ActivationError::MissingIrqBinding { source_id: source })?;
            crate::irq::resolve_binding_irq(binding).map_err(V13ActivationError::Irq)
        })?;
        Self::reserve_resolved(unresolved, online_cpu_count)
    }

    pub fn domain(&self, domain: OwnershipDomainId) -> Option<&FixedDomainOwner> {
        self.domains
            .iter()
            .find(|candidate| candidate.domain == domain)
    }

    fn reserve_resolved(
        unresolved: Vec<UnreservedDomain>,
        online_cpu_count: usize,
    ) -> Result<Self, V13ActivationError> {
        if online_cpu_count == 0 {
            return Err(V13ActivationError::NoOnlineCpu);
        }
        let components = connected_components(&unresolved);
        let fallback_cpu = ax_hal::percpu::this_cpu_id() % online_cpu_count;
        let mut reservations = Vec::with_capacity(components.len());
        let mut owner_by_domain = vec![None; rdif_block::MAX_OWNERSHIP_DOMAINS];
        for (component_index, component) in components.into_iter().enumerate() {
            let lines = component_lines(&unresolved, &component);
            let preferred_cpu =
                preferred_component_cpu(fallback_cpu, component_index, online_cpu_count);
            let reservation =
                IrqLineOwnershipReservation::reserve(lines, online_cpu_count, preferred_cpu)
                    .map_err(V13ActivationError::Topology)?;
            let owner_cpu = reservation.owner_cpu();
            for domain_index in component {
                owner_by_domain[unresolved[domain_index].domain.get()] = Some(owner_cpu);
            }
            reservations.push(reservation);
        }
        let domains = unresolved
            .into_iter()
            .map(|domain| FixedDomainOwner {
                domain: domain.domain,
                owner_cpu: owner_by_domain[domain.domain.get()]
                    .expect("every selected domain belongs to one IRQ component"),
                sources: domain.sources.into_boxed_slice(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self {
            domains,
            _line_reservations: reservations.into_boxed_slice(),
        })
    }
}

struct UnreservedDomain {
    domain: OwnershipDomainId,
    sources: Vec<ResolvedDomainIrq>,
}

fn selected_domains(
    plan: &ActivationPlan,
    mut resolve: impl FnMut(IrqSourceId) -> Result<IrqId, V13ActivationError>,
) -> Result<Vec<UnreservedDomain>, V13ActivationError> {
    let mut domains = plan
        .domains()
        .iter()
        .map(|domain| resolve_domain_sources(domain.domain(), domain.irq_sources(), &mut resolve))
        .collect::<Result<Vec<_>, _>>()?;
    if let ControlDomainActivation::Independent {
        domain,
        irq_sources,
    } = plan.control_activation()
    {
        domains.push(resolve_domain_sources(domain, irq_sources, &mut resolve)?);
    }
    domains.sort_unstable_by_key(|domain| domain.domain);
    Ok(domains)
}

fn resolve_domain_sources(
    domain: OwnershipDomainId,
    source_ids: rdif_block::IdList,
    resolve: &mut impl FnMut(IrqSourceId) -> Result<IrqId, V13ActivationError>,
) -> Result<UnreservedDomain, V13ActivationError> {
    let sources = source_ids
        .iter()
        .map(|source_id| {
            let source = IrqSourceId::new(source_id)
                .expect("IdList iteration yields only source identities in 0..64");
            Ok(ResolvedDomainIrq {
                source,
                irq: resolve(source)?,
            })
        })
        .collect::<Result<Vec<_>, V13ActivationError>>()?;
    Ok(UnreservedDomain { domain, sources })
}

fn connected_components(domains: &[UnreservedDomain]) -> Vec<Vec<usize>> {
    let mut visited = vec![false; domains.len()];
    let mut components = Vec::new();
    for root in 0..domains.len() {
        if visited[root] {
            continue;
        }
        visited[root] = true;
        let mut component = vec![root];
        let mut cursor = 0;
        while cursor < component.len() {
            let current = component[cursor];
            for candidate in 0..domains.len() {
                if !visited[candidate] && domains_share_line(&domains[current], &domains[candidate])
                {
                    visited[candidate] = true;
                    component.push(candidate);
                }
            }
            cursor += 1;
        }
        components.push(component);
    }
    components
}

fn domains_share_line(first: &UnreservedDomain, second: &UnreservedDomain) -> bool {
    first
        .sources
        .iter()
        .any(|source| second.sources.iter().any(|peer| peer.irq == source.irq))
}

fn component_lines(domains: &[UnreservedDomain], component: &[usize]) -> Vec<IrqId> {
    let mut lines = component
        .iter()
        .flat_map(|index| domains[*index].sources.iter().map(|source| source.irq))
        .collect::<Vec<_>>();
    lines.sort_unstable();
    lines.dedup();
    lines
}

fn preferred_component_cpu(
    base_cpu: usize,
    component_index: usize,
    online_cpu_count: usize,
) -> usize {
    (base_cpu + component_index) % online_cpu_count
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use ax_hal::irq::{HwIrq, IrqDomainId};

    use super::*;

    #[test]
    fn physical_shared_line_forms_one_owner_component() {
        let domains = vec![
            test_domain(0, &[(3, test_irq(7))]),
            test_domain(1, &[(19, test_irq(7))]),
        ];

        let components = connected_components(&domains);

        assert_eq!(components, vec![vec![0, 1]]);
    }

    #[test]
    fn transitive_shared_lines_form_one_transaction() {
        let domains = vec![
            test_domain(0, &[(1, test_irq(4))]),
            test_domain(1, &[(2, test_irq(4)), (3, test_irq(9))]),
            test_domain(2, &[(4, test_irq(9))]),
        ];

        let components = connected_components(&domains);

        assert_eq!(components, vec![vec![0, 1, 2]]);
    }

    #[test]
    fn independent_vectors_remain_independent_owner_components() {
        let domains = vec![
            test_domain(0, &[(11, test_irq(5))]),
            test_domain(1, &[(17, test_irq(6))]),
        ];

        let components = connected_components(&domains);

        assert_eq!(components, vec![vec![0], vec![1]]);
    }

    #[test]
    fn independent_vectors_receive_stable_round_robin_owners() {
        let owners = (0..3)
            .map(|component| preferred_component_cpu(1, component, 4))
            .collect::<Vec<_>>();

        assert_eq!(owners, vec![1, 2, 3]);
    }

    #[test]
    fn sparse_source_identity_is_not_used_as_a_binding_index() {
        let domain = test_domain(0, &[(47, test_irq(12))]);

        assert_eq!(domain.sources.len(), 1);
        assert_eq!(domain.sources[0].source.get(), 47);
        assert_eq!(domain.sources[0].irq, test_irq(12));
    }

    fn test_domain(domain: usize, sources: &[(usize, IrqId)]) -> UnreservedDomain {
        UnreservedDomain {
            domain: OwnershipDomainId::new(domain).unwrap(),
            sources: sources
                .iter()
                .map(|(source, irq)| ResolvedDomainIrq {
                    source: IrqSourceId::new(*source).unwrap(),
                    irq: *irq,
                })
                .collect(),
        }
    }

    const fn test_irq(hwirq: u32) -> IrqId {
        IrqId::new(IrqDomainId(23), HwIrq(hwirq))
    }
}
