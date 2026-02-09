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

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use range_alloc_arceos::RangeAllocator;
use spin::Mutex;

use axaddrspace::{
    GuestPhysAddr, GuestPhysAddrRange,
    device::{AccessWidth, DeviceAddrRange, Port, PortRange, SysRegAddr, SysRegAddrRange},
};
use axdevice_base::{BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps};
use axerrno::{AxResult, ax_err};
use axvmconfig::{EmulatedDeviceConfig, EmulatedDeviceType};
use memory_addr::is_aligned_4k;

use crate::AxVmDeviceConfig;

#[cfg(target_arch = "aarch64")]
use arm_vgic::Vgic;
#[cfg(target_arch = "aarch64")]
use memory_addr::PhysAddr;

#[cfg(target_arch = "riscv64")]
use riscv_vplic::VPlicGlobal;

/// A set of emulated device types that can be accessed by a specific address range type.
pub struct AxEmuDevices<R: DeviceAddrRange> {
    emu_devices: Vec<Arc<dyn BaseDeviceOps<R>>>,
}

impl<R: DeviceAddrRange + 'static> AxEmuDevices<R> {
    /// Creates a new [`AxEmuDevices`] instance.
    pub fn new() -> Self {
        Self {
            emu_devices: Vec::new(),
        }
    }

    /// Adds a device to the set.
    pub fn add_dev(&mut self, dev: Arc<dyn BaseDeviceOps<R>>) {
        self.emu_devices.push(dev);
    }

    // pub fn remove_dev(&mut self, ...)
    //
    // `remove_dev` seems to need something like `downcast-rs` to make sense. As it's not likely to
    // be able to have a proper predicate to remove a device from the list without knowing the
    // concrete type of the device.

    /// Find a device by address.
    pub fn find_dev(&self, addr: R::Addr) -> Option<Arc<dyn BaseDeviceOps<R>>> {
        self.emu_devices
            .iter()
            .find(|&dev| dev.address_range().contains(addr))
            .cloned()
    }

    /// Iterates over the devices in the set.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn BaseDeviceOps<R>>> {
        self.emu_devices.iter()
    }

    /// Iterates over the devices in the set mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Arc<dyn BaseDeviceOps<R>>> {
        self.emu_devices.iter_mut()
    }
}

type AxEmuMmioDevices = AxEmuDevices<GuestPhysAddrRange>;
type AxEmuSysRegDevices = AxEmuDevices<SysRegAddrRange>;
type AxEmuPortDevices = AxEmuDevices<PortRange>;

/// represent A vm own devices
pub struct AxVmDevices {
    /// emu devices
    emu_mmio_devices: AxEmuMmioDevices,
    emu_sys_reg_devices: AxEmuSysRegDevices,
    emu_port_devices: AxEmuPortDevices,
    /// IVC channel range allocator
    ivc_channel: Option<Mutex<RangeAllocator<usize>>>,
}

#[inline]
fn log_device_io(
    addr_type: &'static str,
    addr: impl core::fmt::LowerHex,
    addr_range: impl core::fmt::LowerHex,
    read: bool,
    width: AccessWidth,
) {
    let rw = if read { "read" } else { "write" };
    trace!("emu_device {rw}: {addr_type} {addr:#x} in range {addr_range:#x} with width {width:?}")
}

#[inline]
fn panic_device_not_found(
    addr_type: &'static str,
    addr: impl core::fmt::LowerHex,
    read: bool,
    width: AccessWidth,
) -> ! {
    let rw = if read { "read" } else { "write" };
    error!(
        "emu_device {rw} failed: device not found for {addr_type} {addr:#x} with width {width:?}"
    );
    panic!("emu_device not found");
}

/// The implemention for AxVmDevices
impl AxVmDevices {
    /// According AxVmDeviceConfig to init the AxVmDevices
    pub fn new(config: AxVmDeviceConfig) -> Self {
        let mut this = Self {
            emu_mmio_devices: AxEmuMmioDevices::new(),
            emu_sys_reg_devices: AxEmuSysRegDevices::new(),
            emu_port_devices: AxEmuPortDevices::new(),
            ivc_channel: None,
        };

        Self::init(&mut this, &config.emu_configs);
        this
    }

    /// According the emu_configs to init every  specific device
    fn init(this: &mut Self, emu_configs: &Vec<EmulatedDeviceConfig>) {
        for config in emu_configs {
            match config.emu_type {
                EmulatedDeviceType::InterruptController => {
                    #[cfg(target_arch = "aarch64")]
                    {
                        this.add_mmio_dev(Arc::new(Vgic::new()));
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::GPPTRedistributor => {
                    #[cfg(target_arch = "aarch64")]
                    {
                        const GPPT_GICR_ARG_ERR_MSG: &str =
                            "expect 3 args for gppt redistributor (cpu_num, stride, pcpu_id)";

                        let cpu_num = config
                            .cfg_list
                            .first()
                            .copied()
                            .expect(GPPT_GICR_ARG_ERR_MSG);
                        let stride = config
                            .cfg_list
                            .get(1)
                            .copied()
                            .expect(GPPT_GICR_ARG_ERR_MSG);
                        let pcpu_id = config
                            .cfg_list
                            .get(2)
                            .copied()
                            .expect(GPPT_GICR_ARG_ERR_MSG);

                        for i in 0..cpu_num {
                            let addr = config.base_gpa + i * stride;
                            let size = config.length;
                            #[allow(clippy::arc_with_non_send_sync)]
                            this.add_mmio_dev(Arc::new(arm_vgic::v3::vgicr::VGicR::new(
                                addr.into(),
                                Some(size),
                                pcpu_id + i,
                            )));

                            info!(
                                "GPPT Redistributor initialized for vCPU {i} with base GPA {addr:#x} and length {size:#x}"
                            );
                        }
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::GPPTDistributor => {
                    #[cfg(target_arch = "aarch64")]
                    {
                        #[allow(clippy::arc_with_non_send_sync)]
                        this.add_mmio_dev(Arc::new(arm_vgic::v3::vgicd::VGicD::new(
                            config.base_gpa.into(),
                            Some(config.length),
                        )));

                        info!(
                            "GPPT Distributor initialized with base GPA {base_gpa:#x} and length {length:#x}",
                            base_gpa = config.base_gpa,
                            length = config.length
                        );
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::GPPTITS => {
                    #[cfg(target_arch = "aarch64")]
                    {
                        let host_gits_base = config
                            .cfg_list
                            .first()
                            .copied()
                            .map(PhysAddr::from_usize)
                            .expect("expect 1 arg for gppt its (host_gits_base)");

                        #[allow(clippy::arc_with_non_send_sync)]
                        this.add_mmio_dev(Arc::new(arm_vgic::v3::gits::Gits::new(
                            config.base_gpa.into(),
                            Some(config.length),
                            host_gits_base,
                            false,
                        )));

                        info!(
                            "GPPT ITS initialized with base GPA {base_gpa:#x} and length {length:#x}, host GITS base {host_gits_base:#x}",
                            base_gpa = config.base_gpa,
                            length = config.length,
                            host_gits_base = host_gits_base
                        );
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::PPPTGlobal => {
                    #[cfg(target_arch = "riscv64")]
                    {
                        let context_num = config
                            .cfg_list
                            .first()
                            .copied()
                            .expect("expect 1 arg for pppt global (context_num)");
                        this.add_mmio_dev(Arc::new(VPlicGlobal::new(
                            config.base_gpa.into(),
                            Some(config.length),
                            context_num, // Here only 1 core and should be cpu0
                        )));
                        // PLIC Partial Passthrough Global.
                        info!(
                            "Partial PLIC Passthrough Global initialized with base GPA {:#x} and length {:#x}",
                            config.base_gpa, config.length
                        );
                    }
                    #[cfg(not(target_arch = "riscv64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::IVCChannel => {
                    if this.ivc_channel.is_none() {
                        // Initialize the IVC channel range allocator
                        this.ivc_channel = Some(Mutex::new(RangeAllocator::new(Range {
                            start: config.base_gpa,
                            end: config.base_gpa + config.length,
                        })));
                        info!(
                            "IVCChannel initialized with base GPA {base_gpa:#x} and length {length:#x}",
                            base_gpa = config.base_gpa,
                            length = config.length
                        );
                    } else {
                        warn!("IVCChannel already initialized, ignoring additional config");
                    }
                }
                _ => {
                    warn!(
                        "Emulated device {}'s type {:?} is not supported yet",
                        config.name, config.emu_type
                    );
                }
            }
        }
    }

    /// Allocates an IVC (Inter-VM Communication) channel of the specified size.
    pub fn alloc_ivc_channel(&self, size: usize) -> AxResult<GuestPhysAddr> {
        if size == 0 {
            return ax_err!(InvalidInput, "Size must be greater than 0");
        }
        if !is_aligned_4k(size) {
            return ax_err!(InvalidInput, "Size must be aligned to 4K");
        }

        if let Some(allocator) = &self.ivc_channel {
            allocator
                .lock()
                .allocate_range(size)
                .map_err(|e| {
                    warn!("Failed to allocate IVC channel range: {e:x?}");
                    axerrno::ax_err_type!(NoMemory, "IVC channel allocation failed")
                })
                .map(|range| {
                    debug!("Allocated IVC channel range: {range:x?}");
                    GuestPhysAddr::from_usize(range.start)
                })
        } else {
            ax_err!(InvalidInput, "IVC channel not exists")
        }
    }

    /// Releases an IVC channel at the specified address and size.
    pub fn release_ivc_channel(&self, addr: GuestPhysAddr, size: usize) -> AxResult {
        if size == 0 {
            return ax_err!(InvalidInput, "Size must be greater than 0");
        }
        if !is_aligned_4k(size) {
            return ax_err!(InvalidInput, "Size must be aligned to 4K");
        }

        if let Some(allocator) = &self.ivc_channel {
            allocator
                .lock()
                .free_range(addr.as_usize()..addr.as_usize() + size);
            Ok(())
        } else {
            ax_err!(InvalidInput, "IVC channel not exists")
        }
    }

    /// Add a MMIO device to the device list
    pub fn add_mmio_dev(&mut self, dev: Arc<dyn BaseMmioDeviceOps>) {
        self.emu_mmio_devices.add_dev(dev);
    }

    /// Add a system register device to the device list
    pub fn add_sys_reg_dev(&mut self, dev: Arc<dyn BaseSysRegDeviceOps>) {
        self.emu_sys_reg_devices.add_dev(dev);
    }

    /// Add a port device to the device list
    pub fn add_port_dev(&mut self, dev: Arc<dyn BasePortDeviceOps>) {
        self.emu_port_devices.add_dev(dev);
    }

    /// Iterates over the MMIO devices in the set.
    pub fn iter_mmio_dev(&self) -> impl Iterator<Item = &Arc<dyn BaseMmioDeviceOps>> {
        self.emu_mmio_devices.iter()
    }

    /// Iterates over the system register devices in the set.
    pub fn iter_sys_reg_dev(&self) -> impl Iterator<Item = &Arc<dyn BaseSysRegDeviceOps>> {
        self.emu_sys_reg_devices.iter()
    }

    /// Iterates over the port devices in the set.
    pub fn iter_port_dev(&self) -> impl Iterator<Item = &Arc<dyn BasePortDeviceOps>> {
        self.emu_port_devices.iter()
    }

    /// Iterates over the MMIO devices in the set.
    pub fn iter_mut_mmio_dev(&mut self) -> impl Iterator<Item = &mut Arc<dyn BaseMmioDeviceOps>> {
        self.emu_mmio_devices.iter_mut()
    }

    /// Iterates over the system register devices in the set.
    pub fn iter_mut_sys_reg_dev(
        &mut self,
    ) -> impl Iterator<Item = &mut Arc<dyn BaseSysRegDeviceOps>> {
        self.emu_sys_reg_devices.iter_mut()
    }

    /// Iterates over the port devices in the set.
    pub fn iter_mut_port_dev(&mut self) -> impl Iterator<Item = &mut Arc<dyn BasePortDeviceOps>> {
        self.emu_port_devices.iter_mut()
    }

    /// Find specific MMIO device by ipa
    pub fn find_mmio_dev(&self, ipa: GuestPhysAddr) -> Option<Arc<dyn BaseMmioDeviceOps>> {
        self.emu_mmio_devices.find_dev(ipa)
    }

    /// Find specific system register device by ipa
    pub fn find_sys_reg_dev(
        &self,
        sys_reg_addr: SysRegAddr,
    ) -> Option<Arc<dyn BaseSysRegDeviceOps>> {
        self.emu_sys_reg_devices.find_dev(sys_reg_addr)
    }

    /// Find specific port device by port number
    pub fn find_port_dev(&self, port: Port) -> Option<Arc<dyn BasePortDeviceOps>> {
        self.emu_port_devices.find_dev(port)
    }

    /// Handle the MMIO read by GuestPhysAddr and data width, return the value of the guest want to read
    pub fn handle_mmio_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_mmio_dev(addr) {
            log_device_io("mmio", addr, emu_dev.address_range(), true, width);

            return emu_dev.handle_read(addr, width);
        }
        panic_device_not_found("mmio", addr, true, width);
    }

    /// Handle the MMIO write by GuestPhysAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> AxResult {
        if let Some(emu_dev) = self.find_mmio_dev(addr) {
            log_device_io("mmio", addr, emu_dev.address_range(), false, width);

            return emu_dev.handle_write(addr, width, val);
        }
        panic_device_not_found("mmio", addr, false, width);
    }

    /// Handle the system register read by SysRegAddr and data width, return the value of the guest want to read
    pub fn handle_sys_reg_read(&self, addr: SysRegAddr, width: AccessWidth) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_sys_reg_dev(addr) {
            log_device_io("sys_reg", addr.0, emu_dev.address_range(), true, width);

            return emu_dev.handle_read(addr, width);
        }
        panic_device_not_found("sys_reg", addr, true, width);
    }

    /// Handle the system register write by SysRegAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_sys_reg_write(
        &self,
        addr: SysRegAddr,
        width: AccessWidth,
        val: usize,
    ) -> AxResult {
        if let Some(emu_dev) = self.find_sys_reg_dev(addr) {
            log_device_io("sys_reg", addr.0, emu_dev.address_range(), false, width);

            return emu_dev.handle_write(addr, width, val);
        }
        panic_device_not_found("sys_reg", addr, false, width);
    }

    /// Handle the port read by port number and data width, return the value of the guest want to read
    pub fn handle_port_read(&self, port: Port, width: AccessWidth) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_port_dev(port) {
            log_device_io("port", port.0, emu_dev.address_range(), true, width);

            return emu_dev.handle_read(port, width);
        }
        panic_device_not_found("port", port, true, width);
    }

    /// Handle the port write by port number, data width and the value need to write, call specific device to write the value
    pub fn handle_port_write(&self, port: Port, width: AccessWidth, val: usize) -> AxResult {
        if let Some(emu_dev) = self.find_port_dev(port) {
            log_device_io("port", port.0, emu_dev.address_range(), false, width);

            return emu_dev.handle_write(port, width, val);
        }
        panic_device_not_found("port", port, false, width);
    }
}
