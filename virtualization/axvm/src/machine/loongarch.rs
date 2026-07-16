//! LoongArch platform resources outside the interrupt-controller boundary.

use super::AddressRange;

/// PCI host bridge resources exposed by a LoongArch machine profile.
#[derive(Clone, Debug)]
pub struct LoongArchPciProfile {
    ecam: AddressRange,
    mmio: AddressRange,
    io: AddressRange,
    intx_base: u8,
}

impl LoongArchPciProfile {
    /// Creates a PCI host bridge profile.
    pub const fn new(
        ecam: AddressRange,
        mmio: AddressRange,
        io: AddressRange,
        intx_base: u8,
    ) -> Self {
        Self {
            ecam,
            mmio,
            io,
            intx_base,
        }
    }
}

/// Reset and poweroff registers exposed by the LoongArch virtual platform.
#[derive(Clone, Copy, Debug)]
pub struct LoongArchPowerProfile {
    reset_register: u64,
    reset_value: u8,
    poweroff_register: u64,
    poweroff_value: u8,
    sleep_control_register: u64,
    sleep_status_register: u64,
}

impl LoongArchPowerProfile {
    /// Creates a hardware-reduced ACPI power-control profile.
    pub const fn new(
        reset_register: u64,
        reset_value: u8,
        poweroff_register: u64,
        poweroff_value: u8,
        sleep_control_register: u64,
        sleep_status_register: u64,
    ) -> Self {
        Self {
            reset_register,
            reset_value,
            poweroff_register,
            poweroff_value,
            sleep_control_register,
            sleep_status_register,
        }
    }
}

/// RTC and flash resources consumed by LoongArch UEFI firmware.
#[derive(Clone, Debug)]
pub struct LoongArchFirmwareDevicesProfile {
    rtc: AddressRange,
    rtc_interrupt: u32,
    flash_banks: [AddressRange; 2],
    flash_bank_width: u32,
}

impl LoongArchFirmwareDevicesProfile {
    /// Creates firmware-only RTC and flash resource descriptors.
    pub const fn new(
        rtc: AddressRange,
        rtc_interrupt: u32,
        flash_banks: [AddressRange; 2],
        flash_bank_width: u32,
    ) -> Self {
        Self {
            rtc,
            rtc_interrupt,
            flash_banks,
            flash_bank_width,
        }
    }
}

/// Non-controller resources required by LoongArch firmware generation.
#[derive(Clone, Debug)]
pub struct LoongArchPlatformProfile {
    fw_cfg: AddressRange,
    pci: LoongArchPciProfile,
    power: LoongArchPowerProfile,
    firmware_devices: LoongArchFirmwareDevicesProfile,
}

impl LoongArchPlatformProfile {
    /// Creates a complete LoongArch firmware-facing platform profile.
    pub const fn new(
        fw_cfg: AddressRange,
        pci: LoongArchPciProfile,
        power: LoongArchPowerProfile,
        firmware_devices: LoongArchFirmwareDevicesProfile,
    ) -> Self {
        Self {
            fw_cfg,
            pci,
            power,
            firmware_devices,
        }
    }

    pub(crate) fn resolve(&self) -> LoongArchPlatformPlan {
        LoongArchPlatformPlan {
            fw_cfg: self.fw_cfg,
            pci: LoongArchPciPlan {
                ecam: self.pci.ecam,
                mmio: self.pci.mmio,
                io: self.pci.io,
                intx_base: self.pci.intx_base,
            },
            power: LoongArchPowerPlan {
                reset_register: self.power.reset_register,
                reset_value: self.power.reset_value,
                poweroff_register: self.power.poweroff_register,
                poweroff_value: self.power.poweroff_value,
                sleep_control_register: self.power.sleep_control_register,
                sleep_status_register: self.power.sleep_status_register,
            },
            firmware_devices: LoongArchFirmwareDevicesPlan {
                rtc: self.firmware_devices.rtc,
                rtc_interrupt: self.firmware_devices.rtc_interrupt,
                flash_banks: self.firmware_devices.flash_banks,
                flash_bank_width: self.firmware_devices.flash_bank_width,
            },
        }
    }
}

/// Final LoongArch PCI resources consumed by ACPI generation.
#[derive(Clone, Debug)]
pub struct LoongArchPciPlan {
    ecam: AddressRange,
    mmio: AddressRange,
    io: AddressRange,
    intx_base: u8,
}

impl LoongArchPciPlan {
    /// Returns the PCI ECAM aperture.
    pub const fn ecam(&self) -> AddressRange {
        self.ecam
    }

    /// Returns the PCI MMIO aperture.
    pub const fn mmio(&self) -> AddressRange {
        self.mmio
    }

    /// Returns the PCI I/O aperture represented in MMIO space.
    pub const fn io(&self) -> AddressRange {
        self.io
    }

    /// Returns the first PCH-PIC input used by PCI INTx routing.
    pub const fn intx_base(&self) -> u8 {
        self.intx_base
    }
}

/// Final hardware-reduced ACPI power-control resources.
#[derive(Clone, Copy, Debug)]
pub struct LoongArchPowerPlan {
    reset_register: u64,
    reset_value: u8,
    poweroff_register: u64,
    poweroff_value: u8,
    sleep_control_register: u64,
    sleep_status_register: u64,
}

impl LoongArchPowerPlan {
    /// Returns the reset register address.
    pub const fn reset_register(self) -> u64 {
        self.reset_register
    }

    /// Returns the value that requests reset.
    pub const fn reset_value(self) -> u8 {
        self.reset_value
    }

    /// Returns the poweroff register address.
    pub const fn poweroff_register(self) -> u64 {
        self.poweroff_register
    }

    /// Returns the value that requests poweroff.
    pub const fn poweroff_value(self) -> u8 {
        self.poweroff_value
    }

    /// Returns the hardware-reduced sleep control register.
    pub const fn sleep_control_register(self) -> u64 {
        self.sleep_control_register
    }

    /// Returns the hardware-reduced sleep status register.
    pub const fn sleep_status_register(self) -> u64 {
        self.sleep_status_register
    }
}

/// Final firmware-only LoongArch RTC and flash resources.
#[derive(Clone, Copy, Debug)]
pub struct LoongArchFirmwareDevicesPlan {
    rtc: AddressRange,
    rtc_interrupt: u32,
    flash_banks: [AddressRange; 2],
    flash_bank_width: u32,
}

impl LoongArchFirmwareDevicesPlan {
    /// Returns the RTC MMIO aperture.
    pub const fn rtc(self) -> AddressRange {
        self.rtc
    }

    /// Returns the RTC PCH-PIC input.
    pub const fn rtc_interrupt(self) -> u32 {
        self.rtc_interrupt
    }

    /// Returns the two virtual flash banks.
    pub const fn flash_banks(self) -> [AddressRange; 2] {
        self.flash_banks
    }

    /// Returns the flash bank bus width.
    pub const fn flash_bank_width(self) -> u32 {
        self.flash_bank_width
    }
}

/// Final LoongArch platform resources consumed by runtime and firmware.
#[derive(Clone, Debug)]
pub struct LoongArchPlatformPlan {
    fw_cfg: AddressRange,
    pci: LoongArchPciPlan,
    power: LoongArchPowerPlan,
    firmware_devices: LoongArchFirmwareDevicesPlan,
}

impl LoongArchPlatformPlan {
    /// Returns the fw_cfg MMIO aperture.
    pub const fn fw_cfg(&self) -> AddressRange {
        self.fw_cfg
    }

    /// Returns the planned PCI host bridge.
    pub const fn pci(&self) -> &LoongArchPciPlan {
        &self.pci
    }

    /// Returns planned reset and poweroff registers.
    pub const fn power(&self) -> LoongArchPowerPlan {
        self.power
    }

    /// Returns firmware-only RTC and flash resources.
    pub const fn firmware_devices(&self) -> LoongArchFirmwareDevicesPlan {
        self.firmware_devices
    }
}
