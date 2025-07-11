use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use range_alloc::RangeAllocator;
use spin::Mutex;

use axaddrspace::{
    GuestPhysAddr, GuestPhysAddrRange,
    device::{AccessWidth, DeviceAddrRange, Port, PortRange, SysRegAddr, SysRegAddrRange},
};
use axdevice_base::{
    BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps, EmuDeviceType,
};
use axerrno::{AxResult, ax_err};
use axvmconfig::EmulatedDeviceConfig;
use memory_addr::is_aligned_4k;

use crate::AxVmDeviceConfig;

#[cfg(target_arch = "aarch64")]
use arm_vgic::Vgic;

/// A set of emulated device types that can be accessed by a specific address range type.
pub struct AxEmuDevices<R: DeviceAddrRange> {
    emu_devices: Vec<Arc<dyn BaseDeviceOps<R>>>,
}

impl<R: DeviceAddrRange> AxEmuDevices<R> {
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
    trace!(
        "emu_device {}: {} {:#x} in range {:#x} with width {:?}",
        rw, addr_type, addr, addr_range, width
    )
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
        "emu_device {} failed: device not found for {} {:#x} with width {:?}",
        rw, addr_type, addr, width
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
            match EmuDeviceType::from_usize(config.emu_type) {
                EmuDeviceType::EmuDeviceTInterruptController => {
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
                EmuDeviceType::EmuDeviceTIVCChannel => {
                    if this.ivc_channel.is_none() {
                        // Initialize the IVC channel range allocator
                        this.ivc_channel = Some(Mutex::new(RangeAllocator::new(Range {
                            start: config.base_gpa,
                            end: config.base_gpa + config.length,
                        })));
                        info!(
                            "IVCChannel initialized with base GPA {:#x} and length {:#x}",
                            config.base_gpa, config.length
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
                    warn!("Failed to allocate IVC channel range: {:x?}", e);
                    axerrno::ax_err_type!(NoMemory, "IVC channel allocation failed")
                })
                .map(|range| {
                    debug!("Allocated IVC channel range: {:x?}", range);
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
