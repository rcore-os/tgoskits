#[cfg(all(feature = "maintenance", feature = "block"))]
use alloc::boxed::Box;
#[cfg(feature = "maintenance")]
use alloc::string::String;
#[cfg(feature = "maintenance")]
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

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
#[cfg(feature = "maintenance")]
use ax_hal::irq::{
    AutoEnable, CpuId, IrqAffinity, IrqContext, IrqHandle, IrqRequest, IrqReturn, ShareMode,
};
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

#[cfg(feature = "maintenance")]
mod registration {
    use super::*;

    const IRQ_REGISTRATION_QUARANTINE_CAPACITY: usize = 256;
    const QUARANTINE_SLOT_FREE: u8 = 0;
    const QUARANTINE_SLOT_RESERVED: u8 = 1;
    const QUARANTINE_SLOT_OCCUPIED: u8 = 2;

    /// Owned registration of one IRQ action.
    ///
    /// Every live registration reserves fixed fail-closed storage before publishing
    /// its callback. Only explicit [`Self::close`] runs the fallible disable,
    /// synchronize, and removal protocol. Implicit destruction retains the name
    /// and generation-bearing handle in the named quarantine without touching
    /// hardware or the IRQ framework.
    #[must_use = "explicitly close the IRQ registration or it enters fail-closed quarantine"]
    pub(crate) struct Registration {
        name: String,
        handle: Option<IrqHandle>,
        quarantine: Option<IrqRegistrationQuarantineReservation>,
        quarantine_reason: IrqRegistrationQuarantineReason,
    }

    impl core::fmt::Debug for Registration {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter
                .debug_struct("Registration")
                .field("name", &self.name)
                .field("handle", &self.handle)
                .field("quarantine_reserved", &self.quarantine.is_some())
                .finish()
        }
    }

    /// Failed explicit IRQ-action close with the complete registration retained.
    #[derive(Debug, thiserror::Error)]
    #[error("failed to close IRQ registration: {reason:?}")]
    #[must_use = "the retained registration must be retried or explicitly quarantined"]
    pub(crate) struct RegistrationCloseFailure {
        reason: IrqError,
        registration: Registration,
    }

    /// Move-only host IRQ callback retained while a guest owns the device line.
    #[cfg(feature = "block")]
    pub(crate) struct DetachedRegistration {
        name: String,
        action: Option<ax_hal::irq::DetachedIrqAction>,
        /// Proof that the RISC-V controller lease was released before guest
        /// route activation. Other architectures retain their prepared line
        /// until their irqchip supplies the same explicit release contract.
        released_line: Option<ax_hal::irq::ReleasedIrqLineProof>,
        quarantine: Option<IrqRegistrationQuarantineReservation>,
        quarantine_reason: IrqRegistrationQuarantineReason,
    }

    #[cfg(feature = "block")]
    pub(crate) struct ReattachRegistrationError {
        reason: IrqError,
        registration: Box<DetachedRegistration>,
    }

    struct QuarantinedRegistration {
        _name: String,
        _handle: IrqHandle,
        _reason: IrqRegistrationQuarantineReason,
    }

    #[derive(Clone, Copy)]
    enum IrqRegistrationQuarantineReason {
        DropWithoutClose,
        TeardownFailed(IrqError),
    }

    impl core::fmt::Debug for IrqRegistrationQuarantineReason {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::DropWithoutClose => formatter.write_str("DropWithoutClose"),
                Self::TeardownFailed(error) => formatter
                    .debug_tuple("TeardownFailed")
                    .field(error)
                    .finish(),
            }
        }
    }

    struct FixedQuarantineSlot<T> {
        state: AtomicU8,
        record: UnsafeCell<MaybeUninit<T>>,
    }

    impl<T> FixedQuarantineSlot<T> {
        const fn new() -> Self {
            Self {
                state: AtomicU8::new(QUARANTINE_SLOT_FREE),
                record: UnsafeCell::new(MaybeUninit::uninit()),
            }
        }
    }

    // SAFETY: a successful FREE -> RESERVED transition grants one reservation
    // exclusive write access to `record`. OCCUPIED records are immutable and are
    // intentionally retained until shutdown; this module never returns references
    // to them or writes them again.
    unsafe impl<T: Send> Sync for FixedQuarantineSlot<T> {}

    struct FixedQuarantineRegistry<T, const CAPACITY: usize> {
        slots: [FixedQuarantineSlot<T>; CAPACITY],
        occupied: AtomicUsize,
    }

    impl<T, const CAPACITY: usize> FixedQuarantineRegistry<T, CAPACITY> {
        const fn new() -> Self {
            Self {
                slots: [const { FixedQuarantineSlot::new() }; CAPACITY],
                occupied: AtomicUsize::new(0),
            }
        }

        fn reserve_slot(&self) -> Option<usize> {
            self.slots.iter().position(|slot| {
                slot.state
                    .compare_exchange(
                        QUARANTINE_SLOT_FREE,
                        QUARANTINE_SLOT_RESERVED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            })
        }

        fn release_slot(&self, slot_index: usize) {
            let slot = &self.slots[slot_index];
            assert_eq!(
                slot.state.compare_exchange(
                    QUARANTINE_SLOT_RESERVED,
                    QUARANTINE_SLOT_FREE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ),
                Ok(QUARANTINE_SLOT_RESERVED),
                "IRQ quarantine reservation released an invalid slot"
            );
        }

        fn retain(&self, slot_index: usize, record: T) -> usize {
            let slot = &self.slots[slot_index];
            assert_eq!(
                slot.state.load(Ordering::Acquire),
                QUARANTINE_SLOT_RESERVED,
                "IRQ quarantine reservation lost exclusive slot ownership"
            );
            unsafe {
                // SAFETY: the successful reservation transition gives this unique
                // slot owner the only write permission. The record is initialized
                // before OCCUPIED is published with Release.
                (*slot.record.get()).write(record);
            }
            slot.state
                .store(QUARANTINE_SLOT_OCCUPIED, Ordering::Release);
            self.occupied.fetch_add(1, Ordering::AcqRel);
            self.occupied_count()
        }

        fn occupied_count(&self) -> usize {
            self.occupied.load(Ordering::Acquire)
        }
    }

    type IrqRegistrationQuarantineRegistry =
        FixedQuarantineRegistry<QuarantinedRegistration, IRQ_REGISTRATION_QUARANTINE_CAPACITY>;
    static IRQ_REGISTRATION_QUARANTINE: IrqRegistrationQuarantineRegistry =
        IrqRegistrationQuarantineRegistry::new();

    struct IrqRegistrationQuarantineReservation {
        slot: Option<u16>,
    }

    impl IrqRegistrationQuarantineReservation {
        fn reserve() -> Result<Self, IrqError> {
            let slot = IRQ_REGISTRATION_QUARANTINE
                .reserve_slot()
                .ok_or(IrqError::NoMemory)?;
            let slot = match u16::try_from(slot) {
                Ok(slot) => slot,
                Err(_) => {
                    IRQ_REGISTRATION_QUARANTINE.release_slot(slot);
                    return Err(IrqError::NoMemory);
                }
            };
            Ok(Self { slot: Some(slot) })
        }

        fn retain(mut self, registration: QuarantinedRegistration) -> usize {
            let slot = self
                .slot
                .take()
                .expect("live IRQ quarantine reservation owns one slot");
            IRQ_REGISTRATION_QUARANTINE.retain(usize::from(slot), registration)
        }
    }

    impl Drop for IrqRegistrationQuarantineReservation {
        fn drop(&mut self) {
            let Some(slot) = self.slot.take() else {
                return;
            };
            IRQ_REGISTRATION_QUARANTINE.release_slot(usize::from(slot));
        }
    }

    impl Registration {
        /// Registers one disabled shared action on one fixed worker CPU.
        pub(crate) fn register_shared_disabled_on(
            name: impl Into<String>,
            irq: IrqId,
            cpu: usize,
            handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
        ) -> Result<Self, IrqError> {
            let name = name.into();
            let quarantine = IrqRegistrationQuarantineReservation::reserve()?;
            let request = IrqRequest::new(handler)
                .share_mode(ShareMode::Shared)
                .affinity(IrqAffinity::Fixed(CpuId(cpu)))
                .auto_enable(AutoEnable::No);
            match ax_hal::irq::request_irq(irq, request) {
                Ok(handle) => {
                    info!("registered disabled {name} irq {:?}", handle.irq());
                    Ok(Self {
                        name,
                        handle: Some(handle),
                        quarantine: Some(quarantine),
                        quarantine_reason: IrqRegistrationQuarantineReason::DropWithoutClose,
                    })
                }
                Err(error) => {
                    warn!(
                        "failed to register disabled {name} irq handler for irq {irq:?}: {error:?}"
                    );
                    Err(error)
                }
            }
        }

        /// Enables this previously registered action and its backing IRQ line.
        pub(crate) fn enable(&self) -> Result<(), IrqError> {
            ax_hal::irq::enable_irq(self.required_handle()?)
        }

        /// Disables this action and updates the shared backing IRQ line.
        pub(crate) fn disable(&self) -> Result<(), IrqError> {
            ax_hal::irq::disable_irq(self.required_handle()?)
        }

        /// Masks the complete backing line and records containment ownership
        /// for this action until explicit recovery.
        pub(crate) fn quench_line(&self) -> Result<(), IrqError> {
            ax_hal::irq::quench_irq(self.required_handle()?)
        }

        /// Releases this action's emergency line quench after device-side masking.
        ///
        /// The action stays enabled. Callers must prove that the device can no
        /// longer assert this source before reopening a shared backing line.
        pub(crate) fn release_quench(&self) -> Result<(), IrqError> {
            ax_hal::irq::release_irq_quench(self.required_handle()?)
        }

        /// Waits until no hard-IRQ callback for this action remains in flight.
        pub(crate) fn synchronize(&self) -> Result<(), IrqError> {
            ax_hal::irq::synchronize_irq(self.required_handle()?)
        }

        /// Returns a task-context snapshot of this action and backing line.
        pub(crate) fn status(&self) -> Result<ax_hal::irq::IrqStatus, IrqError> {
            ax_hal::irq::irq_status(self.required_handle()?)
        }

        /// Disables, synchronizes, and removes this action as one linear close.
        ///
        /// Failure returns the still-registered owner. Device runtimes must retain
        /// it in a named quarantine together with the device-side source control;
        /// losing only the numeric handle would leave an undiagnosable callback in
        /// the shared descriptor.
        /// # Errors
        ///
        /// Returns a [`RegistrationCloseFailure`] that retains the complete live
        /// registration whenever disable, drain, or framework removal fails.
        pub(crate) fn close(mut self) -> Result<(), RegistrationCloseFailure> {
            if let Err(reason) = self.disable() {
                self.quarantine_reason = IrqRegistrationQuarantineReason::TeardownFailed(reason);
                return Err(RegistrationCloseFailure {
                    reason,
                    registration: self,
                });
            }
            if let Err(reason) = self.synchronize() {
                self.quarantine_reason = IrqRegistrationQuarantineReason::TeardownFailed(reason);
                return Err(RegistrationCloseFailure {
                    reason,
                    registration: self,
                });
            }
            let handle = match self.required_handle() {
                Ok(handle) => handle,
                Err(reason) => {
                    self.quarantine_reason =
                        IrqRegistrationQuarantineReason::TeardownFailed(reason);
                    return Err(RegistrationCloseFailure {
                        reason,
                        registration: self,
                    });
                }
            };
            match ax_hal::irq::free_irq(handle) {
                Ok(()) => {
                    self.handle = None;
                    Ok(())
                }
                Err(reason) => Err(RegistrationCloseFailure {
                    reason,
                    registration: {
                        self.quarantine_reason =
                            IrqRegistrationQuarantineReason::TeardownFailed(reason);
                        self
                    },
                }),
            }
        }

        /// Removes this disabled and drained action without destroying its handler.
        #[cfg(feature = "block")]
        pub(crate) fn detach(mut self) -> Result<DetachedRegistration, (IrqError, Registration)> {
            let handle = match self.required_handle() {
                Ok(handle) => handle,
                Err(error) => return Err((error, self)),
            };
            #[cfg(target_arch = "riscv64")]
            let detached = ax_hal::irq::detach_irq_action_and_release_line(handle)
                .map(|(action, released_line)| (action, Some(released_line)));
            #[cfg(not(target_arch = "riscv64"))]
            let detached = ax_hal::irq::detach_irq_action(handle).map(|action| (action, None));

            match detached {
                Ok((action, released_line)) => {
                    self.handle = None;
                    Ok(DetachedRegistration {
                        name: core::mem::take(&mut self.name),
                        action: Some(action),
                        released_line,
                        quarantine: self.quarantine.take(),
                        quarantine_reason: self.quarantine_reason,
                    })
                }
                Err(error) => Err((error, self)),
            }
        }

        fn required_handle(&self) -> Result<IrqHandle, IrqError> {
            self.handle.ok_or(IrqError::NotFound)
        }
    }

    impl RegistrationCloseFailure {
        /// Splits the failure into its reason and retained registration.
        pub(crate) fn into_parts(self) -> (IrqError, Registration) {
            (self.reason, self.registration)
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
            let released_line = self.released_line.take();
            match ax_hal::irq::reattach_irq_action(action) {
                Ok(handle) => {
                    // A successful reattach published a fresh prepared-line
                    // generation before the disabled host callback. The old
                    // release proof has completed its linear handoff role.
                    drop(released_line);
                    Ok(Registration {
                        name: core::mem::take(&mut self.name),
                        handle: Some(handle),
                        quarantine: self.quarantine.take(),
                        quarantine_reason: self.quarantine_reason,
                    })
                }
                Err(error) => {
                    let (reason, action) = error.into_parts();
                    self.action = Some(action);
                    self.released_line = released_line;
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
            let Some(handle) = self.handle else {
                return;
            };
            self.handle = None;
            let reservation = self
                .quarantine
                .take()
                .expect("live IRQ registration retains fail-closed capacity");
            let reason = self.quarantine_reason;
            let quarantined = reservation.retain(QuarantinedRegistration {
                _name: core::mem::take(&mut self.name),
                _handle: handle,
                _reason: reason,
            });
            error!(
                "quarantined IRQ {:?} action {} without implicit teardown ({reason:?}); \
                 {quarantined} registration(s) retained",
                handle.irq(),
                handle.id()
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::FixedQuarantineRegistry;

        #[test]
        fn unused_quarantine_reservation_returns_to_the_fixed_pool() {
            static REGISTRY: FixedQuarantineRegistry<usize, 1> = FixedQuarantineRegistry::new();

            let slot = REGISTRY.reserve_slot().expect("first reservation succeeds");
            assert!(REGISTRY.reserve_slot().is_none());
            REGISTRY.release_slot(slot);
            assert!(REGISTRY.reserve_slot().is_some());
        }

        #[test]
        fn retained_quarantine_record_permanently_consumes_its_reserved_slot() {
            static REGISTRY: FixedQuarantineRegistry<usize, 1> = FixedQuarantineRegistry::new();

            let slot = REGISTRY.reserve_slot().expect("first reservation succeeds");
            assert_eq!(REGISTRY.retain(slot, 7), 1);
            assert_eq!(REGISTRY.occupied_count(), 1);
            assert!(REGISTRY.reserve_slot().is_none());
        }
    }
}

#[cfg(all(feature = "maintenance", feature = "block"))]
pub(crate) use registration::DetachedRegistration;
#[cfg(feature = "maintenance")]
pub(crate) use registration::Registration;
