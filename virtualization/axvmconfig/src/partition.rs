//! Deterministic guest-vCPU partition planning.

use alloc::{collections::BTreeMap, vec::Vec};

use crate::{AxVmConfigError, AxVmConfigResult};

/// CPU-affinity inputs for all vCPUs belonging to one VM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmCpuPartitionInput {
    vm_id: usize,
    dedicated: bool,
    vcpu_affinities: Vec<Option<usize>>,
}

impl VmCpuPartitionInput {
    /// Creates a VM CPU-placement description.
    pub fn new(vm_id: usize, dedicated: bool, vcpu_affinities: Vec<Option<usize>>) -> Self {
        Self {
            vm_id,
            dedicated,
            vcpu_affinities,
        }
    }

    /// Returns the VM identifier.
    pub const fn vm_id(&self) -> usize {
        self.vm_id
    }
}

/// CPU affinity to apply when constructing one guest vCPU task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VcpuTaskAffinity {
    /// Retain the scheduler's default CPU affinity.
    SchedulerDefault,
    /// Restrict the task to the given physical-CPU mask.
    CpuMask(usize),
}

impl VcpuTaskAffinity {
    /// Returns the selected CPU when this affinity names exactly one enabled CPU.
    pub fn single_enabled_cpu(self, enabled_cpu_mask: usize) -> Option<usize> {
        let Self::CpuMask(cpu_mask) = self else {
            return None;
        };
        (cpu_mask != 0 && cpu_mask.count_ones() == 1 && cpu_mask & enabled_cpu_mask == cpu_mask)
            .then_some(cpu_mask.trailing_zeros() as usize)
    }
}

/// Validated effective CPU masks for guest vCPU tasks.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CpuPartitionPlan {
    reserved_cpu_mask: usize,
    effective_affinities: BTreeMap<(usize, usize), VcpuTaskAffinity>,
    initial_cpus: BTreeMap<(usize, usize), usize>,
}

impl CpuPartitionPlan {
    /// Creates an empty placement plan.
    pub const fn empty() -> Self {
        Self {
            reserved_cpu_mask: 0,
            effective_affinities: BTreeMap::new(),
            initial_cpus: BTreeMap::new(),
        }
    }

    /// Builds an effective guest-vCPU placement plan.
    pub fn build(
        available_cpu_mask: usize,
        placements: &[VmCpuPartitionInput],
    ) -> AxVmConfigResult<Self> {
        let mut plan = Self::empty();
        let mut dedicated_vm_masks = BTreeMap::<usize, usize>::new();
        let mut ordered = placements.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|placement| placement.vm_id);

        for placement in ordered
            .iter()
            .copied()
            .filter(|placement| placement.dedicated)
        {
            if placement.vcpu_affinities.is_empty() {
                return Err(AxVmConfigError::MissingDedicatedCpuAffinity {
                    vm_id: placement.vm_id,
                    vcpu_id: 0,
                });
            }
            let mut vm_reserved_cpu_mask = 0;
            for (vcpu_id, requested) in placement.vcpu_affinities.iter().copied().enumerate() {
                let mask = requested.ok_or(AxVmConfigError::MissingDedicatedCpuAffinity {
                    vm_id: placement.vm_id,
                    vcpu_id,
                })?;
                if mask == 0 {
                    return Err(AxVmConfigError::EmptyCpuAffinity {
                        vm_id: placement.vm_id,
                        vcpu_id,
                    });
                }
                if mask & available_cpu_mask != mask {
                    return Err(AxVmConfigError::CpuAffinityUnavailable {
                        vm_id: placement.vm_id,
                        vcpu_id,
                        requested: mask,
                        available: available_cpu_mask,
                    });
                }
                vm_reserved_cpu_mask |= mask;
            }
            for (conflicting_vm_id, conflicting_mask) in &dedicated_vm_masks {
                let overlap = vm_reserved_cpu_mask & conflicting_mask;
                if overlap != 0 {
                    return Err(AxVmConfigError::DedicatedCpuConflict {
                        vm_id: placement.vm_id,
                        conflicting_vm_id: *conflicting_vm_id,
                        overlap,
                    });
                }
            }
            dedicated_vm_masks.insert(placement.vm_id, vm_reserved_cpu_mask);
            plan.reserved_cpu_mask |= vm_reserved_cpu_mask;
        }

        for placement in ordered {
            for (vcpu_id, requested) in placement.vcpu_affinities.iter().copied().enumerate() {
                let effective = if placement.dedicated {
                    let mask = requested.ok_or(AxVmConfigError::MissingDedicatedCpuAffinity {
                        vm_id: placement.vm_id,
                        vcpu_id,
                    })?;
                    VcpuTaskAffinity::CpuMask(mask)
                } else if plan.reserved_cpu_mask == 0 {
                    requested.map_or(
                        VcpuTaskAffinity::SchedulerDefault,
                        VcpuTaskAffinity::CpuMask,
                    )
                } else {
                    let base = requested.unwrap_or(available_cpu_mask);
                    let available = base & available_cpu_mask;
                    if available == 0 {
                        return Err(AxVmConfigError::CpuAffinityUnavailable {
                            vm_id: placement.vm_id,
                            vcpu_id,
                            requested: base,
                            available: available_cpu_mask,
                        });
                    }
                    let effective = available & !plan.reserved_cpu_mask;
                    if effective == 0 {
                        return Err(AxVmConfigError::SharedCpuAffinityExhausted {
                            vm_id: placement.vm_id,
                            vcpu_id,
                            requested: available,
                            reserved: plan.reserved_cpu_mask,
                        });
                    }
                    VcpuTaskAffinity::CpuMask(effective)
                };
                plan.effective_affinities
                    .insert((placement.vm_id, vcpu_id), effective);
            }
        }
        plan.initial_cpus =
            build_initial_cpu_assignments(available_cpu_mask, &plan.effective_affinities);
        Ok(plan)
    }

    /// Returns the union of pCPUs reserved by dedicated VMs.
    pub const fn reserved_cpu_mask(&self) -> usize {
        self.reserved_cpu_mask
    }

    /// Returns the validated task affinity for one vCPU.
    pub fn task_affinity(&self, vm_id: usize, vcpu_id: usize) -> Option<VcpuTaskAffinity> {
        self.effective_affinities.get(&(vm_id, vcpu_id)).copied()
    }

    /// Returns the host CPU on which a new vCPU task should first be enqueued.
    ///
    /// Initial CPUs are distinct within a VM whenever its effective affinity
    /// masks admit a matching. The task retains its full affinity mask after
    /// launch, so later wakeup and migration policy remain scheduler-owned.
    pub fn task_initial_cpu(&self, vm_id: usize, vcpu_id: usize) -> Option<usize> {
        self.initial_cpus.get(&(vm_id, vcpu_id)).copied()
    }
}

fn build_initial_cpu_assignments(
    available_cpu_mask: usize,
    affinities: &BTreeMap<(usize, usize), VcpuTaskAffinity>,
) -> BTreeMap<(usize, usize), usize> {
    let mut vm_candidates = BTreeMap::<usize, Vec<(usize, usize)>>::new();
    for ((vm_id, vcpu_id), affinity) in affinities {
        vm_candidates.entry(*vm_id).or_default().push((
            *vcpu_id,
            initial_eligible_cpu_mask(*affinity, available_cpu_mask),
        ));
    }

    let mut initial_cpus = BTreeMap::new();
    for (vm_id, candidates) in vm_candidates {
        let eligible_masks = candidates
            .iter()
            .map(|(_, eligible_mask)| *eligible_mask)
            .collect::<Vec<_>>();
        let assignments = assign_initial_cpus(&eligible_masks);
        for ((vcpu_id, _), initial_cpu) in candidates.into_iter().zip(assignments) {
            if let Some(initial_cpu) = initial_cpu {
                initial_cpus.insert((vm_id, vcpu_id), initial_cpu);
            }
        }
    }
    initial_cpus
}

fn initial_eligible_cpu_mask(affinity: VcpuTaskAffinity, available_cpu_mask: usize) -> usize {
    match affinity {
        VcpuTaskAffinity::SchedulerDefault => available_cpu_mask,
        VcpuTaskAffinity::CpuMask(requested_mask) => {
            let enabled_requested_mask = requested_mask & available_cpu_mask;
            if enabled_requested_mask != 0 {
                enabled_requested_mask
            } else {
                lowest_cpu_mask(available_cpu_mask)
            }
        }
    }
}

fn lowest_cpu_mask(mask: usize) -> usize {
    if mask == 0 {
        0
    } else {
        1usize << mask.trailing_zeros()
    }
}

fn assign_initial_cpus(eligible_masks: &[usize]) -> Vec<Option<usize>> {
    let cpu_count = usize::BITS as usize;
    let mut cpu_owners = alloc::vec![None; cpu_count];
    let mut assignments = alloc::vec![None; eligible_masks.len()];
    let mut vcpu_order = (0..eligible_masks.len()).collect::<Vec<_>>();
    vcpu_order.sort_by_key(|vcpu_id| (eligible_masks[*vcpu_id].count_ones(), *vcpu_id));

    for vcpu_id in vcpu_order {
        let mut visited_cpus = alloc::vec![false; cpu_count];
        try_assign_unique_cpu(
            vcpu_id,
            eligible_masks,
            &mut cpu_owners,
            &mut assignments,
            &mut visited_cpus,
        );
    }

    assign_unmatched_vcpus(eligible_masks, &mut assignments, cpu_count);
    assignments
}

fn try_assign_unique_cpu(
    vcpu_id: usize,
    eligible_masks: &[usize],
    cpu_owners: &mut [Option<usize>],
    assignments: &mut [Option<usize>],
    visited_cpus: &mut [bool],
) -> bool {
    let eligible_mask = eligible_masks[vcpu_id];
    for (cpu_id, owner) in cpu_owners.iter_mut().enumerate() {
        if eligible_mask & (1usize << cpu_id) != 0 && owner.is_none() {
            *owner = Some(vcpu_id);
            assignments[vcpu_id] = Some(cpu_id);
            return true;
        }
    }

    let mut cpu_id = 0;
    while cpu_id < cpu_owners.len() {
        if eligible_mask & (1usize << cpu_id) == 0 || visited_cpus[cpu_id] {
            cpu_id += 1;
            continue;
        }
        visited_cpus[cpu_id] = true;
        let Some(owner) = cpu_owners[cpu_id] else {
            cpu_id += 1;
            continue;
        };
        if try_assign_unique_cpu(owner, eligible_masks, cpu_owners, assignments, visited_cpus) {
            cpu_owners[cpu_id] = Some(vcpu_id);
            assignments[vcpu_id] = Some(cpu_id);
            return true;
        }
        cpu_id += 1;
    }
    false
}

fn assign_unmatched_vcpus(
    eligible_masks: &[usize],
    assignments: &mut [Option<usize>],
    cpu_count: usize,
) {
    let mut cpu_loads = alloc::vec![0usize; cpu_count];
    for cpu_id in assignments.iter().flatten() {
        cpu_loads[*cpu_id] += 1;
    }

    for (vcpu_id, assignment) in assignments.iter_mut().enumerate() {
        if assignment.is_some() {
            continue;
        }
        let eligible_mask = eligible_masks[vcpu_id];
        let mut selected_cpu = None;
        for cpu_id in 0..cpu_count {
            if eligible_mask & (1usize << cpu_id) == 0 {
                continue;
            }
            if selected_cpu.is_none_or(|selected| {
                (cpu_loads[cpu_id], cpu_id) < (cpu_loads[selected], selected)
            }) {
                selected_cpu = Some(cpu_id);
            }
        }
        if let Some(cpu_id) = selected_cpu {
            *assignment = Some(cpu_id);
            cpu_loads[cpu_id] += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(vm_id: usize, dedicated: bool, masks: &[Option<usize>]) -> VmCpuPartitionInput {
        VmCpuPartitionInput::new(vm_id, dedicated, masks.to_vec())
    }

    #[test]
    fn full_overlap_is_rejected_instead_of_restoring_the_requested_mask() {
        let placements = [
            placement(1, true, &[Some(0b0001)]),
            placement(2, false, &[Some(0b0001)]),
        ];

        assert_eq!(
            CpuPartitionPlan::build(0b0011, &placements),
            Err(AxVmConfigError::SharedCpuAffinityExhausted {
                vm_id: 2,
                vcpu_id: 0,
                requested: 0b0001,
                reserved: 0b0001,
            })
        );
    }

    #[test]
    fn placement_is_independent_of_vm_registration_order() {
        let shared = placement(2, false, &[Some(0b0011)]);
        let dedicated = placement(1, true, &[Some(0b0001)]);

        let shared_first = CpuPartitionPlan::build(0b0011, &[shared.clone(), dedicated.clone()])
            .expect("shared-first placement should be valid");
        let dedicated_first = CpuPartitionPlan::build(0b0011, &[dedicated, shared])
            .expect("dedicated-first placement should be valid");

        assert_eq!(shared_first, dedicated_first);
        assert_eq!(
            shared_first.task_affinity(2, 0),
            Some(VcpuTaskAffinity::CpuMask(0b0010))
        );
    }

    #[test]
    fn dedicated_vm_masks_must_be_explicit_nonempty_and_available() {
        assert_eq!(
            CpuPartitionPlan::build(0b0011, &[placement(1, true, &[None])]),
            Err(AxVmConfigError::MissingDedicatedCpuAffinity {
                vm_id: 1,
                vcpu_id: 0,
            })
        );
        assert_eq!(
            CpuPartitionPlan::build(0b0011, &[placement(1, true, &[Some(0)])]),
            Err(AxVmConfigError::EmptyCpuAffinity {
                vm_id: 1,
                vcpu_id: 0,
            })
        );
        assert_eq!(
            CpuPartitionPlan::build(0b0011, &[placement(1, true, &[Some(0b0100)])]),
            Err(AxVmConfigError::CpuAffinityUnavailable {
                vm_id: 1,
                vcpu_id: 0,
                requested: 0b0100,
                available: 0b0011,
            })
        );
    }

    #[test]
    fn dedicated_vms_cannot_reserve_the_same_cpu() {
        let placements = [
            placement(2, true, &[Some(0b1111)]),
            placement(1, true, &[Some(0b0101)]),
        ];

        assert_eq!(
            CpuPartitionPlan::build(0b1111, &placements),
            Err(AxVmConfigError::DedicatedCpuConflict {
                vm_id: 2,
                conflicting_vm_id: 1,
                overlap: 0b0101,
            })
        );
    }

    #[test]
    fn shared_masks_are_pruned_against_the_complete_dedicated_set() {
        let placements = [
            placement(3, false, &[Some(0b1111), None]),
            placement(2, true, &[Some(0b0010)]),
            placement(1, true, &[Some(0b0001)]),
        ];

        let plan = CpuPartitionPlan::build(0b1111, &placements).unwrap();

        assert_eq!(plan.reserved_cpu_mask(), 0b0011);
        assert_eq!(
            plan.task_affinity(3, 0),
            Some(VcpuTaskAffinity::CpuMask(0b1100))
        );
        assert_eq!(
            plan.task_affinity(3, 1),
            Some(VcpuTaskAffinity::CpuMask(0b1100))
        );
    }

    #[test]
    fn broad_shared_vcpu_masks_receive_distinct_initial_cpus() {
        let plan = CpuPartitionPlan::build(
            0b1111,
            &[placement(1, false, &[Some(0b1111), Some(0b1111)])],
        )
        .unwrap();

        assert_eq!(plan.task_initial_cpu(1, 0), Some(0));
        assert_eq!(plan.task_initial_cpu(1, 1), Some(1));
    }

    #[test]
    fn asymmetric_masks_avoid_an_unnecessary_initial_cpu_collision() {
        let plan = CpuPartitionPlan::build(
            0b0011,
            &[placement(1, false, &[Some(0b0010), Some(0b0011)])],
        )
        .unwrap();

        assert_eq!(plan.task_initial_cpu(1, 0), Some(1));
        assert_eq!(plan.task_initial_cpu(1, 1), Some(0));
    }

    #[test]
    fn no_dedicated_vm_preserves_existing_affinity_semantics() {
        let placements = [placement(1, false, &[Some(0), None, Some(0b1000)])];

        let plan = CpuPartitionPlan::build(0b0011, &placements).unwrap();

        assert_eq!(plan.reserved_cpu_mask(), 0);
        assert_eq!(plan.task_affinity(1, 0), Some(VcpuTaskAffinity::CpuMask(0)));
        assert_eq!(
            plan.task_affinity(1, 1),
            Some(VcpuTaskAffinity::SchedulerDefault)
        );
        assert_eq!(
            plan.task_affinity(1, 2),
            Some(VcpuTaskAffinity::CpuMask(0b1000))
        );
        assert_eq!(plan.task_initial_cpu(1, 0), Some(0));
        assert_eq!(plan.task_initial_cpu(1, 2), Some(0));
    }

    #[test]
    fn singleton_affinity_requires_one_enabled_cpu() {
        assert_eq!(
            VcpuTaskAffinity::CpuMask(0b0100).single_enabled_cpu(0b1111),
            Some(2)
        );
        assert_eq!(
            VcpuTaskAffinity::SchedulerDefault.single_enabled_cpu(0b1111),
            None
        );
        assert_eq!(
            VcpuTaskAffinity::CpuMask(0).single_enabled_cpu(0b1111),
            None
        );
        assert_eq!(
            VcpuTaskAffinity::CpuMask(0b0110).single_enabled_cpu(0b1111),
            None
        );
        assert_eq!(
            VcpuTaskAffinity::CpuMask(0b1000).single_enabled_cpu(0b0111),
            None
        );
    }
}
