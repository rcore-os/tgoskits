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

/// MMIO address layout: devices start at 0x1000_0000, spaced 0x1000 apart,
/// each covering [base, base + 0x100).
pub const MMIO_BASE: u64 = 0x1000_0000;
pub const MMIO_STRIDE: u64 = 0x1000;
pub const MMIO_SIZE: u64 = 0x100;

/// Port I/O address layout.
pub const PORT_BASE: u16 = 0x100;
pub const PORT_STRIDE: u16 = 0x10;
pub const PORT_SIZE: u16 = 0x8;

/// SysReg address layout (inclusive range: [addr, addr + count - 1]).
pub const SYSREG_BASE: u32 = 0x100;
pub const SYSREG_STRIDE: u32 = 0x10;
pub const SYSREG_COUNT: u32 = 0x4;

/// Returns the MMIO base address for the i-th device.
#[inline]
pub const fn mmio_addr(i: usize) -> u64 {
    MMIO_BASE + (i as u64) * MMIO_STRIDE
}

/// Returns the Port base address for the i-th device.
#[inline]
pub const fn port_addr(i: usize) -> u16 {
    PORT_BASE + (i as u16) * PORT_STRIDE
}

/// Returns the SysReg start address for the i-th device.
#[inline]
pub const fn sysreg_addr(i: usize) -> u32 {
    SYSREG_BASE + (i as u32) * SYSREG_STRIDE
}

/// Returns an MMIO address that falls between the i-th and (i+1)-th device
/// ranges (used for miss-benchmarks).
#[inline]
pub fn mmio_addr_between(i: usize) -> u64 {
    mmio_addr(i) + MMIO_SIZE + (MMIO_STRIDE - MMIO_SIZE) / 2
}

/// Common trait that each lookup-strategy registry implements.
pub trait Registry {
    /// Construct a registry with `n` pre-registered devices (one per bus type
    /// per device, at the standard layout addresses).
    fn new_with_devices(n: usize) -> Self;

    fn lookup_mmio(&self, addr: u64) -> Option<usize>;
    fn lookup_port(&self, addr: u16) -> Option<usize>;
    fn lookup_sysreg(&self, addr: u32) -> Option<usize>;
}
