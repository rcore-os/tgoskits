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

//! RISC-V Hypervisor Extension Support
//!
//! This crate provides low-level access to RISC-V hypervisor extension registers.
//! It implements the hypervisor CSRs defined in the RISC-V Hypervisor Extension
//! specification, enabling virtualization support on RISC-V processors.
//!
//!
//! # Usage
//! ```rust,no_run
//! use riscv_h::register::hstatus;
//!
//! // Read hypervisor status register
//! let hstatus = hstatus::read();
//!
//! // Check if virtualization mode is enabled
//! if hstatus.spv() {
//!     // Handle virtualized context
//! }
//! ```

#![no_std]
#![allow(missing_docs)]

/// RISC-V hypervisor extension register definitions and access functions
pub mod register;
