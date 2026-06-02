use core::ptr::NonNull;

use axvisor_api::irq::{IrqHandler, IrqIf};

unsafe fn irq_handler_adapter(
    ctx: ax_hal::irq::IrqContext,
    data: NonNull<()>,
) -> ax_hal::irq::IrqReturn {
    let handler = unsafe { core::mem::transmute::<*mut (), IrqHandler>(data.as_ptr()) };
    handler(ctx.irq.0);
    ax_hal::irq::IrqReturn::Handled
}

struct IrqImpl;

#[axvisor_api::api_impl]
impl IrqIf for IrqImpl {
    fn handle_irq(vector: usize) {
        ax_hal::irq::handle_irq(vector);
    }

    fn register_irq_handler(vector: usize, handler: IrqHandler) -> bool {
        let Some(data) = NonNull::new(handler as *const () as *mut ()) else {
            return false;
        };
        ax_hal::irq::request_shared_irq(vector, irq_handler_adapter, data).is_ok()
    }
}
