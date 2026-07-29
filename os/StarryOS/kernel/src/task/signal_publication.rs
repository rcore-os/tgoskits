//! Ordering boundary between fatal-signal publication and stop-state wakeup.

/// Runs a signal publication and the wakeup that exposes it to the target.
///
/// Both operations run synchronously in task context. Keeping the ordering in
/// one helper mirrors Linux's `siglock` invariant while Starry stores pending
/// signals and ptrace stop records in separate owners.
pub(crate) fn publish_before_release<T>(
    publish: impl FnOnce() -> T,
    release_stop: impl FnOnce(),
) -> T {
    let publication = publish();
    release_stop();
    publication
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::publish_before_release;

    #[test]
    fn fatal_signal_publication_precedes_stop_release() {
        let phase = Cell::new(0_u8);
        let publication = publish_before_release(
            || {
                assert_eq!(
                    phase.get(),
                    0,
                    "fatal signal must be published before the ptrace stop is released"
                );
                phase.set(1);
                7_u8
            },
            || {
                assert_eq!(
                    phase.get(),
                    1,
                    "ptrace stop release must observe the fatal signal publication"
                );
                phase.set(2);
            },
        );

        assert_eq!(publication, 7);
        assert_eq!(phase.get(), 2);
    }
}
