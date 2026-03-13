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

//! AxVCpu - Virtual CPU abstraction for ArceOS hypervisors.
//!
//! This crate provides a unified, architecture-independent interface for managing virtual CPUs
//! in hypervisor environments. It delegates architecture-specific operations to implementations
//! of the `AxArchVCpu` trait while providing common functionality like state management,
//! CPU binding, and execution control.
//!
//! # Features
//!
//! - Architecture-agnostic virtual CPU management
//! - State machine for VCpu lifecycle (Created → Free → Ready → Running)
//! - Per-CPU virtualization state management
//! - Hardware abstraction layer for hypervisor operations
//! - Support for interrupt injection and register manipulation

#![no_std]

#[macro_use]
extern crate alloc;

// Core modules
mod arch_vcpu; // Architecture-specific VCpu trait definition
mod exit; // VM exit reason enumeration and handling
mod hal; // Hardware abstraction layer interfaces
mod percpu; // Per-CPU virtualization state management
mod test; // Unit tests for VCpu functionality
mod vcpu; // Main VCpu implementation and state management

// Public API exports
pub use arch_vcpu::AxArchVCpu; // Architecture-specific VCpu trait
pub use exit::AxVCpuExitReason;
pub use hal::AxVCpuHal; // Hardware abstraction layer trait
pub use percpu::*; // Per-CPU state management types
pub use vcpu::*; // Main VCpu types and functions // VM exit reasons
