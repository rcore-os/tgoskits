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
//!         _ => Err(axhvc::HyperCallError::Unsupported {
//!             code,
//!             detail: "not implemented by this handler".into(),
//!         }),
//!     }
//! }
//! ```
//!
//! # Features
//!
//! This crate is `no_std` compatible and can be used in bare-metal environments.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

mod error;

pub use error::{HyperCallError, HyperCallResult, InvalidHyperCallCode};

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
    /// PSCI_VERSION.
    PSCIVersion          = 0x8400_0000,

    /// PSCI_CPU_SUSPEND.
    PSCICpuSuspend       = 0x8400_0001,

    /// PSCI_CPU_OFF.
    PSCICpuOff           = 0x8400_0002,

    /// PSCI_CPU_ON.
    PSCICpuOn            = 0x8400_0003,

    /// PSCI_AFFINITY_INFO.
    PSCIAffinityInfo     = 0x8400_0004,

    /// PSCI_MIGRATE_INFO_TYPE.
    PSCIMigrateInfoType  = 0x8400_0006,

    /// PSCI_SYSTEM_OFF.
    PSCISystemOff        = 0x8400_0008,

    /// PSCI_SYSTEM_RESET.
    PSCISystemReset      = 0x8400_0009,

    /// PSCI features.
    PSCIFeatures         = 0x8400_000a,

    /// PSCI CPU suspend, SMC64.
    PSCICpuSuspend64     = 0xc400_0001,

    /// PSCI CPU on, SMC64.
    PSCICpuOn64          = 0xc400_0003,

    /// PSCI affinity info, SMC64.
    PSCIAffinityInfo64   = 0xc400_0004,

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
    HypervisorDisable    = 0,

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
    HyperVisorDebug      = 2,

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
    HIVCPublishChannel   = 3,

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
    HIVCSubscribChannel  = 4,

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

impl TryFrom<u32> for HyperCallCode {
    type Error = InvalidHyperCallCode;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0x8400_0000 => Ok(HyperCallCode::PSCIVersion),
            0x8400_0001 => Ok(HyperCallCode::PSCICpuSuspend),
            0x8400_0002 => Ok(HyperCallCode::PSCICpuOff),
            0x8400_0003 => Ok(HyperCallCode::PSCICpuOn),
            0x8400_0004 => Ok(HyperCallCode::PSCIAffinityInfo),
            0x8400_0006 => Ok(HyperCallCode::PSCIMigrateInfoType),
            0x8400_0008 => Ok(HyperCallCode::PSCISystemOff),
            0x8400_0009 => Ok(HyperCallCode::PSCISystemReset),
            0x8400_000a => Ok(HyperCallCode::PSCIFeatures),
            0xc400_0001 => Ok(HyperCallCode::PSCICpuSuspend64),
            0xc400_0003 => Ok(HyperCallCode::PSCICpuOn64),
            0xc400_0004 => Ok(HyperCallCode::PSCIAffinityInfo64),

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
            Self::PSCIVersion => write!(f, "PSCIVersion"),
            Self::PSCICpuSuspend => write!(f, "PSCICpuSuspend"),
            Self::PSCICpuOff => write!(f, "PSCICpuOff"),
            Self::PSCICpuOn => write!(f, "PSCICpuOn"),
            Self::PSCIAffinityInfo => write!(f, "PSCIAffinityInfo"),
            Self::PSCIMigrateInfoType => write!(f, "PSCIMigrateInfoType"),
            Self::PSCISystemOff => write!(f, "PSCISystemOff"),
            Self::PSCISystemReset => write!(f, "PSCISystemReset"),
            Self::PSCIFeatures => write!(f, "PSCIFeatures"),
            Self::PSCICpuSuspend64 => write!(f, "PSCICpuSuspend64"),
            Self::PSCICpuOn64 => write!(f, "PSCICpuOn64"),
            Self::PSCIAffinityInfo64 => write!(f, "PSCIAffinityInfo64"),
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
