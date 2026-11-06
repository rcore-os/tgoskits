#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

mod app;
pub mod arceos;
pub mod axloader;
pub mod axvisor;
mod build;
mod checks;
mod cli;
pub mod context;
mod execution;
pub mod image;
mod profiling;
mod rootfs;
pub mod starry;
mod support;
mod test;
mod tools;

pub(crate) use cli::{ClippyArgs, SyncLintArgs};
pub use cli::{run, run_from};
