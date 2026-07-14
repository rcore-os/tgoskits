extern crate alloc;

use alloc::{format, string::ToString, vec::Vec};
use core::{ptr::NonNull, time::Duration};

use crab_usb::{
    Dwc2FifoSizes, Dwc2HostParams, Dwc2NewParams, Dwc2Quirks, Dwc2UtmiWidth, USBHost, usb_if::Speed,
};
use fdt_edit::{Node, RegFixed};
use log::{debug, info, warn};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sg200x_bsp::{
    gpio::{Direction, GPIO},
    soc::{
        CLKGEN_BASE, CV182X_USB2_PHY_BASE, FMUX_BASE, GPIO1_BASE, IOBLK_BASE, IOBLK_GRTC_BASE,
        TOP_BASE,
    },
};

use super::{ProbeFdtUsbHost, usb_kernel};
use crate::mmio::iomap;

const DRIVER_NAME: &str = "usb-sg2002-dwc2";

const REG_MMIO_SIZE: usize = 0x1000;
const TOP_MMIO_SIZE: usize = 0x4000;
const DWC2_MMIO_DEFAULT_SIZE: usize = 0x10000;

const CLKGEN_CLK_EN_1: usize = 0x004;
const CLKGEN_CLK_EN_2: usize = 0x008;
const CLKGEN_CLK_BYP_0: usize = 0x030;
const CLKGEN_USB_CLK_EN_1_BITS: u32 = 0x0f << 28;
const CLKGEN_USB_CLK_EN_2_BITS: u32 = 1;
const CLKGEN_USB_BYP0_CLEAR_BITS: u32 = (1 << 17) | (1 << 18);

const TOP_USB_PHY_RESET: usize = 0x3000;
const TOP_USB_IDPAD: usize = 0x048;
const TOP_USB_ECO: usize = 0x0b4;
const TOP_USB_PHY_RESET_N: u32 = 1 << 11;
const TOP_USB_IDPAD_MODE_MASK: u32 = 0xc0;
const TOP_USB_IDPAD_DEVICE_MODE: u32 = 0xc0 | 0x01;
const TOP_USB_IDPAD_HOST_MODE: u32 = 0x40 | 0x01;
const TOP_USB_ECO_HOST_BIT: u32 = 0x80;

const FMUX_USB_VBUS_DET: usize = 0x0fc;
const FMUX_USB_VBUS_DET_XGPIOB6: u32 = 3;
const IOBLK_G1_USB_VBUS_DET: usize = 0x020;
const IOBLK_USB_VBUS_DET_DRIVE_MASK: u32 = 7 << 5;

const VBUS_GPIO_PIN: u8 = 6;
const CV182X_USB2_PHY_REG014: usize = 0x014;
const VBUS_SETTLE_DELAY: Duration = Duration::from_secs(2);

crate::model_register!(
    name: "SG2002 DWC2 USB2 Host",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["cvitek,cv182x-usb"],
            on_probe: probe
        }
    ],
);

#[derive(Clone)]
struct Sg2002Dwc2Resources {
    ctrl: RegFixed,
    phy: RegFixed,
    params: Dwc2HostParams,
    vbus_gpio: Option<VbusGpio>,
}

#[derive(Debug, Clone, Copy)]
struct VbusGpio {
    pin: u8,
    active_low: bool,
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    match prop_str(info.node.as_node(), "dr_mode") {
        Some("host" | "otg") => {}
        Some(mode) => {
            debug!(
                "skip CV182x DWC2 node {} because dr_mode={mode}",
                info.node.name()
            );
            return Err(OnProbeError::NotMatch);
        }
        None => {
            debug!(
                "skip CV182x DWC2 node {} because dr_mode is missing",
                info.node.name()
            );
            return Err(OnProbeError::NotMatch);
        }
    }

    let resources = collect_resources(info)?;
    sg2002_board_usb_host_init(&resources)?;

    let ctrl = map_reg(resources.ctrl, DWC2_MMIO_DEFAULT_SIZE)?;
    let host = USBHost::new_dwc2(Dwc2NewParams {
        mmio: ctrl,
        kernel: usb_kernel(),
        params: resources.params,
    })
    .map_err(|err| {
        OnProbeError::other(format!(
            "failed to create SG2002 DWC2 host for [{}]: {err}",
            info.node.name()
        ))
    })?;

    let node_name = info.node.name().to_string();
    let irq = probe.register_usb_host_with_root_hub_speed(DRIVER_NAME, host, Speed::High)?;
    info!(
        "SG2002 DWC2 USB2 host initialized for {} with irq {:?}",
        node_name, irq
    );
    Ok(())
}

fn collect_resources(info: &FdtInfo<'_>) -> Result<Sg2002Dwc2Resources, OnProbeError> {
    let regs = info.node.regs();
    let ctrl = regs
        .first()
        .copied()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no DWC2 reg", info.node.name())))?;
    let phy = regs.get(1).copied().unwrap_or(RegFixed {
        address: CV182X_USB2_PHY_BASE as u64,
        size: Some(REG_MMIO_SIZE as u64),
        child_bus_address: CV182X_USB2_PHY_BASE as u64,
    });

    Ok(Sg2002Dwc2Resources {
        ctrl,
        phy,
        params: parse_dwc2_params(info.node.as_node()),
        vbus_gpio: parse_vbus_gpio(info.node.as_node()),
    })
}

fn parse_dwc2_params(node: &Node) -> Dwc2HostParams {
    let mut params = Dwc2HostParams::sg2002();
    params.fifo = Dwc2FifoSizes {
        rx_depth: prop_u32(node, "g-rx-fifo-size")
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(params.fifo.rx_depth),
        non_periodic_tx_depth: prop_u32(node, "g-np-tx-fifo-size")
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(params.fifo.non_periodic_tx_depth),
        periodic_tx_depth: prop_u32_list(node, "g-tx-fifo-size")
            .first()
            .copied()
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(params.fifo.periodic_tx_depth),
    };
    params.utmi = Dwc2UtmiWidth::Auto;
    params.quirks = Dwc2Quirks {
        otg_host_session_override: true,
        clear_utmi_override: true,
    };
    params
}

fn parse_vbus_gpio(node: &Node) -> Option<VbusGpio> {
    let cells = prop_u32_list(node, "vbus-gpio");
    if cells.len() < 3 {
        return None;
    }
    Some(VbusGpio {
        pin: cells[1] as u8,
        active_low: cells[2] & 1 != 0,
    })
}

fn sg2002_board_usb_host_init(resources: &Sg2002Dwc2Resources) -> Result<(), OnProbeError> {
    enable_usb_clocks()?;
    usb_top_host_bringup()?;
    prepare_vbus_pin(resources.vbus_gpio)?;
    enable_vbus_gpio(resources.vbus_gpio)?;
    clear_usb2_phy_utmi_override(resources.phy)?;
    axklib::time::busy_wait(VBUS_SETTLE_DELAY);
    Ok(())
}

fn enable_usb_clocks() -> Result<(), OnProbeError> {
    let clkgen = map_fixed(CLKGEN_BASE, REG_MMIO_SIZE, "SG2002 CLKGEN")?;
    update32(clkgen, CLKGEN_CLK_EN_1, |value| {
        value | CLKGEN_USB_CLK_EN_1_BITS
    });
    update32(clkgen, CLKGEN_CLK_EN_2, |value| {
        value | CLKGEN_USB_CLK_EN_2_BITS
    });
    update32(clkgen, CLKGEN_CLK_BYP_0, |value| {
        value & !CLKGEN_USB_BYP0_CLEAR_BITS
    });
    Ok(())
}

fn usb_top_host_bringup() -> Result<(), OnProbeError> {
    let top = map_fixed(TOP_BASE, TOP_MMIO_SIZE, "SG2002 TOP")?;
    let reset_value = read32(top, TOP_USB_PHY_RESET);
    write32(top, TOP_USB_PHY_RESET, reset_value & !TOP_USB_PHY_RESET_N);
    axklib::time::busy_wait(Duration::from_micros(50));
    write32(top, TOP_USB_PHY_RESET, reset_value | TOP_USB_PHY_RESET_N);
    axklib::time::busy_wait(Duration::from_micros(50));

    let idpad = read32(top, TOP_USB_IDPAD);
    write32(
        top,
        TOP_USB_IDPAD,
        (idpad & !TOP_USB_IDPAD_MODE_MASK) | TOP_USB_IDPAD_DEVICE_MODE,
    );
    axklib::time::busy_wait(Duration::from_millis(1));
    write32(
        top,
        TOP_USB_IDPAD,
        (idpad & !TOP_USB_IDPAD_MODE_MASK) | TOP_USB_IDPAD_HOST_MODE,
    );
    axklib::time::busy_wait(Duration::from_millis(1));

    update32(top, TOP_USB_ECO, |value| value | TOP_USB_ECO_HOST_BIT);
    Ok(())
}

fn prepare_vbus_pin(vbus: Option<VbusGpio>) -> Result<(), OnProbeError> {
    if let Some(vbus) = vbus
        && vbus.pin != VBUS_GPIO_PIN
    {
        warn!(
            "SG2002 DWC2 vbus-gpio pin {} is not GPIOB{}, using fixed board pin",
            vbus.pin, VBUS_GPIO_PIN
        );
    }

    let fmux = map_fixed(FMUX_BASE, REG_MMIO_SIZE, "SG2002 FMUX")?;
    let ioblk = map_fixed(IOBLK_BASE, REG_MMIO_SIZE, "SG2002 IOBLK")?;
    let _ioblk_grtc = map_fixed(IOBLK_GRTC_BASE, REG_MMIO_SIZE, "SG2002 IOBLK GRTC")?;

    write32(fmux, FMUX_USB_VBUS_DET, FMUX_USB_VBUS_DET_XGPIOB6);
    update32(ioblk, IOBLK_G1_USB_VBUS_DET, |value| {
        value | IOBLK_USB_VBUS_DET_DRIVE_MASK
    });
    Ok(())
}

fn enable_vbus_gpio(vbus: Option<VbusGpio>) -> Result<(), OnProbeError> {
    let active_high = !vbus.is_some_and(|gpio| gpio.active_low);
    let gpio1 = map_fixed(GPIO1_BASE, REG_MMIO_SIZE, "SG2002 GPIO1")?;
    let gpio = unsafe {
        // SAFETY: `gpio1` is an ioremapped GPIO1 register block owned by this
        // board bring-up path during probe.
        GPIO::new(gpio1.as_ptr() as usize)
    };
    let pin = gpio.pin(VBUS_GPIO_PIN);
    pin.set_direction(Direction::Output);
    pin.set(active_high);
    Ok(())
}

fn clear_usb2_phy_utmi_override(phy: RegFixed) -> Result<(), OnProbeError> {
    let phy = map_reg(phy, REG_MMIO_SIZE)?;
    write32(phy, CV182X_USB2_PHY_REG014, 0);
    Ok(())
}

fn map_reg(reg: RegFixed, default_size: usize) -> Result<NonNull<u8>, OnProbeError> {
    let size = align_up_4k((reg.size.unwrap_or(default_size as u64) as usize).max(1));
    iomap(reg.address as usize, size)
}

fn map_fixed(address: usize, size: usize, name: &str) -> Result<NonNull<u8>, OnProbeError> {
    iomap(address, size).map_err(|err| OnProbeError::other(format!("failed to map {name}: {err}")))
}

fn read32(base: NonNull<u8>, offset: usize) -> u32 {
    unsafe {
        // SAFETY: callers pass an ioremapped MMIO base and a register offset
        // within that mapping.
        (base.as_ptr().add(offset) as *const u32).read_volatile()
    }
}

fn write32(base: NonNull<u8>, offset: usize, value: u32) {
    unsafe {
        // SAFETY: callers pass an ioremapped MMIO base and a register offset
        // within that mapping.
        (base.as_ptr().add(offset) as *mut u32).write_volatile(value)
    }
}

fn update32(base: NonNull<u8>, offset: usize, f: impl FnOnce(u32) -> u32) {
    let value = read32(base, offset);
    write32(base, offset, f(value));
}

fn prop_str<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    node.get_property(name).and_then(|prop| prop.as_str())
}

fn prop_u32(node: &Node, name: &str) -> Option<u32> {
    node.get_property(name).and_then(|prop| prop.get_u32())
}

fn prop_u32_list(node: &Node, name: &str) -> Vec<u32> {
    node.get_property(name)
        .map(|prop| prop.get_u32_iter().collect())
        .unwrap_or_default()
}

fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}
