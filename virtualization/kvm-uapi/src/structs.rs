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

//! Plain data payloads used by KVM ioctls.
//!
//! These types model the userspace ABI only. They intentionally do not own host
//! resources such as pinned pages, eventfd references, or background tasks.

/// Payload for `KVM_SET_USER_MEMORY_REGION`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UserspaceMemoryRegion {
    pub slot: u32,
    pub flags: u32,
    pub guest_phys_addr: u64,
    pub memory_size: u64,
    pub userspace_addr: u64,
}

/// Payload for `KVM_GET_ONE_REG` and `KVM_SET_ONE_REG`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OneReg {
    pub id: u64,
    pub addr: u64,
}

/// Payload for `KVM_IOEVENTFD`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KvmIoEventFd {
    pub datamatch: u64,
    pub addr: u64,
    pub len: u32,
    pub fd: i32,
    pub flags: u32,
}

/// One entry in the variable-length `struct kvm_cpuid2` payload.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KvmCpuidEntry2 {
    pub function: u32,
    pub index: u32,
    pub flags: u32,
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

/// Payload for `KVM_IRQFD`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KvmIrqFd {
    pub fd: u32,
    pub gsi: u32,
    pub flags: u32,
    pub resamplefd: u32,
}

/// Payload for `KVM_ENABLE_CAP`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KvmEnableCap {
    pub cap: u32,
    pub flags: u32,
    pub args: [u64; 4],
}
