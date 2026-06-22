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

mod adapter;
mod config;
mod device;
mod factory;
mod range_alloc;
mod registration;

#[cfg(target_arch = "aarch64")]
pub use adapter::create_vtimer_devices;
pub use axdevice_base::{
    AccessWidth, BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps, Device,
    MmioDeviceAdapter, Port, PortDeviceAdapter, SysRegAddr, SysRegDeviceAdapter,
};
pub use axvm_types::GuestPhysAddr;
pub use config::AxVmDeviceConfig;
pub use device::AxVmDevices;
pub use factory::{
    DeviceBuildContext, DeviceFactory, DeviceFactoryRegistry, IrqResolver,
    register_builtin_factories,
};
pub use registration::{DeviceBundle, DeviceRegistration, PollableDeviceOps};
#[cfg(target_arch = "x86_64")]
pub use x86_vlapic::IoApicInterrupt;
// pub use virtio_dev::*;
