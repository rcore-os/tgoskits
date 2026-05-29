// Copyright 2025 The Axvisor Team
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

//! Axvisor host boundary.
//!
//! This module is the only Axvisor layer that directly adapts ArceOS host
//! capabilities. Business code should use the focused facade modules here
//! instead of importing ArceOS modules directly.

pub mod cache;
pub mod console;
pub mod cpu;
pub mod fdt;
pub mod fs;
pub mod irq;
pub mod memory;
pub mod paging;
pub mod percpu;
pub mod platform;
pub mod task;
pub mod time;
pub mod timer;

mod api_impl;

#[cfg_attr(target_arch = "aarch64", path = "arch/aarch64/mod.rs")]
#[cfg_attr(target_arch = "loongarch64", path = "arch/loongarch64/mod.rs")]
#[cfg_attr(target_arch = "x86_64", path = "arch/x86_64/mod.rs")]
#[cfg_attr(target_arch = "riscv64", path = "arch/riscv64/mod.rs")]
mod arch;
