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

//! AxVisor HyperCall definitions.
//!
//! This crate provides the hypercall interface for AxVisor, a type-1 hypervisor
//! based on ArceOS. It defines the hypercall codes and result types used for
//! communication between guest VMs and the hypervisor.
//!
//! # Overview
//!
//! Hypercalls are the primary mechanism for guest VMs to request services from
//! the hypervisor. This crate defines:
//!
//! - [`HyperCallCode`]: An enumeration of all supported hypercall operations
//! - [`HyperCallResult`]: The result type returned by hypercall handlers
//!
//! # Supported Hypercalls
//!
//! The following hypercall categories are supported:
//!
//! - **Hypervisor Control**: Enable/disable hypervisor functionality
//! - **Inter-VM Communication (IVC)**: Shared memory channels between VMs
//!
//! # Example
//!
//! ```ignore
//! use axhvc::{HyperCallCode, HyperCallResult};
//!
//! fn handle_hypercall(code: HyperCallCode) -> HyperCallResult {
//!     match code {
//!         HyperCallCode::HypervisorDisable => {
//!             // Handle hypervisor disable request
//!             Ok(0)
//!         }
//!         _ => Err(axerrno::AxError::Unsupported),
//!     }
//! }
//! ```
//!
//! # Features
//!
//! This crate is `no_std` compatible and can be used in bare-metal environments.

#![no_std]
#![deny(missing_docs)]

use axerrno::AxResult;

/// Hypercall operation codes for AxVisor.
///
/// Each variant represents a specific operation that a guest VM can request
/// from the hypervisor. The numeric values are used as the hypercall number
/// when invoking hypercalls from guest code.
///
/// # Categories
///
/// - **Hypervisor Control** (0-2): Operations to control the hypervisor lifecycle
/// - **IVC Operations** (3-6): Inter-VM communication channel management
///
/// # Example
///
/// ```
/// use axhvc::HyperCallCode;
///
/// let code = HyperCallCode::HypervisorDisable;
/// assert_eq!(code as u32, 0);
///
/// // Convert from u32 to HyperCallCode
/// let code = HyperCallCode::try_from(0u32).unwrap();
/// assert_eq!(code, HyperCallCode::HypervisorDisable);
/// ```
#[repr(u32)]
#[derive(Eq, PartialEq, Copy, Clone)]
pub enum HyperCallCode {
    /// Disable the hypervisor.
    ///
    /// This hypercall requests the hypervisor to disable itself and return
    /// control to the guest operating system. After this call, the guest
    /// will run in native mode without virtualization.
    ///
    /// # Returns
    ///
    /// - `Ok(0)` on success
    /// - `Err(_)` if the hypervisor cannot be disabled
    HypervisorDisable = 0,

    /// Prepare to disable the hypervisor.
    ///
    /// This hypercall prepares for hypervisor shutdown by mapping the
    /// hypervisor memory to the guest address space. This is typically
    /// called before [`HyperCallCode::HypervisorDisable`].
    ///
    /// # Returns
    ///
    /// - `Ok(0)` on success
    /// - `Err(_)` if preparation fails
    HyperVisorPrepareDisable = 1,

    /// Debug hypercall for development purposes.
    ///
    /// This hypercall is intended for debugging and development. Its behavior
    /// may vary depending on the hypervisor build configuration.
    ///
    /// # Warning
    ///
    /// This hypercall should not be used in production environments.
    HyperVisorDebug = 2,

    /// Publish an IVC (Inter-VM Communication) shared memory channel.
    ///
    /// Creates a new shared memory channel that other VMs can subscribe to.
    /// The publisher VM owns the channel and controls its lifecycle.
    ///
    /// # Arguments
    ///
    /// - `key`: The unique key identifying this IVC channel
    /// - `shm_base_gpa_ptr`: Pointer to receive the base guest physical address
    ///   of the shared memory region
    /// - `shm_size_ptr`: Pointer to receive the size of the shared memory region
    ///
    /// # Returns
    ///
    /// - `Ok(0)` on success, with the shared memory info written to the provided pointers
    /// - `Err(_)` if the channel cannot be created
    HIVCPublishChannel = 3,

    /// Subscribe to an IVC shared memory channel.
    ///
    /// Connects to an existing shared memory channel created by another VM.
    ///
    /// # Arguments
    ///
    /// - `publisher_vm_id`: The ID of the VM that published the channel
    /// - `key`: The key of the IVC channel to subscribe to
    /// - `shm_base_gpa_ptr`: Pointer to receive the base guest physical address
    ///   of the shared memory region
    /// - `shm_size_ptr`: Pointer to receive the size of the shared memory region
    ///
    /// # Returns
    ///
    /// - `Ok(0)` on success, with the shared memory info written to the provided pointers
    /// - `Err(_)` if subscription fails (e.g., channel not found)
    HIVCSubscribChannel = 4,

    /// Unpublish an IVC shared memory channel.
    ///
    /// Removes a previously published IVC channel. All subscribers will be
    /// disconnected when this is called.
    ///
    /// # Arguments
    ///
    /// - `key`: The key of the IVC channel to unpublish
    ///
    /// # Returns
    ///
    /// - `Ok(0)` on success
    /// - `Err(_)` if the channel cannot be unpublished
    HIVCUnPublishChannel = 5,

    /// Unsubscribe from an IVC shared memory channel.
    ///
    /// Disconnects from a previously subscribed IVC channel.
    ///
    /// # Arguments
    ///
    /// - `publisher_vm_id`: The ID of the publisher VM
    /// - `key`: The key of the IVC channel to unsubscribe from
    ///
    /// # Returns
    ///
    /// - `Ok(0)` on success
    /// - `Err(_)` if unsubscription fails
    HIVCUnSubscribChannel = 6,
}

/// Error type for invalid hypercall code conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidHyperCallCode(
    /// The invalid numeric value that was attempted to convert.
    pub u32,
);

impl core::fmt::Display for InvalidHyperCallCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid hypercall code: {:#x}", self.0)
    }
}

impl TryFrom<u32> for HyperCallCode {
    type Error = InvalidHyperCallCode;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(HyperCallCode::HypervisorDisable),
            1 => Ok(HyperCallCode::HyperVisorPrepareDisable),
            2 => Ok(HyperCallCode::HyperVisorDebug),
            3 => Ok(HyperCallCode::HIVCPublishChannel),
            4 => Ok(HyperCallCode::HIVCSubscribChannel),
            5 => Ok(HyperCallCode::HIVCUnPublishChannel),
            6 => Ok(HyperCallCode::HIVCUnSubscribChannel),
            _ => Err(InvalidHyperCallCode(value)),
        }
    }
}

impl core::fmt::Debug for HyperCallCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "(")?;
        match self {
            HyperCallCode::HypervisorDisable => write!(f, "HypervisorDisable {:#x}", *self as u32),
            HyperCallCode::HyperVisorPrepareDisable => {
                write!(f, "HyperVisorPrepareDisable {:#x}", *self as u32)
            }
            HyperCallCode::HyperVisorDebug => write!(f, "HyperVisorDebug {:#x}", *self as u32),
            HyperCallCode::HIVCPublishChannel => {
                write!(f, "HIVCPublishChannel {:#x}", *self as u32)
            }
            HyperCallCode::HIVCSubscribChannel => {
                write!(f, "HIVCSubscribChannel {:#x}", *self as u32)
            }
            HyperCallCode::HIVCUnPublishChannel => {
                write!(f, "HIVCUnPublishChannel {:#x}", *self as u32)
            }
            HyperCallCode::HIVCUnSubscribChannel => {
                write!(f, "HIVCUnSubscribChannel {:#x}", *self as u32)
            }
        }?;
        write!(f, ")")
    }
}

/// The result type for hypercall operations.
///
/// This is an alias for [`AxResult<usize>`], where:
/// - `Ok(value)` indicates successful execution with a return value
/// - `Err(error)` indicates failure with an error code
///
/// # Example
///
/// ```ignore
/// use axhvc::HyperCallResult;
///
/// fn my_hypercall_handler() -> HyperCallResult {
///     // Perform hypercall operation...
///     Ok(0)
/// }
/// ```
pub type HyperCallResult = AxResult<usize>;
