use rdif_intc::Intc;
use rdrive::Device;
use someboot::irq::IrqId;

mod v2;
mod v3;

use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Eq, PartialEq)]
enum GicBackend {
    None = 0,
    V2   = 2,
    V3   = 3,
}

static GIC_BACKEND: AtomicU8 = AtomicU8::new(GicBackend::None as u8);

fn set_backend(backend: GicBackend) {
    GIC_BACKEND.store(backend as u8, Ordering::Release);
}

fn backend() -> GicBackend {
    match GIC_BACKEND.load(Ordering::Acquire) {
        2 => GicBackend::V2,
        3 => GicBackend::V3,
        _ => GicBackend::None,
    }
}

pub fn init_current_cpu() {
    let cpu_idx = crate::cpu::current_cpu_idx()
        .unwrap_or_else(|| panic!("current logical CPU index is not available for GIC init"));
    init_cpu(cpu_idx);
}

pub fn init_cpu(cpu_idx: usize) {
    match backend() {
        GicBackend::V2 => v2::init_cpu(),
        GicBackend::V3 => v3::init_cpu(cpu_idx),
        GicBackend::None => {
            if v3::is_support_icc() {
                v3::init_cpu(cpu_idx);
            } else {
                v2::init_cpu();
            }
        }
    }
}

fn get_gicd() -> Device<Intc> {
    rdrive::get_one().expect("no interrupt controller found")
}

pub fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
    let raw = irq.into();
    match backend() {
        GicBackend::V2 => v2::irq_set_enable(raw, enable),
        GicBackend::V3 => v3::irq_set_enable(raw, enable),
        GicBackend::None => {
            v2::irq_set_enable(raw, enable);
            v3::irq_set_enable(raw, enable);
        }
    }
}

pub fn send_ipi(irq: rdrive::IrqId, target: crate::irq::IpiTarget) {
    let raw = irq.into();
    match backend() {
        GicBackend::V2 => v2::send_ipi(raw, target),
        GicBackend::V3 => v3::send_ipi(raw, target),
        GicBackend::None => {
            if v3::is_support_icc() {
                v3::send_ipi(raw, target);
            } else {
                v2::send_ipi(raw, target);
            }
        }
    }
}

fn hardware_cpu_id(cpu_idx: usize) -> usize {
    someboot::smp::cpu_idx_to_id(cpu_idx).unwrap_or(cpu_idx)
}

#[unsafe(no_mangle)]
fn __aarch64_irq_handler() {
    irq_handler();
}

pub(crate) fn irq_handler() -> someboot::irq::IrqId {
    match backend() {
        GicBackend::V2 => v2::handle_irq(),
        GicBackend::V3 => v3::handle_irq(),
        GicBackend::None => {
            if v3::is_support_icc() {
                v3::handle_irq()
            } else {
                v2::handle_irq()
            }
        }
    }
}

fn _handle_irq(hwirq: IrqId) {
    unsafe extern "Rust" {
        fn _someboot_handle_irq(hwirq: IrqId);
    }
    unsafe {
        _someboot_handle_irq(hwirq);
    }
}
