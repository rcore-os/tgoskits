use rdif_intc::Intc;
use rdrive::Device;
use someboot::irq::IrqId;

mod v2;
mod v3;

pub fn init_current_cpu() {
    if v3::is_support_icc() {
        v3::init_cpu();
    } else {
        v2::init_cpu();
    }
}

fn get_gicd() -> Device<Intc> {
    rdrive::get_one().expect("no interrupt controller found")
}

pub fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
    let raw = irq.into();
    v2::irq_set_enable(raw, enable);
    v3::irq_set_enable(raw, enable);
}

#[unsafe(no_mangle)]
fn __aarch64_irq_handler() {
    irq_handler();
}

pub(crate) fn irq_handler() -> someboot::irq::IrqId {
    if v3::is_support_icc() {
        v3::handle_irq()
    } else {
        v2::handle_irq()
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
