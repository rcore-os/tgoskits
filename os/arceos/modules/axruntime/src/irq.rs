#[cfg(feature = "block")]
use alloc::boxed::Box;
use alloc::string::String;

#[cfg(any(
    target_arch = "loongarch64",
    target_arch = "riscv64",
    target_arch = "x86_64",
))]
use ax_hal::irq::CPU_LOCAL_IRQ_DOMAIN;
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64",
    target_arch = "x86_64",
))]
use ax_hal::irq::HwIrq;
use ax_hal::irq::{
    AutoEnable, CpuId, IrqAffinity, IrqContext, IrqDrainToken, IrqDrainWake, IrqError, IrqHandle,
    IrqId, IrqRequest, IrqReturn, IrqSource, ShareMode,
};

/// Resolves an explicitly legacy numeric IRQ without truncating it.
pub fn resolve_legacy_irq(irq: usize) -> Result<IrqId, IrqError> {
    ax_hal::irq::try_legacy_irq(irq)
}

/// Resolves a discovered device IRQ binding through the platform IRQ domain.
pub fn resolve_binding_irq(irq: ax_driver::BindingIrq) -> Result<IrqId, IrqError> {
    if let Some(id) = irq.irq_id() {
        return Ok(id);
    }

    match irq {
        ax_driver::BindingIrq::Id(id) => Ok(id),
        ax_driver::BindingIrq::Source(source) => resolve_binding_irq_source(source),
    }
}

fn resolve_binding_irq_source(source: ax_driver::BindingIrqSource) -> Result<IrqId, IrqError> {
    match source {
        ax_driver::BindingIrqSource::AcpiGsi(gsi) => {
            ax_hal::irq::resolve_irq_source(IrqSource::AcpiGsi(gsi))
        }
        ax_driver::BindingIrqSource::AcpiGsiRoute(route) => {
            ax_hal::irq::resolve_irq_source(IrqSource::AcpiGsiRoute(route))
        }
        ax_driver::BindingIrqSource::FdtInterrupt(spec) => resolve_fdt_irq_spec(spec),
    }
}

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
fn resolve_fdt_irq_spec(spec: ax_driver::FdtIrqSpec) -> Result<IrqId, IrqError> {
    let mut intc = rdrive::get::<rdif_intc::Intc>(spec.controller)
        .map_err(|_| IrqError::Unsupported)?
        .lock()
        .map_err(|_| IrqError::Controller)?;
    let translation = intc.translate_fdt(&spec.cells)?;
    intc.configure(&translation)?;
    Ok(translation.id)
}

#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
)))]
fn resolve_fdt_irq_spec(_spec: ax_driver::FdtIrqSpec) -> Result<IrqId, IrqError> {
    Err(IrqError::Unsupported)
}

/// Resolves a per-CPU trap IRQ through the platform IRQ domain.
#[cfg(target_arch = "aarch64")]
pub fn resolve_percpu_irq(irq: usize) -> IrqId {
    let hwirq = HwIrq(u32::try_from(irq).expect("AArch64 per-CPU IRQ exceeds GIC INTID width"));
    ax_hal::irq::resolve_percpu_irq(hwirq).expect("AArch64 per-CPU IRQ domain is not registered")
}

/// Resolves a per-CPU trap IRQ through the platform IRQ domain.
#[cfg(any(target_arch = "loongarch64", target_arch = "x86_64"))]
pub fn resolve_percpu_irq(irq: usize) -> IrqId {
    IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(irq as u32))
}

/// Resolves a per-CPU trap IRQ through the platform IRQ domain.
#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
)))]
pub fn resolve_percpu_irq(irq: usize) -> IrqId {
    #[cfg(target_arch = "riscv64")]
    {
        const RISCV_INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);

        if irq & RISCV_INTERRUPT_BIT != 0 {
            return IrqId::new(
                CPU_LOCAL_IRQ_DOMAIN,
                HwIrq((irq & !RISCV_INTERRUPT_BIT) as u32),
            );
        }
    }

    resolve_legacy_irq(irq).expect("legacy per-CPU IRQ exceeds platform IRQ id width")
}

pub struct Registration {
    name: String,
    handle: Option<IrqHandle>,
}

/// Move-only host IRQ callback retained while a guest owns the descriptor.
#[cfg(feature = "block")]
pub(crate) struct DetachedRegistration {
    name: String,
    action: Option<ax_hal::irq::DetachedIrqAction>,
}

#[cfg(feature = "block")]
pub(crate) struct ReattachRegistrationError {
    reason: IrqError,
    registration: Box<DetachedRegistration>,
}

impl Registration {
    /// Registers and immediately enables one shared interrupt action.
    pub fn register_shared(
        name: impl Into<String>,
        irq: IrqId,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> Result<Self, IrqError> {
        let name = name.into();
        match ax_hal::irq::request_shared_irq(irq, handler) {
            Ok(handle) => {
                info!("registered {name} irq {:?}", handle.irq());
                Ok(Self {
                    name,
                    handle: Some(handle),
                })
            }
            Err(err) => {
                warn!("failed to register {name} irq handler for irq {irq:?}: {err:?}");
                Err(err)
            }
        }
    }

    /// Registers one shared interrupt action without making it dispatchable.
    ///
    /// Device runtimes use this form while constructing queue routes. They may
    /// enable the action only after every object reachable from the hard-IRQ
    /// callback has been pinned and published.
    pub fn register_shared_disabled(
        name: impl Into<String>,
        irq: IrqId,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> Result<Self, IrqError> {
        Self::register_shared_disabled_with_affinity(name, irq, IrqAffinity::Any, handler)
    }

    /// Registers one disabled shared action on one fixed worker CPU.
    pub fn register_shared_disabled_on(
        name: impl Into<String>,
        irq: IrqId,
        cpu: usize,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> Result<Self, IrqError> {
        Self::register_shared_disabled_with_affinity(
            name,
            irq,
            IrqAffinity::Fixed(CpuId(cpu)),
            handler,
        )
    }

    fn register_shared_disabled_with_affinity(
        name: impl Into<String>,
        irq: IrqId,
        affinity: IrqAffinity,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> Result<Self, IrqError> {
        let name = name.into();
        let request = IrqRequest::new(handler)
            .share_mode(ShareMode::Shared)
            .affinity(affinity)
            .auto_enable(AutoEnable::No);
        match ax_hal::irq::request_irq(irq, request) {
            Ok(handle) => {
                info!("registered disabled {name} irq {:?}", handle.irq());
                Ok(Self {
                    name,
                    handle: Some(handle),
                })
            }
            Err(error) => {
                warn!("failed to register disabled {name} irq handler for irq {irq:?}: {error:?}");
                Err(error)
            }
        }
    }

    /// Enables this previously registered action and its backing IRQ line.
    pub fn enable(&self) -> Result<(), IrqError> {
        ax_hal::irq::enable_irq(self.required_handle()?)
    }

    /// Disables this action and updates the shared backing IRQ line.
    pub fn disable(&self) -> Result<(), IrqError> {
        ax_hal::irq::disable_irq(self.required_handle()?)
    }

    /// Releases this action's emergency line quench after device-side masking.
    ///
    /// The action stays disabled. Callers must prove that the device can no
    /// longer assert this source before reopening a shared backing line.
    pub fn release_quench(&self) -> Result<(), IrqError> {
        ax_hal::irq::release_irq_quench(self.required_handle()?)
    }

    /// Completes an ordinary deferred acknowledgement for this action.
    pub fn finish_continuation(
        &self,
        token: ax_hal::irq::IrqContinuationToken,
    ) -> Result<(), IrqError> {
        if token.action() != self.required_handle()? {
            return Err(IrqError::NotFound);
        }
        ax_hal::irq::finish_irq_continuation(token)
    }

    /// Disables this action and queues a fixed wake after this action drains.
    pub fn disable_async(&self, wake: &'static IrqDrainWake) -> Result<IrqDrainToken, IrqError> {
        ax_hal::irq::disable_irq_async(self.required_handle()?, wake)
    }

    /// Checks an action-specific asynchronous drain generation.
    pub fn action_drain_complete(&self, token: IrqDrainToken) -> Result<bool, IrqError> {
        ax_hal::irq::irq_action_drain_complete(token)
    }

    /// Waits until no hard-IRQ callback for this action remains in flight.
    pub fn synchronize(&self) -> Result<(), IrqError> {
        ax_hal::irq::synchronize_irq(self.required_handle()?)
    }

    /// Checks whether the disabled action has no hard-IRQ dispatch in flight.
    ///
    /// Unlike [`Self::synchronize`], this operation never waits. Bounded
    /// controller work may use it to advance a teardown state machine without
    /// blocking a shared worker thread.
    pub fn is_synchronized(&self) -> Result<bool, IrqError> {
        Ok(ax_hal::irq::irq_status(self.required_handle()?)?.in_flight == 0)
    }

    pub fn handle(&self) -> Option<IrqHandle> {
        self.handle
    }

    /// Removes this disabled and drained action without destroying its handler.
    #[cfg(feature = "block")]
    pub(crate) fn detach(mut self) -> Result<DetachedRegistration, (IrqError, Registration)> {
        let handle = match self.required_handle() {
            Ok(handle) => handle,
            Err(error) => return Err((error, self)),
        };
        match ax_hal::irq::detach_irq_action(handle) {
            Ok(action) => {
                self.handle = None;
                Ok(DetachedRegistration {
                    name: core::mem::take(&mut self.name),
                    action: Some(action),
                })
            }
            Err(error) => Err((error, self)),
        }
    }

    fn required_handle(&self) -> Result<IrqHandle, IrqError> {
        self.handle.ok_or(IrqError::NotFound)
    }
}

#[cfg(feature = "block")]
impl DetachedRegistration {
    /// Re-registers this handler under its original policy, initially disabled.
    pub(crate) fn reattach(mut self) -> Result<Registration, ReattachRegistrationError> {
        let action = self
            .action
            .take()
            .expect("detached IRQ registration owns exactly one action");
        match ax_hal::irq::reattach_irq_action(action) {
            Ok(handle) => Ok(Registration {
                name: core::mem::take(&mut self.name),
                handle: Some(handle),
            }),
            Err(error) => {
                let (reason, action) = error.into_parts();
                self.action = Some(action);
                Err(ReattachRegistrationError {
                    reason,
                    registration: Box::new(self),
                })
            }
        }
    }
}

#[cfg(feature = "block")]
impl ReattachRegistrationError {
    pub(crate) fn into_parts(self) -> (IrqError, DetachedRegistration) {
        (self.reason, *self.registration)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        if let Err(err) = ax_hal::irq::free_irq(handle) {
            warn!("failed to free {} irq handler: {err:?}", self.name);
        }
    }
}

#[cfg(feature = "net")]
pub(crate) struct RuntimeNetIrqRegistrar;

#[cfg(feature = "net")]
pub(crate) static NET_IRQ_REGISTRAR: RuntimeNetIrqRegistrar = RuntimeNetIrqRegistrar;

#[cfg(feature = "net")]
impl ax_net::EthernetIrqRegistration for Registration {}

#[cfg(feature = "net")]
fn map_net_irq_error(err: IrqError) -> ax_net::EthernetIrqRegistrationError {
    match err {
        IrqError::InvalidIrq | IrqError::InvalidCpu => {
            ax_net::EthernetIrqRegistrationError::InvalidIrq
        }
        IrqError::Busy | IrqError::InIrqContext => ax_net::EthernetIrqRegistrationError::Busy,
        IrqError::Unsupported | IrqError::CpuOffline => {
            ax_net::EthernetIrqRegistrationError::Unsupported
        }
        IrqError::NoMemory | IrqError::NotFound | IrqError::Timeout | IrqError::Controller => {
            ax_net::EthernetIrqRegistrationError::Other
        }
    }
}

#[cfg(feature = "net")]
impl ax_net::EthernetIrqRegistrar for RuntimeNetIrqRegistrar {
    fn register_shared(
        &self,
        name: &str,
        irq: IrqId,
        action: ax_net::EthernetIrqAction,
    ) -> Result<
        alloc::boxed::Box<dyn ax_net::EthernetIrqRegistration>,
        ax_net::EthernetIrqRegistrationError,
    > {
        let mut action = action;
        let handler = move |_ctx| match action.run() {
            ax_net::EthernetIrqOutcome::Handled => ax_hal::irq::IrqReturn::Handled,
            ax_net::EthernetIrqOutcome::Wake => ax_hal::irq::IrqReturn::Wake,
        };
        Registration::register_shared(name, irq, handler)
            .map(|registration| {
                alloc::boxed::Box::new(registration)
                    as alloc::boxed::Box<dyn ax_net::EthernetIrqRegistration>
            })
            .map_err(map_net_irq_error)
    }
}
