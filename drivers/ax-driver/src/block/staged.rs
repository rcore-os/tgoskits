//! Platform-resource prelude for staged block controllers.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, Ordering};

use log::warn;
use rdif_block::{
    BlkError, ControllerInitEndpoint, DeviceInfo, IdList, InitError, InitInput, InitPoll,
    InitSchedule, InitialController, Interface, IrqHandler, IrqSourceList, LifecycleEndpoint,
    QueueHandle, QueueLimits,
};

const INIT_IRQ_ROLLBACK_FAILED: InitError =
    InitError::Hardware("staged controller initialization IRQ rollback failed");

/// Bounded board-level work that must run after IRQ actions are installed but
/// before a portable controller/card state machine touches hardware.
pub(super) trait PlatformPrelude: Send + 'static {
    /// Enable owned board resources and return their required settle time.
    ///
    /// Implementations must not sleep or busy-wait. A partial hardware
    /// failure is fail-closed: the block runtime retains the controller in its
    /// shutdown-lifetime quarantine, with no normal queue or DMA published.
    fn prepare(&mut self) -> Result<u64, InitError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreludeState {
    Prepare,
    Settling(u64),
    Controller,
    Ready,
    Failed,
}

enum PreludeAdvance {
    Pending(InitSchedule),
    Controller,
}

/// Adds a typed board-resource phase in front of an existing staged block
/// controller without moving board semantics into the portable driver core.
pub(super) struct StagedPlatformBlock<T, P> {
    inner: T,
    platform: P,
    prelude: PreludeState,
    init_sources: IdList,
    taken_init_handlers: IdList,
    irq_requested: AtomicBool,
    device_irq_enabled: AtomicBool,
}

impl PreludeState {
    fn advance(
        &mut self,
        now_ns: u64,
        prepare: impl FnOnce() -> Result<u64, InitError>,
    ) -> Result<PreludeAdvance, InitError> {
        match *self {
            Self::Prepare => {
                let settle_ns = prepare()?;
                let wake_at_ns = now_ns.saturating_add(settle_ns);
                *self = if settle_ns == 0 {
                    Self::Controller
                } else {
                    Self::Settling(wake_at_ns)
                };
                Ok(PreludeAdvance::Pending(if settle_ns == 0 {
                    InitSchedule::immediate()
                } else {
                    InitSchedule::wait_until(wake_at_ns)
                }))
            }
            Self::Settling(wake_at_ns) if now_ns < wake_at_ns => Ok(PreludeAdvance::Pending(
                InitSchedule::wait_until(wake_at_ns),
            )),
            Self::Settling(_) => {
                *self = Self::Controller;
                Ok(PreludeAdvance::Pending(InitSchedule::immediate()))
            }
            Self::Controller => Ok(PreludeAdvance::Controller),
            Self::Ready | Self::Failed => Err(InitError::InvalidState),
        }
    }
}

impl<T, P> StagedPlatformBlock<T, P>
where
    T: Interface + 'static,
    P: PlatformPrelude,
{
    pub(super) fn new(mut inner: T, platform: P) -> Self {
        let init_sources = match inner.controller_init() {
            ControllerInitEndpoint::Pending(initializer) => initializer.irq_sources(),
            ControllerInitEndpoint::Ready => IdList::none(),
        };
        Self {
            inner,
            platform,
            prelude: PreludeState::Prepare,
            init_sources,
            taken_init_handlers: IdList::none(),
            irq_requested: AtomicBool::new(false),
            device_irq_enabled: AtomicBool::new(false),
        }
    }

    fn enable_init_device_irq(&self) -> Result<(), InitError> {
        if !self.irq_requested.load(Ordering::Acquire) {
            return Err(InitError::MissingInterrupt);
        }
        if self.device_irq_enabled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.inner.enable_irq().map_err(|error| {
            warn!(
                "{}: failed to unmask staged controller IRQ: {error:?}",
                self.inner.name()
            );
            InitError::Hardware("platform controller initialization IRQ enable failed")
        })?;
        self.device_irq_enabled.store(true, Ordering::Release);
        Ok(())
    }

    fn disable_device_irq(&self) -> Result<(), BlkError> {
        if !self.device_irq_enabled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.inner.disable_irq()?;
        self.device_irq_enabled.store(false, Ordering::Release);
        Ok(())
    }

    fn fail_initialization(&mut self, error: InitError) -> InitPoll<()> {
        self.prelude = PreludeState::Failed;
        self.irq_requested.store(false, Ordering::Release);
        if let Err(rollback_error) = self.disable_device_irq() {
            warn!(
                "{}: initialization failed ({error}); device IRQ rollback also failed \
                 ({rollback_error:?})",
                self.inner.name()
            );
            InitPoll::Failed(INIT_IRQ_ROLLBACK_FAILED)
        } else {
            InitPoll::Failed(error)
        }
    }
}

impl<T, P> rdif_block::DriverGeneric for StagedPlatformBlock<T, P>
where
    T: Interface + 'static,
    P: PlatformPrelude,
{
    fn name(&self) -> &str {
        self.inner.name()
    }
}

impl<T, P> Interface for StagedPlatformBlock<T, P>
where
    T: Interface + 'static,
    P: PlatformPrelude,
{
    fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
        if self.prelude == PreludeState::Ready {
            self.inner.controller_init()
        } else {
            ControllerInitEndpoint::Pending(self)
        }
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
        self.irq_requested.store(true, Ordering::Release);
        if self.prelude == PreludeState::Ready {
            self.inner.enable_irq()?;
            self.device_irq_enabled.store(true, Ordering::Release);
        }
        Ok(())
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        self.irq_requested.store(false, Ordering::Release);
        self.disable_device_irq()
    }

    fn is_irq_enabled(&self) -> bool {
        self.device_irq_enabled.load(Ordering::Acquire)
    }

    fn irq_sources(&self) -> IrqSourceList {
        Interface::irq_sources(&self.inner)
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        Interface::take_irq_handler(&mut self.inner, source_id)
    }
}

impl<T, P> InitialController for StagedPlatformBlock<T, P>
where
    T: Interface + 'static,
    P: PlatformPrelude,
{
    fn irq_sources(&self) -> IdList {
        self.init_sources
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        let handler = match self.inner.controller_init() {
            ControllerInitEndpoint::Pending(initializer) => initializer.take_irq_handler(source_id),
            ControllerInitEndpoint::Ready => None,
        };
        if handler.is_some() && self.init_sources.contains(source_id) {
            self.taken_init_handlers.insert(source_id);
        }
        handler
    }

    fn service_deferred_irq(&mut self, source_id: usize) -> rdif_block::InitIrqProgress {
        match self.inner.controller_init() {
            ControllerInitEndpoint::Pending(initializer) => {
                initializer.service_deferred_irq(source_id)
            }
            ControllerInitEndpoint::Ready => rdif_block::InitIrqProgress::Unhandled,
        }
    }

    fn poll_init(&mut self, input: InitInput) -> InitPoll<()> {
        if !self.irq_requested.load(Ordering::Acquire)
            || self.taken_init_handlers.bits() != self.init_sources.bits()
        {
            return self.fail_initialization(InitError::MissingInterrupt);
        }
        let prelude = self
            .prelude
            .advance(input.now_ns, || self.platform.prepare());
        match prelude {
            Ok(PreludeAdvance::Pending(schedule)) => InitPoll::Pending(schedule),
            Err(error) => self.fail_initialization(error),
            Ok(PreludeAdvance::Controller) => {
                if let Err(error) = self.enable_init_device_irq() {
                    return self.fail_initialization(error);
                }
                let progress = match self.inner.controller_init() {
                    ControllerInitEndpoint::Pending(initializer) => initializer.poll_init(input),
                    ControllerInitEndpoint::Ready => InitPoll::Ready(()),
                };
                match progress {
                    InitPoll::Ready(()) => {
                        self.prelude = PreludeState::Ready;
                        InitPoll::Ready(())
                    }
                    InitPoll::Failed(error) => self.fail_initialization(error),
                    InitPoll::Pending(schedule) => InitPoll::Pending(schedule),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, AtomicUsize};

    use super::*;

    #[derive(Clone)]
    struct TestControl {
        device_irq_enabled: Arc<AtomicBool>,
        fail_disable: Arc<AtomicBool>,
        enable_calls: Arc<AtomicUsize>,
        disable_calls: Arc<AtomicUsize>,
    }

    impl TestControl {
        fn new() -> Self {
            Self {
                device_irq_enabled: Arc::new(AtomicBool::new(false)),
                fail_disable: Arc::new(AtomicBool::new(false)),
                enable_calls: Arc::new(AtomicUsize::new(0)),
                disable_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    struct FailingInitializer {
        handler: Option<Box<dyn IrqHandler>>,
    }

    impl FailingInitializer {
        fn new() -> Self {
            Self {
                handler: Some(Box::new(TestIrq)),
            }
        }
    }

    struct TestIrq;

    impl IrqHandler for TestIrq {
        fn handle_irq(&mut self) -> rdif_block::IrqOutcome {
            rdif_block::IrqOutcome::unhandled()
        }
    }

    impl InitialController for FailingInitializer {
        fn irq_sources(&self) -> IdList {
            IdList::from_bits(1)
        }

        fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
            (source_id == 0).then(|| self.handler.take()).flatten()
        }

        fn service_deferred_irq(&mut self, _source_id: usize) -> rdif_block::InitIrqProgress {
            rdif_block::InitIrqProgress::Unhandled
        }

        fn poll_init(&mut self, _input: InitInput) -> InitPoll<()> {
            InitPoll::Failed(InitError::Hardware("test initialization failure"))
        }
    }

    struct TestBlock {
        control: TestControl,
        initializer: FailingInitializer,
    }

    impl rdif_block::DriverGeneric for TestBlock {
        fn name(&self) -> &str {
            "test-staged-block"
        }
    }

    impl Interface for TestBlock {
        fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
            ControllerInitEndpoint::Pending(&mut self.initializer)
        }

        fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
            LifecycleEndpoint::Inline
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::new(1, 512)
        }

        fn queue_limits(&self) -> QueueLimits {
            QueueLimits::simple(512, u64::MAX)
        }

        fn create_queue(&mut self) -> Option<QueueHandle> {
            None
        }

        fn enable_irq(&self) -> Result<(), BlkError> {
            self.control.enable_calls.fetch_add(1, Ordering::Relaxed);
            self.control
                .device_irq_enabled
                .store(true, Ordering::Release);
            Ok(())
        }

        fn disable_irq(&self) -> Result<(), BlkError> {
            self.control.disable_calls.fetch_add(1, Ordering::Relaxed);
            if self.control.fail_disable.load(Ordering::Acquire) {
                return Err(BlkError::Io);
            }
            self.control
                .device_irq_enabled
                .store(false, Ordering::Release);
            Ok(())
        }

        fn is_irq_enabled(&self) -> bool {
            self.control.device_irq_enabled.load(Ordering::Acquire)
        }

        fn irq_sources(&self) -> IrqSourceList {
            alloc::vec![rdif_block::IrqSourceInfo::legacy(IdList::from_bits(1))]
        }

        fn take_irq_handler(&mut self, _source_id: usize) -> Option<Box<dyn IrqHandler>> {
            None
        }
    }

    struct ImmediatePrelude;

    impl PlatformPrelude for ImmediatePrelude {
        fn prepare(&mut self) -> Result<u64, InitError> {
            Ok(0)
        }
    }

    #[test]
    fn prelude_starts_only_when_polled_and_uses_absolute_deadline() {
        let mut state = PreludeState::Prepare;
        let mut prepare_calls = 0;

        let first = state
            .advance(1_000, || {
                prepare_calls += 1;
                Ok(250)
            })
            .unwrap();
        assert_eq!(prepare_calls, 1);
        assert_eq!(state, PreludeState::Settling(1_250));
        let PreludeAdvance::Pending(first) = first else {
            panic!("settling prelude must publish a deadline");
        };
        assert_eq!(first.wake_at_ns(), Some(1_250));

        let early = state
            .advance(1_100, || panic!("prelude ran twice"))
            .unwrap();
        let PreludeAdvance::Pending(early) = early else {
            panic!("early prelude poll must preserve its deadline");
        };
        assert_eq!(early.wake_at_ns(), Some(1_250));
        let at_deadline = state
            .advance(1_250, || panic!("prelude ran twice"))
            .unwrap();
        let PreludeAdvance::Pending(at_deadline) = at_deadline else {
            panic!("deadline transition must requeue the controller phase");
        };
        assert!(at_deadline.run_again());
        assert!(matches!(
            state.advance(1_250, || panic!("prelude ran twice")),
            Ok(PreludeAdvance::Controller)
        ));
    }

    #[test]
    fn failed_device_irq_mask_remains_retryable() {
        let control = TestControl::new();
        let block = StagedPlatformBlock::new(
            TestBlock {
                control: control.clone(),
                initializer: FailingInitializer::new(),
            },
            ImmediatePrelude,
        );
        block.device_irq_enabled.store(true, Ordering::Release);
        control.device_irq_enabled.store(true, Ordering::Release);
        control.fail_disable.store(true, Ordering::Release);

        assert_eq!(Interface::disable_irq(&block), Err(BlkError::Io));
        assert!(block.device_irq_enabled.load(Ordering::Acquire));

        control.fail_disable.store(false, Ordering::Release);
        assert_eq!(Interface::disable_irq(&block), Ok(()));
        assert!(!block.device_irq_enabled.load(Ordering::Acquire));
        assert_eq!(control.disable_calls.load(Ordering::Acquire), 2);
    }

    #[test]
    fn terminal_controller_init_failure_masks_the_device_irq_source() {
        let control = TestControl::new();
        let mut block = StagedPlatformBlock::new(
            TestBlock {
                control: control.clone(),
                initializer: FailingInitializer::new(),
            },
            ImmediatePrelude,
        );

        let _handler = InitialController::take_irq_handler(&mut block, 0)
            .expect("test initialization IRQ handler must be available");
        assert_eq!(Interface::enable_irq(&block), Ok(()));
        let InitPoll::Pending(schedule) =
            InitialController::poll_init(&mut block, InitInput::at(0))
        else {
            panic!("staged controller prelude must requeue controller initialization");
        };
        assert!(schedule.run_again());
        assert!(matches!(
            InitialController::poll_init(&mut block, InitInput::at(0)),
            InitPoll::Failed(InitError::Hardware("test initialization failure"))
        ));

        assert_eq!(control.enable_calls.load(Ordering::Acquire), 1);
        assert_eq!(control.disable_calls.load(Ordering::Acquire), 1);
        assert!(!control.device_irq_enabled.load(Ordering::Acquire));
        assert!(!block.device_irq_enabled.load(Ordering::Acquire));
    }

    #[test]
    fn platform_prelude_cannot_run_before_the_initialization_irq_handler_is_taken() {
        struct CountingPrelude(Arc<AtomicUsize>);

        impl PlatformPrelude for CountingPrelude {
            fn prepare(&mut self) -> Result<u64, InitError> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(0)
            }
        }

        let prepare_calls = Arc::new(AtomicUsize::new(0));
        let control = TestControl::new();
        let mut block = StagedPlatformBlock::new(
            TestBlock {
                control,
                initializer: FailingInitializer::new(),
            },
            CountingPrelude(Arc::clone(&prepare_calls)),
        );

        Interface::enable_irq(&block).unwrap();
        assert!(matches!(
            InitialController::poll_init(&mut block, InitInput::at(0)),
            InitPoll::Failed(InitError::MissingInterrupt)
        ));
        assert_eq!(
            prepare_calls.load(Ordering::Acquire),
            0,
            "board reset/clock preparation must start only after the IRQ endpoint is owned"
        );
    }

    #[test]
    fn revoked_initialization_irq_prevents_the_platform_prelude_and_first_command() {
        struct CountingPrelude(Arc<AtomicUsize>);

        impl PlatformPrelude for CountingPrelude {
            fn prepare(&mut self) -> Result<u64, InitError> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(0)
            }
        }

        let prepare_calls = Arc::new(AtomicUsize::new(0));
        let control = TestControl::new();
        let mut block = StagedPlatformBlock::new(
            TestBlock {
                control: control.clone(),
                initializer: FailingInitializer::new(),
            },
            CountingPrelude(Arc::clone(&prepare_calls)),
        );
        let _handler = InitialController::take_irq_handler(&mut block, 0)
            .expect("test initialization IRQ handler must be available");
        Interface::enable_irq(&block).unwrap();
        Interface::disable_irq(&block).unwrap();

        assert!(matches!(
            InitialController::poll_init(&mut block, InitInput::at(0)),
            InitPoll::Failed(InitError::MissingInterrupt)
        ));
        assert_eq!(prepare_calls.load(Ordering::Acquire), 0);
        assert_eq!(
            control.enable_calls.load(Ordering::Acquire),
            0,
            "the controller must not unmask or issue its first command after IRQ revocation"
        );
    }
}
