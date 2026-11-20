use std::collections::BTreeMap;

use anyhow::ensure;

use super::check::ClippyCheck;

/// Keep related compiler inputs together without leaving one large package
/// permanently pinned to a single CI job.
pub(super) fn partition(checks: &[ClippyCheck], count: usize) -> anyhow::Result<Vec<Vec<usize>>> {
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
    blocks.sort_by_key(|block| (std::cmp::Reverse(block.len()), block[0]));
    let mut shards = vec![Vec::new(); count.min(blocks.len())];
    for block in blocks {
        let selected = (0..shards.len())
            .min_by_key(|&i| (shards[i].len(), i))
            .unwrap();
        shards[selected].extend(block);
    }
    for shard in &mut shards {
        shard.sort_unstable();
    }
    Ok(shards)
}
