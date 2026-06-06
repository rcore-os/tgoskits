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

//! AxVisor KVM-compatible host control endpoint callbacks.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ax_errno::{AxResult, ax_err};
use axvisor_api::control::{self as api_control, ControlOps, EndpointSpec};

const KVMIO: u32 = 0xae;

/// Current Linux KVM userspace API version.
pub const KVM_API_VERSION: usize = 12;

/// Returns [`KVM_API_VERSION`].
pub const KVM_GET_API_VERSION: u32 = ioc(KVMIO, 0x00);
/// Creates a VM fd.
pub const KVM_CREATE_VM: u32 = ioc(KVMIO, 0x01);
/// Checks whether a KVM capability is supported.
pub const KVM_CHECK_EXTENSION: u32 = ioc(KVMIO, 0x03);
/// Returns the size of the vCPU mmap area.
pub const KVM_GET_VCPU_MMAP_SIZE: u32 = ioc(KVMIO, 0x04);

pub const KVM_CAP_USER_MEMORY: usize = 3;
pub const KVM_CAP_NR_VCPUS: usize = 9;
pub const KVM_CAP_NR_MEMSLOTS: usize = 10;
pub const KVM_CAP_MAX_VCPUS: usize = 66;
pub const KVM_CAP_IMMEDIATE_EXIT: usize = 136;

const KVM_MAX_VCPUS: usize = 1;
const KVM_MAX_MEMORY_SLOTS: usize = 32;
const KVM_VCPU_MMAP_SIZE: usize = 0x1000;

static REGISTERED: AtomicBool = AtomicBool::new(false);
static ENDPOINT_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Registers the host-visible KVM-compatible control endpoint.
pub fn init() -> AxResult {
    if REGISTERED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }

    let endpoint = api_control::register_endpoint(EndpointSpec {
        name: "kvm",
        ops: ControlOps {
            open,
            release,
            ioctl,
            read: None,
            write: None,
            poll: None,
            mmap: None,
        },
    })?;

    ENDPOINT_ID.store(endpoint, Ordering::Release);
    info!("AxVisor KVM control endpoint registered: {}", endpoint);
    Ok(())
}

/// Unregisters the host-visible KVM-compatible control endpoint.
pub fn shutdown() -> AxResult {
    if !REGISTERED.swap(false, Ordering::AcqRel) {
        return Ok(());
    }

    let endpoint = ENDPOINT_ID.swap(0, Ordering::AcqRel);
    api_control::unregister_endpoint(endpoint)
}

fn open() -> AxResult<api_control::SessionId> {
    let session = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    if session == 0 {
        return ax_err!(OutOfRange);
    }
    Ok(session)
}

fn release(_session: api_control::SessionId) -> AxResult {
    Ok(())
}

fn ioctl(_session: api_control::SessionId, cmd: u32, arg: usize) -> AxResult<isize> {
    match cmd {
        KVM_GET_API_VERSION => Ok(KVM_API_VERSION as isize),
        KVM_CHECK_EXTENSION => Ok(check_extension(arg) as isize),
        KVM_GET_VCPU_MMAP_SIZE => Ok(KVM_VCPU_MMAP_SIZE as isize),
        KVM_CREATE_VM => ax_err!(Unsupported),
        _ => ax_err!(Unsupported),
    }
}

fn check_extension(capability: usize) -> usize {
    match capability {
        KVM_CAP_USER_MEMORY => 1,
        KVM_CAP_NR_VCPUS => KVM_MAX_VCPUS,
        KVM_CAP_MAX_VCPUS => KVM_MAX_VCPUS,
        KVM_CAP_NR_MEMSLOTS => KVM_MAX_MEMORY_SLOTS,
        KVM_CAP_IMMEDIATE_EXIT => 1,
        _ => 0,
    }
}

const fn ioc(type_: u32, nr: u32) -> u32 {
    (type_ << 8) | nr
}
