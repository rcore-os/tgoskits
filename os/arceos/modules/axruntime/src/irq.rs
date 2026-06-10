use alloc::{boxed::Box, string::String};
use core::ptr::NonNull;

use ax_hal::irq::{IrqError, IrqHandle, RawIrqHandler};

pub struct Registration {
    name: String,
    handle: Option<IrqHandle>,
}

impl Registration {
    pub fn register_shared(
        name: impl Into<String>,
        irq: usize,
        handler: RawIrqHandler,
        data: NonNull<()>,
    ) -> Result<Self, IrqError> {
        let name = name.into();
        match ax_hal::irq::request_shared_irq(irq, handler, data) {
            Ok(handle) => {
                info!("registered {name} irq {}", handle.irq().0);
                Ok(Self {
                    name,
                    handle: Some(handle),
                })
            }
            Err(err) => {
                warn!("failed to register {name} irq handler for irq {irq}: {err:?}");
                Err(err)
            }
        }
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
    _state: Box<T>,
}

impl<T> HandlerRegistration<T> {
    pub fn register_shared(
        name: impl Into<String>,
        irq: usize,
        state: T,
        handler: RawIrqHandler,
    ) -> Result<Self, IrqError> {
        let mut state = Box::new(state);
        let data = NonNull::from(state.as_mut()).cast();
        let registration = Registration::register_shared(name, irq, handler, data)?;
        Ok(Self {
            _registration: registration,
            _state: state,
        })
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
        IrqError::InvalidIrq | IrqError::InvalidCpu => ax_net::EthernetIrqRegistrationError::InvalidIrq,
        IrqError::Busy | IrqError::InIrqContext => ax_net::EthernetIrqRegistrationError::Busy,
        IrqError::Unsupported | IrqError::CpuOffline => {
            ax_net::EthernetIrqRegistrationError::Unsupported
        }
        IrqError::NoMemory | IrqError::NotFound | IrqError::Controller => ax_net::EthernetIrqRegistrationError::Other,
    }
}

#[cfg(feature = "net")]
impl ax_net::EthernetIrqRegistrar for RuntimeNetIrqRegistrar {
    fn register_shared(
        &self,
        name: &str,
        irq: usize,
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
