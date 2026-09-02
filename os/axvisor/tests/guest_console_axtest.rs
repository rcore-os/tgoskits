#![no_std]
#![no_main]

extern crate alloc;
extern crate std;

use ax_hal as _;
use ax_std as _;
use axvm as _;

mod host {
    pub(crate) fn submit_host_bytes(_bytes: &[u8]) {}

    pub(crate) fn submit_host_transaction(transaction: impl FnOnce(&mut dyn FnMut(&[u8]))) {
        transaction(&mut |_| {});
    }
}

mod manager {
    use alloc::vec::Vec;
    use anyhow::Result;
    use axvm::{AxVMRef, VMId};

    pub(crate) struct AxvmManager;

    impl AxvmManager {
        pub(crate) fn notify_vm(_vm_id: VMId) -> Result<()> {
            Ok(())
        }

        pub(crate) fn vm_by_id(_vm_id: VMId) -> Option<AxVMRef> {
            None
        }

        pub(crate) fn vm_list() -> Vec<AxVMRef> {
            Vec::new()
        }
    }
}

#[path = "../src/guest_console/mux/mod.rs"]
mod mux;

#[axtest::tests]
mod tests {}
