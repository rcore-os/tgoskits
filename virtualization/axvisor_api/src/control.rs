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

//! Host control endpoint APIs for AxVisor.
//!
//! This module lets `axvisor-core` register a host-visible KVM endpoint such
//! as `/dev/kvm`. The host adapter owns the OS file/device plumbing, while
//! `axvisor-core` owns KVM command semantics and per-file state.

use alloc::vec::Vec;

use ax_errno::AxResult;

use crate::memory::PhysAddr;

/// A core-provided identifier for one open control file.
pub type ControlFileId = u64;

/// A userspace file descriptor returned to the current userspace task.
///
/// On Unix-like hosts this is typically a file descriptor.
pub type Fd = i32;

/// A host-provided identifier for one userspace-mappable memory area.
pub type MmapAreaId = u64;

/// A host-provided identifier for one retained userspace file descriptor reference.
pub type UserFdRefId = u64;

/// A host-provided identifier for one pinned userspace page range.
pub type PinnedUserPagesId = u64;

/// Host physical pages backing pinned userspace memory.
pub struct PinnedUserPages {
    /// Host-owned identifier used to release the pinned pages.
    pub id: PinnedUserPagesId,
    /// Page-sized host physical addresses, in userspace virtual-address order.
    pub pages: Vec<PhysAddr>,
}

/// Operations implemented by `axvisor-core` for one control file.
#[derive(Clone, Copy)]
pub struct ControlOps {
    /// Opens a new control file.
    pub open: fn() -> AxResult<ControlFileId>,
    /// Closes a previously opened control file.
    pub close: fn(ControlFileId) -> AxResult,
    /// Dispatches a KVM ioctl command.
    ///
    /// The `cmd` and `arg` values are the raw ioctl request and third argument
    /// supplied by userspace. The callback return value is the ioctl syscall
    /// return value. KVM commands decide whether `arg` is unused, an immediate
    /// value, or a userspace pointer.
    pub ioctl: fn(ControlFileId, u32, usize) -> AxResult<isize>,
}

/// The host control endpoint API required by AxVisor.
#[crate::api_def]
pub trait ControlIf {
    // Endpoint publication.

    /// Registers a host-visible control endpoint.
    ///
    /// The endpoint remains published for the host lifetime.
    fn register_endpoint(ops: ControlOps) -> AxResult;

    // Userspace file descriptors.

    /// Creates a userspace file descriptor owned by the current userspace process.
    ///
    /// On success, the new fd owns `control_file` and must close it when the fd
    /// is closed. If `mmap_area` is present, userspace may also `mmap` that
    /// area through the returned fd. On failure, ownership remains with
    /// `axvisor-core`.
    fn create_user_fd(
        control_file: ControlFileId,
        ops: ControlOps,
        mmap_area: Option<MmapAreaId>,
    ) -> AxResult<Fd>;

    /// Retains a userspace file descriptor from the current userspace task.
    ///
    /// The host may implement this by capturing the kernel object referenced by
    /// `fd`, such as a file, doorbell, or other userspace-visible handle.
    fn get_user_fd_ref(fd: Fd) -> AxResult<UserFdRefId>;

    /// Writes raw bytes to a previously retained userspace fd.
    ///
    /// The host performs the underlying object I/O. `axvisor-core` owns any
    /// higher-level protocol semantics imposed on those bytes.
    fn write_user_fd_ref(user_fd_ref: UserFdRefId, buf: &[u8]) -> AxResult<usize>;

    /// Releases a previously retained userspace fd.
    fn release_user_fd_ref(user_fd_ref: UserFdRefId) -> AxResult;

    // Userspace-mappable memory areas.

    /// Creates a userspace-mappable memory area.
    fn create_mmap_area(len: usize) -> AxResult<MmapAreaId>;

    /// Reads bytes from a userspace-mappable memory area.
    fn read_mmap_area(area: MmapAreaId, offset: usize, buf: &mut [u8]) -> AxResult;

    /// Writes bytes into a userspace-mappable memory area.
    fn write_mmap_area(area: MmapAreaId, offset: usize, buf: &[u8]) -> AxResult;

    /// Releases a previously created mmap area.
    fn release_mmap_area(area: MmapAreaId) -> AxResult;

    // Current userspace address space access.

    /// Copies bytes from the current userspace task.
    ///
    /// This is the host-neutral copy-from-user primitive used by KVM ioctls
    /// whose third argument is a userspace pointer. The host validates and
    /// copies from its current user address space; `axvisor-core` owns the ABI
    /// layout and command semantics.
    fn copy_from_user(addr: usize, buf: &mut [u8]) -> AxResult;

    /// Copies bytes into the current userspace task.
    ///
    /// This is the host-neutral copy-to-user primitive used by KVM ioctls whose
    /// third argument is a userspace pointer. The host validates and copies
    /// into its current user address space; `axvisor-core` owns the ABI layout
    /// and command semantics.
    fn copy_to_user(addr: usize, buf: &[u8]) -> AxResult;

    // Pinned userspace pages.

    /// Pins userspace memory from the current userspace task and returns its backing pages.
    ///
    /// The host must keep the returned pages stable until [`release_pinned_user_pages`]
    /// is called with the returned handle. The host may implement this by pinning
    /// frames, holding VM object references, or any other mechanism with the same
    /// lifetime semantics.
    fn pin_user_pages(addr: usize, len: usize, writable: bool) -> AxResult<PinnedUserPages>;

    /// Releases a previously pinned userspace page range.
    fn release_pinned_user_pages(id: PinnedUserPagesId) -> AxResult;
}
