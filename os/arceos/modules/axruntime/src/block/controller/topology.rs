//! Immutable CPU and IRQ-line ownership for one controller maintenance domain.

use alloc::{boxed::Box, vec::Vec};

use ax_driver::block::RdifBlockDevice;
use ax_hal::irq::IrqId;
use ax_kspin::SpinNoPreempt;

use super::BlockControllerError;

const BLOCK_IRQ_OWNERSHIP_CAPACITY: usize = 256;

/// A controller-lifetime reservation of every platform IRQ line that may be
/// used by one maintenance domain.
///
/// The runtime does not support CPU hotplug or owner migration. Consequently,
/// both the online-CPU snapshot and the selected owner remain immutable until
/// this value is dropped with the controller.
pub(in crate::block) struct OwnershipDomainTopology {
    owner_cpu: usize,
    online_cpu_count: usize,
    sources: Box<[ResolvedIrqSource]>,
    lines: Box<[IrqId]>,
}

/// Controller-lifetime reservation of one connected physical IRQ-line set.
///
/// Staged v0.13 activation groups ownership domains that share a physical line
/// before calling this API. One reservation therefore fixes the CPU for the
/// complete connected component without exposing the global registry.
pub(in crate::block) struct IrqLineOwnershipReservation {
    owner_cpu: usize,
    lines: Box<[IrqId]>,
}

impl IrqLineOwnershipReservation {
    pub(in crate::block) fn reserve(
        mut lines: Vec<IrqId>,
        online_cpu_count: usize,
        fallback_cpu: usize,
    ) -> Result<Self, BlockControllerError> {
        lines.sort_unstable();
        lines.dedup();
        let owner_cpu = BLOCK_IRQ_OWNERS.lock().reserve_domain_on_cpu(
            &lines,
            online_cpu_count,
            fallback_cpu,
        )?;
        Ok(Self {
            owner_cpu,
            lines: lines.into_boxed_slice(),
        })
    }

    pub(in crate::block) const fn owner_cpu(&self) -> usize {
        self.owner_cpu
    }
}

impl Drop for IrqLineOwnershipReservation {
    fn drop(&mut self) {
        BLOCK_IRQ_OWNERS
            .lock()
            .release_domain(&self.lines, self.owner_cpu);
    }
}

impl OwnershipDomainTopology {
    /// Resolves platform bindings and reserves a stable CPU for every physical
    /// line before the maintenance thread is created.
    pub(in crate::block) fn reserve(
        device: &RdifBlockDevice,
    ) -> Result<Self, BlockControllerError> {
        let online_cpu_count = crate::runtime_cpu_count();
        if online_cpu_count == 0 {
            return Err(BlockControllerError::NoOnlineCpu);
        }
        let (sources, lines) = resolve_irq_topology(device)?;
        let fallback_cpu = ax_hal::percpu::this_cpu_id() % online_cpu_count;
        let owner_cpu =
            BLOCK_IRQ_OWNERS
                .lock()
                .reserve_domain(&lines, online_cpu_count, fallback_cpu)?;
        Ok(Self {
            owner_cpu,
            online_cpu_count,
            sources: sources.into_boxed_slice(),
            lines: lines.into_boxed_slice(),
        })
    }

    pub(in crate::block) const fn owner_cpu(&self) -> usize {
        self.owner_cpu
    }

    pub(in crate::block) const fn online_cpu_count(&self) -> usize {
        self.online_cpu_count
    }

    pub(in crate::block) fn irq_for_source(&self, source_id: usize) -> Option<IrqId> {
        self.sources
            .iter()
            .find(|source| source.source_id == source_id)
            .map(|source| source.irq)
    }
}

impl Drop for OwnershipDomainTopology {
    fn drop(&mut self) {
        BLOCK_IRQ_OWNERS
            .lock()
            .release_domain(&self.lines, self.owner_cpu);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResolvedIrqSource {
    source_id: usize,
    irq: IrqId,
}

fn resolve_irq_topology(
    device: &RdifBlockDevice,
) -> Result<(Vec<ResolvedIrqSource>, Vec<IrqId>), BlockControllerError> {
    let mut sources = Vec::with_capacity(device.irq_sources().len());
    for binding in device.irq_sources() {
        if sources
            .iter()
            .any(|source: &ResolvedIrqSource| source.source_id == binding.source_id)
        {
            return Err(BlockControllerError::DuplicateIrqSource(binding.source_id));
        }
        sources.push(ResolvedIrqSource {
            source_id: binding.source_id,
            irq: crate::irq::resolve_binding_irq(binding.irq.clone())?,
        });
    }
    sources.sort_unstable_by_key(|source| source.source_id);
    let mut lines = sources.iter().map(|source| source.irq).collect::<Vec<_>>();
    lines.sort_unstable();
    lines.dedup();
    Ok((sources, lines))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LineOwnership {
    irq: IrqId,
    owner_cpu: usize,
    domains: usize,
}

struct OwnershipRegistry {
    entries: [Option<LineOwnership>; BLOCK_IRQ_OWNERSHIP_CAPACITY],
}

impl OwnershipRegistry {
    const fn new() -> Self {
        Self {
            entries: [None; BLOCK_IRQ_OWNERSHIP_CAPACITY],
        }
    }

    fn reserve_domain(
        &mut self,
        lines: &[IrqId],
        online_cpu_count: usize,
        fallback_cpu: usize,
    ) -> Result<usize, BlockControllerError> {
        if online_cpu_count == 0 {
            return Err(BlockControllerError::NoOnlineCpu);
        }
        let existing_owner = self.existing_owner(lines)?;
        let owner_cpu = existing_owner
            .map(|(_, cpu)| cpu)
            .unwrap_or_else(|| stable_owner_cpu(lines, online_cpu_count, fallback_cpu));
        self.commit_domain(lines, online_cpu_count, owner_cpu)
    }

    fn reserve_domain_on_cpu(
        &mut self,
        lines: &[IrqId],
        online_cpu_count: usize,
        preferred_cpu: usize,
    ) -> Result<usize, BlockControllerError> {
        if online_cpu_count == 0 {
            return Err(BlockControllerError::NoOnlineCpu);
        }
        let owner_cpu = self
            .existing_owner(lines)?
            .map_or(preferred_cpu, |(_, cpu)| cpu);
        self.commit_domain(lines, online_cpu_count, owner_cpu)
    }

    fn commit_domain(
        &mut self,
        lines: &[IrqId],
        online_cpu_count: usize,
        owner_cpu: usize,
    ) -> Result<usize, BlockControllerError> {
        if owner_cpu >= online_cpu_count {
            return Err(BlockControllerError::IrqOwnerOutsideOnlineSet {
                owner_cpu,
                online_cpu_count,
            });
        }
        let new_line_count = lines
            .iter()
            .filter(|line| self.find_entry(**line).is_none())
            .count();
        if new_line_count > self.entries.iter().filter(|entry| entry.is_none()).count() {
            return Err(BlockControllerError::IrqOwnershipCapacity);
        }
        if lines.iter().any(|line| {
            self.find_entry(*line).is_some_and(|index| {
                self.entries[index].is_some_and(|entry| entry.domains == usize::MAX)
            })
        }) {
            return Err(BlockControllerError::IrqOwnershipCapacity);
        }

        for line in lines {
            match self.find_entry(*line) {
                Some(index) => {
                    let entry = self.entries[index]
                        .as_mut()
                        .expect("the ownership entry index was just resolved");
                    entry.domains += 1;
                }
                None => {
                    let index = self
                        .entries
                        .iter()
                        .position(Option::is_none)
                        .expect("ownership capacity was validated before insertion");
                    self.entries[index] = Some(LineOwnership {
                        irq: *line,
                        owner_cpu,
                        domains: 1,
                    });
                }
            }
        }
        Ok(owner_cpu)
    }

    fn existing_owner(
        &self,
        lines: &[IrqId],
    ) -> Result<Option<(IrqId, usize)>, BlockControllerError> {
        let mut existing_owner = None;
        for line in lines {
            let Some(index) = self.find_entry(*line) else {
                continue;
            };
            let ownership =
                self.entries[index].expect("the ownership entry index was just resolved");
            match existing_owner {
                None => existing_owner = Some((ownership.irq, ownership.owner_cpu)),
                Some((first_irq, first_cpu)) if first_cpu != ownership.owner_cpu => {
                    return Err(BlockControllerError::IrqOwnershipConflict {
                        first_irq,
                        first_cpu,
                        conflicting_irq: ownership.irq,
                        conflicting_cpu: ownership.owner_cpu,
                    });
                }
                Some(_) => {}
            }
        }
        Ok(existing_owner)
    }

    fn release_domain(&mut self, lines: &[IrqId], owner_cpu: usize) {
        for line in lines {
            let index = self
                .find_entry(*line)
                .expect("a live ownership domain must retain every reserved IRQ line");
            let entry = self.entries[index]
                .as_mut()
                .expect("the ownership entry index was just resolved");
            assert_eq!(
                entry.owner_cpu, owner_cpu,
                "an IRQ ownership domain changed CPU before release"
            );
            assert_ne!(entry.domains, 0, "IRQ ownership reference underflow");
            entry.domains -= 1;
            if entry.domains == 0 {
                self.entries[index] = None;
            }
        }
    }

    fn find_entry(&self, irq: IrqId) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.irq == irq))
    }
}

fn stable_owner_cpu(lines: &[IrqId], online_cpu_count: usize, fallback_cpu: usize) -> usize {
    lines.iter().min().map_or(fallback_cpu, |irq| {
        let seed = (u64::from(irq.domain.0) << 32) | u64::from(irq.hwirq.0);
        (seed % online_cpu_count as u64) as usize
    })
}

static BLOCK_IRQ_OWNERS: SpinNoPreempt<OwnershipRegistry> =
    SpinNoPreempt::new(OwnershipRegistry::new());

#[cfg(test)]
mod tests {
    use ax_hal::irq::{HwIrq, IrqDomainId};

    use super::*;

    #[test]
    fn shared_line_inherits_the_existing_owner_cpu() {
        let mut registry = OwnershipRegistry::new();
        let irq = test_irq(1);

        let first = registry.reserve_domain(&[irq], 4, 0).unwrap();
        let second = registry.reserve_domain(&[irq], 4, 3).unwrap();

        assert_eq!(second, first);
        registry.release_domain(&[irq], first);
        registry.release_domain(&[irq], second);
        assert!(registry.find_entry(irq).is_none());
    }

    #[test]
    fn conflicting_existing_lines_fail_without_partial_reservation() {
        let mut registry = OwnershipRegistry::new();
        let first_irq = test_irq(1);
        let second_irq = test_irq(2);
        let first_cpu = registry.reserve_domain(&[first_irq], 4, 0).unwrap();
        let second_cpu = registry.reserve_domain(&[second_irq], 4, 0).unwrap();
        assert_ne!(first_cpu, second_cpu);

        let error = registry
            .reserve_domain(&[first_irq, second_irq], 4, 0)
            .unwrap_err();

        assert!(matches!(
            error,
            BlockControllerError::IrqOwnershipConflict { .. }
        ));
        assert_eq!(
            registry.entries.iter().flatten().count(),
            2,
            "a rejected multi-line domain must not mutate the registry"
        );
    }

    #[test]
    fn vector_based_owner_selection_is_stable_and_bounded() {
        let lines = [test_irq(11), test_irq(7)];
        let first = stable_owner_cpu(&lines, 3, 2);
        let second = stable_owner_cpu(&lines, 3, 0);

        assert_eq!(first, second);
        assert!(first < 3);
        assert_eq!(stable_owner_cpu(&[], 3, 2), 2);
    }

    #[test]
    fn explicit_owner_reservation_reuses_shared_line_owner() {
        let mut registry = OwnershipRegistry::new();
        let irq = test_irq(13);

        let first = registry.reserve_domain_on_cpu(&[irq], 4, 2).unwrap();
        let second = registry.reserve_domain_on_cpu(&[irq], 4, 1).unwrap();

        assert_eq!(first, 2);
        assert_eq!(second, first);
        registry.release_domain(&[irq], first);
        registry.release_domain(&[irq], second);
    }

    const fn test_irq(hwirq: u32) -> IrqId {
        IrqId::new(IrqDomainId(7), HwIrq(hwirq))
    }
}
