//! Firmware descriptions generated from finalized machine resources.

use alloc::{format, string::String, vec, vec::Vec};

use vm_fdt::FdtWriter;

use super::{
    Aarch64GicV3Plan, AddressRange, InterruptControllerPlan, MachinePlanError, MachinePlanResult,
    VmMachinePlan,
};

const AARCH64_GIC_PHANDLE: u32 = 1;
const AARCH64_PL011_CLOCK_PHANDLE: u32 = 2;
const AARCH64_PL011_CLOCK_HZ: u32 = 24_000_000;

/// Guest-specific properties needed in addition to a finalized AArch64 plan.
#[derive(Clone, Debug)]
pub struct Aarch64FdtConfig {
    cpu_count: u32,
    bootargs: Option<String>,
    initrd: Option<AddressRange>,
}

impl Aarch64FdtConfig {
    /// Creates a configuration for `cpu_count` sequential virtual MPIDRs.
    pub fn new(cpu_count: usize) -> MachinePlanResult<Self> {
        let cpu_count =
            u32::try_from(cpu_count).map_err(|_| MachinePlanError::InvalidFirmware {
                detail: format!("AArch64 vCPU count {cpu_count} exceeds the FDT cell width"),
            })?;
        if cpu_count == 0 {
            return Err(MachinePlanError::InvalidFirmware {
                detail: "AArch64 firmware requires at least one vCPU".into(),
            });
        }
        Ok(Self {
            cpu_count,
            bootargs: None,
            initrd: None,
        })
    }

    /// Adds the kernel command line written to `/chosen`.
    pub fn with_bootargs(mut self, bootargs: impl Into<String>) -> Self {
        self.bootargs = Some(bootargs.into());
        self
    }

    /// Adds an initialized ramdisk range written to `/chosen`.
    pub const fn with_initrd(mut self, initrd: AddressRange) -> Self {
        self.initrd = Some(initrd);
        self
    }

    /// Returns the number of vCPU nodes to emit.
    pub const fn cpu_count(&self) -> u32 {
        self.cpu_count
    }
}

/// Generates an AArch64 GICv3 platform DTB from one finalized machine plan.
///
/// Device MMIO addresses and interrupt identifiers are read only from the
/// plan. The firmware writer does not allocate resources or inspect the host.
pub fn generate_aarch64_fdt(
    plan: &VmMachinePlan,
    config: &Aarch64FdtConfig,
) -> MachinePlanResult<Vec<u8>> {
    let pl011_devices = planned_pl011_devices(plan)?;
    let gic = planned_aarch64_gic(plan)?;
    let serial_path = pl011_devices
        .first()
        .map(|serial| format!("/pl011@{:x}", serial.mmio.base()));

    let mut fdt = FdtWriter::new()?;
    fdt.set_boot_cpuid_phys(0);
    let root = fdt.begin_node("")?;
    fdt.property_string_list(
        "compatible",
        vec!["axvisor,virtual-machine".into(), "linux,dummy-virt".into()],
    )?;
    fdt.property_string("model", "AxVM virtual machine")?;
    fdt.property_u32("#address-cells", 2)?;
    fdt.property_u32("#size-cells", 2)?;
    fdt.property_u32("interrupt-parent", AARCH64_GIC_PHANDLE)?;

    write_chosen(&mut fdt, config, serial_path.as_deref())?;
    write_aliases(&mut fdt, serial_path.as_deref())?;
    write_memory(&mut fdt, plan.fixed_guest_memory())?;
    write_cpus(&mut fdt, config.cpu_count)?;
    write_psci(&mut fdt)?;
    write_gicv3(&mut fdt, gic)?;
    write_timer(&mut fdt)?;
    if !pl011_devices.is_empty() {
        write_pl011_clock(&mut fdt)?;
        for serial in pl011_devices {
            write_pl011(&mut fdt, serial)?;
        }
    }

    fdt.end_node(root)?;
    Ok(fdt.finish()?)
}

fn planned_aarch64_gic(plan: &VmMachinePlan) -> MachinePlanResult<&Aarch64GicV3Plan> {
    match plan.interrupt_controller() {
        Some(InterruptControllerPlan::Aarch64GicV3(gic)) => Ok(gic),
        Some(_) => Err(MachinePlanError::InvalidFirmware {
            detail: "cannot generate an AArch64 FDT from another architecture's controller plan"
                .into(),
        }),
        None => Err(MachinePlanError::InvalidFirmware {
            detail: "cannot generate an AArch64 FDT without a GICv3 controller plan".into(),
        }),
    }
}

#[derive(Clone, Copy)]
struct PlannedPl011 {
    mmio: AddressRange,
    intid: u32,
}

fn planned_pl011_devices(plan: &VmMachinePlan) -> MachinePlanResult<Vec<PlannedPl011>> {
    plan.virtual_devices()
        .iter()
        .filter(|device| device.model_id().as_str() == "arm-pl011")
        .map(|device| {
            let mmio = device
                .mmio()
                .iter()
                .find(|resource| resource.slot().as_str() == "registers")
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: format!(
                        "PL011 instance '{}' has no 'registers' resource",
                        device.instance_id()
                    ),
                })?
                .range();
            let intid = device
                .interrupts()
                .iter()
                .find(|resource| resource.slot().as_str() == "irq")
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: format!(
                        "PL011 instance '{}' has no 'irq' resource",
                        device.instance_id()
                    ),
                })?
                .id();
            if intid < 32 {
                return Err(MachinePlanError::InvalidFirmware {
                    detail: format!(
                        "PL011 instance '{}' uses private INTID {intid}; an SPI is required",
                        device.instance_id()
                    ),
                });
            }
            Ok(PlannedPl011 { mmio, intid })
        })
        .collect()
}

fn write_chosen(
    fdt: &mut FdtWriter,
    config: &Aarch64FdtConfig,
    serial_path: Option<&str>,
) -> vm_fdt::FdtWriterResult<()> {
    let chosen = fdt.begin_node("chosen")?;
    if let Some(bootargs) = config.bootargs.as_deref() {
        fdt.property_string("bootargs", bootargs)?;
    }
    if let Some(path) = serial_path {
        fdt.property_string("stdout-path", &format!("{path}:115200n8"))?;
    }
    if let Some(initrd) = config.initrd {
        fdt.property_u64("linux,initrd-start", initrd.base())?;
        fdt.property_u64("linux,initrd-end", initrd.end())?;
    }
    fdt.end_node(chosen)
}

fn write_aliases(fdt: &mut FdtWriter, serial_path: Option<&str>) -> vm_fdt::FdtWriterResult<()> {
    let aliases = fdt.begin_node("aliases")?;
    if let Some(path) = serial_path {
        fdt.property_string("serial0", path)?;
    }
    fdt.end_node(aliases)
}

fn write_memory(
    fdt: &mut FdtWriter,
    memory: impl IntoIterator<Item = AddressRange>,
) -> vm_fdt::FdtWriterResult<()> {
    for region in memory {
        let node = fdt.begin_node(&format!("memory@{:x}", region.base()))?;
        fdt.property_string("device_type", "memory")?;
        fdt.property_array_u64("reg", &[region.base(), region.size()])?;
        fdt.end_node(node)?;
    }
    Ok(())
}

fn write_cpus(fdt: &mut FdtWriter, cpu_count: u32) -> vm_fdt::FdtWriterResult<()> {
    let cpus = fdt.begin_node("cpus")?;
    fdt.property_u32("#address-cells", 2)?;
    fdt.property_u32("#size-cells", 0)?;
    for cpu in 0..cpu_count {
        let node = fdt.begin_node(&format!("cpu@{cpu:x}"))?;
        fdt.property_string("device_type", "cpu")?;
        fdt.property_string("compatible", "arm,arm-v8")?;
        fdt.property_array_u64("reg", &[u64::from(cpu)])?;
        fdt.property_string("enable-method", "psci")?;
        fdt.end_node(node)?;
    }
    fdt.end_node(cpus)
}

fn write_psci(fdt: &mut FdtWriter) -> vm_fdt::FdtWriterResult<()> {
    let psci = fdt.begin_node("psci")?;
    fdt.property_string_list(
        "compatible",
        vec!["arm,psci-1.0".into(), "arm,psci-0.2".into()],
    )?;
    fdt.property_string("method", "hvc")?;
    fdt.end_node(psci)
}

fn write_gicv3(fdt: &mut FdtWriter, layout: &Aarch64GicV3Plan) -> vm_fdt::FdtWriterResult<()> {
    let distributor = layout.distributor();
    let redistributors = layout.redistributors();
    let gic = fdt.begin_node(&format!("interrupt-controller@{:x}", distributor.base()))?;
    fdt.property_string("compatible", "arm,gic-v3")?;
    fdt.property_null("interrupt-controller")?;
    fdt.property_u32("#interrupt-cells", 3)?;
    fdt.property_u32("#address-cells", 2)?;
    fdt.property_u32("#size-cells", 2)?;
    fdt.property_array_u64(
        "reg",
        &[
            distributor.base(),
            distributor.size(),
            redistributors.base(),
            redistributors.size(),
        ],
    )?;
    fdt.property_u64("redistributor-stride", layout.redistributor_stride())?;
    fdt.property_phandle(AARCH64_GIC_PHANDLE)?;
    fdt.property_null("ranges")?;

    if let Some(region) = layout.its() {
        let its = fdt.begin_node(&format!("its@{:x}", region.base()))?;
        fdt.property_string("compatible", "arm,gic-v3-its")?;
        fdt.property_null("msi-controller")?;
        fdt.property_u32("#msi-cells", 1)?;
        fdt.property_array_u64("reg", &[region.base(), region.size()])?;
        fdt.end_node(its)?;
    }
    fdt.end_node(gic)
}

fn write_timer(fdt: &mut FdtWriter) -> vm_fdt::FdtWriterResult<()> {
    let timer = fdt.begin_node("timer")?;
    fdt.property_string("compatible", "arm,armv8-timer")?;
    fdt.property_null("always-on")?;
    fdt.property_u32("interrupt-parent", AARCH64_GIC_PHANDLE)?;
    fdt.property_array_u32(
        "interrupts",
        &[
            1, 14, 4, // non-secure physical timer, PPI 30
            1, 11, 4, // virtual timer, PPI 27
        ],
    )?;
    fdt.property_string_list("interrupt-names", vec!["phys".into(), "virt".into()])?;
    fdt.end_node(timer)
}

fn write_pl011_clock(fdt: &mut FdtWriter) -> vm_fdt::FdtWriterResult<()> {
    let clock = fdt.begin_node("pl011-clock")?;
    fdt.property_string("compatible", "fixed-clock")?;
    fdt.property_u32("#clock-cells", 0)?;
    fdt.property_u32("clock-frequency", AARCH64_PL011_CLOCK_HZ)?;
    fdt.property_string("clock-output-names", "pl011clk")?;
    fdt.property_phandle(AARCH64_PL011_CLOCK_PHANDLE)?;
    fdt.end_node(clock)
}

fn write_pl011(fdt: &mut FdtWriter, serial: PlannedPl011) -> vm_fdt::FdtWriterResult<()> {
    let node = fdt.begin_node(&format!("pl011@{:x}", serial.mmio.base()))?;
    fdt.property_string_list(
        "compatible",
        vec!["arm,pl011".into(), "arm,primecell".into()],
    )?;
    fdt.property_array_u64("reg", &[serial.mmio.base(), serial.mmio.size()])?;
    fdt.property_u32("interrupt-parent", AARCH64_GIC_PHANDLE)?;
    fdt.property_array_u32("interrupts", &[0, serial.intid - 32, 4])?;
    fdt.property_array_u32(
        "clocks",
        &[AARCH64_PL011_CLOCK_PHANDLE, AARCH64_PL011_CLOCK_PHANDLE],
    )?;
    fdt.property_string_list("clock-names", vec!["uartclk".into(), "apb_pclk".into()])?;
    fdt.property_u32("current-speed", 115_200)?;
    fdt.property_string("status", "okay")?;
    fdt.end_node(node)
}
