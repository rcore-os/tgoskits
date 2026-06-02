use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use axvisor_api::irq::{IrqHandler, IrqIf};

static IRQ_HOOK: AtomicUsize = AtomicUsize::new(0);

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
        if ax_hal::irq::handle_irq(vector) {
            let hook = IRQ_HOOK.load(Ordering::Acquire);
            if hook != 0 {
                let hook = unsafe { core::mem::transmute::<usize, IrqHandler>(hook) };
                hook(vector);
            }
        }
    }

    fn register_irq_handler(vector: usize, handler: IrqHandler) -> bool {
        let Some(data) = NonNull::new(handler as *const () as *mut ()) else {
            return false;
        };
        ax_hal::irq::request_shared_irq(vector, irq_handler_adapter, data).is_ok()
    }

    fn register_irq_hook(hook: IrqHandler) -> bool {
        IRQ_HOOK
            .compare_exchange(
                0,
                hook as *const () as usize,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
    }
}
