//! Root-device discovery, selection, and mount orchestration.

mod implementation;

pub(crate) use implementation::ensure_mountpoint_dir_result;
pub use implementation::{RootSpec, init_root};
