use alloc::boxed::Box;

use rdif_block::{
    BQueue, DeviceInfo, Interface, IrqHandler, IrqSourceList, QueueHandle, QueueLimits,
};

use crate::IrqBindingLease;

pub struct IrqBoundBlock<T, L> {
    inner: T,
    irq_lease: L,
}

impl<T, L> IrqBoundBlock<T, L> {
    pub const fn new(inner: T, irq_lease: L) -> Self {
        Self { inner, irq_lease }
    }
}

impl<T: Interface, L: IrqBindingLease> rdif_block::DriverGeneric for IrqBoundBlock<T, L> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        self.inner.raw_any()
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        self.inner.raw_any_mut()
    }
}

impl<T: Interface, L: IrqBindingLease> Interface for IrqBoundBlock<T, L> {
    fn device_info(&self) -> DeviceInfo {
        self.inner.device_info()
    }

    fn queue_limits(&self) -> QueueLimits {
        self.inner.queue_limits()
    }

    fn create_queue(&mut self) -> Option<BQueue> {
        self.inner.create_queue()
    }

    fn create_owned_queue(&mut self) -> Option<QueueHandle> {
        self.inner.create_owned_queue()
    }

    fn enable_irq(&self) {
        self.inner.enable_irq();
        self.irq_lease.enable_binding_irq();
    }

    fn disable_irq(&self) {
        self.irq_lease.disable_binding_irq();
        self.inner.disable_irq();
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.is_irq_enabled()
    }

    fn irq_sources(&self) -> IrqSourceList {
        self.inner.irq_sources()
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        self.inner.take_irq_handler(source_id)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::vec;
    use std::sync::{Arc, Mutex};

    use irq_framework::{HwIrq, IrqDomainId, IrqId};
    use rdif_block::{DeviceInfo, IdList, Interface, IrqSourceInfo, IrqSourceList, QueueLimits};

    use super::IrqBoundBlock;
    use crate::{BindingInfo, BindingIrq, IrqBindingLease};

    #[derive(Clone)]
    struct TestLease {
        log: Arc<Mutex<alloc::vec::Vec<&'static str>>>,
        irq: IrqId,
    }

    impl IrqBindingLease for TestLease {
        fn binding_info(&self) -> BindingInfo {
            BindingInfo::with_irq_sources([(0, BindingIrq::id(self.irq))])
        }

        fn enable_binding_irq(&self) {
            self.log.lock().unwrap().push("lease-enable");
        }

        fn disable_binding_irq(&self) {
            self.log.lock().unwrap().push("lease-disable");
        }
    }

    struct TestBlock {
        log: Arc<Mutex<alloc::vec::Vec<&'static str>>>,
    }

    impl rdif_block::DriverGeneric for TestBlock {
        fn name(&self) -> &str {
            "test-block"
        }
    }

    impl Interface for TestBlock {
        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::new(1, 512)
        }

        fn queue_limits(&self) -> QueueLimits {
            QueueLimits::simple(512, u64::MAX)
        }

        fn create_queue(&mut self) -> Option<rdif_block::BQueue> {
            None
        }

        fn enable_irq(&self) {
            self.log.lock().unwrap().push("block-enable");
        }

        fn disable_irq(&self) {
            self.log.lock().unwrap().push("block-disable");
        }

        fn is_irq_enabled(&self) -> bool {
            true
        }

        fn irq_sources(&self) -> IrqSourceList {
            vec![IrqSourceInfo::legacy(IdList::from_bits(1))]
        }
    }

    #[test]
    fn irq_bound_block_orders_lease_and_inner_irq_transitions() {
        let log = Arc::new(Mutex::new(alloc::vec::Vec::new()));
        let irq = IrqId::new(IrqDomainId(8), HwIrq(0));
        let block = TestBlock {
            log: Arc::clone(&log),
        };
        let lease = TestLease {
            log: Arc::clone(&log),
            irq,
        };
        let wrapper = IrqBoundBlock::new(block, lease.clone());

        assert_eq!(
            lease.binding_info().irq_sources()[0].irq,
            BindingIrq::id(irq)
        );
        assert_eq!(
            wrapper.irq_sources(),
            vec![IrqSourceInfo::legacy(IdList::from_bits(1))]
        );

        wrapper.enable_irq();
        wrapper.disable_irq();

        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "block-enable",
                "lease-enable",
                "lease-disable",
                "block-disable"
            ]
        );
    }
}
