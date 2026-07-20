use core::num::NonZeroUsize;

use log::{error, warn};
use rdif_block::{
    BlkError, BlockIrqSource, BundleError, ControllerBundle, DeviceInfo, Interface, IrqSourceList,
    LifecycleEndpoint, LogicalDevice, LogicalDeviceId, LogicalDeviceIds, QueueHandle, QueueLimits,
};

use crate::IrqBindingLease;

const BINDING_ENABLE_FAILED: BlkError = BlkError::Other("platform IRQ binding enable failed");
const BINDING_DISABLE_FAILED: BlkError = BlkError::Other("platform IRQ binding disable failed");
const ACTIVATION_ROLLBACK_FAILED: BlkError =
    BlkError::Other("block IRQ activation rollback failed");

pub struct IrqBoundBlock<T, L> {
    inner: T,
    irq_lease: L,
}

/// Retains a platform IRQ lease around one multi-device controller bundle.
pub struct IrqBoundControllerBundle<T, L> {
    inner: T,
    irq_lease: L,
}

impl<T, L> IrqBoundControllerBundle<T, L> {
    pub const fn new(inner: T, irq_lease: L) -> Self {
        Self { inner, irq_lease }
    }
}

impl<T, L> IrqBoundBlock<T, L> {
    pub const fn new(inner: T, irq_lease: L) -> Self {
        Self { inner, irq_lease }
    }
}

impl<T: Interface, L: IrqBindingLease> IrqBoundBlock<T, L> {
    fn enable_irq_transaction(&self) -> Result<(), BlkError> {
        enable_irq_transaction(
            &self.irq_lease,
            || self.inner.enable_irq(),
            || self.inner.disable_irq(),
        )
    }

    fn disable_irq_transaction(&self) -> Result<(), BlkError> {
        disable_irq_transaction(&self.irq_lease, || self.inner.disable_irq())
    }
}

impl<T: ControllerBundle, L: IrqBindingLease> IrqBoundControllerBundle<T, L> {
    fn enable_irq_transaction(&self) -> Result<(), BlkError> {
        enable_irq_transaction(
            &self.irq_lease,
            || self.inner.enable_irq(),
            || self.inner.disable_irq(),
        )
    }

    fn disable_irq_transaction(&self) -> Result<(), BlkError> {
        disable_irq_transaction(&self.irq_lease, || self.inner.disable_irq())
    }
}

fn enable_irq_transaction<L: IrqBindingLease>(
    irq_lease: &L,
    enable_device_source: impl FnOnce() -> Result<(), BlkError>,
    disable_device_source: impl FnOnce() -> Result<(), BlkError>,
) -> Result<(), BlkError> {
    if let Err(enable_error) = enable_device_source() {
        let device_rollback = disable_device_source();
        if let Err(rollback_error) = device_rollback {
            error!(
                "block IRQ source enable failed ({enable_error}); source rollback also failed \
                 ({rollback_error})"
            );
        }
        return if device_rollback.is_err() {
            Err(ACTIVATION_ROLLBACK_FAILED)
        } else {
            Err(enable_error)
        };
    }

    // The registered OS action is already enabled by the runtime. Publish the
    // device endpoint/source before opening the outer PCI or platform gate so
    // a pending level interrupt cannot enter an endpoint that still reports
    // itself offline.
    if let Err(binding_error) = irq_lease.enable_binding_irq() {
        let device_rollback = disable_device_source();
        let binding_rollback = irq_lease.disable_binding_irq();

        if let Err(rollback_error) = device_rollback {
            error!(
                "platform IRQ binding enable failed ({binding_error}); source rollback also \
                 failed ({rollback_error})"
            );
        }
        if let Err(rollback_error) = binding_rollback {
            error!(
                "platform IRQ binding enable failed ({binding_error}); binding rollback also \
                 failed ({rollback_error})"
            );
        }
        return if device_rollback.is_err() || binding_rollback.is_err() {
            Err(ACTIVATION_ROLLBACK_FAILED)
        } else {
            warn!("platform IRQ binding enable failed: {binding_error}");
            Err(BINDING_ENABLE_FAILED)
        };
    }

    Ok(())
}

fn disable_irq_transaction<L: IrqBindingLease>(
    irq_lease: &L,
    disable_device_source: impl FnOnce() -> Result<(), BlkError>,
) -> Result<(), BlkError> {
    let device_result = disable_device_source();
    let binding_result = irq_lease.disable_binding_irq();

    match (device_result, binding_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(binding_error)) => {
            warn!("platform IRQ binding disable failed: {binding_error}");
            Err(BINDING_DISABLE_FAILED)
        }
        (Err(device_error), Ok(())) => {
            error!(
                "device IRQ source disable failed ({device_error}); platform binding masked as \
                 outer containment"
            );
            Err(device_error)
        }
        (Err(device_error), Err(binding_error)) => {
            error!(
                "device IRQ source disable failed ({device_error}); platform binding disable also \
                 failed ({binding_error})"
            );
            Err(ACTIVATION_ROLLBACK_FAILED)
        }
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
    fn controller_init(&mut self) -> rdif_block::ControllerInitEndpoint<'_> {
        self.inner.controller_init()
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        self.inner.lifecycle()
    }

    fn device_info(&self) -> DeviceInfo {
        self.inner.device_info()
    }

    fn queue_limits(&self) -> QueueLimits {
        self.inner.queue_limits()
    }

    fn create_queue(&mut self) -> Option<QueueHandle> {
        self.inner.create_queue()
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        self.enable_irq_transaction()
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        self.disable_irq_transaction()
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.is_irq_enabled()
    }

    fn irq_sources(&self) -> IrqSourceList {
        self.inner.irq_sources()
    }

    fn take_irq_source(&mut self, source_id: usize) -> Option<BlockIrqSource> {
        self.inner.take_irq_source(source_id)
    }
}

impl<T: ControllerBundle, L: IrqBindingLease> rdif_block::DriverGeneric
    for IrqBoundControllerBundle<T, L>
{
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

impl<T: ControllerBundle, L: IrqBindingLease> ControllerBundle for IrqBoundControllerBundle<T, L> {
    fn controller_init(&mut self) -> rdif_block::ControllerInitEndpoint<'_> {
        self.inner.controller_init()
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        self.inner.lifecycle()
    }

    fn logical_device_ids(&self) -> LogicalDeviceIds {
        self.inner.logical_device_ids()
    }

    fn take_logical_device(
        &mut self,
        device_id: LogicalDeviceId,
        max_queues: NonZeroUsize,
    ) -> Result<LogicalDevice, BundleError> {
        self.inner.take_logical_device(device_id, max_queues)
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        self.enable_irq_transaction()
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        self.disable_irq_transaction()
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.is_irq_enabled()
    }

    fn irq_sources(&self) -> IrqSourceList {
        self.inner.irq_sources()
    }

    fn take_irq_source(&mut self, source_id: usize) -> Option<BlockIrqSource> {
        self.inner.take_irq_source(source_id)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::vec;
    use std::sync::{Arc, Mutex};

    use irq_framework::{HwIrq, IrqDomainId, IrqId};
    use rdif_block::{
        ControllerInitEndpoint, DeviceInfo, IdList, InitInput, InitPoll, InitialController,
        Interface, IrqSourceInfo, IrqSourceList, QueueLimits,
    };

    use super::{
        ACTIVATION_ROLLBACK_FAILED, BINDING_DISABLE_FAILED, BINDING_ENABLE_FAILED, IrqBoundBlock,
    };
    use crate::{
        BindingInfo, BindingIrq, IrqBindingError, IrqBindingFailure, IrqBindingFault,
        IrqBindingLease, IrqBindingOperation, IrqBindingStage,
    };

    #[derive(Clone)]
    struct TestLease {
        log: Arc<Mutex<alloc::vec::Vec<&'static str>>>,
        irq: IrqId,
        fail_enable: bool,
        fail_disable: bool,
    }

    impl IrqBindingLease for TestLease {
        fn binding_info(&self) -> BindingInfo {
            BindingInfo::with_irq_sources([(0, BindingIrq::id(self.irq))])
        }

        fn enable_binding_irq(&self) -> Result<(), IrqBindingError> {
            self.log.lock().unwrap().push("lease-enable");
            if self.fail_enable {
                Err(test_binding_error(IrqBindingOperation::Enable))
            } else {
                Ok(())
            }
        }

        fn disable_binding_irq(&self) -> Result<(), IrqBindingError> {
            self.log.lock().unwrap().push("lease-disable");
            if self.fail_disable {
                Err(test_binding_error(IrqBindingOperation::Disable))
            } else {
                Ok(())
            }
        }
    }

    struct TestBlock {
        log: Arc<Mutex<alloc::vec::Vec<&'static str>>>,
        initializer: TestInitializer,
        fail_enable: bool,
        fail_disable: bool,
    }

    struct TestInitializer;

    impl InitialController for TestInitializer {
        fn irq_sources(&self) -> IdList {
            IdList::from_bits(1)
        }

        fn take_irq_source(&mut self, _source_id: usize) -> Option<rdif_block::BlockIrqSource> {
            None
        }

        fn poll_init(&mut self, _input: InitInput) -> InitPoll<()> {
            InitPoll::Ready(())
        }
    }

    impl rdif_block::DriverGeneric for TestBlock {
        fn name(&self) -> &str {
            "test-block"
        }
    }

    impl Interface for TestBlock {
        fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
            ControllerInitEndpoint::Pending(&mut self.initializer)
        }

        fn lifecycle(&mut self) -> rdif_block::LifecycleEndpoint<'_> {
            rdif_block::LifecycleEndpoint::Inline
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::new(1, 512)
        }

        fn queue_limits(&self) -> QueueLimits {
            QueueLimits::simple(512, u64::MAX)
        }

        fn create_queue(&mut self) -> Option<rdif_block::QueueHandle> {
            None
        }

        fn enable_irq(&self) -> Result<(), rdif_block::BlkError> {
            self.log.lock().unwrap().push("block-enable");
            if self.fail_enable {
                Err(rdif_block::BlkError::Io)
            } else {
                Ok(())
            }
        }

        fn disable_irq(&self) -> Result<(), rdif_block::BlkError> {
            self.log.lock().unwrap().push("block-disable");
            if self.fail_disable {
                Err(rdif_block::BlkError::Io)
            } else {
                Ok(())
            }
        }

        fn is_irq_enabled(&self) -> bool {
            true
        }

        fn irq_sources(&self) -> IrqSourceList {
            vec![IrqSourceInfo::legacy(IdList::from_bits(1))]
        }

        fn take_irq_source(&mut self, _source_id: usize) -> Option<rdif_block::BlockIrqSource> {
            None
        }
    }

    #[test]
    fn irq_bound_block_orders_lease_and_inner_irq_transitions() {
        let log = Arc::new(Mutex::new(alloc::vec::Vec::new()));
        let irq = IrqId::new(IrqDomainId(8), HwIrq(0));
        let block = TestBlock {
            log: Arc::clone(&log),
            initializer: TestInitializer,
            fail_enable: false,
            fail_disable: false,
        };
        let lease = TestLease {
            log: Arc::clone(&log),
            irq,
            fail_enable: false,
            fail_disable: false,
        };
        let mut wrapper = IrqBoundBlock::new(block, lease.clone());

        assert_eq!(
            lease.binding_info().irq_sources()[0].irq,
            BindingIrq::id(irq)
        );
        assert_eq!(
            wrapper.irq_sources(),
            vec![IrqSourceInfo::legacy(IdList::from_bits(1))]
        );

        assert!(matches!(
            wrapper.controller_init(),
            ControllerInitEndpoint::Pending(_)
        ));

        wrapper.enable_irq().unwrap();
        wrapper.disable_irq().unwrap();

        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "block-enable",
                "lease-enable",
                "block-disable",
                "lease-disable"
            ]
        );
    }

    #[test]
    fn failed_device_unmask_rolls_back_the_binding_lease() {
        let log = Arc::new(Mutex::new(alloc::vec::Vec::new()));
        let irq = IrqId::new(IrqDomainId(8), HwIrq(0));
        let block = TestBlock {
            log: Arc::clone(&log),
            initializer: TestInitializer,
            fail_enable: true,
            fail_disable: false,
        };
        let lease = TestLease {
            log: Arc::clone(&log),
            irq,
            fail_enable: false,
            fail_disable: false,
        };
        let wrapper = IrqBoundBlock::new(block, lease);

        assert_eq!(wrapper.enable_irq(), Err(rdif_block::BlkError::Io));
        assert_eq!(*log.lock().unwrap(), vec!["block-enable", "block-disable"]);
    }

    #[test]
    fn failed_binding_enable_rolls_back_the_device_before_the_outer_gate() {
        let log = Arc::new(Mutex::new(alloc::vec::Vec::new()));
        let irq = IrqId::new(IrqDomainId(8), HwIrq(0));
        let block = TestBlock {
            log: Arc::clone(&log),
            initializer: TestInitializer,
            fail_enable: false,
            fail_disable: false,
        };
        let lease = TestLease {
            log: Arc::clone(&log),
            irq,
            fail_enable: true,
            fail_disable: false,
        };
        let wrapper = IrqBoundBlock::new(block, lease);

        assert_eq!(wrapper.enable_irq(), Err(BINDING_ENABLE_FAILED));
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "block-enable",
                "lease-enable",
                "block-disable",
                "lease-disable"
            ]
        );
    }

    #[test]
    fn failed_binding_disable_still_disables_device_source() {
        let log = Arc::new(Mutex::new(alloc::vec::Vec::new()));
        let irq = IrqId::new(IrqDomainId(8), HwIrq(0));
        let block = TestBlock {
            log: Arc::clone(&log),
            initializer: TestInitializer,
            fail_enable: false,
            fail_disable: false,
        };
        let lease = TestLease {
            log: Arc::clone(&log),
            irq,
            fail_enable: false,
            fail_disable: true,
        };
        let wrapper = IrqBoundBlock::new(block, lease);

        assert_eq!(wrapper.disable_irq(), Err(BINDING_DISABLE_FAILED));
        assert_eq!(*log.lock().unwrap(), vec!["block-disable", "lease-disable"]);
    }

    #[test]
    fn failed_device_disable_masks_binding_as_outer_containment() {
        let log = Arc::new(Mutex::new(alloc::vec::Vec::new()));
        let irq = IrqId::new(IrqDomainId(8), HwIrq(0));
        let block = TestBlock {
            log: Arc::clone(&log),
            initializer: TestInitializer,
            fail_enable: false,
            fail_disable: true,
        };
        let lease = TestLease {
            log: Arc::clone(&log),
            irq,
            fail_enable: false,
            fail_disable: false,
        };
        let wrapper = IrqBoundBlock::new(block, lease);

        assert_eq!(wrapper.disable_irq(), Err(rdif_block::BlkError::Io));
        assert_eq!(*log.lock().unwrap(), vec!["block-disable", "lease-disable"]);
    }

    #[test]
    fn failed_device_and_binding_disable_report_incomplete_containment() {
        let log = Arc::new(Mutex::new(alloc::vec::Vec::new()));
        let irq = IrqId::new(IrqDomainId(8), HwIrq(0));
        let block = TestBlock {
            log: Arc::clone(&log),
            initializer: TestInitializer,
            fail_enable: false,
            fail_disable: true,
        };
        let lease = TestLease {
            log: Arc::clone(&log),
            irq,
            fail_enable: false,
            fail_disable: true,
        };
        let wrapper = IrqBoundBlock::new(block, lease);

        assert_eq!(wrapper.disable_irq(), Err(ACTIVATION_ROLLBACK_FAILED));
        assert_eq!(*log.lock().unwrap(), vec!["block-disable", "lease-disable"]);
    }

    #[test]
    fn failed_source_enable_never_opens_outer_gate_and_reports_failed_rollback() {
        let log = Arc::new(Mutex::new(alloc::vec::Vec::new()));
        let irq = IrqId::new(IrqDomainId(8), HwIrq(0));
        let block = TestBlock {
            log: Arc::clone(&log),
            initializer: TestInitializer,
            fail_enable: true,
            fail_disable: true,
        };
        let lease = TestLease {
            log: Arc::clone(&log),
            irq,
            fail_enable: false,
            fail_disable: false,
        };
        let wrapper = IrqBoundBlock::new(block, lease);

        assert_eq!(wrapper.enable_irq(), Err(ACTIVATION_ROLLBACK_FAILED));
        assert_eq!(*log.lock().unwrap(), vec!["block-enable", "block-disable"]);
    }

    fn test_binding_error(operation: IrqBindingOperation) -> IrqBindingError {
        IrqBindingError::new(
            operation,
            IrqBindingFault::new(
                IrqBindingStage::ProviderVector,
                Some(0),
                IrqBindingFailure::Irq(irq_framework::IrqError::Controller),
            ),
        )
    }
}
