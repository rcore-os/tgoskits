use alloc::{boxed::Box, string::String};
use core::ptr::NonNull;

#[cfg(all(
    any(
        target_arch = "loongarch64",
        target_arch = "riscv64",
        target_arch = "x86_64",
    ),
    feature = "plat-dyn"
))]
use ax_hal::irq::CPU_LOCAL_IRQ_DOMAIN;
#[cfg(all(
    any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv64",
        target_arch = "x86_64",
    ),
    feature = "plat-dyn"
))]
use ax_hal::irq::HwIrq;
use ax_hal::irq::{IrqError, IrqHandle, IrqId, IrqSource, RawIrqHandler};

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

#[cfg(all(
    feature = "plat-dyn",
    any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv64"
    )
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

#[cfg(not(all(
    feature = "plat-dyn",
    any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv64"
    )
)))]
fn resolve_fdt_irq_spec(_spec: ax_driver::FdtIrqSpec) -> Result<IrqId, IrqError> {
    Err(IrqError::Unsupported)
}

/// Resolves a per-CPU trap IRQ through the platform IRQ domain.
#[cfg(all(target_arch = "aarch64", feature = "plat-dyn"))]
pub fn resolve_percpu_irq(irq: usize) -> IrqId {
    let hwirq = HwIrq(u32::try_from(irq).expect("AArch64 per-CPU IRQ exceeds GIC INTID width"));
    ax_hal::irq::resolve_percpu_irq(hwirq).expect("AArch64 per-CPU IRQ domain is not registered")
}

/// Resolves a per-CPU trap IRQ through the platform IRQ domain.
#[cfg(all(
    any(target_arch = "loongarch64", target_arch = "x86_64"),
    feature = "plat-dyn"
))]
pub fn resolve_percpu_irq(irq: usize) -> IrqId {
    IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(irq as u32))
}

/// Resolves a per-CPU trap IRQ through the platform IRQ domain.
#[cfg(not(all(
    any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "x86_64"
    ),
    feature = "plat-dyn"
)))]
pub fn resolve_percpu_irq(irq: usize) -> IrqId {
    #[cfg(all(target_arch = "riscv64", feature = "plat-dyn"))]
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

impl Registration {
    pub fn register_shared(
        name: impl Into<String>,
        irq: IrqId,
        handler: RawIrqHandler,
        data: NonNull<()>,
    ) -> Result<Self, IrqError> {
        let name = name.into();
        match ax_hal::irq::request_shared_irq(irq, handler, data) {
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

    pub fn handle(&self) -> Option<IrqHandle> {
        self.handle
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

pub struct HandlerRegistration<T> {
    _registration: Registration,
    state: Box<T>,
}

impl<T> HandlerRegistration<T> {
    pub fn register_shared(
        name: impl Into<String>,
        irq: IrqId,
        state: T,
        handler: RawIrqHandler,
    ) -> Result<Self, IrqError> {
        let mut state = Box::new(state);
        let data = NonNull::from(state.as_mut()).cast();
        let registration = Registration::register_shared(name, irq, handler, data)?;
        Ok(Self {
            _registration: registration,
            state,
        })
    }

    pub fn state(&self) -> &T {
        &self.state
    }

    pub fn handle(&self) -> Option<IrqHandle> {
        self._registration.handle()
    }
}

#[cfg(feature = "net")]
pub(crate) struct RuntimeNetIrqRegistrar;

#[cfg(feature = "net")]
pub(crate) static NET_IRQ_REGISTRAR: RuntimeNetIrqRegistrar = RuntimeNetIrqRegistrar;

#[cfg(feature = "net")]
struct RuntimeNetIrqState {
    action: ax_net::EthernetIrqAction,
}

#[cfg(feature = "net")]
impl ax_net::EthernetIrqRegistration for HandlerRegistration<RuntimeNetIrqState> {}

#[cfg(feature = "net")]
unsafe fn handle_net_irq(
    _ctx: ax_hal::irq::IrqContext,
    data: NonNull<()>,
) -> ax_hal::irq::IrqReturn {
    let state = unsafe { data.cast::<RuntimeNetIrqState>().as_ref() };
    match unsafe { state.action.run() } {
        ax_net::EthernetIrqOutcome::Handled => ax_hal::irq::IrqReturn::Handled,
        ax_net::EthernetIrqOutcome::Wake => ax_hal::irq::IrqReturn::Wake,
    }
}

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
    ) -> Result<Box<dyn ax_net::EthernetIrqRegistration>, ax_net::EthernetIrqRegistrationError>
    {
        HandlerRegistration::register_shared(
            name,
            irq,
            RuntimeNetIrqState { action },
            handle_net_irq,
        )
        .map(|registration| Box::new(registration) as Box<dyn ax_net::EthernetIrqRegistration>)
        .map_err(map_net_irq_error)
    }
}
