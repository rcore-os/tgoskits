use alloc::{format, vec};

use ax_errno::{AxError, AxResult, ax_err_type};

use super::GuestPlatform;
use crate::fdt::vm_fdt::FdtWriter;

const PHANDLE_CPU0: u32 = 0x8000;
const PHANDLE_CPUIC: u32 = 0x8001;
const PHANDLE_EIOINTC: u32 = 0x8002;
const PHANDLE_PCH_PIC: u32 = 0x8003;
const PHANDLE_PCH_MSI: u32 = 0x8004;
const PHANDLE_GED_SYSCON: u32 = 0x8005;

pub fn build(platform: &GuestPlatform) -> AxResult<Vec<u8>> {
    let mut fdt = FdtWriter::new().map_err(fdt_err)?;
    let root = fdt.begin_node("").map_err(fdt_err)?;
    fdt.property_u32("#address-cells", 2).map_err(fdt_err)?;
    fdt.property_u32("#size-cells", 2).map_err(fdt_err)?;
    fdt.property_string("compatible", "linux,dummy-loongson3")
        .map_err(fdt_err)?;

    add_chosen(&mut fdt, platform)?;
    add_cpus(&mut fdt)?;
    add_memory(&mut fdt, platform)?;
    add_interrupt_controllers(&mut fdt, platform)?;
    add_platform_bus(&mut fdt, platform)?;
    add_power(&mut fdt, platform)?;
    add_rtc(&mut fdt, platform)?;
    add_serial(&mut fdt, platform)?;
    add_flash(&mut fdt, platform)?;
    add_fw_cfg(&mut fdt, platform)?;

    fdt.end_node(root).map_err(fdt_err)?;
    fdt.finish().map_err(fdt_err)
}

fn fdt_err(err: impl core::fmt::Debug) -> AxError {
    ax_err_type!(
        InvalidData,
        format!("failed to build LoongArch UEFI firmware FDT: {err:?}")
    )
}

fn add_platform_bus(fdt: &mut FdtWriter, platform: &GuestPlatform) -> AxResult {
    let (bus_base, bus_size) = platform_bus_range(platform);
    let platform_bus = fdt
        .begin_node(&format!("platform-bus@{bus_base:x}"))
        .map_err(fdt_err)?;
    fdt.property_string_list(
        "compatible",
        vec!["qemu,platform".into(), "simple-bus".into()],
    )
    .map_err(fdt_err)?;
    fdt.property_u32("#address-cells", 1).map_err(fdt_err)?;
    fdt.property_u32("#size-cells", 1).map_err(fdt_err)?;
    fdt.property_array_u32("ranges", &[0, 0, bus_base as u32, bus_size as u32])
        .map_err(fdt_err)?;
    fdt.property_u32("interrupt-parent", PHANDLE_PCH_PIC)
        .map_err(fdt_err)?;
    fdt.end_node(platform_bus).map_err(fdt_err)?;
    Ok(())
}

fn platform_bus_range(platform: &GuestPlatform) -> (u64, u64) {
    const PAGE_SIZE: u64 = 0x1000;

    let base = platform
        .firmware_devices
        .ged
        .mmio
        .base
        .min(platform.firmware_devices.rtc.mmio.base)
        & !(PAGE_SIZE - 1);
    let end = platform
        .firmware_devices
        .ged
        .mmio
        .base
        .saturating_add(platform.firmware_devices.ged.mmio.size)
        .max(
            platform
                .firmware_devices
                .rtc
                .mmio
                .base
                .saturating_add(platform.firmware_devices.rtc.mmio.size),
        )
        .div_ceil(PAGE_SIZE)
        * PAGE_SIZE;

    (base, end.saturating_sub(base).max(PAGE_SIZE))
}

fn add_chosen(fdt: &mut FdtWriter, platform: &GuestPlatform) -> AxResult {
    let chosen = fdt.begin_node("chosen").map_err(fdt_err)?;
    fdt.property_string(
        "stdout-path",
        &format!("/serial@{:x}", platform.serial.mmio.base),
    )
    .map_err(fdt_err)?;
    fdt.end_node(chosen).map_err(fdt_err)?;
    Ok(())
}

fn add_cpus(fdt: &mut FdtWriter) -> AxResult {
    let cpus = fdt.begin_node("cpus").map_err(fdt_err)?;
    fdt.property_u32("#address-cells", 1).map_err(fdt_err)?;
    fdt.property_u32("#size-cells", 0).map_err(fdt_err)?;
    let cpu_map = fdt.begin_node("cpu-map").map_err(fdt_err)?;
    let socket0 = fdt.begin_node("socket0").map_err(fdt_err)?;
    let core0 = fdt.begin_node("core0").map_err(fdt_err)?;
    fdt.property_u32("cpu", PHANDLE_CPU0).map_err(fdt_err)?;
    fdt.end_node(core0).map_err(fdt_err)?;
    fdt.end_node(socket0).map_err(fdt_err)?;
    fdt.end_node(cpu_map).map_err(fdt_err)?;
    let cpu = fdt.begin_node("cpu@0").map_err(fdt_err)?;
    fdt.property_string("device_type", "cpu").map_err(fdt_err)?;
    fdt.property_string("compatible", "loongarch,Loongson-3A5000")
        .map_err(fdt_err)?;
    fdt.property_u32("reg", 0).map_err(fdt_err)?;
    fdt.property_phandle(PHANDLE_CPU0).map_err(fdt_err)?;
    fdt.end_node(cpu).map_err(fdt_err)?;
    fdt.end_node(cpus).map_err(fdt_err)?;
    Ok(())
}

fn add_memory(fdt: &mut FdtWriter, platform: &GuestPlatform) -> AxResult {
    for region in &platform.ram_regions {
        let memory = fdt
            .begin_node(&format!("memory@{:x}", region.base))
            .map_err(fdt_err)?;
        fdt.property_string("device_type", "memory")
            .map_err(fdt_err)?;
        fdt.property_array_u32(
            "reg",
            &[
                (region.base >> 32) as u32,
                region.base as u32,
                (region.size >> 32) as u32,
                region.size as u32,
            ],
        )
        .map_err(fdt_err)?;
        fdt.end_node(memory).map_err(fdt_err)?;
    }
    Ok(())
}

fn add_interrupt_controllers(fdt: &mut FdtWriter, platform: &GuestPlatform) -> AxResult {
    let cpuic = fdt.begin_node("cpuic").map_err(fdt_err)?;
    fdt.property_null("interrupt-controller").map_err(fdt_err)?;
    fdt.property_u32("#interrupt-cells", 1).map_err(fdt_err)?;
    fdt.property_string("compatible", "loongson,cpu-interrupt-controller")
        .map_err(fdt_err)?;
    fdt.property_phandle(PHANDLE_CPUIC).map_err(fdt_err)?;
    fdt.end_node(cpuic).map_err(fdt_err)?;

    let eiointc = fdt.begin_node("eiointc@1400").map_err(fdt_err)?;
    fdt.property_string("compatible", "loongson,ls2k2000-eiointc")
        .map_err(fdt_err)?;
    fdt.property_null("interrupt-controller").map_err(fdt_err)?;
    fdt.property_u32("#interrupt-cells", 1).map_err(fdt_err)?;
    fdt.property_u32("interrupt-parent", PHANDLE_CPUIC)
        .map_err(fdt_err)?;
    fdt.property_array_u32("interrupts", &[platform.interrupt.eiointc_irq])
        .map_err(fdt_err)?;
    fdt.property_array_u32("reg", &[0, 0x0000_1400, 0, 0x800])
        .map_err(fdt_err)?;
    fdt.property_phandle(PHANDLE_EIOINTC).map_err(fdt_err)?;
    fdt.end_node(eiointc).map_err(fdt_err)?;

    let pch_pic = fdt
        .begin_node(&format!("platic@{:x}", platform.interrupt.pch_pic.base))
        .map_err(fdt_err)?;
    fdt.property_string("compatible", "loongson,pch-pic-1.0")
        .map_err(fdt_err)?;
    fdt.property_null("interrupt-controller").map_err(fdt_err)?;
    fdt.property_u32("#interrupt-cells", 2).map_err(fdt_err)?;
    fdt.property_u32("interrupt-parent", PHANDLE_EIOINTC)
        .map_err(fdt_err)?;
    prop_reg(
        fdt,
        platform.interrupt.pch_pic.base,
        platform.interrupt.pch_pic.size,
    )?;
    fdt.property_array_u32(
        "loongson,pic-base-vec",
        &[platform.interrupt.pch_pic_gsi_base],
    )
    .map_err(fdt_err)?;
    fdt.property_phandle(PHANDLE_PCH_PIC).map_err(fdt_err)?;
    fdt.end_node(pch_pic).map_err(fdt_err)?;

    let msi = fdt
        .begin_node(&format!("msi@{:x}", platform.interrupt.pch_msi.base))
        .map_err(fdt_err)?;
    fdt.property_string("compatible", "loongson,pch-msi-1.0")
        .map_err(fdt_err)?;
    fdt.property_null("interrupt-controller").map_err(fdt_err)?;
    fdt.property_u32("interrupt-parent", PHANDLE_EIOINTC)
        .map_err(fdt_err)?;
    prop_reg(
        fdt,
        platform.interrupt.pch_msi.base,
        platform.interrupt.pch_msi.size,
    )?;
    fdt.property_u32("loongson,msi-base-vec", platform.interrupt.pch_msi_start)
        .map_err(fdt_err)?;
    fdt.property_u32("loongson,msi-num-vecs", platform.interrupt.pch_msi_count)
        .map_err(fdt_err)?;
    fdt.property_phandle(PHANDLE_PCH_MSI).map_err(fdt_err)?;
    fdt.end_node(msi).map_err(fdt_err)?;
    Ok(())
}

fn add_power(fdt: &mut FdtWriter, platform: &GuestPlatform) -> AxResult {
    let ged = platform.firmware_devices.ged;
    let ged_node = fdt
        .begin_node(&format!("ged@{:x}", ged.mmio.base))
        .map_err(fdt_err)?;
    fdt.property_string("compatible", "syscon")
        .map_err(fdt_err)?;
    prop_reg(fdt, ged.mmio.base, ged.mmio.size)?;
    fdt.property_u32("reg-shift", 0).map_err(fdt_err)?;
    fdt.property_u32("reg-io-width", 1).map_err(fdt_err)?;
    fdt.property_phandle(PHANDLE_GED_SYSCON).map_err(fdt_err)?;
    fdt.end_node(ged_node).map_err(fdt_err)?;

    let poweroff = fdt.begin_node("poweroff").map_err(fdt_err)?;
    fdt.property_string("compatible", "syscon-poweroff")
        .map_err(fdt_err)?;
    fdt.property_u32("regmap", PHANDLE_GED_SYSCON)
        .map_err(fdt_err)?;
    fdt.property_u32("offset", ged.poweroff_offset)
        .map_err(fdt_err)?;
    fdt.property_u32("value", ged.poweroff_value)
        .map_err(fdt_err)?;
    fdt.end_node(poweroff).map_err(fdt_err)?;

    let reboot = fdt.begin_node("reboot").map_err(fdt_err)?;
    fdt.property_string("compatible", "syscon-reboot")
        .map_err(fdt_err)?;
    fdt.property_u32("regmap", PHANDLE_GED_SYSCON)
        .map_err(fdt_err)?;
    fdt.property_u32("offset", ged.reboot_offset)
        .map_err(fdt_err)?;
    fdt.property_u32("value", ged.reboot_value)
        .map_err(fdt_err)?;
    fdt.end_node(reboot).map_err(fdt_err)?;
    Ok(())
}

fn add_rtc(fdt: &mut FdtWriter, platform: &GuestPlatform) -> AxResult {
    let rtc = platform.firmware_devices.rtc;
    let rtc_node = fdt
        .begin_node(&format!("rtc@{:x}", rtc.mmio.base))
        .map_err(fdt_err)?;
    fdt.property_string("compatible", "loongson,ls7a-rtc")
        .map_err(fdt_err)?;
    prop_reg(fdt, rtc.mmio.base, rtc.mmio.size)?;
    fdt.property_u32("interrupt-parent", PHANDLE_PCH_PIC)
        .map_err(fdt_err)?;
    fdt.property_array_u32("interrupts", &[rtc.irq, 4])
        .map_err(fdt_err)?;
    fdt.end_node(rtc_node).map_err(fdt_err)?;
    Ok(())
}

fn add_serial(fdt: &mut FdtWriter, platform: &GuestPlatform) -> AxResult {
    let serial = fdt
        .begin_node(&format!("serial@{:x}", platform.serial.mmio.base))
        .map_err(fdt_err)?;
    fdt.property_string("compatible", "ns16550a")
        .map_err(fdt_err)?;
    prop_reg(fdt, platform.serial.mmio.base, platform.serial.mmio.size)?;
    fdt.property_u32("clock-frequency", platform.serial.clock_hz)
        .map_err(fdt_err)?;
    fdt.property_u32("interrupt-parent", PHANDLE_PCH_PIC)
        .map_err(fdt_err)?;
    fdt.property_array_u32("interrupts", &[platform.serial.irq, 4])
        .map_err(fdt_err)?;
    fdt.end_node(serial).map_err(fdt_err)?;
    Ok(())
}

fn add_flash(fdt: &mut FdtWriter, platform: &GuestPlatform) -> AxResult {
    let flash = platform.firmware_devices.flash;
    let flash_node = fdt
        .begin_node(&format!("flash@{:x}", flash.banks[0].base))
        .map_err(fdt_err)?;
    fdt.property_string("compatible", "cfi-flash")
        .map_err(fdt_err)?;
    fdt.property_u32("bank-width", flash.bank_width)
        .map_err(fdt_err)?;
    fdt.property_array_u32(
        "reg",
        &[
            (flash.banks[0].base >> 32) as u32,
            flash.banks[0].base as u32,
            (flash.banks[0].size >> 32) as u32,
            flash.banks[0].size as u32,
            (flash.banks[1].base >> 32) as u32,
            flash.banks[1].base as u32,
            (flash.banks[1].size >> 32) as u32,
            flash.banks[1].size as u32,
        ],
    )
    .map_err(fdt_err)?;
    fdt.end_node(flash_node).map_err(fdt_err)?;
    Ok(())
}

fn add_fw_cfg(fdt: &mut FdtWriter, platform: &GuestPlatform) -> AxResult {
    let fw_cfg = fdt
        .begin_node(&format!("fw_cfg@{:x}", platform.fw_cfg.base))
        .map_err(fdt_err)?;
    fdt.property_string("compatible", "qemu,fw-cfg-mmio")
        .map_err(fdt_err)?;
    prop_reg(fdt, platform.fw_cfg.base, platform.fw_cfg.size)?;
    fdt.property_null("dma-coherent").map_err(fdt_err)?;
    fdt.end_node(fw_cfg).map_err(fdt_err)?;
    Ok(())
}

fn prop_reg(fdt: &mut FdtWriter, base: u64, size: u64) -> AxResult {
    fdt.property_array_u32(
        "reg",
        &[
            (base >> 32) as u32,
            base as u32,
            (size >> 32) as u32,
            size as u32,
        ],
    )
    .map_err(fdt_err)
}
