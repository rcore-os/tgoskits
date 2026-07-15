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

//! Per-VM Arm GICv3 interrupt-controller model.
//!
//! The crate owns only architecture state and validated backend capabilities.
//! Guest buses, host GIC discovery, timers, and VM scheduling are supplied by
//! the AxVM integration layer. Only Group 1 Non-secure delivery is modeled.

extern crate alloc;

mod backend;
mod config;
mod controller;
mod cpu_interface;
mod distributor;
mod error;
mod interrupt;
mod its;
mod redistributor;
mod register;
mod types;

pub use backend::*;
pub use config::*;
pub use controller::*;
pub use cpu_interface::*;
pub(crate) use distributor::DistributorState;
pub use error::*;
pub(crate) use interrupt::InterruptRecord;
pub use its::{GuestMemory, GuestMemoryError};
pub(crate) use its::{ItsAction, ItsState};
pub(crate) use redistributor::RedistributorState;
pub use types::*;
