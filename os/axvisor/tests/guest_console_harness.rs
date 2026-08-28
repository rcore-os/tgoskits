//! Axtest adapters for the production guest-console mux.

#![allow(
    dead_code,
    reason = "the harness compiles the complete production module but tests its private state machine"
)]

pub(crate) mod host {
    pub(crate) fn submit_host_bytes(_bytes: &[u8]) {}

    pub(crate) fn submit_host_transaction(transaction: impl FnOnce(&mut dyn FnMut(&[u8]))) {
        transaction(&mut |_bytes| {});
    }
}

#[path = "../src/guest_console/mux/mod.rs"]
pub(crate) mod mux;
