//! Runs AxVM's private IOCSR behavior tests in the real LoongArch runtime.

#![cfg_attr(target_os = "none", no_std)]
#![no_main]

use ax_runtime as _;
use ax_std as _;
use axvm as _;

// The cases and descriptors remain in AxVM; this target only owns the runner.
#[axtest::tests]
mod tests {}
