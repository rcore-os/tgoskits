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

#![no_std]

//! This module is designed for an environment where the standard library is not available (`no_std`).
//!
//! The `alloc` crate is used to enable dynamic memory allocation in the absence of the standard library.
//!
//! The `log` crate is included for logging purposes, with macros being imported globally.
//!
//! The module is structured into two main parts: `config` and `device`, which manage the configuration and handling of AxVm devices respectively.

extern crate alloc;
#[macro_use]
extern crate log;

mod config;
mod device;
mod factory;
mod legacy;
mod range_alloc;
mod registry;

pub use axdevice_base::{
    AccessWidth, BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps,
    BusAccess, BusAddress, BusKind, BusOp, BusResponse, DeviceBuildContext, DeviceCapabilities,
    DeviceError, DeviceFactory, DeviceFactoryRegister, DeviceId, DeviceMeta, DeviceOps,
    DeviceResult, IrqLine, IrqSink, IrqTarget, MsiMessage, PciBarKind, Port, Resource, SysRegAddr,
};
pub use axvm_types::GuestPhysAddr;
pub use config::AxVmDeviceConfig;
pub use device::{AxEmuDevices, AxVmDevices};
pub use factory::DeviceFactoryCatalog;
pub use legacy::{LegacyDeviceAdapter, LegacyDeviceInner};
pub use registry::DeviceRegistry;
#[cfg(target_arch = "x86_64")]
pub use x86_vlapic::IoApicInterrupt;
// pub use virtio_dev::*;
