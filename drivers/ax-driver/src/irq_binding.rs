use crate::BindingInfo;

pub trait IrqBindingLease: Send + 'static {
    fn binding_info(&self) -> BindingInfo;

    fn enable_binding_irq(&self);

    fn disable_binding_irq(&self);
}
