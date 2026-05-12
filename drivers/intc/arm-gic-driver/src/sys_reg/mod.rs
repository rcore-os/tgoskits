// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// GICv3 System Register definitions

#[macro_use]
mod macros;

// ICC (Interrupt Controller CPU interface) registers
#[macro_use]
pub mod icc;

// ICH (Interrupt Controller Hypervisor) registers
#[cfg(target_arch = "aarch64")]
#[macro_use]
pub mod ich;

// Re-export all ICC registers
pub use icc::*;
// Re-export all ICH registers
#[cfg(target_arch = "aarch64")]
pub use ich::*;
