// Copyright 2026 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![no_std]

//! Per-VM Arm virtual Generic Interrupt Controller.
//!
//! The crate owns guest architectural interrupt state. Host GIC discovery,
//! guest firmware, VM scheduling, and bus registration remain in the AArch64
//! AxVM integration. GICv2 and GICv3 frontends share the same canonical state
//! owner; the host CPU-interface implementation is a checked backend.

extern crate alloc;

mod arm_config;
mod backend;
mod config;
mod controller;
mod core;
mod cpu_interface;
mod devices;
mod distributor;
mod error;
mod interrupt;
mod its;
mod redistributor;
mod register;
mod types;

pub use core::*;

pub use arm_config::*;
pub use backend::*;
pub use config::*;
pub use controller::*;
pub use cpu_interface::*;
pub use devices::{VgicAccessContext, VgicDeviceSet};
pub(crate) use distributor::DistributorState;
pub use error::*;
pub(crate) use interrupt::InterruptRecord;
pub use its::{GuestMemory, GuestMemoryError};
pub(crate) use its::{ItsAction, ItsState};
pub(crate) use redistributor::{QueuedDelivery, RedistributorState};
pub use types::*;
