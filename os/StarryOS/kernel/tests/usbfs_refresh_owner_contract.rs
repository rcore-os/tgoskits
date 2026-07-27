//! Source-level ownership contract for USB topology refresh.

#[path = "../src/pseudofs/usbfs/refresh.rs"]
mod refresh;

use core::time::Duration;

use refresh::{HostRefreshCursor, HostRefreshState, RefreshRetryBackoff};

const MANAGER: &str = include_str!("../src/pseudofs/usbfs/manager.rs");
const IRQ: &str = include_str!("../src/pseudofs/usbfs/irq.rs");
const TREE: &str = include_str!("../src/pseudofs/usbfs/tree.rs");

fn function_source<'a>(source: &'a str, signature: &str, next_signature: &str) -> &'a str {
    source
        .split_once(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"))
        .1
        .split_once(next_signature)
        .unwrap_or_else(|| panic!("missing following function signature: {next_signature}"))
        .0
}

#[test]
fn inventory_reads_never_probe_usb_hosts() {
    let device_numbers = function_source(
        MANAGER,
        "pub(super) fn device_numbers",
        "pub(super) fn device_snapshot",
    );
    let device_snapshot = function_source(
        MANAGER,
        "pub(super) fn device_snapshot",
        "pub(super) fn acquire_device",
    );

    for (name, source) in [
        ("device_numbers", device_numbers),
        ("device_snapshot", device_snapshot),
        ("USBFS tree", TREE),
    ] {
        assert!(
            !source.contains("probe_devices")
                && !source.contains("refresh_dirty_hosts")
                && !source.contains("block_on"),
            "{name} must consume a published snapshot without synchronously probing hardware"
        );
    }
}

#[test]
fn runtime_reprobe_has_one_service_thread_owner() {
    assert!(
        MANAGER.contains("fn service_refresh_batch"),
        "USBFS must expose one task-context refresh service entry"
    );
    assert!(
        !MANAGER.contains("fn refresh_host"),
        "device-open and read paths must not create a second synchronous probe owner"
    );

    let refresh_service = function_source(
        MANAGER,
        "fn service_refresh_batch",
        "pub(super) fn device_numbers",
    );
    assert!(refresh_service.contains("probe_devices"));
    assert!(
        refresh_service.contains("try_lock()"),
        "the refresh owner must never busy-spin on an rdrive device guard"
    );
    assert!(
        !refresh_service.contains("host.lock()"),
        "a competing rdrive owner must defer refresh instead of pinning the only CPU"
    );
    assert_eq!(
        MANAGER.matches(".probe_devices()").count(),
        2,
        "only boot initialization and the refresh service may probe USB topology"
    );

    let open_device = function_source(MANAGER, "fn open_device", "fn snapshot_by_id");
    assert!(open_device.contains("try_lock()"));
    assert!(!open_device.contains("host.lock()"));
}

#[test]
fn initial_probe_consumes_the_bootstrap_request_once() {
    let initialize_hosts = function_source(
        MANAGER,
        "pub(super) fn initialize_hosts",
        "fn map_transfer_error",
    );
    let begin = initialize_hosts
        .find("manager.begin_initial_probe(device_id)")
        .expect("initial probe must claim the bootstrap refresh state");
    let probe = initialize_hosts
        .find(".probe_devices()")
        .expect("initial probe must enumerate the host");
    let finish = initialize_hosts
        .find("manager.finish_initial_probe(device_id)")
        .expect("a successful initial probe must clear its bootstrap request");

    assert!(begin < probe && probe < finish);
}

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
fn failed_runtime_probe_stays_queued_for_retry() {
    let refresh_service = function_source(
        MANAGER,
        "fn service_refresh_batch",
        "pub(super) fn device_numbers",
    );
    let probe_section = refresh_service
        .split_once("let probe_result")
        .expect("runtime refresh must execute the host probe")
        .1;
    let error_arm = probe_section
        .split_once("Err(err) =>")
        .expect("runtime refresh must handle probe errors explicitly")
        .1;
    let before_next_match = error_arm
        .split_once("self.finish_host_refresh(device_id)")
        .map_or(error_arm, |(before_finish, _)| before_finish);

    assert!(
        before_next_match.contains("self.defer_host_refresh(device_id)"),
        "a failed topology probe must return its host to the queued state"
    );
    assert!(
        before_next_match.contains("RefreshBatchOutcome::Retry"),
        "a failed topology probe must schedule a bounded retry"
    );
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
fn hard_irq_event_drain_is_allocation_free_and_bounded() {
    let hard_irq = function_source(
        IRQ,
        "fn usbfs_irq_handler_by_slot",
        "fn usbfs_event_handler",
    );
    let event_handler = function_source(IRQ, "fn usbfs_event_handler", "fn defer_event_drain");

    for forbidden in ["format!", "to_owned()", "Box::", "Vec::", "loop {"] {
        assert!(
            !hard_irq.contains(forbidden) && !event_handler.contains(forbidden),
            "hard IRQ event handling must not contain {forbidden}"
        );
    }
    assert!(
        !hard_irq.contains("iter_slots") && !hard_irq.contains("slot_by_irq"),
        "hard IRQ dispatch must use its prebound slot instead of scanning the registry"
    );
    assert!(IRQ.contains("USBFS_EVENT_BATCH_LIMIT"));
    assert!(
        event_handler.contains("for _ in 0..USBFS_EVENT_BATCH_LIMIT"),
        "each IRQ or deferred pass must have a fixed event budget"
    );
}

#[test]
fn exhausted_irq_batch_is_deferred_to_the_service_thread() {
    assert!(
        IRQ.contains("deferred: AtomicBool"),
        "each USB IRQ slot needs a preallocated deferred bit"
    );
    assert!(
        IRQ.contains("handler_busy: AtomicBool"),
        "IRQ and deferred task drains must have one atomic owner"
    );
    assert!(
        IRQ.contains("deferred_notify: IrqNotify"),
        "hard IRQ must target a preallocated direct worker notification"
    );
    assert!(
        IRQ.contains("fn usbfs_event_service_task"),
        "one fixed task-context worker must own deferred handler progress"
    );
    assert!(
        IRQ.contains("fn service_deferred_events"),
        "the existing USBFS service thread must continue exhausted batches"
    );
    assert!(
        IRQ.contains("registry.deferred_notify.notify_irq()"),
        "hard IRQ exhaustion must directly wake the fixed event worker"
    );
    assert!(
        IRQ.contains("deferred.store(true, Ordering::Release)"),
        "an exhausted or contended handler must publish deferred work"
    );
    assert!(
        IRQ.contains("deferred.swap(false, Ordering::AcqRel)"),
        "task context must atomically claim deferred work"
    );
}

#[test]
fn deferred_event_service_has_one_global_budget_and_rotates_hosts() {
    let service = function_source(
        IRQ,
        "fn service_deferred_events",
        "fn usbfs_event_service_task",
    );

    assert!(
        IRQ.contains("service_cursor: AtomicUsize"),
        "the event worker must rotate its first host between service passes"
    );
    assert!(
        service.contains("service_cursor") && service.contains("break;"),
        "one service pass must spend its fixed handler budget on at most one rotating host"
    );
    assert_eq!(
        service.matches("usbfs_event_handler(slot)").count(),
        1,
        "one task-context pass must not multiply the event budget by the host count"
    );
}

#[test]
fn failed_initialization_disables_irq_and_polling_slots() {
    let initialize_hosts = function_source(
        MANAGER,
        "pub(super) fn initialize_hosts",
        "fn map_transfer_error",
    );
    let cleanup = initialize_hosts
        .split_once("for (device_id, _) in failed_device_ids")
        .expect("failed hosts must have one explicit endpoint cleanup pass")
        .1;

    assert!(
        cleanup.contains("irq::disable_device(device_id)")
            && cleanup.contains("irq::free_device_irq(device_id)"),
        "failed hosts must disable both IRQ-backed and polling event slots"
    );
    assert!(
        !cleanup.contains("if host_irq.is_some()"),
        "polling hosts need the same endpoint teardown as IRQ-backed hosts"
    );
}

#[test]
fn permanently_released_runtime_host_is_disabled() {
    let refresh_service = function_source(
        MANAGER,
        "fn service_refresh_batch",
        "pub(super) fn device_numbers",
    );
    let lock_failure = refresh_service
        .split_once("failed to lock USB host")
        .expect("the refresh service must classify host-lock failures")
        .1;

    assert!(
        lock_failure.contains("irq::disable_device(device_id)")
            && lock_failure.contains("self.disable_missing_host(device_id)")
            && lock_failure.contains("irq::free_device_irq(device_id)"),
        "a permanent rdrive lock failure must transition the host to Disabled"
    );
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
