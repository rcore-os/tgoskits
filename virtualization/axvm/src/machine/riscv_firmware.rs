//! RISC-V firmware descriptions generated from finalized machine resources.

use alloc::{format, string::String, vec::Vec};

use vm_fdt::FdtWriter;

use super::{
    AddressRange, InterruptControllerPlan, MachinePlanError, MachinePlanResult, RiscvPlicPlan,
    VmMachinePlan,
};

const PLIC_PHANDLE: u32 = 1;
const CPU_INTC_PHANDLE_BASE: u32 = 0x100;
const TIMEBASE_FREQUENCY_HZ: u32 = 10_000_000;
const UART_CLOCK_HZ: u32 = 3_686_400;

/// Guest-specific properties needed in addition to a finalized RISC-V plan.
#[derive(Clone, Debug)]
pub struct RiscvFdtConfig {
    cpu_count: u32,
    bootargs: Option<String>,
    initrd: Option<AddressRange>,
}

impl RiscvFdtConfig {
    /// Creates a configuration for sequential guest hart identifiers.
    pub fn new(cpu_count: usize) -> MachinePlanResult<Self> {
        let cpu_count =
            u32::try_from(cpu_count).map_err(|_| MachinePlanError::InvalidFirmware {
                detail: format!("RISC-V vCPU count {cpu_count} exceeds the FDT cell width"),
            })?;
        if cpu_count == 0 {
            return Err(MachinePlanError::InvalidFirmware {
                detail: "RISC-V firmware requires at least one vCPU".into(),
            });
        }
        CPU_INTC_PHANDLE_BASE
            .checked_add(cpu_count)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: "RISC-V CPU interrupt-controller phandles overflow".into(),
            })?;
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
}

/// Generates a RISC-V PLIC/SBI platform DTB from one finalized machine plan.
pub fn generate_riscv_fdt(
    plan: &VmMachinePlan,
    config: &RiscvFdtConfig,
) -> MachinePlanResult<Vec<u8>> {
    let plic = planned_plic(plan)?;
    let serials = planned_ns16550_devices(plan)?;
    let serial_path = serials
        .first()
        .map(|serial| format!("/soc/serial@{:x}", serial.mmio.base()));

    if plic.context_count() != config.cpu_count as usize * 2 {
        return Err(MachinePlanError::InvalidFirmware {
            detail: format!(
                "PLIC has {} contexts but {} are required for {} vCPUs",
                plic.context_count(),
                config.cpu_count * 2,
                config.cpu_count
            ),
        });
    }

    let mut fdt = FdtWriter::new()?;
    fdt.set_boot_cpuid_phys(0);
    let root = fdt.begin_node("")?;
    fdt.property_string_list(
        "compatible",
        alloc::vec!["axvisor,riscv-virt".into(), "riscv-virtio".into()],
    )?;
    fdt.property_string("model", "AxVM RISC-V virtual machine")?;
    fdt.property_u32("#address-cells", 2)?;
    fdt.property_u32("#size-cells", 2)?;

    write_chosen(&mut fdt, config, serial_path.as_deref())?;
    write_aliases(&mut fdt, serial_path.as_deref())?;
    write_memory(&mut fdt, plan.guest_memory())?;
    write_cpus(&mut fdt, config.cpu_count)?;
    write_sbi(&mut fdt)?;
    write_soc(&mut fdt, plic, &serials, config.cpu_count)?;

    fdt.end_node(root)?;
    Ok(fdt.finish()?)
}

fn planned_plic(plan: &VmMachinePlan) -> MachinePlanResult<&RiscvPlicPlan> {
    match plan.interrupt_controller() {
        Some(InterruptControllerPlan::RiscvPlic(plic)) => Ok(plic),
        Some(_) => Err(MachinePlanError::InvalidFirmware {
            detail: "cannot generate a RISC-V FDT from another architecture's controller plan"
                .into(),
        }),
        None => Err(MachinePlanError::InvalidFirmware {
            detail: "cannot generate a RISC-V FDT without a PLIC controller plan".into(),
        }),
    }
}

#[derive(Clone, Copy)]
struct PlannedNs16550 {
    mmio: AddressRange,
    interrupt: u32,
}

fn planned_ns16550_devices(plan: &VmMachinePlan) -> MachinePlanResult<Vec<PlannedNs16550>> {
    plan.virtual_devices()
        .iter()
        .filter(|device| device.model_id().as_str() == "ns16550a")
        .map(|device| {
            let mmio = device
                .mmio()
                .iter()
                .find(|resource| resource.slot().as_str() == "registers")
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: format!(
                        "16550 instance '{}' has no 'registers' resource",
                        device.instance_id()
                    ),
                })?
                .range();
            let interrupt = device
                .interrupts()
                .iter()
                .find(|resource| resource.slot().as_str() == "irq")
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: format!(
                        "16550 instance '{}' has no 'irq' resource",
                        device.instance_id()
                    ),
                })?
                .id();
            Ok(PlannedNs16550 { mmio, interrupt })
        })
        .collect()
}

fn write_chosen(
    fdt: &mut FdtWriter,
    config: &RiscvFdtConfig,
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

fn write_memory(fdt: &mut FdtWriter, memory: &[AddressRange]) -> vm_fdt::FdtWriterResult<()> {
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
    fdt.property_u32("#address-cells", 1)?;
    fdt.property_u32("#size-cells", 0)?;
    fdt.property_u32("timebase-frequency", TIMEBASE_FREQUENCY_HZ)?;
    for hart in 0..cpu_count {
        let cpu = fdt.begin_node(&format!("cpu@{hart:x}"))?;
        fdt.property_string("device_type", "cpu")?;
        fdt.property_string("compatible", "riscv")?;
        fdt.property_u32("reg", hart)?;
        fdt.property_string("status", "okay")?;
        fdt.property_string("riscv,isa", "rv64imafdc")?;
        fdt.property_string("mmu-type", "riscv,sv48")?;
        let intc = fdt.begin_node("interrupt-controller")?;
        fdt.property_string("compatible", "riscv,cpu-intc")?;
        fdt.property_null("interrupt-controller")?;
        fdt.property_u32("#interrupt-cells", 1)?;
        fdt.property_phandle(CPU_INTC_PHANDLE_BASE + hart)?;
        fdt.end_node(intc)?;
        fdt.end_node(cpu)?;
    }
    fdt.end_node(cpus)
}

fn write_sbi(fdt: &mut FdtWriter) -> vm_fdt::FdtWriterResult<()> {
    let sbi = fdt.begin_node("sbi")?;
    fdt.property_string("compatible", "riscv,sbi")?;
    fdt.property_string("method", "ecall")?;
    fdt.end_node(sbi)
}

fn write_soc(
    fdt: &mut FdtWriter,
    plic: &RiscvPlicPlan,
    serials: &[PlannedNs16550],
    cpu_count: u32,
) -> vm_fdt::FdtWriterResult<()> {
    let soc = fdt.begin_node("soc")?;
    fdt.property_string("compatible", "simple-bus")?;
    fdt.property_u32("#address-cells", 2)?;
    fdt.property_u32("#size-cells", 2)?;
    fdt.property_null("ranges")?;

    write_plic(fdt, plic, cpu_count)?;
    for serial in serials {
        write_ns16550(fdt, *serial)?;
    }
    fdt.end_node(soc)
}

fn write_plic(
    fdt: &mut FdtWriter,
    plic: &RiscvPlicPlan,
    cpu_count: u32,
) -> vm_fdt::FdtWriterResult<()> {
    let mmio = plic.mmio();
    let node = fdt.begin_node(&format!("interrupt-controller@{:x}", mmio.base()))?;
    fdt.property_string_list(
        "compatible",
        alloc::vec!["sifive,plic-1.0.0".into(), "riscv,plic0".into()],
    )?;
    fdt.property_null("interrupt-controller")?;
    fdt.property_u32("#interrupt-cells", 1)?;
    fdt.property_array_u64("reg", &[mmio.base(), mmio.size()])?;
    fdt.property_u32("riscv,ndev", plic.source_count())?;
    fdt.property_phandle(PLIC_PHANDLE)?;
    let mut contexts = Vec::with_capacity(cpu_count as usize * 4);
    for hart in 0..cpu_count {
        let phandle = CPU_INTC_PHANDLE_BASE + hart;
        contexts.extend_from_slice(&[phandle, 11, phandle, 9]);
    }
    fdt.property_array_u32("interrupts-extended", &contexts)?;
    fdt.end_node(node)
}

fn write_ns16550(fdt: &mut FdtWriter, serial: PlannedNs16550) -> vm_fdt::FdtWriterResult<()> {
    let node = fdt.begin_node(&format!("serial@{:x}", serial.mmio.base()))?;
    fdt.property_string("compatible", "ns16550a")?;
    fdt.property_array_u64("reg", &[serial.mmio.base(), serial.mmio.size()])?;
    fdt.property_u32("interrupt-parent", PLIC_PHANDLE)?;
    fdt.property_u32("interrupts", serial.interrupt)?;
    fdt.property_u32("clock-frequency", UART_CLOCK_HZ)?;
    fdt.property_u32("current-speed", 115_200)?;
    fdt.property_u32("reg-io-width", 1)?;
    fdt.property_string("status", "okay")?;
    fdt.end_node(node)
}
