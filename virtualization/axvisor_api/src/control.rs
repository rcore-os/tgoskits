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

//! Host control endpoint registration APIs for AxVisor.
//!
//! This module lets `axvisor-core` register a host-visible control endpoint
//! such as `/dev/axvisor`, a syscall entry, or a shell command. The endpoint
//! transports bytes and operation codes only. VM lifecycle semantics and object
//! handles remain owned by `axvisor-core`.

use ax_errno::AxResult;

/// A host-provided control endpoint identifier.
pub type EndpointId = u64;

/// A core-provided session identifier for one open control connection.
pub type SessionId = u64;

/// Events reported by a host control endpoint.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ControlEvents {
    /// The endpoint can be read without blocking.
    pub readable: bool,
    /// The endpoint can be written without blocking.
    pub writable: bool,
    /// The endpoint has a pending error or hangup event.
    pub error: bool,
}

/// A host memory mapping request for a control endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MmapRequest {
    /// User-visible offset requested by the caller.
    pub offset: usize,
    /// Requested mapping size in bytes.
    pub len: usize,
    /// Host-specific protection flags.
    pub prot: usize,
    /// Host-specific mapping flags.
    pub flags: usize,
}

/// Result of a host memory mapping request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MmapResult {
    /// Host virtual address selected for the mapping, if the host exposes one.
    pub addr: usize,
    /// Mapping size in bytes.
    pub len: usize,
}

/// Operations implemented by `axvisor-core` for a registered control endpoint.
#[derive(Clone, Copy)]
pub struct ControlOps {
    /// Opens a new control session.
    pub open: fn() -> AxResult<SessionId>,
    /// Releases a previously opened control session.
    pub release: fn(SessionId) -> AxResult,
    /// Dispatches an ioctl-like command.
    ///
    /// Host adapters must copy user memory into `input` and copy `output`
    /// back to userspace after this function returns. Raw userspace pointers
    /// must not cross into `axvisor-core`.
    pub ioctl: fn(SessionId, u32, &[u8], &mut [u8]) -> AxResult<usize>,
    /// Optional stream read operation.
    pub read: Option<fn(SessionId, &mut [u8]) -> AxResult<usize>>,
    /// Optional stream write operation.
    pub write: Option<fn(SessionId, &[u8]) -> AxResult<usize>>,
    /// Optional readiness query.
    pub poll: Option<fn(SessionId) -> AxResult<ControlEvents>>,
    /// Optional memory mapping operation.
    pub mmap: Option<fn(SessionId, MmapRequest) -> AxResult<MmapResult>>,
}

/// Specification for a host-visible control endpoint.
#[derive(Clone, Copy)]
pub struct EndpointSpec {
    /// Stable endpoint name, for example `axvisor`.
    pub name: &'static str,
    /// Core callbacks for endpoint operations.
    pub ops: ControlOps,
}

/// The host control endpoint API required by AxVisor.
#[crate::api_def]
pub trait ControlIf {
    /// Registers a host-visible control endpoint.
    fn register_endpoint(spec: EndpointSpec) -> AxResult<EndpointId>;

    /// Unregisters a host-visible control endpoint.
    fn unregister_endpoint(id: EndpointId) -> AxResult;
}
