//! x86 nested-paging formats and their runtime-selected address-space adapter.
//!
//! Intel processors use Extended Page Tables (EPT), while AMD processors use
//! Nested Page Tables (NPT). The runtime adapter selects the format after the
//! x86 virtualization backend is initialized.

mod ept;
mod npt;
mod runtime;

pub(crate) use runtime::NestedPageTable;
