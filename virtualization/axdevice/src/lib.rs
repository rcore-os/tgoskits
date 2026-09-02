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
#[cfg(test)]
extern crate std;
#[macro_use]
extern crate log;

mod build_context;
mod builder;
mod device;
mod error;
mod fw_cfg;
mod graph;
mod interrupt;
mod model;
mod pci;
// Keep the LoongArch-only implementation out of other production targets, but
// compile its unit tests on the host so output-port behavior is covered by CI.
#[cfg(any(target_arch = "loongarch64", test))]
#[cfg_attr(test, allow(dead_code))]
mod loongarch_pch_pic;
mod registration;
mod resources;
mod runtime_resources;
mod serial;
mod service;
#[cfg(target_arch = "x86_64")]
mod x86;

pub use axdevice_base::{AccessWidth, Device, Port, SysRegAddr};
pub use axvm_types::GuestPhysAddr;
pub use build_context::{DeviceBuildContext, MsiEndpointRange};
pub use builder::DeviceRuntimeBuilder;
pub use device::{
    DeviceRuntime, RuntimeAccessPorts, StopAccessPort, TimerAccessPort, WakeAccessPort,
};
pub use error::{DeviceManagerError, DeviceManagerResult};
pub use fw_cfg::{
    FwCfg, FwCfgAcpiBlobs, FwCfgBuildConfig, FwCfgDeviceFactory, FwCfgDmaDevice,
    FwCfgKernelPayload, FwCfgPayloadConfig, FwCfgPayloadFactory, FwCfgPayloadSlot, FwCfgPioDevice,
    FwCfgPlatformConfig, FwCfgRamRegion,
};
pub use graph::{
    DeclaredDeviceGraph, DeviceFirmwareBinding, DeviceGraphBuilder, DeviceGraphError, DeviceNodeId,
    DeviceNodeKind, DeviceNodeSpec, HostPassthroughMapping, ResolvedDeviceGraph,
    ResolvedDeviceNode,
};
pub use interrupt::{ControllerRegistration, InterruptRegistrationError};
#[cfg(target_arch = "loongarch64")]
// Reusable LoongArch device models. These are target-gated device packages,
// not part of the architecture-neutral framework core.
pub use loongarch_pch_pic::{
    LoongArchInterruptDomainFactory, LoongArchPchPic, LoongArchPchPicFactory, PchPicOutputEvent,
    PchPicOutputPort, PchPicOutputPortKey, PchPicOutputSink,
};
pub use model::{
    AcpiContributionSpec, AcpiDeviceSpec, DeviceFirmwareProperty, DeviceFirmwareSpec, DeviceModel,
    FdtContributionSpec, FdtNodeSpec,
};
pub use pci::{
    ConfigOffset, PCI_BUS_ZERO_ECAM_SIZE, PciBarAccess, PciBarDecodePolicy, PciBarIndex,
    PciBarRoute, PciBdf, PciClass, PciEcamFrontend, PciEndpointIdentity, PciError, PciFunction,
    PciFunctionRequirement, PciFunctionSpec, PciHostKey, PciHostProvider, PciMemoryBar,
    PciMmioApertureDevice, PciResult, PciRootBinding, PciRootBindingKey, PciRootState,
    PciRootStateLifecycle, PciSegment, ResolvedPciBar, ResolvedPciFunction, ResolvedPciTopology,
};
pub(crate) use pci::{PciTopologyBuilder, all_ones, read_bytes};
pub use registration::{
    DeviceBundle, DeviceLifecycle, DeviceRegistration, DmaPollableDeviceOps, PollableDeviceOps,
};
pub use resources::{
    DevicePlanRequest, DeviceRequirement, DeviceRequirements, MsiResourceRequest,
    ResolvedDeviceResources, ResolvedMsi, ResolvedWiredIrq, ResourceClaimSet, ResourceLease,
    ResourceNamespace, ResourcePlanningError, ResourcePools, ResourceRequest, ResourceSlot,
    VmResourcePlan, VmResourcePlanner,
};
pub use serial::{
    NullSerialBackend, NullSerialBackendFactory, Pl011, SerialBackend, SerialBackendFactory,
    Uart16550, build_16550_mmio, build_16550_port, build_pl011_mmio,
};
pub use service::{DeviceServices, ServiceCardinality, ServiceKey};
#[cfg(target_arch = "x86_64")]
// Reusable x86 device models and narrow typed services. These are target-gated
// device packages, not part of the architecture-neutral framework core.
pub use x86::{
    PciMemoryApertureDevice, PciRootLifecycle, X86AcpiPmTimerDevice, X86CmosDevice,
    X86InterruptDomainKey, X86InterruptDomainOps, X86IoApicDevice, X86IoApicDeviceOps,
    X86IoApicServiceKey, X86MonotonicNanos, X86PciConfigFrontend, X86PicDevice, X86PicDeviceOps,
    X86PicServiceKey, X86PitDevice,
};
#[cfg(target_arch = "x86_64")]
pub use x86_vlapic::IoApicInterrupt;
// pub use virtio_dev::*;
