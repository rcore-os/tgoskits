//! M0 smoke tests: confirm the host test harness and mock provider work.
//!
//! Real per-controller round-trip coverage lives in M1 (this file grows then).

use super::mock::{ensure_init, test_guard};
use crate::{CgroupProvider, ensure_node_exists, root_id};

#[test]
fn harness_boots_with_mock_provider() {
    let _g = test_guard();
    // First call performs `crate::init()` + `register_provider`; later calls
    // are no-ops. The root cgroup must exist afterwards.
    let _mock = ensure_init();
    assert!(ensure_node_exists(root_id()).is_ok());
}

#[test]
fn mock_provider_tracks_zombie_and_reset() {
    let _g = test_guard();
    let mock = ensure_init();
    mock.reset();

    // A pid is not a zombie until marked, and migration helpers rely on this.
    assert!(!mock.is_zombie(4242));
    mock.set_zombie(4242, true);
    assert!(mock.is_zombie(4242));

    // reset() clears per-test state so tests stay independent.
    mock.reset();
    assert!(!mock.is_zombie(4242));
}
