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

//! AxVisor host control endpoint callbacks.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ax_errno::{AxResult, ax_err, ax_err_type};
use axvisor_api::control::{self as api_control, ControlOps, EndpointSpec};

/// Current AxVisor control ABI version.
pub const ABI_VERSION: u32 = 1;

/// Returns [`ABI_VERSION`].
pub const AXVISOR_GET_API_VERSION: u32 = 0x0000;
/// Checks whether an optional control extension is supported.
pub const AXVISOR_CHECK_EXTENSION: u32 = 0x0001;

/// A placeholder extension id used to validate the control path.
pub const EXT_BASE_CONTROL: u32 = 0;

static REGISTERED: AtomicBool = AtomicBool::new(false);
static ENDPOINT_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Registers the host-visible AxVisor control endpoint.
pub fn init() -> AxResult {
    if REGISTERED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }

    let endpoint = api_control::register_endpoint(EndpointSpec {
        name: "axvisor",
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
    info!("AxVisor control endpoint registered: {}", endpoint);
    Ok(())
}

/// Unregisters the host-visible AxVisor control endpoint.
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

fn ioctl(
    _session: api_control::SessionId,
    cmd: u32,
    input: &[u8],
    output: &mut [u8],
) -> AxResult<usize> {
    match cmd {
        AXVISOR_GET_API_VERSION => write_u32(output, ABI_VERSION),
        AXVISOR_CHECK_EXTENSION => {
            let extension = read_u32(input)?;
            write_u32(output, is_extension_supported(extension) as u32)
        }
        _ => ax_err!(Unsupported),
    }
}

fn is_extension_supported(extension: u32) -> bool {
    extension == EXT_BASE_CONTROL
}

fn read_u32(input: &[u8]) -> AxResult<u32> {
    let bytes = input.get(..4).ok_or_else(|| ax_err_type!(InvalidInput))?;
    Ok(u32::from_ne_bytes(bytes.try_into().unwrap()))
}

fn write_u32(output: &mut [u8], value: u32) -> AxResult<usize> {
    let bytes = output
        .get_mut(..4)
        .ok_or_else(|| ax_err_type!(InvalidInput))?;
    bytes.copy_from_slice(&value.to_ne_bytes());
    Ok(4)
}
