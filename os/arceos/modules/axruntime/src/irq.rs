#[cfg(feature = "net")]
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
#[cfg(feature = "net")]
use ax_hal::irq::IrqHandle;
use ax_hal::irq::{IrqError, IrqId, IrqSource};

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

#[cfg(feature = "net")]
pub(crate) struct RuntimeNetIrqRegistrar;

#[cfg(feature = "net")]
pub(crate) static NET_IRQ_REGISTRAR: RuntimeNetIrqRegistrar = RuntimeNetIrqRegistrar;

#[cfg(feature = "net")]
struct RuntimeNetIrqRegistration {
    name: String,
    handle: IrqHandle,
    owner_cpu: usize,
}

#[cfg(feature = "net")]
impl ax_net::PinnedNetIrqRegistration for RuntimeNetIrqRegistration {
    fn owner_cpu(&self) -> usize {
        self.owner_cpu
    }

    fn enable(&self) -> Result<(), ax_net::PinnedNetIrqError> {
        ax_hal::irq::enable_irq(self.handle).map_err(map_net_irq_error)
    }

    fn disable_and_synchronize(&self) -> Result<(), ax_net::PinnedNetIrqError> {
        match ax_hal::irq::disable_irq(self.handle) {
            Ok(()) | Err(IrqError::NotFound) => {}
            Err(error) => return Err(map_net_irq_error(error)),
        }
        match ax_hal::irq::synchronize_irq(self.handle) {
            Ok(()) | Err(IrqError::NotFound) => Ok(()),
            Err(error) => Err(map_net_irq_error(error)),
        }
    }
}

#[cfg(feature = "net")]
impl Drop for RuntimeNetIrqRegistration {
    fn drop(&mut self) {
        if let Err(error) = ax_hal::irq::free_irq(self.handle) {
            warn!(
                "failed to free network IRQ registration {}: {error:?}",
                self.name
            );
        }
    }
}

#[cfg(feature = "net")]
fn map_net_irq_error(err: IrqError) -> ax_net::PinnedNetIrqError {
    match err {
        IrqError::InvalidIrq | IrqError::InvalidCpu => ax_net::PinnedNetIrqError::Invalid,
        IrqError::Busy => ax_net::PinnedNetIrqError::AffinityConflict,
        IrqError::Unsupported | IrqError::CpuOffline => ax_net::PinnedNetIrqError::Unsupported,
        IrqError::NoMemory
        | IrqError::NotFound
        | IrqError::Timeout
        | IrqError::Controller
        | IrqError::InIrqContext => ax_net::PinnedNetIrqError::Other,
    }
}

#[cfg(feature = "net")]
impl ax_net::PinnedNetIrqRegistrar for RuntimeNetIrqRegistrar {
    fn register(
        &self,
        name: String,
        irq: IrqId,
        owner_cpu: usize,
        action: ax_net::PinnedNetIrqAction,
    ) -> Result<alloc::boxed::Box<dyn ax_net::PinnedNetIrqRegistration>, ax_net::PinnedNetIrqError>
    {
        let mut action = action;
        let request = ax_hal::irq::IrqRequest::new(move |_context| match action.run() {
            ax_net::PinnedNetIrqOutcome::Unhandled => ax_hal::irq::IrqReturn::Unhandled,
            ax_net::PinnedNetIrqOutcome::Handled => ax_hal::irq::IrqReturn::Handled,
            ax_net::PinnedNetIrqOutcome::Wake => ax_hal::irq::IrqReturn::Wake,
        })
        .execution(ax_hal::irq::IrqExecution::NonReentrant)
        .share_mode(ax_hal::irq::ShareMode::Shared)
        .auto_enable(ax_hal::irq::AutoEnable::No)
        .affinity(ax_hal::irq::IrqAffinity::Fixed(ax_hal::irq::CpuId(
            owner_cpu,
        )));
        let handle = ax_hal::irq::request_irq(irq, request).map_err(map_net_irq_error)?;
        info!("registered {name} IRQ {irq:?} on fixed CPU {owner_cpu}");
        Ok(alloc::boxed::Box::new(RuntimeNetIrqRegistration {
            name,
            handle,
            owner_cpu,
        }))
    }
}

#[cfg(all(test, feature = "net"))]
mod tests {
    #[test]
    fn network_irq_registration_requires_an_explicit_fixed_owner_cpu() {
        let source = include_str!("irq.rs");
        let registrar = source
            .split("impl ax_net::PinnedNetIrqRegistrar for RuntimeNetIrqRegistrar")
            .nth(1)
            .expect("network IRQ registrar implementation must exist");

        assert!(registrar.contains("owner_cpu"));
        assert!(registrar.contains("IrqAffinity::Fixed"));
        assert!(!registrar.contains("IrqAffinity::Any"));
    }
}
