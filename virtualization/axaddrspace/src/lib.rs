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

//! [ArceOS-Hypervisor](https://github.com/arceos-hypervisor/) guest VM address space management module.

#![no_std]
#[macro_use]
extern crate log;
extern crate alloc;

mod address_space;
mod error;
mod memory_accessor;
mod paging;

pub use address_space::{AddrSpace, Backend};
pub use axvm_types::MappingFlags;
pub use error::{AddrSpaceError, AddrSpaceResult};
pub use memory_accessor::GuestMemoryAccessor;
pub use paging::{NestedPageTableOps, PageSize};
