#![no_std]
//! Portable register core for the Intel iTCO watchdog.
//!
//! The core deliberately does not discover PCI devices, map resources, parse
//! boot arguments, register a watchdog node, or arm the timer during creation.
//! The owning kernel must explicitly call Tco::start after its own opt-in
//! and lifetime policy has authorized a watchdog that can reset the machine.

mod regs;

use thiserror::Error;

/// iTCO generation admitted by this implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Version {
    /// Intel ICH9 TCO v2 with NO_REBOOT in the chipset GCS register.
    Ich9V2,
    /// Intel device 8086:54a3 using the TCO v6 control-register NO_REBOOT bit.
    TcoV6,
}

impl Version {
    /// Numeric generation reported to watchdog consumers.
    pub const fn number(self) -> u8 {
        match self {
            Self::Ich9V2 => 2,
            Self::TcoV6 => 6,
        }
    }
}

/// An admitted Intel PCI function. Only identify can create this value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupportedDevice {
    vendor_id: u16,
    device_id: u16,
    version: Version,
}

impl SupportedDevice {
    /// PCI vendor identifier.
    pub const fn vendor_id(self) -> u16 {
        self.vendor_id
    }

    /// PCI device identifier.
    pub const fn device_id(self) -> u16 {
        self.device_id
    }

    /// Supported iTCO generation.
    pub const fn version(self) -> Version {
        self.version
    }
}

/// Admits only controller IDs whose iTCO register layout is implemented.
pub const fn identify(vendor_id: u16, device_id: u16) -> Option<SupportedDevice> {
    let version = match (vendor_id, device_id) {
        (0x8086, 0x2918) => Version::Ich9V2,
        (0x8086, 0x54a3) => Version::TcoV6,
        _ => return None,
    };
    Some(SupportedDevice {
        vendor_id,
        device_id,
        version,
    })
}

/// Validated chipset resources produced by the platform discovery layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Resources {
    device: SupportedDevice,
    tco_port: u16,
    smi_enable_port: Option<u16>,
    no_reboot_gcs: Option<u64>,
}

impl Resources {
    /// Computes the supported resource layout from chipset configuration values.
    ///
    /// base and control are raw chipset values read by the platform layer:
    /// for v6 they describe the TCO port window; for ICH9 they describe the PM
    /// base/enable and RCBA configuration. Resource mapping and PCI config-space
    /// reads remain outside this portable core.
    pub fn from_chipset_config(device: SupportedDevice, base: u32, control: u32) -> Result<Self> {
        match device.version {
            Version::TcoV6 => {
                let port = base & !1;
                if control & 0x100 == 0 || port == 0 || port > 0xffe0 || port & 0x1f != 0 {
                    return Err(Error::InvalidResources);
                }
                Ok(Self {
                    device,
                    tco_port: port as u16,
                    smi_enable_port: None,
                    no_reboot_gcs: None,
                })
            }
            Version::Ich9V2 => {
                let pm_base = base & 0xff80;
                let rcba = u64::from(control & 0xffff_c000);
                if pm_base == 0 || control & 1 == 0 || rcba == 0 {
                    return Err(Error::InvalidResources);
                }
                Ok(Self {
                    device,
                    tco_port: (pm_base + 0x60) as u16,
                    smi_enable_port: Some((pm_base + 0x30) as u16),
                    no_reboot_gcs: Some(rcba + 0x3410),
                })
            }
        }
    }

    /// The PCI function for which these resources were decoded.
    pub const fn device(self) -> SupportedDevice {
        self.device
    }

    /// TCO I/O-port base address.
    pub const fn tco_port(self) -> u16 {
        self.tco_port
    }

    /// SMI enable register port for generations that require it.
    pub const fn smi_enable_port(self) -> Option<u16> {
        self.smi_enable_port
    }

    /// NO_REBOOT GCS physical address for generations that use it.
    pub const fn no_reboot_gcs(self) -> Option<u64> {
        self.no_reboot_gcs
    }
}

/// Narrow platform capability for legacy port I/O and chipset MMIO.
///
/// The adapter owns I/O permissions and any GCS mapping. read_gcs32 and
/// write_gcs32 must access the same live, mapped chipset register. No method
/// is called from interrupt context by this core.
pub trait RegisterIo {
    /// Reads a 16-bit register at an I/O-port address.
    fn read_port16(&mut self, port: u16) -> u16;
    /// Writes a 16-bit register at an I/O-port address.
    fn write_port16(&mut self, port: u16, value: u16);
    /// Reads a 32-bit I/O-port register such as SMI_EN.
    fn read_port32(&mut self, port: u16) -> u32;
    /// Writes a 32-bit I/O-port register such as SMI_EN.
    fn write_port32(&mut self, port: u16, value: u32);
    /// Reads a 32-bit GCS register at a physical MMIO address.
    fn read_gcs32(&mut self, address: u64) -> u32;
    /// Writes a 32-bit GCS register at a physical MMIO address.
    fn write_gcs32(&mut self, address: u64, value: u32);
}

/// Watchdog register state or resource failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum Error {
    /// The supplied platform resource values are malformed or disabled.
    #[error("invalid or disabled iTCO resources")]
    InvalidResources,
    /// The requested timeout is outside the supported iTCO v2/v6 range.
    #[error("iTCO timeout is outside the supported range")]
    InvalidTimeout,
    /// Hardware refused a write or is locked by firmware/platform policy.
    #[error("iTCO register write did not take effect")]
    Locked,
    /// A timer register readback did not match the requested value.
    #[error("iTCO timer register readback failed")]
    Io,
    /// The controller is not running or caller requested an invalid transition.
    #[error("invalid iTCO watchdog state")]
    BadState,
    /// The timer was already counting; use explicit takeover rather than rearming it.
    #[error("iTCO watchdog is already running")]
    AlreadyRunning,
}

/// Result type for the iTCO core.
pub type Result<T = ()> = core::result::Result<T, Error>;

/// Single-owner iTCO watchdog controller.
///
/// Creating this object is passive. start is the only operation that clears
/// NO_REBOOT and enables the timer. The owner must serialize all access and
/// must keep RegisterIo's port permissions and optional GCS mapping valid for
/// this object's lifetime.
pub struct Tco<I: RegisterIo> {
    io: I,
    device: SupportedDevice,
    resources: Resources,
    boot_status: bool,
    running: bool,
    owns_watchdog: bool,
    timeout_secs: Option<u32>,
}

impl<I: RegisterIo> Tco<I> {
    /// Creates a controller without touching hardware or arming the watchdog.
    ///
    /// The supplied resource values must have been produced for this exact
    /// admitted device by Resources::from_chipset_config.
    pub fn new(io: I, device: SupportedDevice, resources: Resources) -> Result<Self> {
        if device != resources.device {
            return Err(Error::InvalidResources);
        }
        Ok(Self {
            io,
            device,
            resources,
            boot_status: false,
            running: false,
            owns_watchdog: false,
            timeout_secs: None,
        })
    }

    /// Returns the admitted device generation.
    pub const fn version(&self) -> Version {
        self.device.version
    }

    /// Whether the legacy v2 BOOT_STS bit was observed during the last start or adoption.
    ///
    /// This reports that specific early-boot status, not every watchdog timeout.
    pub const fn boot_status(&self) -> bool {
        self.boot_status
    }

    /// Whether the core has verified the timer is counting.
    pub const fn is_running(&self) -> bool {
        self.running
    }

    /// Configured watchdog timeout, if one was accepted.
    pub const fn timeout_seconds(&self) -> Option<u32> {
        self.timeout_secs
    }

    /// Configures, acknowledges prior status, and explicitly starts the timer.
    ///
    /// timeout_seconds is an operator policy input; this method never supplies
    /// a default. On a rejected register readback, the controller is not marked
    /// running and the error is returned to the owner.
    pub fn start(&mut self, timeout_seconds: u32) -> Result {
        if self.running {
            return Err(Error::BadState);
        }
        let ticks = timeout_to_ticks(timeout_seconds)?;
        if self.io.read_port16(self.port(regs::CONTROL1)) & regs::HALT == 0 {
            return Err(Error::AlreadyRunning);
        }
        self.boot_status = self.device.version == Version::Ich9V2
            && self.io.read_port16(self.port(regs::STATUS2)) & regs::BOOT_STATUS != 0;

        self.io
            .write_port16(self.port(regs::STATUS1), regs::STATUS1_W1C);
        let status2_clear = match self.device.version {
            Version::Ich9V2 => regs::STATUS2_W1C_V2,
            Version::TcoV6 => regs::STATUS2_W1C_V6,
        };
        self.io
            .write_port16(self.port(regs::STATUS2), status2_clear);
        self.disable_smi_watchdog_clear()?;
        self.set_timer_ticks(ticks)?;
        self.set_no_reboot(false)?;
        self.io.write_port16(self.port(regs::RELOAD), 1);
        let control = self.io.read_port16(self.port(regs::CONTROL1)) & !regs::NMI_NOW;
        self.io
            .write_port16(self.port(regs::CONTROL1), control & !regs::HALT);
        if self.io.read_port16(self.port(regs::CONTROL1)) & regs::HALT != 0 {
            return Err(Error::Locked);
        }
        self.timeout_secs = Some(timeout_seconds);
        self.running = true;
        self.owns_watchdog = true;
        Ok(())
    }

    /// Takes over a watchdog that firmware left counting.
    ///
    /// This is deliberately separate from start: the caller must explicitly
    /// choose to assume responsibility for an already-active reset timer. Once
    /// HALT confirms active hardware, later configuration failures retain that
    /// ownership so the caller can still ping or stop the timer.
    pub fn adopt_running(&mut self, timeout_seconds: u32) -> Result {
        if self.running {
            return Err(Error::BadState);
        }
        let ticks = timeout_to_ticks(timeout_seconds)?;
        if self.io.read_port16(self.port(regs::CONTROL1)) & regs::HALT != 0 {
            return Err(Error::BadState);
        }
        // Ownership is explicit and the hardware is already counting. Keep it
        // manageable even if a later configuration readback fails.
        self.running = true;
        self.owns_watchdog = true;
        self.boot_status = self.device.version == Version::Ich9V2
            && self.io.read_port16(self.port(regs::STATUS2)) & regs::BOOT_STATUS != 0;
        self.disable_smi_watchdog_clear()?;
        self.set_timer_ticks(ticks)?;
        self.set_no_reboot(false)?;
        self.io.write_port16(self.port(regs::RELOAD), 1);
        self.timeout_secs = Some(timeout_seconds);
        Ok(())
    }

    /// Stops a watchdog started or adopted by this controller and sets NO_REBOOT.
    ///
    /// A controller created over an already-running timer must call
    /// `adopt_running` before it can stop that timer. If HALT succeeds but the
    /// NO_REBOOT update fails, another call retries the protective update.
    pub fn stop(&mut self) -> Result {
        if !self.owns_watchdog {
            return Err(Error::BadState);
        }
        let control =
            (self.io.read_port16(self.port(regs::CONTROL1)) & !regs::NMI_NOW) | regs::HALT;
        self.io.write_port16(self.port(regs::CONTROL1), control);
        if self.io.read_port16(self.port(regs::CONTROL1)) & regs::HALT == 0 {
            return Err(Error::Locked);
        }
        self.running = false;
        self.set_no_reboot(true)
    }

    /// Reloads a running watchdog.
    pub fn ping(&mut self) -> Result {
        if !self.running {
            return Err(Error::BadState);
        }
        self.io.write_port16(self.port(regs::RELOAD), 1);
        Ok(())
    }

    /// Changes the timeout of an already running watchdog.
    pub fn set_timeout(&mut self, timeout_seconds: u32) -> Result {
        if !self.running {
            return Err(Error::BadState);
        }
        self.set_timer_ticks(timeout_to_ticks(timeout_seconds)?)?;
        self.timeout_secs = Some(timeout_seconds);
        Ok(())
    }

    /// Reads the current time remaining, rounded down to whole seconds.
    pub fn time_left_seconds(&mut self) -> Result<u32> {
        if !self.running {
            return Err(Error::BadState);
        }
        let ticks = u32::from(self.io.read_port16(self.port(regs::RELOAD)) & regs::TIMER_MASK);
        Ok(ticks * 6 / 10)
    }

    /// Returns the owned register adapter to the platform layer.
    pub fn into_io(self) -> I {
        self.io
    }

    fn port(&self, offset: u16) -> u16 {
        self.resources.tco_port + offset
    }

    fn disable_smi_watchdog_clear(&mut self) -> Result {
        if let Some(smi_port) = self.resources.smi_enable_port {
            let mask = 1 << regs::SMI_WDT_CLEAR_BIT;
            let old = self.io.read_port32(smi_port);
            self.io.write_port32(smi_port, old & !mask);
            if self.io.read_port32(smi_port) & mask != 0 {
                return Err(Error::Locked);
            }
        }
        Ok(())
    }

    fn set_timer_ticks(&mut self, ticks: u16) -> Result {
        let port = self.port(regs::TIMER);
        let old = self.io.read_port16(port) & !regs::TIMER_MASK;
        self.io.write_port16(port, old | ticks);
        if self.io.read_port16(port) & regs::TIMER_MASK != ticks {
            return Err(Error::Io);
        }
        Ok(())
    }

    fn set_no_reboot(&mut self, set: bool) -> Result {
        match self.device.version {
            Version::Ich9V2 => {
                let address = self
                    .resources
                    .no_reboot_gcs
                    .ok_or(Error::InvalidResources)?;
                let old = self.io.read_gcs32(address);
                let new = if set {
                    old | regs::NO_REBOOT_V2
                } else {
                    old & !regs::NO_REBOOT_V2
                };
                self.io.write_gcs32(address, new);
                if self.io.read_gcs32(address) != new {
                    return Err(Error::Locked);
                }
            }
            Version::TcoV6 => {
                let port = self.port(regs::CONTROL1);
                let old = self.io.read_port16(port) & !regs::NMI_NOW;
                let new = if set {
                    old | regs::NO_REBOOT_V6
                } else {
                    old & !regs::NO_REBOOT_V6
                };
                self.io.write_port16(port, new);
                if self.io.read_port16(port) & !regs::NMI_NOW != new {
                    return Err(Error::Locked);
                }
            }
        }
        Ok(())
    }
}

/// Converts seconds to the 0.6-second ticks used by supported iTCO v2/v6.
pub const fn timeout_to_ticks(seconds: u32) -> Result<u16> {
    if seconds < 3 || seconds > 614 {
        return Err(Error::InvalidTimeout);
    }
    let Some(ticks) = seconds.checked_mul(10) else {
        return Err(Error::InvalidTimeout);
    };
    let ticks = ticks / 6;
    if ticks < 4 || ticks > u16::MAX as u32 {
        return Err(Error::InvalidTimeout);
    }
    Ok(ticks as u16)
}

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod tests;
