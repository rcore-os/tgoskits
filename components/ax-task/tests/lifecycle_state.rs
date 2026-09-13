//! Exercise the production lifecycle and its Loom model without a fake runtime.
mod thread {
    pub use ax_task::thread::{TaskError, ThreadState};
}

#[path = "../src/thread/state.rs"]
mod state;
