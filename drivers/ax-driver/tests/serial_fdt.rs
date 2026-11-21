#![cfg(feature = "serial")]

mod common;

use core::ptr::NonNull;

use ax_driver::register::DriverRegister;
use fdt_edit::{Fdt, Node, Property};
use rdif_clk::{Clk, ClockId, DriverGeneric, Interface, KError};
use rdif_serial::{Config, ConfigError};
use rdrive::{
    Platform,
    probe::OnProbeError,
    register::{ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
};

const UART_UNRESOLVED_PADDR: usize = 0x1000;
const UART_FIXED_CLOCK_PADDR: usize = 0x2000;
const UART_FIRMWARE_SPEED_PADDR: usize = 0x3000;
const UART_UNNAMED_CLOCK_PADDR: usize = 0x4000;
const UART_MAIN_CLOCK_PADDR: usize = 0x5000;
const UART_INDEXED_CLOCK_PADDR: usize = 0x6000;
const UART_MMIO_SIZE: usize = 0x100;
const UART_LCR: usize = 3;
const UART_CORE_RATE: u32 = 14_745_600;
const UART_CLOCK_SELECTOR: u32 = 7;
const UART_UNREADABLE_CLOCK_SELECTOR: u32 = 8;

unsafe extern "Rust" {
    #[link_name = "__DRIVER_NS16550_SERIAL"]
    static NS16550_DRIVER: DriverRegister;
}

struct IndexedClock;

impl DriverGeneric for IndexedClock {
    fn name(&self) -> &str {
        "UART test clock"
    }
}

impl Interface for IndexedClock {
    fn perper_enable(&mut self) {}

    fn get_rate(&self, id: ClockId) -> Result<u64, KError> {
        if id.raw() == UART_UNREADABLE_CLOCK_SELECTOR as usize {
            return Err(KError::Again);
        }
        assert_eq!(id.raw(), UART_CLOCK_SELECTOR as usize);
        Ok(UART_CORE_RATE.into())
    }

    fn set_rate(&mut self, _id: ClockId, _rate: u64) -> Result<(), KError> {
        unreachable!("the UART fixture only reads the clock rate")
    }
}

fn probe_indexed_clock(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(Clk::new(IndexedClock));
    Ok(())
}

static INDEXED_CLOCK_DRIVER: DriverRegister = DriverRegister {
    name: "UART test clock",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,uart-clock"],
        on_probe: probe_indexed_clock,
    }],
};

#[test]
fn xscale_uart_uses_resolved_core_clock_and_preserves_unresolved_divisor() {
    let unresolved_reg = map_uart(UART_UNRESOLVED_PADDR);
    let fixed_uarts = [
        UART_FIXED_CLOCK_PADDR,
        UART_UNNAMED_CLOCK_PADDR,
        UART_MAIN_CLOCK_PADDR,
        UART_INDEXED_CLOCK_PADDR,
    ]
    .map(|paddr| (paddr, map_uart(paddr)));
    let firmware_reg = map_uart(UART_FIRMWARE_SPEED_PADDR);
    let encoded = Box::leak(Box::new(xscale_uart_fdt().encode()));
    let addr = NonNull::new(encoded.as_ref().as_ptr().cast_mut()).unwrap();
    rdrive::init(Platform::Fdt { addr }).unwrap();
    // SAFETY: ax-driver defines this linked registration with this exact type.
    rdrive::register_add(unsafe { NS16550_DRIVER.clone() });
    rdrive::register_add(INDEXED_CLOCK_DRIVER.clone());
    rdrive::probe_all(true).unwrap();

    let mut devices = ax_driver::serial::take_serial_devices();
    assert_eq!(devices.len(), 6);
    let uart = devices
        .iter_mut()
        .find(|uart| uart.info.address.base() == UART_UNRESOLVED_PADDR)
        .unwrap();
    assert_eq!(uart.info.initial_baudrate, 0);
    assert_eq!(
        uart.port.set_config(&Config::new().baudrate(115_200)),
        Err(ConfigError::BaudrateUnavailable)
    );
    uart.port.startup(&Config::new()).unwrap();
    // SAFETY: the test-owned mapping was leaked and has no concurrent users.
    assert_eq!(unsafe { unresolved_reg.read_volatile() }, 8);

    for (paddr, regs) in fixed_uarts {
        let uart = devices
            .iter_mut()
            .find(|uart| uart.info.address.base() == paddr)
            .unwrap();
        assert_eq!(uart.info.initial_baudrate, 115_200, "UART at {paddr:#x}");
        uart.port
            .set_config(&Config::new().baudrate(230_400))
            .unwrap();
        // SAFETY: the test-owned mapping was leaked and has no concurrent users.
        assert_eq!(unsafe { regs.read_volatile() }, 4);
    }

    let uart = devices
        .iter_mut()
        .find(|uart| uart.info.address.base() == UART_FIRMWARE_SPEED_PADDR)
        .unwrap();
    assert_eq!(uart.info.initial_baudrate, 115_200);
    uart.port.startup(&Config::new()).unwrap();
    assert_eq!(
        uart.port.set_config(&Config::new().baudrate(230_400)),
        Err(ConfigError::BaudrateUnavailable)
    );
    // SAFETY: the test-owned mapping was leaked and has no concurrent users.
    assert_eq!(unsafe { firmware_reg.read_volatile() }, 8);
}

fn map_uart(paddr: usize) -> *mut u32 {
    let regs = Box::leak(Box::new([0u32; UART_MMIO_SIZE / 4]));
    regs[0] = 8;
    regs[UART_LCR] = 0x03;
    let base = regs.as_mut_ptr();
    common::MMIO_MAPPINGS
        .lock_irqsave()
        .push(common::MmioMapping {
            paddr,
            size: UART_MMIO_SIZE,
            vaddr: base as usize,
        });
    base
}

fn cells(name: &str, values: &[u32]) -> Property {
    Property::new(
        name,
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect(),
    )
}

fn xscale_uart_fdt() -> Fdt {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    let root_node = fdt.node_mut(root).unwrap();
    root_node.set_property(cells("#address-cells", &[1]));
    root_node.set_property(cells("#size-cells", &[1]));

    let provider = fdt.add_node(root, Node::new("clock-controller"));
    let provider = fdt.node_mut(provider).unwrap();
    provider.set_property(cells("phandle", &[1]));
    provider.set_property(cells("#clock-cells", &[1]));

    let provider = fdt.add_node(root, Node::new("uart-clock"));
    let provider = fdt.node_mut(provider).unwrap();
    provider.set_property(Property::new("compatible", b"fixed-clock\0".to_vec()));
    provider.set_property(cells("phandle", &[2]));
    provider.set_property(cells("#clock-cells", &[0]));
    provider.set_property(cells("clock-frequency", &[UART_CORE_RATE]));

    let provider = fdt.add_node(root, Node::new("indexed-clock"));
    let provider = fdt.node_mut(provider).unwrap();
    provider.set_property(Property::new("compatible", b"test,uart-clock\0".to_vec()));
    provider.set_property(cells("phandle", &[3]));
    provider.set_property(cells("#clock-cells", &[1]));

    for (paddr, clock_refs) in [
        (UART_UNRESOLVED_PADDR, [1, 0, 1, 1].as_slice()),
        (UART_FIXED_CLOCK_PADDR, [2, 1, 1].as_slice()),
        (
            UART_FIRMWARE_SPEED_PADDR,
            [3, UART_UNREADABLE_CLOCK_SELECTOR, 1, 1].as_slice(),
        ),
        (UART_UNNAMED_CLOCK_PADDR, [2].as_slice()),
        (UART_MAIN_CLOCK_PADDR, [2].as_slice()),
        (
            UART_INDEXED_CLOCK_PADDR,
            [3, UART_CLOCK_SELECTOR, 1, 1].as_slice(),
        ),
    ] {
        let uart = fdt.add_node(root, Node::new(&format!("serial@{paddr:x}")));
        let uart = fdt.node_mut(uart).unwrap();
        let compatible = if matches!(paddr, UART_UNNAMED_CLOCK_PADDR | UART_MAIN_CLOCK_PADDR) {
            b"intel,xscale-uart\0".as_slice()
        } else {
            b"spacemit,k3-uart\0intel,xscale-uart\0".as_slice()
        };
        uart.set_property(Property::new("compatible", compatible.to_vec()));
        uart.set_property(cells("reg", &[paddr as u32, UART_MMIO_SIZE as u32]));
        uart.set_property(cells("reg-shift", &[2]));
        uart.set_property(cells("reg-io-width", &[4]));
        uart.set_property(cells("clocks", clock_refs));
        let clock_names = match paddr {
            UART_UNNAMED_CLOCK_PADDR => None,
            UART_MAIN_CLOCK_PADDR => Some(b"main\0".as_slice()),
            _ => Some(b"core\0bus\0".as_slice()),
        };
        if let Some(names) = clock_names {
            uart.set_property(Property::new("clock-names", names.to_vec()));
        }
        if paddr == UART_FIRMWARE_SPEED_PADDR {
            uart.set_property(cells("current-speed", &[115_200]));
        }
    }
    fdt
}
