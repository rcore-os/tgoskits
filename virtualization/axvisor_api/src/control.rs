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

/// A host-visible userspace handle returned to the current userspace task.
///
/// On Unix-like hosts this is typically a file descriptor.
pub type HostFd = i32;

/// A host-provided acquired userspace memory handle.
pub type UserMemoryHandle = u64;

/// A host-provided shared userspace mapping handle.
pub type UserMappingHandle = u64;

/// A host-provided userspace notification object handle.
pub type UserNotifierHandle = u64;

/// A host-visible userspace handle plus optional shared mapping capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreatedUserHandle {
    /// A host-visible userspace handle owned by the current userspace task.
    pub fd: HostFd,
    /// An optional shared mapping capability associated with the handle.
    pub mapping: Option<UserMappingHandle>,
}

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

/// Optional stream read callback for a control endpoint.
pub type ControlReadFn = fn(SessionId, &mut [u8]) -> AxResult<usize>;

/// Optional stream write callback for a control endpoint.
pub type ControlWriteFn = fn(SessionId, &[u8]) -> AxResult<usize>;

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
    pub read: Option<ControlReadFn>,
    /// Optional stream write operation.
    pub write: Option<ControlWriteFn>,
    /// Optional readiness query.
    pub poll: Option<fn(SessionId) -> AxResult<ControlEvents>>,
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

    /// Creates a host userspace handle owned by the current userspace process.
    ///
    /// On success, the host handle owns `session` and must release it when the
    /// handle is closed. On failure, ownership remains with `axvisor-core`.
    fn create_user_handle(
        endpoint: EndpointId,
        session: SessionId,
        shared_mapping_size: usize,
    ) -> AxResult<CreatedUserHandle>;

    /// Writes bytes into a previously created shared userspace mapping.
    fn write_user_mapping(handle: UserMappingHandle, offset: usize, buf: &[u8]) -> AxResult;

    /// Reads bytes from a previously created shared userspace mapping.
    fn read_user_mapping(handle: UserMappingHandle, offset: usize, buf: &mut [u8]) -> AxResult;

    /// Releases a previously created shared userspace mapping handle.
    fn release_user_mapping(handle: UserMappingHandle) -> AxResult;

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

    /// Acquires a signalable userspace notification object from the current
    /// userspace task.
    ///
    /// The host may implement this using eventfd, doorbells, or any other
    /// userspace-visible signaling primitive with equivalent semantics.
    fn acquire_user_notifier(fd: HostFd) -> AxResult<UserNotifierHandle>;

    /// Signals a previously acquired userspace notification object.
    fn signal_user_notifier(handle: UserNotifierHandle) -> AxResult;

    /// Releases a previously acquired userspace notification object.
    fn release_user_notifier(handle: UserNotifierHandle) -> AxResult;

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
