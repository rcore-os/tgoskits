//! Owned storage for a movable SD/MMC initialization transaction.

use alloc::boxed::Box;
use core::{fmt, pin::Pin};

#[cfg(feature = "rdif")]
use super::SdioIrqHost;
use super::{
    CardInfo, CardInitPreference, InitInput, InitPoll, InitSchedule, SdioHost, SdioHost2Adapter,
    SdioHost2Irq, SdioInitRequest, SdioInitScratch, SdioSdmmc,
};
use crate::Error;

/// Host contract required by [`OwnedSdioInit`]'s internal lifetime extension.
///
/// # Safety
///
/// A data or bus request must not leak a borrowed initialization buffer. Once
/// its request guard is dropped, the host and IRQ endpoint must never access
/// that request's buffer again. Implementations must also keep request guards
/// valid when the outer [`OwnedSdioInit`] value moves while its pinned scratch
/// allocation stays fixed.
pub unsafe trait OwnedSdioInitHost: SdioHost + 'static {}

// SAFETY: the host2 adapter stores transaction guards that borrow the pinned
// buffer, aborts unfinished guards in `Drop`, and never exposes or copies the
// borrowed slice outside the guard.
unsafe impl<H: SdioHost2Irq + 'static> OwnedSdioInitHost for SdioHost2Adapter<H> {}

/// Move-only proof that card identification completed successfully.
///
/// The fields and constructor are private so a raw [`SdioSdmmc`] cannot be
/// promoted into a runtime block device. The only public producer is
/// [`OwnedSdioInit::try_into_ready`], after the initialization state machine
/// has published a successful terminal result.
pub struct InitializedSdioCard<H: SdioHost> {
    card: SdioSdmmc<H>,
    info: CardInfo,
}

impl<H: SdioHost> InitializedSdioCard<H> {
    /// Borrow the initialized card for read-only protocol inspection.
    pub fn card(&self) -> &SdioSdmmc<H> {
        &self.card
    }

    /// Return the immutable metadata established by card identification.
    pub fn card_info(&self) -> &CardInfo {
        &self.info
    }

    #[cfg(feature = "rdif")]
    pub(crate) fn into_parts(self) -> (SdioSdmmc<H>, CardInfo) {
        (self.card, self.info)
    }
}

impl<H: SdioHost> fmt::Debug for InitializedSdioCard<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InitializedSdioCard")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// A movable SD/MMC initialization transaction with pinned scratch storage.
///
/// The protocol request can temporarily lend its EXT_CSD or switch-status
/// buffer to a host data request. This type pins those buffers for the whole
/// transaction and centralizes the one lifetime extension needed to keep the
/// request in an OS-owned initialization object. Platform glue therefore does
/// not need to construct a self-reference or retain probe-stack storage.
///
/// Dropping this object drops every host request before releasing the pinned
/// scratch allocation. A hardware host must uphold [`SdioHost`]'s request-drop
/// contract and stop using a borrowed buffer when its request guard is
/// dropped.
pub struct OwnedSdioInit<H: OwnedSdioInitHost> {
    // Field order is part of the safety argument: Rust drops fields in
    // declaration order, so host loans in `request` disappear before `card`
    // and the backing allocation in `scratch`.
    request: Box<SdioInitRequest<'static, H>>,
    card: SdioSdmmc<H>,
    scratch: Pin<Box<SdioInitScratch>>,
    terminal: Option<Result<CardInfo, Error>>,
    not_before_ns: Option<u64>,
    last_now_ns: Option<u64>,
}

impl<H: OwnedSdioInitHost> OwnedSdioInit<H> {
    /// Allocate pinned protocol scratch and create an inactive transaction.
    pub fn new(card: SdioSdmmc<H>, preference: CardInitPreference) -> Self {
        let mut scratch = Box::pin(SdioInitScratch::new());
        let scratch_ptr = unsafe {
            // SAFETY: `scratch` has just been pinned in its final allocation.
            // The pointer is used only by `request`, which is dropped before
            // the allocation and is never exposed by this type.
            Pin::get_unchecked_mut(scratch.as_mut()) as *mut SdioInitScratch
        };
        let scratch_ref: &'static mut SdioInitScratch = unsafe {
            // SAFETY: the allocation is exclusively owned by this object and
            // stays pinned until after `request` is dropped. No API exposes a
            // second reference to the scratch storage.
            &mut *scratch_ptr
        };
        let request = Box::new(SdioInitRequest::new(preference, scratch_ref));

        Self {
            request,
            card,
            scratch,
            terminal: None,
            not_before_ns: None,
            last_now_ns: None,
        }
    }

    /// Delay the first hardware transition until an absolute monotonic time.
    ///
    /// This is intended for regulator, reset, and clock-settle preconditions
    /// established before the generic card protocol starts. Calls before the
    /// deadline perform no host operation and return the same absolute wake.
    pub fn with_not_before_ns(mut self, not_before_ns: u64) -> Self {
        self.not_before_ns = Some(not_before_ns);
        self
    }

    /// Advance one bounded initialization transition.
    pub fn poll_init(&mut self, input: InitInput) -> InitPoll<CardInfo> {
        if let Some(terminal) = &self.terminal {
            return clone_terminal(terminal);
        }
        if self
            .last_now_ns
            .is_some_and(|previous| input.now_ns < previous)
        {
            self.terminal = Some(Err(Error::InvalidArgument));
            return InitPoll::Failed(Error::InvalidArgument);
        }
        self.last_now_ns = Some(input.now_ns);

        if let Some(not_before_ns) = self.not_before_ns
            && input.now_ns < not_before_ns
        {
            return InitPoll::Pending(InitSchedule::wait_until(not_before_ns));
        }
        self.not_before_ns = None;

        let progress = self.card.poll_init_request(&mut self.request, input);
        match progress {
            InitPoll::Ready(info) => {
                self.terminal = Some(Ok(info.clone()));
                InitPoll::Ready(info)
            }
            InitPoll::Pending(schedule) => InitPoll::Pending(schedule),
            InitPoll::Failed(error) => {
                self.terminal = Some(Err(error));
                InitPoll::Failed(error)
            }
        }
    }

    /// Return initialized card metadata after a successful terminal result.
    pub fn card_info(&self) -> Option<&CardInfo> {
        self.terminal
            .as_ref()
            .and_then(|result| result.as_ref().ok())
    }

    #[cfg(feature = "rdif")]
    pub(crate) fn take_irq_source(
        &mut self,
    ) -> Option<super::SdioIrqSource<H::IrqEndpoint, H::IrqControl>>
    where
        H: SdioIrqHost,
    {
        self.card.host_mut().take_irq_source()
    }

    #[cfg(feature = "rdif")]
    pub(crate) fn take_evidence_irq_source(
        &mut self,
    ) -> Option<super::SdioIrqSource<H::IrqEndpoint, H::IrqControl>>
    where
        H: SdioIrqHost,
    {
        self.card.host_mut().take_evidence_irq_source()
    }

    #[cfg(feature = "rdif")]
    pub(crate) fn enable_completion_irq(&mut self) -> Result<(), Error> {
        self.card.host_mut().enable_completion_irq()
    }

    #[cfg(feature = "rdif")]
    pub(crate) fn disable_completion_irq(&mut self) -> Result<(), Error> {
        self.card.host_mut().disable_completion_irq()
    }

    #[cfg(feature = "rdif")]
    pub(crate) fn completion_irq_enabled(&self) -> bool {
        self.card.host().completion_irq_enabled()
    }

    /// Consume a successful transaction and return the initialized card.
    ///
    /// On a pending or failed transaction, ownership is returned unchanged so
    /// the controller owner can continue driving or quarantine it.
    pub fn try_into_ready(self) -> Result<InitializedSdioCard<H>, Box<Self>> {
        let Some(Ok(info)) = self.terminal.clone() else {
            return Err(Box::new(self));
        };
        let Self {
            request,
            card,
            scratch,
            terminal: _,
            not_before_ns: _,
            last_now_ns: _,
        } = self;
        drop(request);
        drop(scratch);
        Ok(InitializedSdioCard { card, info })
    }
}

fn clone_terminal(terminal: &Result<CardInfo, Error>) -> InitPoll<CardInfo> {
    match terminal {
        Ok(info) => InitPoll::Ready(info.clone()),
        Err(error) => InitPoll::Failed(*error),
    }
}

// SAFETY: all raw scratch pointers target the pinned allocation owned by the
// same object. Moving the outer object between threads does not move that
// allocation, and every mutable operation still requires `&mut self`. Host
// request guards and the host itself are required to be transferable too.
unsafe impl<H> Send for OwnedSdioInit<H>
where
    H: OwnedSdioInitHost + Send,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
}
