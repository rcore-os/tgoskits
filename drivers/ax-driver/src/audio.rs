//! SG2002 firmware resources and one-time transfer to the kernel audio runtime.

use alloc::{format, vec::Vec};

use dma_api::{DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDomainId};
use rdrive::{
    DriverGeneric,
    probe::{OnProbeError, fdt::NodeType},
    register::{FdtInfo, ProbeFdt},
};
pub use sg200x_audio::{Capture, Config, Error, Interrupt};
use sg200x_audio::{DmaRoute, Resources};

use crate::BindingIrq;

crate::model_register!(
    name: "SG2002 onboard microphone",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt { compatibles: &["cvitek,cv182xaadc"], on_probe: probe }],
);

/// Exclusive control and IRQ endpoints, moved out of rdrive exactly once.
pub struct Ports {
    pub capture: Capture,
    pub interrupt: Interrupt,
    pub irqs: [BindingIrq; 2],
}

struct Device(Option<Ports>);
impl DriverGeneric for Device {
    fn name(&self) -> &str {
        "SG2002 microphone"
    }
}

pub fn take() -> Option<Ports> {
    rdrive::get_one::<Device>()?.lock().ok()?.0.take()
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, device) = probe.into_parts();
    unique(&info, "cvitek,cv182xaadc")?;
    let i2s_nodes = info.find_compatible(&["cvitek,cv1835-i2s"]);
    let find_i2s = |id| {
        let mut nodes = i2s_nodes
            .iter()
            .filter(|node| property(**node, "dev-id") == Some(id));
        match (nodes.next(), nodes.next()) {
            (Some(node), None) => Ok(*node),
            _ => Err(missing("unique I2S0/I2S3 resources")),
        }
    };
    let i2s = find_i2s(0)?;
    let mclk = find_i2s(3)?;
    let dma = unique(&info, "snps,dmac-bm")?;
    if mclk.regs().into_iter().next().map(|r| r.address)
        != property(info.node, "clk_source").map(u64::from)
    {
        return Err(missing("I2S3 ADC clock source"));
    }
    let crg = unique(&info, "cvitek,cv181x-clk")?;
    let oscillator = crg
        .as_node()
        .get_property("clocks")
        .and_then(|p| p.get_u32_iter().next())
        .and_then(|ph| info.get_by_phandle(ph.into()))
        .ok_or_else(|| missing("oscillator"))?;
    let oscillator_hz =
        property(oscillator, "clock-frequency").ok_or_else(|| missing("oscillator rate"))?;
    let dma_cells: Vec<_> = i2s
        .as_node()
        .get_property("dmas")
        .ok_or_else(|| missing("rx DMA"))?
        .get_u32_iter()
        .collect();
    if dma_cells.len() != 4
        || info.get_by_phandle(dma_cells[0].into()).map(|n| n.id()) != Some(dma.id())
        || dma_cells[1] >= 8
        || dma_cells[2] > 1
        || dma_cells[3] > 1
    {
        return Err(missing("valid RX DMA route"));
    }
    let dma_mmio = map::<u64>(dma, 0x900)?;
    if dma_mmio.read::<u64>(0x18) & 0xff != 0 {
        return Err(OnProbeError::other(
            "cannot initialize DMA routing with active channels",
        ));
    }
    let remap_node = unique(&info, "cvitek,sysdma_remap")?;
    initialize_routes(remap_node, dma_cells[1] as usize)?;
    let resources = Resources {
        adc: map::<u32>(info.node, 0x20)?,
        dac: map::<u32>(unique(&info, "cvitek,cv182xadac")?, 0x24)?,
        i2s: map::<u32>(i2s, 0x84)?,
        mclk: map::<u32>(mclk, 0x68)?,
        aiao: map::<u32>(unique(&info, "cvitek,i2s_tdm_subsys")?, 0x64)?,
        crg: map::<u32>(crg, 0x858)?,
        reset: map::<u32>(unique(&info, "cvitek,reset")?, 0xc)?,
        dma: dma_mmio,
        oscillator_hz,
    };
    let allocator = axklib::dma::device(DmaDeviceInfo::new(
        DmaDomainId::Direct,
        DmaCoherency::NonCoherent,
        DmaConstraints::new(u32::MAX as u64),
    ));
    let route = DmaRoute {
        channel: 0,
        request: dma_cells[1] as u8,
        memory_master: dma_cells[2] as u8,
        peripheral_master: dma_cells[3] as u8,
    };
    let irqs = [irq(&info, dma)?, irq(&info, i2s)?];
    // SAFETY: rdrive probes serially. No other TG driver owns the system DMAC
    // or ADC/I2S0/I2S3. All resource ranges are validated against the FDT;
    // startup routing rejects active DMA users. Runtime owns only channel 0.
    let (capture, interrupt) = unsafe { Capture::new(resources, route, allocator) }
        .map_err(|error| OnProbeError::other(format!("audio: {error}")))?;
    device.register(Device(Some(Ports {
        capture,
        interrupt,
        irqs,
    })));
    Ok(())
}

fn unique<'a>(info: &FdtInfo<'a>, compatible: &str) -> Result<NodeType<'a>, OnProbeError> {
    let nodes = info.find_compatible(&[compatible]);
    match nodes.as_slice() {
        [node] => Ok(*node),
        _ => Err(missing(compatible)),
    }
}

fn property(node: NodeType<'_>, name: &str) -> Option<u32> {
    node.as_node().get_property(name)?.get_u32()
}

fn map<T>(node: NodeType<'_>, minimum: usize) -> Result<mmio_api::Mmio, OnProbeError> {
    let reg = node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| missing("reg"))?;
    let size = reg.size.ok_or_else(|| missing("reg size"))? as usize;
    let alignment = core::mem::align_of::<T>();
    if size < minimum || !(reg.address as usize).is_multiple_of(alignment) {
        return Err(OnProbeError::other(format!(
            "SG2002 audio invalid reg range {:#x}+{size:#x}: requires {minimum:#x} bytes aligned \
             to {alignment}",
            reg.address,
        )));
    }
    mmio_api::ioremap(reg.address.into(), size)
        .map_err(|e| OnProbeError::other(format!("audio MMIO: {e}")))
}

fn irq(info: &FdtInfo<'_>, node: NodeType<'_>) -> Result<BindingIrq, OnProbeError> {
    let interrupt = node
        .interrupts()
        .into_iter()
        .next()
        .ok_or_else(|| missing("interrupt"))?;
    let controller = info
        .phandle_to_device_id(interrupt.interrupt_parent)
        .ok_or_else(|| missing("interrupt-parent"))?;
    Ok(BindingIrq::fdt_interrupt_with_controller(
        controller,
        interrupt.specifier,
    ))
}

fn initialize_routes(node: NodeType<'_>, request: usize) -> Result<(), OnProbeError> {
    let routes: Vec<_> = node
        .as_node()
        .get_property("ch-remap")
        .ok_or_else(|| missing("ch-remap"))?
        .get_u32_iter()
        .collect();
    if routes.len() != 8
        || routes[request] != 0
        || routes.iter().any(|r| *r > 63)
        || routes.iter().filter(|r| **r == 0).count() != 1
    {
        return Err(missing("unique I2S0 RX mapping"));
    }
    let remap = map::<u32>(node, 8)?;
    let mux_address = property(node, "int_mux_base").ok_or_else(|| missing("int_mux_base"))?;
    let mux_value = property(node, "int_mux").ok_or_else(|| missing("int_mux"))?;
    if !mux_address.is_multiple_of(4) {
        return Err(missing("aligned int_mux_base"));
    }
    let mux = mmio_api::ioremap((mux_address as usize).into(), 4)
        .map_err(|e| OnProbeError::other(format!("DMA mux: {e}")))?;
    for (i, group) in routes.as_chunks::<4>().0.iter().enumerate() {
        let value = group
            .iter()
            .enumerate()
            .fold(1u32 << 31, |value, (n, route)| value | (route << (8 * n)));
        remap.write(i * 4, value);
    }
    mux.write(0, mux_value);
    Ok(())
}

fn missing(resource: &str) -> OnProbeError {
    OnProbeError::other(format!("SG2002 audio requires {resource}"))
}
