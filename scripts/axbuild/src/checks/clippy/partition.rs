use std::collections::BTreeMap;

use anyhow::ensure;

use super::{check::ClippyCheck, weights::TimingWeights};

/// Keep related compiler inputs together without leaving one large package
/// permanently pinned to a single CI job.
pub(super) fn partition(
    checks: &[ClippyCheck],
    count: usize,
    weights: Option<&TimingWeights>,
) -> anyhow::Result<Vec<Vec<usize>>> {
    ensure!(count > 0 && count <= 256, "invalid Clippy shard count");
    ensure!(!checks.is_empty(), "cannot partition an empty Clippy plan");
    let mut domains = BTreeMap::new();
    for (index, check) in checks.iter().enumerate() {
        domains
            .entry((&check.package, &check.target, &check.env))
            .or_insert_with(Vec::new)
            .push(index);
    }
    let mut blocks = domains
        .into_values()
        .flat_map(|indices| {
            indices
                .chunks(32)
                .map(<[usize]>::to_vec)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let costs = weights
        .map(|weights| weights.costs(checks))
        .transpose()?
        .unwrap_or_else(|| vec![1.0; checks.len()]);
    let block_cost = |block: &[usize]| block.iter().map(|&id| costs[id]).sum::<f64>();
    blocks.sort_by(|left, right| {
        block_cost(right)
            .total_cmp(&block_cost(left))
            .then(left[0].cmp(&right[0]))
    });
    let mut shards = vec![Vec::new(); count.min(blocks.len())];
    let mut loads = vec![0.0_f64; shards.len()];
    for block in blocks {
        let selected = (0..shards.len())
            .min_by(|&left, &right| {
                loads[left]
                    .total_cmp(&loads[right])
                    .then(shards[left].len().cmp(&shards[right].len()))
                    .then(left.cmp(&right))
            })
            .expect("nonempty Clippy blocks always create a shard");
        loads[selected] += block_cost(&block);
        shards[selected].extend(block);
    }
    for shard in &mut shards {
        shard.sort_unstable();
    }
    Ok(shards)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::clippy::check::ClippyCheckKind;

    #[test]
    fn shards_preserve_the_complete_ordered_check_set_and_split_heavy_domains() {
        let checks = (0..100)
            .map(|i| ClippyCheck {
                package: if i < 90 { "heavy" } else { "light" }.into(),
                kind: ClippyCheckKind::Feature(format!("capability-{i}")),
                target: Some("fixture-target".into()),
                env: Vec::new(),
            })
            .collect::<Vec<_>>();
        for count in [1, 2, 4, 200] {
            let shards = partition(&checks, count, None).unwrap();
            assert!(shards.iter().all(|shard| !shard.is_empty()));
            assert!(
                shards
                    .iter()
                    .all(|shard| shard.windows(2).all(|p| p[0] < p[1]))
            );
            let mut union = shards.into_iter().flatten().collect::<Vec<_>>();
            union.sort_unstable();
            assert_eq!(union, (0..checks.len()).collect::<Vec<_>>());
        }
        assert_eq!(
            partition(&checks, 4, None).unwrap(),
            partition(&checks, 4, None).unwrap()
        );
        assert!(partition(&[], 4, None).is_err());
        assert!(partition(&checks, 0, None).is_err());
    }
    #[test]
    fn measured_costs_balance_work_without_excluding_new_or_changed_checks() {
        let checks = (0..6)
            .map(|i| ClippyCheck {
                package: format!("fixture-{i}"),
                kind: ClippyCheckKind::Base,
                target: None,
                env: Vec::new(),
            })
            .collect::<Vec<_>>();
        let known = checks[..5]
            .iter()
            .zip([20.0, 18.0, 1.0, 1.0, 1.0])
            .map(|(check, cost)| (TimingWeights::identity(check).unwrap(), cost))
            .collect::<std::collections::BTreeMap<_, _>>();
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut document = serde_json::json!({"schema_version":1,"rustc":"fixture-compiler","source":"synthetic costs","checks":known});
        std::fs::write(file.path(), serde_json::to_vec(&document).unwrap()).unwrap();
        let weights = TimingWeights::load(file.path(), "fixture-compiler")
            .unwrap()
            .unwrap();
        let weighted = partition(&checks, 2, Some(&weights)).unwrap();
        let unweighted = partition(&checks, 2, None).unwrap();
        let costs = weights.costs(&checks).unwrap();
        let makespan = |shards: &[Vec<usize>]| {
            shards
                .iter()
                .map(|shard| shard.iter().map(|&id| costs[id]).sum::<f64>())
                .fold(0.0_f64, f64::max)
        };
        assert!(makespan(&weighted) < makespan(&unweighted));
        assert_eq!(weighted, partition(&checks, 2, Some(&weights)).unwrap());
        let mut members = weighted.into_iter().flatten().collect::<Vec<_>>();
        members.sort_unstable();
        assert_eq!(members, (0..checks.len()).collect::<Vec<_>>());
        let mut changed = checks.clone();
        changed[0].kind = ClippyCheckKind::Feature("new-capability".into());
        assert_ne!(
            TimingWeights::identity(&checks[0]).unwrap(),
            TimingWeights::identity(&changed[0]).unwrap()
        );
        assert_eq!(weights.costs(&changed).unwrap().len(), checks.len());
        assert!(
            TimingWeights::load(file.path(), "updated-compiler")
                .unwrap()
                .is_none()
        );
        document["checks"] = serde_json::json!({"invalid":"duration"});
        std::fs::write(file.path(), serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(TimingWeights::load(file.path(), "fixture-compiler").is_err());
    }
}
