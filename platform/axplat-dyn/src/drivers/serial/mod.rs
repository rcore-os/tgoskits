// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(feature = "irq")]
use ax_plat::irq;
use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};
use some_serial::{InterfaceRaw, InterruptMask, ns16550, pl011};
use spin::Mutex;

use crate::drivers::iomap;

const MAX_SERIAL_IRQS: usize = 4;
const NO_IRQ: usize = usize::MAX;

static CONSOLE_IRQ: AtomicUsize = AtomicUsize::new(NO_IRQ);
static CONSOLE_SERIAL_SELECTED: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "irq")]
static SERIAL_IRQ_NUMS: [AtomicUsize; MAX_SERIAL_IRQS] = [
    AtomicUsize::new(NO_IRQ),
    AtomicUsize::new(NO_IRQ),
    AtomicUsize::new(NO_IRQ),
    AtomicUsize::new(NO_IRQ),
];
#[cfg(feature = "irq")]
static SERIAL_IRQ_HANDLERS: [Mutex<Option<some_serial::BIrqHandler>>; MAX_SERIAL_IRQS] = [
    Mutex::new(None),
    Mutex::new(None),
    Mutex::new(None),
    Mutex::new(None),
];

trait PlatformSerialDevice: Send {
    fn name(&self) -> &str;

    fn base_addr(&self) -> usize;

    #[cfg(feature = "irq")]
    fn take_irq_handler(&mut self) -> Option<some_serial::BIrqHandler>;

    #[cfg(feature = "irq")]
    fn enable_interrupts(&mut self, mask: InterruptMask);
}

impl<T> PlatformSerialDevice for T
where
    T: InterfaceRaw + Send,
{
    fn name(&self) -> &str {
        InterfaceRaw::name(self)
    }

    fn base_addr(&self) -> usize {
        InterfaceRaw::base_addr(self)
    }

    #[cfg(feature = "irq")]
    fn take_irq_handler(&mut self) -> Option<some_serial::BIrqHandler> {
        InterfaceRaw::irq_handler(self).map(|handler| Box::new(handler) as some_serial::BIrqHandler)
    }

    #[cfg(feature = "irq")]
    fn enable_interrupts(&mut self, mask: InterruptMask) {
        let mut enabled = InterfaceRaw::get_irq_mask(self);
        enabled |= mask;
        InterfaceRaw::set_irq_mask(self, enabled);
    }
}

impl rdrive::DriverGeneric for Box<dyn PlatformSerialDevice> {
    fn name(&self) -> &str {
        PlatformSerialDevice::name(self.as_ref())
    }
}

type DynPlatformSerial = Box<dyn PlatformSerialDevice>;

module_driver!(
    name: "common serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["arm,pl011", "snps,dw-apb-uart"],
            on_probe: probe
        }
    ],
);

pub(crate) fn console_irq_num() -> Option<usize> {
    let irq = CONSOLE_IRQ.load(Ordering::Acquire);
    (irq != NO_IRQ).then_some(irq)
}

fn parse_irq_num(info: &FdtInfo<'_>) -> Option<usize> {
    let interrupts = info.interrupts();
    let spec = &interrupts.first()?.specifier;
    match spec.as_slice() {
        [irq] => Some(*irq as usize),
        // GIC binding: <type, number, flags>.
        [irq_type, irq, _flags] => Some(match *irq_type {
            0 => *irq as usize + 32, // SPI
            1 => *irq as usize + 16, // PPI
            _ => *irq as usize,
        }),
        [irq, ..] => Some(*irq as usize),
        [] => None,
    }
}

fn maybe_record_console_irq(irq_num: Option<usize>) {
    if !CONSOLE_SERIAL_SELECTED.swap(true, Ordering::AcqRel)
        && let Some(irq_num) = irq_num
    {
        CONSOLE_IRQ.store(irq_num, Ordering::Release);
    }
}

#[cfg(feature = "irq")]
fn serial_irq_handler_0() {
    if let Some(handler) = SERIAL_IRQ_HANDLERS[0].lock().as_ref() {
        let _ = handler.clean_interrupt_status();
    }
}

#[cfg(feature = "irq")]
fn serial_irq_handler_1() {
    if let Some(handler) = SERIAL_IRQ_HANDLERS[1].lock().as_ref() {
        let _ = handler.clean_interrupt_status();
    }
}

#[cfg(feature = "irq")]
fn serial_irq_handler_2() {
    if let Some(handler) = SERIAL_IRQ_HANDLERS[2].lock().as_ref() {
        let _ = handler.clean_interrupt_status();
    }
}

#[cfg(feature = "irq")]
fn serial_irq_handler_3() {
    if let Some(handler) = SERIAL_IRQ_HANDLERS[3].lock().as_ref() {
        let _ = handler.clean_interrupt_status();
    }
}

#[cfg(feature = "irq")]
fn irq_handler_for_slot(slot: usize) -> irq::IrqHandler {
    match slot {
        0 => serial_irq_handler_0,
        1 => serial_irq_handler_1,
        2 => serial_irq_handler_2,
        3 => serial_irq_handler_3,
        _ => panic!("too many serial irq slots"),
    }
}

#[cfg(feature = "irq")]
fn register_serial_irq(irq_num: usize, serial: &mut dyn PlatformSerialDevice) {
    let Some(handler) = serial.take_irq_handler() else {
        return;
    };

    for slot in 0..MAX_SERIAL_IRQS {
        if SERIAL_IRQ_NUMS[slot]
            .compare_exchange(NO_IRQ, irq_num, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            *SERIAL_IRQ_HANDLERS[slot].lock() = Some(handler);
            serial.enable_interrupts(InterruptMask::RX_AVAILABLE);
            let _ = irq::register(irq_num, irq_handler_for_slot(slot));
            return;
        }
    }

    warn!("No free serial IRQ slot for IRQ {}", irq_num);
}

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    info!("Probing serial device: {}", info.node.name());
    let base_reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    let mmio_base = iomap((base_reg.address as usize).into(), mmio_size as usize)?;

    let clock_freq = info
        .node
        .as_node()
        .get_property("clock-frequency")
        .and_then(|prop| prop.get_u32())
        .unwrap_or(24_000_000);

    let mut serial: Option<DynPlatformSerial> = None;
    for c in info.node.as_node().compatibles() {
        if c == "arm,pl011" {
            serial = Some(Box::new(pl011::Pl011::new(mmio_base, clock_freq)));
            break;
        }

        if c == "snps,dw-apb-uart" {
            let reg_width = info
                .node
                .as_node()
                .get_property("reg-io-width")
                .and_then(|prop| prop.get_u32())
                .unwrap_or(1) as usize;
            serial = Some(Box::new(ns16550::Ns16550::new_mmio(
                mmio_base, clock_freq, reg_width,
            )));
            break;
        }
    }
    if let Some(mut serial) = serial {
        let irq_num = parse_irq_num(&info);
        info!(
            "Serial {}@{:#x} registered successfully",
            info.node.name(),
            serial.base_addr()
        );
        maybe_record_console_irq(irq_num);
        #[cfg(feature = "irq")]
        if let Some(irq_num) = irq_num {
            register_serial_irq(irq_num, serial.as_mut());
        }
        plat_dev.register(serial);
    }

    Ok(())
}
