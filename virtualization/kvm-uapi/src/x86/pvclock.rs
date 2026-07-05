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

//! KVM pvclock wire-format structures.

/// Wall-clock payload written to the guest page referenced by `MSR_KVM_WALL_CLOCK_NEW`.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PvClockWallClock {
    pub version: u32,
    pub sec: u32,
    pub nsec: u32,
}

/// Per-vCPU time info payload written to the page referenced by `MSR_KVM_SYSTEM_TIME_NEW`.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PvClockVcpuTimeInfo {
    pub version: u32,
    pub pad0: u32,
    pub tsc_timestamp: u64,
    pub system_time: u64,
    pub tsc_to_system_mul: u32,
    pub tsc_shift: i8,
    pub flags: u8,
    pub pad: [u8; 2],
}
