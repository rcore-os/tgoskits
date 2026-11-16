//! CPU checks that depend on a particular physical board.

// The filesystem keeps the public board name; Rust uses its identifier form.
// This is an ordinary module of this test crate, not included production code.
#[path = "orangepi-5-plus/mod.rs"]
pub mod orangepi_5_plus;
