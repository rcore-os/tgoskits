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

//! Host-neutral Linux KVM userspace ABI definitions.
//!
//! This crate intentionally contains only UAPI constants, wire-format structs,
//! and small encoding/decoding helpers. It must not depend on AxVisor runtime
//! state, host control callbacks, VM objects, or vCPU implementations.

#![no_std]

pub mod error;
pub mod ioctl;
pub mod riscv;
pub mod structs;
pub mod x86;

pub use error::{KvmUapiError, Result};
pub use structs::*;
