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

//! Host system related APIs for the AxVisor hypervisor.
//!
//! This module provides APIs for querying information about the host system
//! on which the hypervisor is running.
//!
//! # Overview
//!
//! The host system APIs provide essential information about the underlying
//! hardware that the hypervisor needs to manage virtual machines effectively.
//!
//! # Available APIs
//!
//! - [`get_host_cpu_num`] - Get the total number of CPUs in the host system.
//!
//! # Implementation
//!
//! To implement these APIs, use the [`api_impl`](crate::api_impl) attribute
//! macro on an impl block:
//!
//! ```rust,ignore
//! struct HostIfImpl;
//!
//! #[axvisor_api::api_impl]
//! impl axvisor_api::host::HostIf for HostIfImpl {
//!     fn get_host_cpu_num() -> usize {
//!         // Return the number of CPUs from your platform
//!         4
//!     }
//! }
//! ```

/// The API trait for host system functionalities.
///
/// This trait defines the interface for querying host system information.
/// Implementations should be provided by the host system or HAL layer.
#[crate::api_def]
pub trait HostIf {
    /// Get the total number of CPUs (logical processors) in the host system.
    ///
    /// This function returns the number of CPUs available to the hypervisor,
    /// which is typically the same as the number of physical or logical
    /// processors in the system.
    ///
    /// # Returns
    ///
    /// The number of CPUs in the host system.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let cpu_count = axvisor_api::host::get_host_cpu_num();
    /// println!("Host has {} CPUs", cpu_count);
    /// ```
    fn get_host_cpu_num() -> usize;
}
