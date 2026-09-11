//! USB topology refresh ownership and retry behavior.

#[path = "../src/pseudofs/usbfs/refresh.rs"]
mod refresh;

use core::time::Duration;

use refresh::{HostRefreshCursor, HostRefreshState, RefreshRetryBackoff};

#[test]
fn topology_events_are_coalesced_around_the_single_probe_owner() {
    let mut state = HostRefreshState::Idle;

    state.mark_dirty();
    state.mark_dirty();
    assert_eq!(state, HostRefreshState::Queued);
    assert!(state.begin_probe());
    assert_eq!(state, HostRefreshState::Probing);

    state.mark_dirty();
    assert_eq!(state, HostRefreshState::DirtyAgain);
    assert!(state.finish_probe());
    assert_eq!(state, HostRefreshState::Queued);

    assert!(state.begin_probe());
    assert!(!state.finish_probe());
    assert_eq!(state, HostRefreshState::Idle);
}

#[test]
fn busy_device_defers_without_dropping_the_queued_refresh() {
    let mut state = HostRefreshState::Queued;

    assert!(state.begin_probe());
    state.mark_dirty();
    state.defer_probe();

    assert_eq!(state, HostRefreshState::Queued);
    assert!(state.is_queued());
}

#[test]
fn busy_first_host_does_not_starve_the_next_host() {
    let mut cursor = HostRefreshCursor::default();
    let mut states = [HostRefreshState::Queued, HostRefreshState::Queued];

    let first = cursor
        .claim_next(states.len(), |index| states[index].begin_probe())
        .expect("the first host must initially be selected");
    assert_eq!(first, 0);
    states[first].defer_probe();

    let second = cursor
        .claim_next(states.len(), |index| states[index].begin_probe())
        .expect("a busy first host must not starve the second host");
    assert_eq!(second, 1);
}

#[test]
fn dirty_again_first_host_does_not_starve_the_next_host() {
    let mut cursor = HostRefreshCursor::default();
    let mut states = [HostRefreshState::Queued, HostRefreshState::Queued];

    let first = cursor
        .claim_next(states.len(), |index| states[index].begin_probe())
        .expect("the first host must initially be selected");
    states[first].mark_dirty();
    assert!(states[first].finish_probe());

    let second = cursor
        .claim_next(states.len(), |index| states[index].begin_probe())
        .expect("a continuously dirty first host must not starve the second host");
    assert_eq!(second, 1);
}

#[test]
fn refresh_retry_backoff_is_bounded_and_resettable() {
    let mut backoff = RefreshRetryBackoff::default();
    let mut previous = Duration::ZERO;

    for _ in 0..32 {
        let delay = backoff.next_delay();
        assert!(delay >= previous);
        assert!(delay <= RefreshRetryBackoff::MAX_DELAY);
        previous = delay;
    }
    assert_eq!(previous, RefreshRetryBackoff::MAX_DELAY);

    backoff.reset();
    assert_eq!(backoff.next_delay(), RefreshRetryBackoff::MIN_DELAY);
}

#[test]
fn successful_initial_probe_clears_the_bootstrap_refresh() {
    let mut state = HostRefreshState::Queued;

    assert!(state.begin_probe());
    assert!(!state.finish_initial_probe());

    assert_eq!(state, HostRefreshState::Idle);
    assert!(!state.is_queued());
}

#[test]
fn initial_probe_preserves_a_concurrent_topology_event() {
    let mut state = HostRefreshState::Queued;

    assert!(state.begin_probe());
    state.mark_dirty();
    assert!(state.finish_initial_probe());

    assert_eq!(state, HostRefreshState::Queued);
}

#[test]
fn disabled_host_cannot_be_requeued_by_a_late_irq() {
    let mut state = HostRefreshState::Queued;

    state.disable();
    state.mark_dirty();

    assert!(!state.is_enabled());
    assert!(!state.is_queued());
    assert!(!state.begin_probe());
}
