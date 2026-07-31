use super::*;

/// Scheduler-visible runtime bindings and committed CPU accounting.
#[derive(Debug)]
pub(in crate::system) struct ThreadRuntimeState {
    pub(in crate::system) charged_runtime_ns: u64,
    pub(in crate::system) context: ExecutionContextHandle,
    pub(in crate::system) address_space: AddressSpaceHandle,
}

impl ThreadRuntimeState {
    pub(super) const fn new(
        context: ExecutionContextHandle,
        address_space: AddressSpaceHandle,
    ) -> Self {
        Self {
            charged_runtime_ns: 0,
            context,
            address_space,
        }
    }
}
