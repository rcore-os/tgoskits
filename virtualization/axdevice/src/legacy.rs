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

//! Adapters from the existing `BaseDeviceOps` traits into the new device model.

use alloc::{string::String, sync::Arc, vec::Vec};

use axdevice_base::{
    BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps, BusAccess, BusAddress, BusKind,
    BusOp, BusResponse, DeviceCapabilities, DeviceError, DeviceId, DeviceOps, DeviceResult,
    Resource,
};

/// The old single-bus device object carried by a legacy adapter.
pub enum LegacyDeviceInner {
    /// Existing MMIO device implementation.
    Mmio(Arc<dyn BaseMmioDeviceOps>),
    /// Existing port I/O device implementation.
    Pio(Arc<dyn BasePortDeviceOps>),
    /// Existing system register device implementation.
    SysReg(Arc<dyn BaseSysRegDeviceOps>),
}

impl LegacyDeviceInner {
    fn kind(&self) -> BusKind {
        match self {
            Self::Mmio(_) => BusKind::Mmio,
            Self::Pio(_) => BusKind::Pio,
            Self::SysReg(_) => BusKind::SysReg,
        }
    }
}

/// Adapter that exposes an existing `BaseDeviceOps` object as [`DeviceOps`].
pub struct LegacyDeviceAdapter {
    id: DeviceId,
    name: String,
    resources: Vec<Resource>,
    capabilities: DeviceCapabilities,
    inner: LegacyDeviceInner,
}

impl LegacyDeviceAdapter {
    /// Creates an adapter from raw parts.
    pub fn new(
        id: DeviceId,
        name: String,
        resources: Vec<Resource>,
        capabilities: DeviceCapabilities,
        inner: LegacyDeviceInner,
    ) -> Self {
        Self {
            id,
            name,
            resources,
            capabilities,
            inner,
        }
    }

    /// Creates an MMIO legacy adapter.
    pub fn mmio(
        id: DeviceId,
        name: String,
        resources: Vec<Resource>,
        capabilities: DeviceCapabilities,
        device: Arc<dyn BaseMmioDeviceOps>,
    ) -> Self {
        Self::new(
            id,
            name,
            resources,
            capabilities,
            LegacyDeviceInner::Mmio(device),
        )
    }

    /// Creates a port I/O legacy adapter.
    pub fn pio(
        id: DeviceId,
        name: String,
        resources: Vec<Resource>,
        capabilities: DeviceCapabilities,
        device: Arc<dyn BasePortDeviceOps>,
    ) -> Self {
        Self::new(
            id,
            name,
            resources,
            capabilities,
            LegacyDeviceInner::Pio(device),
        )
    }

    /// Creates a system register legacy adapter.
    pub fn sysreg(
        id: DeviceId,
        name: String,
        resources: Vec<Resource>,
        capabilities: DeviceCapabilities,
        device: Arc<dyn BaseSysRegDeviceOps>,
    ) -> Self {
        Self::new(
            id,
            name,
            resources,
            capabilities,
            LegacyDeviceInner::SysReg(device),
        )
    }
}

impl DeviceOps for LegacyDeviceAdapter {
    fn id(&self) -> DeviceId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.capabilities
    }

    fn access(&self, access: BusAccess) -> DeviceResult<BusResponse> {
        if access.kind != access.addr.kind() || access.kind != self.inner.kind() {
            return Err(DeviceError::BusAddressMismatch {
                kind: self.inner.kind(),
                address: access.addr,
            });
        }

        match (&self.inner, access.addr, access.op) {
            (LegacyDeviceInner::Mmio(device), BusAddress::Mmio(addr), BusOp::Read) => device
                .handle_read(addr, access.width)
                .map(|value| BusResponse::Read { value })
                .map_err(DeviceError::from),
            (LegacyDeviceInner::Mmio(device), BusAddress::Mmio(addr), BusOp::Write { value }) => {
                device
                    .handle_write(addr, access.width, value)
                    .map(|()| BusResponse::Write)
                    .map_err(DeviceError::from)
            }
            (LegacyDeviceInner::Pio(device), BusAddress::Pio(port), BusOp::Read) => device
                .handle_read(port, access.width)
                .map(|value| BusResponse::Read { value })
                .map_err(DeviceError::from),
            (LegacyDeviceInner::Pio(device), BusAddress::Pio(port), BusOp::Write { value }) => {
                device
                    .handle_write(port, access.width, value)
                    .map(|()| BusResponse::Write)
                    .map_err(DeviceError::from)
            }
            (LegacyDeviceInner::SysReg(device), BusAddress::SysReg(addr), BusOp::Read) => device
                .handle_read(addr, access.width)
                .map(|value| BusResponse::Read { value })
                .map_err(DeviceError::from),
            (
                LegacyDeviceInner::SysReg(device),
                BusAddress::SysReg(addr),
                BusOp::Write { value },
            ) => device
                .handle_write(addr, access.width, value)
                .map(|()| BusResponse::Write)
                .map_err(DeviceError::from),
            _ => Err(DeviceError::BusAddressMismatch {
                kind: self.inner.kind(),
                address: access.addr,
            }),
        }
    }
}
