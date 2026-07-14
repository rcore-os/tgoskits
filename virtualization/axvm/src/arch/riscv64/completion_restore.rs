// Failure-safe restoration helpers for bounded forwarded-IRQ batches.

pub(super) fn restore_all<Items, Restore>(items: Items, mut restore: Restore) -> bool
where
    Items: IntoIterator,
    Restore: FnMut(Items::Item) -> bool,
{
    let mut restored = true;
    for item in items {
        restored &= restore(item);
    }
    restored
}

pub(super) fn restore_present_suffix<T: Copy>(
    claims: &[Option<T>],
    first_uncompleted: usize,
    restore: impl FnMut(T) -> bool,
) -> bool {
    let Some(suffix) = claims.get(first_uncompleted..) else {
        return false;
    };
    restore_all(suffix.iter().flatten().copied(), restore)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;

    #[test]
    fn failed_restore_does_not_skip_later_decode_failure_entries() {
        let mut attempted = Vec::new();

        let restored = restore_all([20usize, 21, 22], |source| {
            attempted.push(source);
            source != 21
        });

        assert!(!restored);
        assert_eq!(attempted, [20, 21, 22]);
    }

    #[test]
    fn unmask_failure_restores_only_the_uncompleted_suffix_once() {
        let claims = [Some(10usize), Some(11), Some(12), Some(13)];
        let mut attempted = Vec::new();

        let restored = restore_present_suffix(&claims, 2, |source| {
            attempted.push(source);
            source != 12
        });

        assert!(!restored);
        assert_eq!(attempted, [12, 13]);
    }
}
