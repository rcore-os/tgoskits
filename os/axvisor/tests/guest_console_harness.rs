//! Axtest adapters for the production guest-console mux.

#![allow(
    dead_code,
    reason = "the harness compiles the complete production module but tests its private state machine"
)]

pub(crate) mod host {
    use core::sync::atomic::{AtomicBool, Ordering};

    use ax_std::os::arceos::modules::ax_runtime::{RuntimeError, RuntimeResult};

    static OUTPUT_BLOCKED: AtomicBool = AtomicBool::new(false);

    pub(crate) fn set_output_blocked(blocked: bool) {
        OUTPUT_BLOCKED.store(blocked, Ordering::Release);
    }

    pub(crate) fn queue_guest_output(_tag: u128, _bytes: &[u8]) -> RuntimeResult<bool> {
        if OUTPUT_BLOCKED.load(Ordering::Acquire) {
            return Err(RuntimeError::WouldBlock);
        }
        Ok(false)
    }

    pub(crate) fn submit_host_bytes(_bytes: &[u8]) {}

    pub(crate) fn submit_host_transaction(transaction: impl FnOnce(&mut dyn FnMut(&[u8]))) {
        transaction(&mut |_bytes| {});
    }
}

#[path = "../src/guest_console/mux/mod.rs"]
pub(crate) mod mux;
