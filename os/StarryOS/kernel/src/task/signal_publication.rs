//! Ordering boundary between fatal-signal publication and stop-state wakeup.

/// Publishes a fatal signal before releasing stop states that hide it.
///
/// Both operations run synchronously in task context. Keeping the ordering in
/// one helper mirrors Linux's `siglock` invariant while Starry stores pending
/// signals and ptrace stop records in separate owners.
pub(crate) fn publish_before_fatal_stop_release<T>(
    publish: impl FnOnce() -> T,
    release_ptrace_stop: Option<impl FnOnce()>,
    release_job_stop: impl FnOnce(),
) -> T {
    let publication = publish();
    if let Some(release_ptrace_stop) = release_ptrace_stop {
        release_ptrace_stop();
    }
    release_job_stop();
    publication
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::publish_before_fatal_stop_release;

    #[test]
    fn fatal_signal_publication_precedes_all_stop_releases() {
        let phase = Cell::new(0_u8);
        let publication = publish_before_fatal_stop_release(
            || {
                assert_eq!(
                    phase.get(),
                    0,
                    "fatal signal must be published before the ptrace stop is released"
                );
                phase.set(1);
                7_u8
            },
            Some(|| {
                assert_eq!(
                    phase.get(),
                    1,
                    "ptrace stop release must observe the fatal signal publication"
                );
                phase.set(2);
            }),
            || {
                assert_eq!(
                    phase.get(),
                    2,
                    "job stop release must follow ptrace stop release"
                );
                phase.set(3);
            },
        );

        assert_eq!(publication, 7);
        assert_eq!(phase.get(), 3);
    }
}
