use alloc::{format, vec::Vec};

use ax_errno::{AxResult, ax_err_type};
use fdt_edit::{Fdt, Node, NodeId};

use super::property::{prop_null, prop_string, prop_string_list, prop_u32, prop_u32_array};
use crate::arch::guest_platform::GuestPlatform;

const PHANDLE_CPU0: u32 = 0x8000;
const PHANDLE_CPUIC: u32 = 0x8001;
const PHANDLE_EIOINTC: u32 = 0x8002;
const PHANDLE_PCH_PIC: u32 = 0x8003;
const PHANDLE_PCH_MSI: u32 = 0x8004;
const PHANDLE_GED_SYSCON: u32 = 0x8005;

pub fn build(platform: &GuestPlatform) -> AxResult<Vec<u8>> {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    set_prop(&mut fdt, root, prop_u32("#address-cells", 2))?;
    set_prop(&mut fdt, root, prop_u32("#size-cells", 2))?;
    set_prop(
        &mut fdt,
        root,
        prop_string("compatible", "linux,dummy-loongson3"),
    )?;

    add_chosen(&mut fdt, root, platform)?;
    add_cpus(&mut fdt, root)?;
    add_memory(&mut fdt, root, platform)?;
    add_interrupt_controllers(&mut fdt, root, platform)?;
    add_platform_bus(&mut fdt, root, platform)?;
    add_power(&mut fdt, root, platform)?;
    add_rtc(&mut fdt, root, platform)?;
    add_serial(&mut fdt, root, platform)?;
    add_flash(&mut fdt, root, platform)?;
    add_fw_cfg(&mut fdt, root, platform)?;

    Ok(fdt.encode().as_ref().to_vec())
}

fn set_prop(fdt: &mut Fdt, node: NodeId, prop: fdt_edit::Property) -> AxResult {
    fdt.node_mut(node)
        .ok_or_else(|| ax_err_type!(InvalidData, "FDT node id is invalid"))?
        .set_property(prop);
    Ok(())
}

fn add_child(fdt: &mut Fdt, parent: NodeId, name: &str) -> NodeId {
    fdt.add_node(parent, Node::new(name))
}

fn add_platform_bus(fdt: &mut Fdt, root: NodeId, platform: &GuestPlatform) -> AxResult {
    let (bus_base, bus_size) = platform_bus_range(platform);
    let platform_bus = add_child(fdt, root, &format!("platform-bus@{bus_base:x}"));
    set_prop(
        fdt,
        platform_bus,
        prop_string_list("compatible", &["qemu,platform", "simple-bus"]),
    )?;
    set_prop(fdt, platform_bus, prop_u32("#address-cells", 1))?;
    set_prop(fdt, platform_bus, prop_u32("#size-cells", 1))?;
    set_prop(
        fdt,
        platform_bus,
        prop_u32_array("ranges", &[0, 0, bus_base as u32, bus_size as u32]),
    )?;
    set_prop(
        fdt,
        platform_bus,
        prop_u32("interrupt-parent", PHANDLE_PCH_PIC),
    )
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

fn add_chosen(fdt: &mut Fdt, root: NodeId, platform: &GuestPlatform) -> AxResult {
    let chosen = add_child(fdt, root, "chosen");
    set_prop(
        fdt,
        chosen,
        prop_string(
            "stdout-path",
            &format!("/serial@{:x}", platform.serial.mmio.base),
        ),
    )
}

fn add_cpus(fdt: &mut Fdt, root: NodeId) -> AxResult {
    let cpus = add_child(fdt, root, "cpus");
    set_prop(fdt, cpus, prop_u32("#address-cells", 1))?;
    set_prop(fdt, cpus, prop_u32("#size-cells", 0))?;

    let cpu_map = add_child(fdt, cpus, "cpu-map");
    let socket0 = add_child(fdt, cpu_map, "socket0");
    let core0 = add_child(fdt, socket0, "core0");
    set_prop(fdt, core0, prop_u32("cpu", PHANDLE_CPU0))?;

    let cpu = add_child(fdt, cpus, "cpu@0");
    set_prop(fdt, cpu, prop_string("device_type", "cpu"))?;
    set_prop(
        fdt,
        cpu,
        prop_string("compatible", "loongarch,Loongson-3A5000"),
    )?;
    set_prop(fdt, cpu, prop_u32("reg", 0))?;
    set_prop(fdt, cpu, prop_u32("phandle", PHANDLE_CPU0))
}

fn add_memory(fdt: &mut Fdt, root: NodeId, platform: &GuestPlatform) -> AxResult {
    for region in &platform.ram_regions {
        let memory = add_child(fdt, root, &format!("memory@{:x}", region.base));
        set_prop(fdt, memory, prop_string("device_type", "memory"))?;
        prop_reg(fdt, memory, region.base, region.size)?;
    }
    Ok(())
}

fn add_interrupt_controllers(fdt: &mut Fdt, root: NodeId, platform: &GuestPlatform) -> AxResult {
    let cpuic = add_child(fdt, root, "cpuic");
    set_prop(fdt, cpuic, prop_null("interrupt-controller"))?;
    set_prop(fdt, cpuic, prop_u32("#interrupt-cells", 1))?;
    set_prop(
        fdt,
        cpuic,
        prop_string("compatible", "loongson,cpu-interrupt-controller"),
    )?;
    set_prop(fdt, cpuic, prop_u32("phandle", PHANDLE_CPUIC))?;

    let eiointc = add_child(fdt, root, "eiointc@1400");
    set_prop(
        fdt,
        eiointc,
        prop_string("compatible", "loongson,ls2k2000-eiointc"),
    )?;
    set_prop(fdt, eiointc, prop_null("interrupt-controller"))?;
    set_prop(fdt, eiointc, prop_u32("#interrupt-cells", 1))?;
    set_prop(fdt, eiointc, prop_u32("interrupt-parent", PHANDLE_CPUIC))?;
    set_prop(
        fdt,
        eiointc,
        prop_u32_array("interrupts", &[platform.interrupt.eiointc_irq]),
    )?;
    set_prop(
        fdt,
        eiointc,
        prop_u32_array("reg", &[0, 0x0000_1400, 0, 0x800]),
    )?;
    set_prop(fdt, eiointc, prop_u32("phandle", PHANDLE_EIOINTC))?;

    let pch_pic = add_child(
        fdt,
        root,
        &format!("platic@{:x}", platform.interrupt.pch_pic.base),
    );
    set_prop(
        fdt,
        pch_pic,
        prop_string("compatible", "loongson,pch-pic-1.0"),
    )?;
    set_prop(fdt, pch_pic, prop_null("interrupt-controller"))?;
    set_prop(fdt, pch_pic, prop_u32("#interrupt-cells", 2))?;
    set_prop(fdt, pch_pic, prop_u32("interrupt-parent", PHANDLE_EIOINTC))?;
    prop_reg(
        fdt,
        pch_pic,
        platform.interrupt.pch_pic.base,
        platform.interrupt.pch_pic.size,
    )?;
    set_prop(
        fdt,
        pch_pic,
        prop_u32_array(
            "loongson,pic-base-vec",
            &[platform.interrupt.pch_pic_gsi_base],
        ),
    )?;
    set_prop(fdt, pch_pic, prop_u32("phandle", PHANDLE_PCH_PIC))?;

    let msi = add_child(
        fdt,
        root,
        &format!("msi@{:x}", platform.interrupt.pch_msi.base),
    );
    set_prop(fdt, msi, prop_string("compatible", "loongson,pch-msi-1.0"))?;
    set_prop(fdt, msi, prop_null("interrupt-controller"))?;
    set_prop(fdt, msi, prop_u32("interrupt-parent", PHANDLE_EIOINTC))?;
    prop_reg(
        fdt,
        msi,
        platform.interrupt.pch_msi.base,
        platform.interrupt.pch_msi.size,
    )?;
    set_prop(
        fdt,
        msi,
        prop_u32("loongson,msi-base-vec", platform.interrupt.pch_msi_start),
    )?;
    set_prop(
        fdt,
        msi,
        prop_u32("loongson,msi-num-vecs", platform.interrupt.pch_msi_count),
    )?;
    set_prop(fdt, msi, prop_u32("phandle", PHANDLE_PCH_MSI))
}

fn add_power(fdt: &mut Fdt, root: NodeId, platform: &GuestPlatform) -> AxResult {
    let ged = platform.firmware_devices.ged;
    let ged_node = add_child(fdt, root, &format!("ged@{:x}", ged.mmio.base));
    set_prop(fdt, ged_node, prop_string("compatible", "syscon"))?;
    prop_reg(fdt, ged_node, ged.mmio.base, ged.mmio.size)?;
    set_prop(fdt, ged_node, prop_u32("reg-shift", 0))?;
    set_prop(fdt, ged_node, prop_u32("reg-io-width", 1))?;
    set_prop(fdt, ged_node, prop_u32("phandle", PHANDLE_GED_SYSCON))?;

    let poweroff = add_child(fdt, root, "poweroff");
    set_prop(fdt, poweroff, prop_string("compatible", "syscon-poweroff"))?;
    set_prop(fdt, poweroff, prop_u32("regmap", PHANDLE_GED_SYSCON))?;
    set_prop(fdt, poweroff, prop_u32("offset", ged.poweroff_offset))?;
    set_prop(fdt, poweroff, prop_u32("value", ged.poweroff_value))?;

    let reboot = add_child(fdt, root, "reboot");
    set_prop(fdt, reboot, prop_string("compatible", "syscon-reboot"))?;
    set_prop(fdt, reboot, prop_u32("regmap", PHANDLE_GED_SYSCON))?;
    set_prop(fdt, reboot, prop_u32("offset", ged.reboot_offset))?;
    set_prop(fdt, reboot, prop_u32("value", ged.reboot_value))
}

fn add_rtc(fdt: &mut Fdt, root: NodeId, platform: &GuestPlatform) -> AxResult {
    let rtc = platform.firmware_devices.rtc;
    let rtc_node = add_child(fdt, root, &format!("rtc@{:x}", rtc.mmio.base));
    set_prop(
        fdt,
        rtc_node,
        prop_string("compatible", "loongson,ls7a-rtc"),
    )?;
    prop_reg(fdt, rtc_node, rtc.mmio.base, rtc.mmio.size)?;
    set_prop(fdt, rtc_node, prop_u32("interrupt-parent", PHANDLE_PCH_PIC))?;
    set_prop(fdt, rtc_node, prop_u32_array("interrupts", &[rtc.irq, 4]))
}

fn add_serial(fdt: &mut Fdt, root: NodeId, platform: &GuestPlatform) -> AxResult {
    let serial = add_child(
        fdt,
        root,
        &format!("serial@{:x}", platform.serial.mmio.base),
    );
    set_prop(fdt, serial, prop_string("compatible", "ns16550a"))?;
    prop_reg(
        fdt,
        serial,
        platform.serial.mmio.base,
        platform.serial.mmio.size,
    )?;
    set_prop(
        fdt,
        serial,
        prop_u32("clock-frequency", platform.serial.clock_hz),
    )?;
    set_prop(fdt, serial, prop_u32("interrupt-parent", PHANDLE_PCH_PIC))?;
    set_prop(
        fdt,
        serial,
        prop_u32_array("interrupts", &[platform.serial.irq, 4]),
    )
}

fn add_flash(fdt: &mut Fdt, root: NodeId, platform: &GuestPlatform) -> AxResult {
    let flash = platform.firmware_devices.flash;
    let flash_node = add_child(fdt, root, &format!("flash@{:x}", flash.banks[0].base));
    set_prop(fdt, flash_node, prop_string("compatible", "cfi-flash"))?;
    set_prop(fdt, flash_node, prop_u32("bank-width", flash.bank_width))?;
    set_prop(
        fdt,
        flash_node,
        prop_u32_array(
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
        ),
    )
}

fn add_fw_cfg(fdt: &mut Fdt, root: NodeId, platform: &GuestPlatform) -> AxResult {
    let fw_cfg = add_child(fdt, root, &format!("fw_cfg@{:x}", platform.fw_cfg.base));
    set_prop(fdt, fw_cfg, prop_string("compatible", "qemu,fw-cfg-mmio"))?;
    prop_reg(fdt, fw_cfg, platform.fw_cfg.base, platform.fw_cfg.size)?;
    set_prop(fdt, fw_cfg, prop_null("dma-coherent"))
}

fn prop_reg(fdt: &mut Fdt, node: NodeId, base: u64, size: u64) -> AxResult {
    set_prop(
        fdt,
        node,
        prop_u32_array(
            "reg",
            &[
                (base >> 32) as u32,
                base as u32,
                (size >> 32) as u32,
                size as u32,
            ],
        ),
    )
}

#[cfg(test)]
mod tests {
    use fdt_edit::Fdt;

    use super::*;

    #[test]
    fn loongarch_firmware_dtb_is_reparseable() {
        let platform = GuestPlatform::default();
        let dtb = build(&platform).unwrap();
        let fdt = Fdt::from_bytes(&dtb).unwrap();

        assert!(fdt.get_by_path_id("/chosen").is_some());
        assert!(fdt.get_by_path_id("/cpus/cpu@0").is_some());
    }
}
