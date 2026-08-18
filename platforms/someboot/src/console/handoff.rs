use core::{
    marker::PhantomData,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum OwnershipState {
    Early        = 0,
    Preparing    = 1,
    Runtime      = 2,
    FailedClosed = 3,
}

impl OwnershipState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Early,
            1 => Self::Preparing,
            2 => Self::Runtime,
            3 => Self::FailedClosed,
            _ => unreachable!("invalid console ownership state"),
        }
    }
}

/// Error returned by an invalid early/runtime console ownership transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConsoleHandoffError {
    /// The requested transition is not valid from the current ownership state.
    #[error("invalid console ownership transition")]
    InvalidState,
}

static OWNERSHIP: AtomicU8 = AtomicU8::new(OwnershipState::Early as u8);
static EARLY_ACCESS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

pub(super) struct EarlyAccessGuard {
    counted: bool,
    _not_send: PhantomData<*mut ()>,
}

impl Drop for EarlyAccessGuard {
    fn drop(&mut self) {
        if self.counted {
            EARLY_ACCESS_IN_FLIGHT.fetch_sub(1, Ordering::Release);
        }
    }
}

#[cfg(not(test))]
fn synchronization_available() -> bool {
    // AArch64 exclusive atomics are not reliable before MMU enablement. The
    // transition itself is a post-MMU runtime operation, while the pre-MMU
    // console remains single-core and bypasses these atomic words entirely.
    crate::mem::mmu::is_mmu_enabled()
}

#[cfg(test)]
const fn synchronization_available() -> bool {
    true
}

pub(super) fn try_enter_early() -> Option<EarlyAccessGuard> {
    if !synchronization_available() {
        return Some(EarlyAccessGuard {
            counted: false,
            _not_send: PhantomData,
        });
    }

    if OwnershipState::from_raw(OWNERSHIP.load(Ordering::Acquire)) != OwnershipState::Early {
        return None;
    }
    EARLY_ACCESS_IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
    if OwnershipState::from_raw(OWNERSHIP.load(Ordering::Acquire)) == OwnershipState::Early {
        Some(EarlyAccessGuard {
            counted: true,
            _not_send: PhantomData,
        })
    } else {
        EARLY_ACCESS_IN_FLIGHT.fetch_sub(1, Ordering::Release);
        None
    }
}

/// Stops new early-console accesses and waits for in-flight accesses to leave.
pub fn begin() -> Result<(), ConsoleHandoffError> {
    if !synchronization_available() {
        return Err(ConsoleHandoffError::InvalidState);
    }
    OWNERSHIP
        .compare_exchange(
            OwnershipState::Early as u8,
            OwnershipState::Preparing as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| ConsoleHandoffError::InvalidState)?;

    while EARLY_ACCESS_IN_FLIGHT.load(Ordering::Acquire) != 0 {
        core::hint::spin_loop();
    }
    Ok(())
}

/// Publishes successful runtime ownership after configuration and routing.
pub fn commit() -> Result<(), ConsoleHandoffError> {
    OWNERSHIP
        .compare_exchange(
            OwnershipState::Preparing as u8,
            OwnershipState::Runtime as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map(|_| ())
        .map_err(|_| ConsoleHandoffError::InvalidState)
}

/// Restores early ownership after a failure known to have restored hardware.
pub fn rollback() -> Result<(), ConsoleHandoffError> {
    OWNERSHIP
        .compare_exchange(
            OwnershipState::Preparing as u8,
            OwnershipState::Early as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map(|_| ())
        .map_err(|_| ConsoleHandoffError::InvalidState)
}

/// Permanently prevents low-level register access after an uncertain failure.
pub fn fail_closed() {
    OWNERSHIP.store(OwnershipState::FailedClosed as u8, Ordering::Release);
    while EARLY_ACCESS_IN_FLIGHT.load(Ordering::Acquire) != 0 {
        core::hint::spin_loop();
    }
}

#[cfg(test)]
pub(super) fn reset() {
    EARLY_ACCESS_IN_FLIGHT.store(0, Ordering::Release);
    OWNERSHIP.store(OwnershipState::Early as u8, Ordering::Release);
}

#[cfg(test)]
pub(super) fn state() -> u8 {
    OWNERSHIP.load(Ordering::Acquire)
}

#[cfg(test)]
pub(super) const EARLY: u8 = OwnershipState::Early as u8;
#[cfg(test)]
pub(super) const PREPARING: u8 = OwnershipState::Preparing as u8;
#[cfg(test)]
pub(super) const RUNTIME: u8 = OwnershipState::Runtime as u8;
#[cfg(test)]
pub(super) const FAILED_CLOSED: u8 = OwnershipState::FailedClosed as u8;
