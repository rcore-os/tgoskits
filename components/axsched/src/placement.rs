//! Initial placement over an already affinity-filtered active CPU set.


/// Compares instantaneous nice-weighted demand per unit of boot capacity.
///
/// Linux's fork slow path compares capacity-normalized group load. This flat
/// topology has no sched domains, task-fit utilization or energy model; the
/// capacity tie-break is a throughput policy, not Linux EAS. The caller includes
/// current work and reserved incoming deliveries in demand. These are advisory
/// snapshots; the existing admission transaction owns lifecycle validation.
pub fn select_initial_cpu(
    candidates: impl Iterator<Item = (usize, u64, u16)>,
    preferred: Option<usize>,
) -> Option<usize> {
    candidates
        .min_by_key(|(cpu, demand, _)| (*demand, Some(*cpu) != preferred, *cpu))
        .map(|(cpu, _, _)| cpu)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(candidates: &[(u32, u64, u16)], preferred: u32) -> Option<u32> {
        select_initial_cpu(
            candidates.iter().map(|&(cpu, demand, capacity)| (cpu as usize, demand, capacity)),
            Some(preferred as usize),
        ).map(|cpu| cpu as u32)
    }

    #[test]
    fn initial_placement_balances_capacity_weighted_demand() {
        assert_eq!(pick(&[(0, 0, 512), (1, 0, 1024)], 0), Some(1));
        assert_eq!(pick(&[(0, 1024, 512), (1, 1536, 1024)], 0), Some(1));
        assert_eq!(pick(&[(0, 0, 512), (1, 1024, 1024)], 1), Some(0));
        // Exact comparison must not invent ties by integer division.
        assert_eq!(pick(&[(0, 1, 1023), (1, 1, 1024)], 0), Some(1));
        assert_eq!(pick(&[(0, u64::MAX, 512), (1, u64::MAX, 1024)], 0), Some(1));
    }

    #[test]
    fn initial_placement_preserves_eligibility_and_homogeneous_locality() {
        assert_eq!(pick(&[], 0), None);
        assert_eq!(pick(&[(70, 4096, 512)], 0), Some(70));
        assert_eq!(pick(&[(0, 1024, 1024), (1, 1024, 1024)], 1), Some(1));
        assert_eq!(pick(&[(1, 1024, 1024), (0, 1024, 1024)], 7), Some(0));
        // Firmware normalization can round a CPU down to zero. Keep it usable
        // for pinned tasks, but rank positive-capacity CPUs ahead of it.
        assert_eq!(pick(&[(0, 0, 0), (1, 1024, 1024)], 0), Some(1));
        assert_eq!(pick(&[(70, 1024, 0)], 0), Some(70));
    }
}
