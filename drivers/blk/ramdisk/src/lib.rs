//! Retired in-memory block adapter.
//!
//! The public synchronous queue implementation was removed with the IRQ-only
//! block-runtime migration. Runtime tests use a private fake hardware queue
//! instead, so this crate deliberately exports no block device.

#![no_std]
