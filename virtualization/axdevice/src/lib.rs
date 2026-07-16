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

#![cfg_attr(not(feature = "std"), no_std)]

//! This module is designed for an environment where the standard library is not available (`no_std`).
//!
//! The `alloc` crate is used to enable dynamic memory allocation in the absence of the standard library.
//!
//! The `log` crate is included for logging purposes, with macros being imported globally.
//!
//! Runtime devices are constructed from named resources resolved by AxVM's
//! machine planner and registered in one VM-local device registry.

extern crate alloc;
#[macro_use]
extern crate log;

#[cfg(target_arch = "aarch64")]
mod aarch64_gic;
mod device;
mod error;
mod fw_cfg;
mod interrupt;
#[cfg(any(target_arch = "loongarch64", test))]
mod loongarch_pch_pic;
mod model;
mod range_alloc;
mod registration;
#[cfg(target_arch = "x86_64")]
mod x86;

#[cfg(target_arch = "aarch64")]
pub use aarch64_gic::GicV3DeviceSet;
pub use axdevice_base::{
    AccessWidth, BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps,
    ControllerInputId, Device, InterruptControllerId, InterruptEndpoint, InterruptSourceId,
    InterruptTriggerMode, IrqError, IrqLine, IrqResult, MessageInterruptSink, MmioDeviceAdapter,
    MsiDeviceId, MsiEndpoint, MsiEventId, MsiMessage, Port, PortDeviceAdapter, SysRegAddr,
    SysRegDeviceAdapter, WiredIrqInput, WiredIrqSink,
};
pub use axvm_types::GuestPhysAddr;
pub use device::AxVmDevices;
pub use error::{DeviceManagerError, DeviceManagerResult};
pub use fw_cfg::{FwCfg, FwCfgAcpiFiles, FwCfgConfig, FwCfgMemoryConfig, FwCfgRamRegion};
pub use interrupt::*;
#[cfg(target_arch = "loongarch64")]
pub use loongarch_pch_pic::{LoongArchPchPic, LoongArchPchPicRuntimeOps, PchPicOutputEvent};
pub use model::*;
pub use registration::{DeviceBundle, DeviceRegistration, PollableDeviceOps};
#[cfg(target_arch = "x86_64")]
pub use x86::{
    X86IoApicDevice, X86IoApicDeviceOps, X86IoApicRuntimeOps, X86PitDevice, X86PitDeviceOps,
    X86SerialDeviceOps, X86SerialPortDevice,
};
#[cfg(target_arch = "x86_64")]
pub use x86_vlapic::IoApicInterrupt;
#[cfg(target_arch = "x86_64")]
pub use x86_vlapic::X86SerialBackend;
// pub use virtio_dev::*;
