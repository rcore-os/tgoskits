//! CPU-local maintenance domains for IRQ-driven device progression.
//!
//! A maintenance domain has one CPU-pinned owner thread. Hard IRQ callbacks
//! only publish Copy event snapshots into a fixed mailbox and directly wake
//! that owner. The owner alone advances device state, performs recovery, and
//! tears the domain down. This keeps device lifetime independent from the
//! shared workqueue's pending/running snapshots.

mod action;
mod lifecycle;
mod mailbox;
mod owner_cell;
mod runtime;

pub use action::*;
pub use lifecycle::*;
pub use mailbox::*;
pub use owner_cell::*;
pub use runtime::*;
