//! Target architecture namespaces and compile-time selection.

use crate::{architecture::*, *};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "loongarch64")]
mod loongarch64;
#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

pub(crate) mod current;
