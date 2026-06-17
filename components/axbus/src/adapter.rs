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

//! Adapters that wrap the old `BaseDeviceOps<R>` traits into the new
//! `VirtualDevice` interface. This lets us use existing device implementations
//! (vGIC, vLAPIC, vPLIC, …) without any code changes to those crates.
//!
//! # Strategy
//!
//! Each adapter stores:
//! - An `Arc<dyn BaseDeviceOps<GuestPhysAddrRange>>` (or `<PortRange>` / `<SysRegAddrRange>`)
//! - A pre-computed list of `Resource`s (extracted once from `address_range()`)
//!
//! When `handle_access()` is called, the adapter converts the `BusAccess` into
//! the appropriate `handle_read()` / `handle_write()` call on the inner device.
//!
//! # Zero-cost principle
//!
//! After all devices are migrated to native `VirtualDevice` implementations,
//! these adapters can be removed with no functional change.

use alloc::{format, string::String, sync::Arc, vec::Vec};
use core::any::Any;

use axaddrspace::{
    GuestPhysAddr, GuestPhysAddrRange,
    device::{AccessWidth as LegacyWidth, Port, PortRange, SysRegAddr, SysRegAddrRange},
};
use axdevice_base::BaseDeviceOps;

use crate::{send_sync::AssertSendSync, r#trait::*};

// ── helpers ────────────────────────────────────────────────────────────────

/// Extract the start and end (as u64) from a `GuestPhysAddrRange` (AddrRange<GuestPhysAddr>).
fn mmio_range_bounds(range: &GuestPhysAddrRange) -> (u64, u64) {
    let start = range.start.as_usize() as u64;
    let end = range.end.as_usize() as u64;
    (start, end)
}

/// Build a `[Resource::Mmio]` vec from a legacy MMIO device.
fn mmio_resource_from_dev(dev: &dyn BaseDeviceOps<GuestPhysAddrRange>) -> Vec<Resource> {
    let range = dev.address_range();
    let (start, end) = mmio_range_bounds(&range);
    if end > start {
        // 由于 start 和 end 本身已通过 mmio_range_bounds 转换为 u64，此处无需冗余的 as u64 转换
        alloc::vec![Resource::Mmio(start..end)]
    } else {
        alloc::vec![]
    }
}

/// Extract PIO resources from a legacy Port device.
fn port_resource_from_dev(dev: &dyn BaseDeviceOps<PortRange>) -> Vec<Resource> {
    let range = dev.address_range();
    let start = range.start.0 as u32;
    let end = range.end.0 as u32 + 1; // PortRange is inclusive, convert to exclusive

    if end > start {
        alloc::vec![Resource::Pio(start..end)]
    } else {
        alloc::vec![]
    }
}

/// Extract SysReg resources from a legacy SysReg device.
fn sysreg_resource_from_dev(dev: &dyn BaseDeviceOps<SysRegAddrRange>) -> Vec<Resource> {
    let range = dev.address_range();

    // 【安全修复】：将系统寄存器地址先转换为 u64 再执行加法，避免转换至开区间时的边界溢出。
    let start = range.start.0 as u64;
    let end = range.end.0 as u64 + 1; // SysRegAddrRange is inclusive, convert to exclusive

    if end > start {
        alloc::vec![Resource::SysReg(start..end)]
    } else {
        alloc::vec![]
    }
}
// ── MMIO adapter ───────────────────────────────────────────────────────────

/// Wraps an `Arc<dyn BaseDeviceOps<GuestPhysAddrRange>>` as a `VirtualDevice`.
#[derive(Debug)]
pub struct LegacyMmioAdapter {
    id: DeviceId,
    name: String,
    inner: AssertSendSync<Arc<dyn BaseDeviceOps<GuestPhysAddrRange>>>,
    resources: Vec<Resource>,
}

impl LegacyMmioAdapter {
    pub fn new(id: DeviceId, inner: Arc<dyn BaseDeviceOps<GuestPhysAddrRange>>) -> Self {
        let name = alloc::format!("{:?}", inner.emu_type());
        let resources = mmio_resource_from_dev(inner.as_ref());
        Self {
            id,
            name,
            inner: AssertSendSync(inner),
            resources,
        }
    }

    /// Access the inner legacy device for type-specific operations (e.g., downcasting).
    pub fn inner(&self) -> &Arc<dyn BaseDeviceOps<GuestPhysAddrRange>> {
        &self.inner.0
    }
}

impl VirtualDevice for LegacyMmioAdapter {
    fn id(&self) -> DeviceId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle_access(&self, bus: BusKind, access: &BusAccess) -> BusResponse {
        let gpa = GuestPhysAddr::from(access.addr() as usize);
        let width = match access.width() {
            AccessWidth::U8 => LegacyWidth::Byte,
            AccessWidth::U16 => LegacyWidth::Word,
            AccessWidth::U32 => LegacyWidth::Dword,
            AccessWidth::U64 => LegacyWidth::Qword,
        };

        match access {
            BusAccess::Read { .. } => match self.inner.0.handle_read(gpa, width) {
                Ok(val) => BusResponse::Success(Some(val as u64)),
                Err(_) => BusResponse::DeviceError {
                    bus,
                    addr: access.addr(),
                    msg: "legacy mmio read error",
                },
            },
            BusAccess::Write { val, .. } => {
                match self.inner.0.handle_write(gpa, width, *val as usize) {
                    Ok(_) => BusResponse::Success(None),
                    Err(_) => BusResponse::DeviceError {
                        bus,
                        addr: access.addr(),
                        msg: "legacy mmio write error",
                    },
                }
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ── SysReg adapter ─────────────────────────────────────────────────────────

/// Wraps an `Arc<dyn BaseDeviceOps<SysRegAddrRange>>` as a `VirtualDevice`.
#[derive(Debug)]
pub struct LegacySysRegAdapter {
    id: DeviceId,
    name: String,
    inner: AssertSendSync<Arc<dyn BaseDeviceOps<SysRegAddrRange>>>,
    resources: Vec<Resource>,
}

impl LegacySysRegAdapter {
    pub fn new(id: DeviceId, inner: Arc<dyn BaseDeviceOps<SysRegAddrRange>>) -> Self {
        let name = format!("sysreg@{:?}", inner.emu_type());
        let resources = sysreg_resource_from_dev(inner.as_ref());
        Self {
            id,
            name,
            inner: AssertSendSync(inner),
            resources,
        }
    }
}

impl VirtualDevice for LegacySysRegAdapter {
    fn id(&self) -> DeviceId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle_access(&self, bus: BusKind, access: &BusAccess) -> BusResponse {
        // SysReg accesses are always 64-bit (Qword).
        let sysreg = SysRegAddr(access.addr() as usize);
        match access {
            BusAccess::Read { .. } => match self.inner.0.handle_read(sysreg, LegacyWidth::Qword) {
                Ok(val) => BusResponse::Success(Some(val as u64)),
                Err(_) => BusResponse::DeviceError {
                    bus,
                    addr: access.addr(),
                    msg: "legacy sysreg read error",
                },
            },
            BusAccess::Write { val, .. } => {
                match self
                    .inner
                    .0
                    .handle_write(sysreg, LegacyWidth::Qword, *val as usize)
                {
                    Ok(_) => BusResponse::Success(None),
                    Err(_) => BusResponse::DeviceError {
                        bus,
                        addr: access.addr(),
                        msg: "legacy sysreg write error",
                    },
                }
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ── Port adapter ───────────────────────────────────────────────────────────

/// Wraps an `Arc<dyn BaseDeviceOps<PortRange>>` as a `VirtualDevice`.
#[derive(Debug)]
pub struct LegacyPortAdapter {
    id: DeviceId,
    name: String,
    inner: AssertSendSync<Arc<dyn BaseDeviceOps<PortRange>>>,
    resources: Vec<Resource>,
}

impl LegacyPortAdapter {
    pub fn new(id: DeviceId, inner: Arc<dyn BaseDeviceOps<PortRange>>) -> Self {
        let name = format!("port@{:x}", inner.emu_type() as u32);
        let resources = port_resource_from_dev(inner.as_ref());
        Self {
            id,
            name,
            inner: AssertSendSync(inner),
            resources,
        }
    }
}

impl VirtualDevice for LegacyPortAdapter {
    fn id(&self) -> DeviceId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle_access(&self, bus: BusKind, access: &BusAccess) -> BusResponse {
        let port = Port(access.addr() as u16);
        let width = match access.width() {
            AccessWidth::U8 => LegacyWidth::Byte,
            AccessWidth::U16 => LegacyWidth::Word,
            AccessWidth::U32 => LegacyWidth::Dword,
            AccessWidth::U64 => LegacyWidth::Qword,
        };
        match access {
            BusAccess::Read { .. } => match self.inner.0.handle_read(port, width) {
                Ok(val) => BusResponse::Success(Some(val as u64)),
                Err(_) => BusResponse::DeviceError {
                    bus,
                    addr: access.addr(),
                    msg: "legacy port read error",
                },
            },
            BusAccess::Write { val, .. } => {
                match self.inner.0.handle_write(port, width, *val as usize) {
                    Ok(_) => BusResponse::Success(None),
                    Err(_) => BusResponse::DeviceError {
                        bus,
                        addr: access.addr(),
                        msg: "legacy port write error",
                    },
                }
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
