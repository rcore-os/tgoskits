//! Network-output capture used by the guest-console axtest harness.

use alloc::{collections::BTreeMap, vec::Vec};
use ax_std::sync::Mutex;
use axvm::VMId;
use std::sync::LazyLock;

static GUEST_OUTPUT: LazyLock<Mutex<BTreeMap<VMId, Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
pub(crate) fn submit_guest_output(vm_id: VMId, bytes: &[u8]) {
    GUEST_OUTPUT
        .lock()
        .entry(vm_id)
        .or_default()
        .extend_from_slice(bytes);
}

pub(crate) fn reset() {
    GUEST_OUTPUT.lock().clear();
}

pub(crate) fn take_guest_output(vm_id: VMId) -> Vec<u8> {
    GUEST_OUTPUT.lock().remove(&vm_id).unwrap_or_default()
}
