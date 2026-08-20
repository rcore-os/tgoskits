use super::*;

/// Scheduler-visible runtime bindings and committed CPU accounting.
#[derive(Debug)]
pub(in crate::system) struct ThreadRuntimeState {
    pub(in crate::system) context: ExecutionContextHandle,
    pub(in crate::system) address_space: AddressSpaceHandle,
}

impl ThreadRuntimeState {
    pub(super) const fn new(
        context: ExecutionContextHandle,
        address_space: AddressSpaceHandle,
    ) -> Self {
        Self {
            context,
            address_space,
        }
    }

    pub(super) const fn binding(&self) -> crate::runtime::ThreadRuntimeBinding {
        crate::runtime::ThreadRuntimeBinding::new(self.context, self.address_space)
    }
}
