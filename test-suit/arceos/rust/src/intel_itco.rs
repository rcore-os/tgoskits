use core::ptr::NonNull;

use ax_std::time::Duration;
use intel_itco_watchdog::{RegisterIo, Resources, Tco, Version, identify};

use crate::TestResult;

const ICH9_LPC_BDF: u32 = (1 << 31) | (0x1f << 11);
const PCI_CONFIG_ADDRESS: u16 = 0x0cf8;
const PCI_CONFIG_DATA: u16 = 0x0cfc;
const ICH9_PMBASE: u8 = 0x40;
const ICH9_RCBA: u8 = 0xf0;
const TCO_CONTROL1: u16 = 0x08;
const TCO_HALT: u16 = 1 << 11;
const GCS_NO_REBOOT: u32 = 1 << 5;

pub fn run() -> TestResult {
    verify_intel_itco_v2()?;
    println!("INTEL_ITCO_QEMU_OK");
    Ok(())
}

fn verify_intel_itco_v2() -> TestResult {
    let id = pci_config_read32(0x00);
    let vendor_id = id as u16;
    let device_id = (id >> 16) as u16;
    if (vendor_id, device_id) != (0x8086, 0x2918) {
        return Err("QEMU q35 ICH9 LPC function 00:1f.0 was not present");
    }
    let device = identify(vendor_id, device_id).ok_or("ICH9 LPC was not admitted by iTCO core")?;
    if device.version() != Version::Ich9V2 {
        return Err("ICH9 LPC did not select the iTCO v2 register layout");
    }

    let pmbase = pci_config_read32(ICH9_PMBASE);
    let rcba = pci_config_read32(ICH9_RCBA);
    let resources = Resources::from_chipset_config(device, pmbase, rcba)
        .map_err(|_| "invalid ICH9 TCO resources")?;
    let gcs_address = resources
        .no_reboot_gcs()
        .ok_or("ICH9 GCS address was not decoded")?;
    let gcs = ax_driver::mmio::iomap(gcs_address as usize, core::mem::size_of::<u32>())
        .map_err(|_| "failed to map ICH9 GCS from QEMU RCBA")?
        .cast::<u32>();
    let io = ChipsetPortIo { gcs_address, gcs };
    let tco_control1 = resources.tco_port() + TCO_CONTROL1;

    let mut tco = Tco::new(io, device, resources).map_err(|_| "iTCO v2 core creation failed")?;
    // QEMU's ICH9 TCO reset state is already counting. Take explicit
    // ownership and stop it before testing the separate start transition.
    if read_port16(tco_control1) & TCO_HALT == 0 {
        if tco.adopt_running(30).is_err() {
            if tco.is_running() {
                let _ = tco.stop();
            }
            return Err("iTCO v2 could not safely adopt QEMU's initial running timer");
        }
        tco.stop()
            .map_err(|_| "iTCO v2 could not stop QEMU's initial running timer")?;
        if read_port16(tco_control1) & TCO_HALT == 0 || read_gcs32(gcs) & GCS_NO_REBOOT == 0 {
            return Err("iTCO v2 initial timer stop did not leave a protected halted state");
        }
    }

    tco.start(30).map_err(|_| "iTCO v2 start failed")?;
    let running_checks = (|| {
        if !tco.is_running() || tco.timeout_seconds() != Some(30) {
            return Err("iTCO v2 core did not retain the started timeout");
        }
        if read_port16(tco_control1) & TCO_HALT != 0 {
            return Err("iTCO v2 start did not clear the hardware HALT bit");
        }
        if read_gcs32(gcs) & GCS_NO_REBOOT != 0 {
            return Err("iTCO v2 start did not clear GCS NO_REBOOT");
        }
        let time_left = tco
            .time_left_seconds()
            .map_err(|_| "iTCO v2 time-left query failed")?;
        if time_left == 0 || time_left > 30 {
            return Err("iTCO v2 returned an invalid running time-left value");
        }

        ax_std::thread::sleep(Duration::from_secs(1));
        let time_left_before_ping = tco
            .time_left_seconds()
            .map_err(|_| "iTCO v2 pre-ping time-left query failed")?;
        if time_left_before_ping == 0 || time_left_before_ping >= time_left {
            return Err("iTCO v2 countdown did not decrease before ping");
        }
        tco.ping().map_err(|_| "iTCO v2 ping failed")?;
        let time_left_after_ping = tco
            .time_left_seconds()
            .map_err(|_| "iTCO v2 post-ping time-left query failed")?;
        if time_left_after_ping <= time_left_before_ping || time_left_after_ping > 30 {
            return Err("iTCO v2 ping did not reload the timer");
        }
        Ok(())
    })();

    // Even a failed post-start assertion must try to halt the QEMU watchdog
    // before the test returns; the VM timeout is not a substitute for cleanup.
    let stop_result = tco.stop();
    running_checks?;
    stop_result.map_err(|_| "iTCO v2 stop failed")?;
    if tco.is_running() {
        return Err("iTCO v2 core remained running after stop");
    }
    if read_port16(tco_control1) & TCO_HALT == 0 {
        return Err("iTCO v2 stop did not set the hardware HALT bit");
    }
    if read_gcs32(gcs) & GCS_NO_REBOOT == 0 {
        return Err("iTCO v2 stop did not set GCS NO_REBOOT");
    }
    println!("ICH9_ITCO_QEMU_STATE=halted_no_reboot");
    Ok(())
}

struct ChipsetPortIo {
    gcs_address: u64,
    gcs: NonNull<u32>,
}

impl RegisterIo for ChipsetPortIo {
    fn read_port16(&mut self, port: u16) -> u16 {
        read_port16(port)
    }

    fn write_port16(&mut self, port: u16, value: u16) {
        write_port16(port, value);
    }

    fn read_port32(&mut self, port: u16) -> u32 {
        read_port32(port)
    }

    fn write_port32(&mut self, port: u16, value: u32) {
        write_port32(port, value);
    }

    fn read_gcs32(&mut self, address: u64) -> u32 {
        assert_eq!(
            address, self.gcs_address,
            "iTCO core changed the mapped GCS address"
        );
        read_gcs32(self.gcs)
    }

    fn write_gcs32(&mut self, address: u64, value: u32) {
        assert_eq!(
            address, self.gcs_address,
            "iTCO core changed the mapped GCS address"
        );
        write_gcs32(self.gcs, value);
    }
}

fn pci_config_read32(register: u8) -> u32 {
    // CF8/CFC are a paired legacy configuration mechanism. Keep the address
    // selection and data read on one CPU with local interrupts disabled.
    let _irq = ax_std::os::arceos::guard::IrqSaveGuard::new();
    let address = ICH9_LPC_BDF | u32::from(register & !3);
    write_port32(PCI_CONFIG_ADDRESS, address);
    read_port32(PCI_CONFIG_DATA)
}

fn read_gcs32(gcs: NonNull<u32>) -> u32 {
    // SAFETY: `gcs` is a live, aligned device mapping of the ICH9 GCS register.
    unsafe { gcs.as_ptr().read_volatile() }
}

fn write_gcs32(gcs: NonNull<u32>, value: u32) {
    // SAFETY: `gcs` is a live, aligned device mapping of the ICH9 GCS register.
    unsafe { gcs.as_ptr().write_volatile(value) }
}

fn read_port16(port: u16) -> u16 {
    let value: u16;
    // SAFETY: this case runs as ring-0 x86_64 guest code against the QEMU ICH9 fixture.
    unsafe {
        core::arch::asm!(
            "in ax, dx",
            in("dx") port,
            out("ax") value,
            options(nostack, preserves_flags)
        );
    }
    value
}

fn write_port16(port: u16, value: u16) {
    // SAFETY: this case runs as ring-0 x86_64 guest code against the QEMU ICH9 fixture.
    unsafe {
        core::arch::asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nostack, preserves_flags)
        );
    }
}

fn read_port32(port: u16) -> u32 {
    let value: u32;
    // SAFETY: this case runs as ring-0 x86_64 guest code against the QEMU ICH9 fixture.
    unsafe {
        core::arch::asm!(
            "in eax, dx",
            in("dx") port,
            out("eax") value,
            options(nostack, preserves_flags)
        );
    }
    value
}

fn write_port32(port: u16, value: u32) {
    // SAFETY: this case runs as ring-0 x86_64 guest code against the QEMU ICH9 fixture.
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") port,
            in("eax") value,
            options(nostack, preserves_flags)
        );
    }
}
