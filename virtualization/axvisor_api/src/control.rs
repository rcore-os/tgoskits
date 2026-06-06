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
//! This module lets `axvisor-core` register a host-visible KVM endpoint such
//! as `/dev/kvm`. The host adapter owns the OS file/device plumbing, while
//! `axvisor-core` owns KVM command semantics and object handles.

use alloc::vec::Vec;

use ax_errno::AxResult;

use crate::memory::PhysAddr;

/// A host-provided control endpoint identifier.
pub type EndpointId = u64;

/// A core-provided session identifier for one open control connection.
pub type SessionId = u64;

/// A host file descriptor returned to userspace.
pub type HostFd = i32;

/// A host-provided acquired userspace memory handle.
pub type UserMemoryHandle = u64;

/// Host physical pages backing an acquired userspace memory range.
pub struct AcquiredUserMemory {
    /// Host-owned handle used to release the acquired pages.
    pub handle: UserMemoryHandle,
    /// Page-sized host physical addresses, in userspace virtual-address order.
    pub pages: Vec<PhysAddr>,
}

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
    /// Dispatches a KVM ioctl command.
    ///
    /// The `cmd` and `arg` values are the raw ioctl request and third argument
    /// supplied by userspace. The callback return value is the ioctl syscall
    /// return value. KVM commands decide whether `arg` is unused, an immediate
    /// value, or a userspace pointer.
    pub ioctl: fn(SessionId, u32, usize) -> AxResult<isize>,
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
    /// Stable endpoint name, for example `kvm`.
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

    /// Creates a VM object fd owned by the current userspace process.
    ///
    /// On success, the host fd owns `session` and must release it when the fd
    /// is closed. On failure, ownership remains with `axvisor-core`.
    fn create_vm_fd(endpoint: EndpointId, session: SessionId) -> AxResult<HostFd>;

    /// Creates a vCPU object fd owned by the current userspace process.
    ///
    /// On success, the host fd owns `session` and must release it when the fd
    /// is closed. On failure, ownership remains with `axvisor-core`.
    fn create_vcpu_fd(
        endpoint: EndpointId,
        session: SessionId,
        mmap_size: usize,
    ) -> AxResult<HostFd>;

    /// Reads bytes from the current userspace task.
    ///
    /// This is the host-neutral copy-from-user primitive used by KVM ioctls
    /// whose third argument is a userspace pointer. The host validates and
    /// copies from its current user address space; `axvisor-core` owns the ABI
    /// layout and command semantics.
    fn read_user(addr: usize, buf: &mut [u8]) -> AxResult;

    /// Writes bytes into the current userspace task.
    ///
    /// This is the host-neutral copy-to-user primitive used by KVM ioctls whose
    /// third argument is a userspace pointer. The host validates and copies
    /// into its current user address space; `axvisor-core` owns the ABI layout
    /// and command semantics.
    fn write_user(addr: usize, buf: &[u8]) -> AxResult;

    /// Acquires pages from the current userspace task and returns their physical backing pages.
    ///
    /// The host must keep the returned pages stable until [`release_user_memory`]
    /// is called with the returned handle. The host may implement this by pinning
    /// frames, holding VM object references, or any other mechanism with the same
    /// lifetime semantics.
    fn acquire_user_memory(addr: usize, len: usize, writable: bool)
    -> AxResult<AcquiredUserMemory>;

    /// Releases a userspace memory range returned by [`acquire_user_memory`].
    fn release_user_memory(handle: UserMemoryHandle) -> AxResult;
}
