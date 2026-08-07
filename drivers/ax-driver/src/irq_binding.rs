use crate::BindingInfo;

pub trait IrqBindingLease: Send + 'static {
    fn binding_info(&self) -> BindingInfo;

    fn enable_binding_irq(&self);

    fn enable_binding_source(&self, _source_id: usize) {
        self.enable_binding_irq();
    }

    fn disable_binding_irq(&self);
}
