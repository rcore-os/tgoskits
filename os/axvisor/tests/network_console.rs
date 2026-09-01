//! Network-output capture used by the guest-console axtest harness.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};
use ax_std::sync::Mutex;
use axvm::VMId;
use std::sync::LazyLock;

static GUEST_OUTPUT: LazyLock<Mutex<BTreeMap<VMId, Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
static CONNECTED_GUESTS: LazyLock<Mutex<BTreeSet<VMId>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

pub(crate) fn guest_output_connected(vm_id: VMId) -> bool {
    CONNECTED_GUESTS.lock().contains(&vm_id)
}

pub(crate) fn submit_guest_output(vm_id: VMId, bytes: &[u8]) {
    if !CONNECTED_GUESTS.lock().contains(&vm_id) {
        return;
    }
    GUEST_OUTPUT
        .lock()
        .entry(vm_id)
        .or_default()
        .extend_from_slice(bytes);
}

pub(crate) fn reset() {
    GUEST_OUTPUT.lock().clear();
    CONNECTED_GUESTS.lock().clear();
}

pub(crate) fn set_guest_connected(vm_id: VMId) {
    CONNECTED_GUESTS.lock().insert(vm_id);
}

pub(crate) fn take_guest_output(vm_id: VMId) -> Vec<u8> {
    GUEST_OUTPUT.lock().remove(&vm_id).unwrap_or_default()
}
