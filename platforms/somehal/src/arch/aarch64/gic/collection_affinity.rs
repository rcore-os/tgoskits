#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CollectionAffinityDecision {
    Keep,
    InvalidCpu,
    Unsupported,
}

pub(crate) const fn decide_collection_affinity(
    requested_cpu: Option<usize>,
    collection_cpu: usize,
    cpu_count: usize,
) -> CollectionAffinityDecision {
    match requested_cpu {
        None => CollectionAffinityDecision::Keep,
        Some(cpu) if cpu >= cpu_count => CollectionAffinityDecision::InvalidCpu,
        Some(cpu) if cpu == collection_cpu => CollectionAffinityDecision::Keep,
        Some(_) => CollectionAffinityDecision::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::{CollectionAffinityDecision, decide_collection_affinity};

    #[test]
    fn fixed_current_collection_is_idempotent() {
        assert_eq!(
            decide_collection_affinity(Some(0), 0, 4),
            CollectionAffinityDecision::Keep
        );
    }

    #[test]
    fn any_keeps_the_existing_collection() {
        assert_eq!(
            decide_collection_affinity(None, 0, 4),
            CollectionAffinityDecision::Keep
        );
    }

    #[test]
    fn another_valid_cpu_requires_collection_migration() {
        assert_eq!(
            decide_collection_affinity(Some(1), 0, 4),
            CollectionAffinityDecision::Unsupported
        );
    }

    #[test]
    fn out_of_range_cpu_is_rejected() {
        assert_eq!(
            decide_collection_affinity(Some(4), 0, 4),
            CollectionAffinityDecision::InvalidCpu
        );
    }
}
