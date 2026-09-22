//! Axtest adapters for the production guest-console mux.

#![allow(
    dead_code,
    reason = "the harness compiles the complete production module but tests its private state machine"
)]

pub(crate) mod host {
    use alloc::{collections::VecDeque, vec::Vec};
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::sync::LazyLock;

    use ax_std::{
        os::arceos::modules::ax_runtime::{RuntimeError, RuntimeResult},
        sync::Mutex,
    };

    /// One record retained by the modeled ordered subscription.
    ///
    /// The production queue carries guest output tagged `(VMId, generation)`
    /// together with complete untagged host log records, so the stub keeps both
    /// in one FIFO and lets a test pop either variant.
    enum OrderedRecord {
        /// Guest output tagged `(VMId, generation)`.
        Guest(u128),
        /// Complete host log record bytes.
        HostLog(Vec<u8>),
    }

    /// Records the modeled ordered queue retains before it reports backpressure.
    ///
    /// A single slot is enough to expose the production branch: once a record
    /// occupies it, the next submission is rejected with `WouldBlock` until the
    /// shell pops the retained record.
    const ORDERED_RECORD_CAPACITY: usize = 1;

    /// Forces every guest-output submission to report backpressure.
    static OUTPUT_BLOCKED: AtomicBool = AtomicBool::new(false);
    /// Selects the ordered-subscription branch of [`queue_guest_output`].
    static ORDERED_OUTPUT_AVAILABLE: AtomicBool = AtomicBool::new(false);
    /// Retained ordered records in publication order.
    static ORDERED_RECORDS: LazyLock<Mutex<VecDeque<OrderedRecord>>> =
        LazyLock::new(|| Mutex::new(VecDeque::new()));
    static HOST_BYTES: LazyLock<Mutex<Vec<u8>>> = LazyLock::new(|| Mutex::new(Vec::new()));

    pub(crate) fn set_output_blocked(blocked: bool) {
        OUTPUT_BLOCKED.store(blocked, Ordering::Release);
    }

    /// Enables the ordered record queue, matching the production mux branch that
    /// hands guest output to the host log subscription instead of replaying it
    /// directly onto the host transport.
    pub(crate) fn set_ordered_output_available(available: bool) {
        ORDERED_OUTPUT_AVAILABLE.store(available, Ordering::Release);
        ORDERED_RECORDS.lock().clear();
    }

    /// Enqueues one untagged host log record, mirroring a complete host record
    /// the runtime published into the ordered queue.
    ///
    /// Returns `false` when there is no ordered subscription or the single slot
    /// is already occupied.
    pub(crate) fn queue_host_log_record(record: &[u8]) -> bool {
        if !ORDERED_OUTPUT_AVAILABLE.load(Ordering::Acquire) {
            return false;
        }
        let mut records = ORDERED_RECORDS.lock();
        if records.len() >= ORDERED_RECORD_CAPACITY {
            return false;
        }
        records.push_back(OrderedRecord::HostLog(record.to_vec()));
        true
    }

    /// Consumes the oldest guest record, releasing one queue slot.
    pub(crate) fn pop_ordered_record() -> Option<u128> {
        match ORDERED_RECORDS.lock().pop_front() {
            Some(OrderedRecord::Guest(tag)) => Some(tag),
            Some(OrderedRecord::HostLog(_)) => {
                panic!("the front ordered record is a host log, not guest output")
            }
            None => None,
        }
    }

    /// Consumes the oldest host log record, releasing one queue slot.
    pub(crate) fn pop_ordered_host_record() -> Option<Vec<u8>> {
        match ORDERED_RECORDS.lock().pop_front() {
            Some(OrderedRecord::HostLog(record)) => Some(record),
            Some(OrderedRecord::Guest(_)) => {
                panic!("the front ordered record is guest output, not a host log")
            }
            None => None,
        }
    }

    /// Clears every modeled host-output state between axtest cases.
    pub(crate) fn reset_output() {
        set_output_blocked(false);
        set_ordered_output_available(false);
        HOST_BYTES.lock().clear();
    }

    pub(crate) fn take_host_bytes() -> Vec<u8> {
        core::mem::take(&mut HOST_BYTES.lock())
    }

    pub(crate) fn queue_guest_output(tag: u128, _bytes: &[u8]) -> RuntimeResult<bool> {
        if OUTPUT_BLOCKED.load(Ordering::Acquire) {
            return Err(RuntimeError::WouldBlock);
        }
        if !ORDERED_OUTPUT_AVAILABLE.load(Ordering::Acquire) {
            return Ok(false);
        }
        let mut records = ORDERED_RECORDS.lock();
        if records.len() >= ORDERED_RECORD_CAPACITY {
            return Err(RuntimeError::WouldBlock);
        }
        records.push_back(OrderedRecord::Guest(tag));
        Ok(true)
    }

    pub(crate) fn submit_host_bytes(bytes: &[u8]) {
        HOST_BYTES.lock().extend_from_slice(bytes);
    }

    pub(crate) fn submit_host_transaction(transaction: impl FnOnce(&mut dyn FnMut(&[u8]))) {
        transaction(&mut |bytes| HOST_BYTES.lock().extend_from_slice(bytes));
    }
}

#[path = "../src/guest_console/mux/mod.rs"]
pub(crate) mod mux;
