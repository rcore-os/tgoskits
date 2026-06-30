//! Shared QEMU test case asset orchestration.
//!
//! Main responsibilities:
//! - Decide whether a test case needs extra build or injection work
//! - Prepare case-scoped work directories, overlays, and auxiliary QEMU assets
//! - Dispatch C, shell, and Python case flows before rootfs content injection

mod assets;
mod cache;
mod grouped_runner;
mod hash;
mod layout;
mod qemu_run;
mod shell;
mod types;

pub(crate) use assets::*;
#[cfg(test)]
use cache::save_rootfs_cache_image;
pub(crate) use grouped_runner::*;
#[cfg(test)]
use hash::case_asset_cache_key;
pub(crate) use layout::*;
pub(crate) use qemu_run::*;
pub(crate) use shell::{case_sh_source_dir, prepare_sh_case_assets_sync};
pub(crate) use types::*;

#[cfg(test)]
mod tests;
