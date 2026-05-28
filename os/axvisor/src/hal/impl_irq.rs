use axvisor_api::irq::{IrqHandler, IrqIf};

struct IrqImpl;

#[axvisor_api::api_impl]
impl IrqIf for IrqImpl {
    fn handle_irq(vector: usize) {
        ax_hal::trap::irq_handler(vector);
    }

    fn register_irq_handler(vector: usize, handler: IrqHandler) -> bool {
        ax_hal::irq::register(vector, handler)
    }

    fn register_irq_hook(hook: IrqHandler) -> bool {
        ax_hal::irq::register_irq_hook(hook)
    }
}
