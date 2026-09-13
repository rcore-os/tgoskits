//! Initial placement over an already affinity-filtered active CPU set.

/// Compares instantaneous nice-weighted demand per unit of boot capacity.
///
/// Linux's fork slow path compares capacity-normalized group load. This flat
/// topology has no sched domains, task-fit utilization or energy model; the
/// capacity tie-break is a throughput policy, not Linux EAS. The caller includes
/// current work and reserved incoming deliveries in demand. These are advisory
/// snapshots; the existing admission transaction owns lifecycle validation.
///
/// Each candidate is `(logical_cpu, demand, capacity)`. Capacities must share
/// one scale. Positive capacity always outranks zero; zero-capacity CPUs remain
/// eligible when affinity leaves no positive-capacity alternative. Equal ratios
/// prefer greater capacity, then `preferred`, then the lowest logical index.
/// Empty input returns `None`. No allocation or mutable scheduler access occurs.
pub fn select_initial_cpu(
    candidates: impl Iterator<Item = (usize, u64, u16)>,
    preferred: Option<usize>,
) -> Option<usize> {
    candidates
        .min_by(
            |&(cpu_a, demand_a, capacity_a), &(cpu_b, demand_b, capacity_b)| {
                (capacity_a == 0)
                    .cmp(&(capacity_b == 0))
                    // Cross multiplication preserves the ratio without truncation.
                    // u64 demand times u16 capacity fits in u128 even at saturation.
                    .then_with(|| {
                        (u128::from(demand_a) * u128::from(capacity_b.max(1)))
                            .cmp(&(u128::from(demand_b) * u128::from(capacity_a.max(1))))
                    })
                    .then_with(|| capacity_b.cmp(&capacity_a))
                    .then_with(|| (Some(cpu_a) != preferred).cmp(&(Some(cpu_b) != preferred)))
                    .then_with(|| cpu_a.cmp(&cpu_b))
            },
        )
        .map(|(cpu, ..)| cpu)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(candidates: &[(u32, u64, u16)], preferred: u32) -> Option<u32> {
        select_initial_cpu(
            candidates
                .iter()
                .map(|&(cpu, demand, capacity)| (cpu as usize, demand, capacity)),
            Some(preferred as usize),
        )
        .map(|cpu| cpu as u32)
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
