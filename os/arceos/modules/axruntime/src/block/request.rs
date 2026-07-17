use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use thiserror::Error;

const STATE_BITS: u32 = 4;
const STATE_MASK: u64 = (1 << STATE_BITS) - 1;
const MAX_GENERATION: u64 = u64::MAX >> STATE_BITS;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum RequestState {
    Free        = 0,
    Reserved    = 1,
    Staged      = 2,
    Dispatching = 3,
    InFlight    = 4,
    Completing  = 5,
    TimingOut   = 6,
    Terminal    = 7,
    Canceling   = 8,
}

impl RequestState {
    fn decode(value: u64) -> Result<Self, TagError> {
        match value & STATE_MASK {
            0 => Ok(Self::Free),
            1 => Ok(Self::Reserved),
            2 => Ok(Self::Staged),
            3 => Ok(Self::Dispatching),
            4 => Ok(Self::InFlight),
            5 => Ok(Self::Completing),
            6 => Ok(Self::TimingOut),
            7 => Ok(Self::Terminal),
            8 => Ok(Self::Canceling),
            _ => Err(TagError::CorruptState),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RequestTag {
    slot: u8,
    generation: u64,
}

impl RequestTag {
    pub(crate) const fn slot(self) -> usize {
        self.slot as usize
    }

    #[cfg(test)]
    pub(crate) const fn generation(self) -> u64 {
        self.generation
    }

    pub(crate) fn into_request_id(self) -> Result<rdif_block::RequestId, TagError> {
        let generation = usize::try_from(self.generation).map_err(|_| TagError::IdOverflow)?;
        let encoded = generation
            .checked_mul(64)
            .and_then(|value| value.checked_add(self.slot()))
            .ok_or(TagError::IdOverflow)?;
        rdif_block::RequestId::try_new(encoded).ok_or(TagError::IdOverflow)
    }

    pub(crate) fn from_request_id(id: rdif_block::RequestId) -> Result<Self, TagError> {
        if id.is_inline() {
            return Err(TagError::InlineIdentity);
        }
        let encoded = usize::from(id);
        let generation = encoded / 64;
        let slot = encoded % 64;
        if generation == 0 || generation as u64 > MAX_GENERATION {
            return Err(TagError::Stale);
        }
        Ok(Self {
            slot: slot as u8,
            generation: generation as u64,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum TagError {
    #[error("block request tag capacity must be in 1..=64")]
    InvalidCapacity,
    #[error("no block request tag is available")]
    Exhausted,
    #[error("block request tag generation is exhausted")]
    GenerationExhausted,
    #[error("block request tag does not identify the current slot generation")]
    Stale,
    #[error("an inline request identity cannot enter the hardware tag namespace")]
    InlineIdentity,
    #[error("block request state transition is invalid")]
    InvalidTransition,
    #[error("block request slot contains an invalid encoded state")]
    CorruptState,
    #[error("block request identity cannot be represented by rdif-block")]
    IdOverflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClaimOwner {
    Completion,
    Timeout,
    Cancel,
}

#[must_use = "a completion claim must publish the recovered request ownership"]
pub(crate) struct CompletionClaim<'tags, const N: usize> {
    tags: &'tags RequestTagSet<N>,
    tag: RequestTag,
}

impl<const N: usize> CompletionClaim<'_, N> {
    pub(crate) fn finish(self) -> Result<(), TagError> {
        self.tags
            .transition(self.tag, RequestState::Completing, RequestState::Terminal)
    }
}

/// Proof that the watchdog exclusively owns timeout recovery for one request.
///
/// This type deliberately exposes no terminal-publication operation. The
/// request remains `TimingOut` until controller recovery returns its DMA
/// ownership and calls [`RequestTagSet::finish_timeout_after_return`].
#[must_use = "a timeout claim must be retained until recovery is scheduled"]
pub(crate) struct TimeoutClaim<'tags, const N: usize> {
    _tags: &'tags RequestTagSet<N>,
    _tag: RequestTag,
}

/// Proof that cancellation exclusively owns the terminal request race.
///
/// A staged request can return directly from the serialized hctx worker. An
/// in-flight request remains `Canceling` until controller recovery proves that
/// DMA stopped and returns the original owned request.
#[must_use = "a cancellation claim must be serviced by the hardware-queue worker"]
pub(crate) struct CancelClaim<'tags, const N: usize> {
    _tags: &'tags RequestTagSet<N>,
    _tag: RequestTag,
    previous: RequestState,
}

impl<const N: usize> CancelClaim<'_, N> {
    pub(crate) const fn requires_dma_quiesce(&self) -> bool {
        matches!(self.previous, RequestState::InFlight)
    }
}

pub(crate) struct RequestTagSet<const N: usize> {
    capacity: usize,
    cursor: AtomicUsize,
    slots: [AtomicU64; N],
}

impl<const N: usize> RequestTagSet<N> {
    pub(crate) fn new(capacity: usize) -> Result<Self, TagError> {
        if capacity == 0 || capacity > N || capacity > 64 {
            return Err(TagError::InvalidCapacity);
        }
        Ok(Self {
            capacity,
            cursor: AtomicUsize::new(0),
            slots: [const { AtomicU64::new(0) }; N],
        })
    }

    pub(crate) fn reserve(&self) -> Result<RequestTag, TagError> {
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % self.capacity;
        let mut exhausted_generation = false;
        for offset in 0..self.capacity {
            let slot = (start + offset) % self.capacity;
            let control = self.slots[slot].load(Ordering::Acquire);
            if RequestState::decode(control)? != RequestState::Free {
                continue;
            }
            let generation = decode_generation(control);
            let Some(next_generation) = generation.checked_add(1) else {
                exhausted_generation = true;
                continue;
            };
            if next_generation > MAX_GENERATION {
                exhausted_generation = true;
                continue;
            }
            let reserved = encode(next_generation, RequestState::Reserved);
            if self.slots[slot]
                .compare_exchange(control, reserved, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(RequestTag {
                    slot: slot as u8,
                    generation: next_generation,
                });
            }
        }
        if exhausted_generation {
            Err(TagError::GenerationExhausted)
        } else {
            Err(TagError::Exhausted)
        }
    }

    pub(crate) fn mark_staged(&self, tag: RequestTag) -> Result<(), TagError> {
        self.transition(tag, RequestState::Reserved, RequestState::Staged)
    }

    /// Claims the exact interval in which the driver decides whether it keeps
    /// request ownership. Timeout and cancellation cannot claim this state.
    pub(crate) fn begin_dispatch(&self, tag: RequestTag) -> Result<(), TagError> {
        let slot = self.slot(tag)?;
        loop {
            let observed = slot.load(Ordering::Acquire);
            validate_generation(observed, tag)?;
            if !matches!(
                RequestState::decode(observed)?,
                RequestState::Reserved | RequestState::Staged
            ) {
                return Err(TagError::InvalidTransition);
            }
            let updated = encode(tag.generation, RequestState::Dispatching);
            if slot
                .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    /// Returns a driver-rejected request to software staging.
    pub(crate) fn restore_after_rejection(&self, tag: RequestTag) -> Result<(), TagError> {
        self.transition(tag, RequestState::Dispatching, RequestState::Staged)
    }

    pub(crate) fn mark_inflight(&self, tag: RequestTag) -> Result<(), TagError> {
        self.transition(tag, RequestState::Dispatching, RequestState::InFlight)
    }

    pub(crate) fn claim_completion(
        &self,
        tag: RequestTag,
    ) -> Result<CompletionClaim<'_, N>, TagError> {
        let _previous = self.claim(tag, ClaimOwner::Completion)?;
        Ok(CompletionClaim { tags: self, tag })
    }

    pub(crate) fn claim_timeout(&self, tag: RequestTag) -> Result<TimeoutClaim<'_, N>, TagError> {
        let _previous = self.claim(tag, ClaimOwner::Timeout)?;
        Ok(TimeoutClaim {
            _tags: self,
            _tag: tag,
        })
    }

    pub(crate) fn claim_cancel(&self, tag: RequestTag) -> Result<CancelClaim<'_, N>, TagError> {
        let previous = self.claim(tag, ClaimOwner::Cancel)?;
        Ok(CancelClaim {
            _tags: self,
            _tag: tag,
            previous,
        })
    }

    pub(crate) fn release(&self, tag: RequestTag) -> Result<(), TagError> {
        self.transition(tag, RequestState::Terminal, RequestState::Free)
    }

    /// Returns a request that never crossed the driver acceptance boundary.
    pub(crate) fn abandon_unaccepted(&self, tag: RequestTag) -> Result<(), TagError> {
        let slot = self.slot(tag)?;
        loop {
            let observed = slot.load(Ordering::Acquire);
            validate_generation(observed, tag)?;
            if !matches!(
                RequestState::decode(observed)?,
                RequestState::Reserved | RequestState::Staged
            ) {
                return Err(TagError::InvalidTransition);
            }
            if slot
                .compare_exchange_weak(
                    observed,
                    encode(tag.generation, RequestState::Free),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    /// Publishes terminal state only after timeout recovery returned the owned
    /// request and proved that device access has stopped.
    pub(crate) fn finish_timeout_after_return(&self, tag: RequestTag) -> Result<(), TagError> {
        self.transition(tag, RequestState::TimingOut, RequestState::Terminal)
    }

    /// Publishes a cancelled request only after software staging or recovered
    /// DMA ownership returned the complete request value.
    pub(crate) fn finish_cancel_after_return(&self, tag: RequestTag) -> Result<(), TagError> {
        self.transition(tag, RequestState::Canceling, RequestState::Terminal)
    }

    pub(crate) fn state(&self, tag: RequestTag) -> Result<RequestState, TagError> {
        let control = self.slot(tag)?.load(Ordering::Acquire);
        validate_generation(control, tag)?;
        RequestState::decode(control)
    }

    fn claim(&self, tag: RequestTag, owner: ClaimOwner) -> Result<RequestState, TagError> {
        let desired = match owner {
            ClaimOwner::Completion => RequestState::Completing,
            ClaimOwner::Timeout => RequestState::TimingOut,
            ClaimOwner::Cancel => RequestState::Canceling,
        };
        let slot = self.slot(tag)?;
        loop {
            let observed = slot.load(Ordering::Acquire);
            validate_generation(observed, tag)?;
            let state = RequestState::decode(observed)?;
            let claimable = match owner {
                ClaimOwner::Completion => matches!(
                    state,
                    RequestState::Reserved
                        | RequestState::Staged
                        | RequestState::Dispatching
                        | RequestState::InFlight
                ),
                ClaimOwner::Timeout => {
                    matches!(state, RequestState::Staged | RequestState::InFlight)
                }
                ClaimOwner::Cancel => {
                    matches!(state, RequestState::Staged | RequestState::InFlight)
                }
            };
            if !claimable {
                return Err(TagError::InvalidTransition);
            }
            let updated = encode(tag.generation, desired);
            if slot
                .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(state);
            }
        }
    }

    fn transition(
        &self,
        tag: RequestTag,
        expected: RequestState,
        desired: RequestState,
    ) -> Result<(), TagError> {
        let slot = self.slot(tag)?;
        let expected = encode(tag.generation, expected);
        let desired = encode(tag.generation, desired);
        slot.compare_exchange(expected, desired, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|observed| {
                if decode_generation(observed) != tag.generation {
                    TagError::Stale
                } else {
                    TagError::InvalidTransition
                }
            })
    }

    fn slot(&self, tag: RequestTag) -> Result<&AtomicU64, TagError> {
        if tag.slot() >= self.capacity {
            return Err(TagError::Stale);
        }
        Ok(&self.slots[tag.slot()])
    }
}

fn encode(generation: u64, state: RequestState) -> u64 {
    (generation << STATE_BITS) | state as u64
}

fn decode_generation(control: u64) -> u64 {
    control >> STATE_BITS
}

fn validate_generation(control: u64, tag: RequestTag) -> Result<(), TagError> {
    if decode_generation(control) == tag.generation {
        Ok(())
    } else {
        Err(TagError::Stale)
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use super::*;

    #[test]
    fn generation_changes_before_a_reused_slot_is_published() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let first = tags.reserve().unwrap();
        tags.begin_dispatch(first).unwrap();
        tags.mark_inflight(first).unwrap();
        tags.claim_completion(first).unwrap().finish().unwrap();
        tags.release(first).unwrap();

        let second = tags.reserve().unwrap();
        assert_ne!(first.generation(), second.generation());
        assert_eq!(tags.state(first), Err(TagError::Stale));
        assert_eq!(tags.state(second), Ok(RequestState::Reserved));
    }

    #[test]
    fn completion_and_timeout_have_exactly_one_terminal_owner() {
        let tags = Arc::new(RequestTagSet::<1>::new(1).unwrap());
        let tag = tags.reserve().unwrap();
        tags.begin_dispatch(tag).unwrap();
        tags.mark_inflight(tag).unwrap();

        let completion_tags = Arc::clone(&tags);
        let completion = std::thread::spawn(move || {
            let claim = completion_tags.claim_completion(tag).ok()?;
            claim.finish().unwrap();
            Some(ClaimOwner::Completion)
        });
        let timeout_tags = Arc::clone(&tags);
        let timeout = std::thread::spawn(move || {
            let claim = timeout_tags.claim_timeout(tag).ok()?;
            drop(claim);
            Some(ClaimOwner::Timeout)
        });
        let completion = completion.join().unwrap();
        let timeout = timeout.join().unwrap();

        assert_ne!(completion.is_some(), timeout.is_some());
        let owner = completion.or(timeout).unwrap();
        if owner == ClaimOwner::Timeout {
            tags.finish_timeout_after_return(tag).unwrap();
        }
        assert_eq!(tags.state(tag), Ok(RequestState::Terminal));
    }

    #[test]
    fn completion_timeout_and_cancel_have_exactly_one_terminal_owner() {
        let tags = Arc::new(RequestTagSet::<1>::new(1).unwrap());
        let tag = tags.reserve().unwrap();
        tags.begin_dispatch(tag).unwrap();
        tags.mark_inflight(tag).unwrap();

        let completion_tags = Arc::clone(&tags);
        let completion = std::thread::spawn(move || {
            let claim = completion_tags.claim_completion(tag).ok()?;
            claim.finish().unwrap();
            Some(ClaimOwner::Completion)
        });
        let timeout_tags = Arc::clone(&tags);
        let timeout = std::thread::spawn(move || {
            let claim = timeout_tags.claim_timeout(tag).ok()?;
            drop(claim);
            Some(ClaimOwner::Timeout)
        });
        let cancel_tags = Arc::clone(&tags);
        let cancel = std::thread::spawn(move || {
            let claim = cancel_tags.claim_cancel(tag).ok()?;
            drop(claim);
            Some(ClaimOwner::Cancel)
        });

        let owners = [
            completion.join().unwrap(),
            timeout.join().unwrap(),
            cancel.join().unwrap(),
        ];
        assert_eq!(owners.iter().filter(|owner| owner.is_some()).count(), 1);
        match owners.into_iter().flatten().next().unwrap() {
            ClaimOwner::Completion => {}
            ClaimOwner::Timeout => tags.finish_timeout_after_return(tag).unwrap(),
            ClaimOwner::Cancel => tags.finish_cancel_after_return(tag).unwrap(),
        }
        assert_eq!(tags.state(tag), Ok(RequestState::Terminal));
    }

    #[test]
    fn staged_request_can_be_claimed_by_timeout_before_hardware_submission() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();
        tags.mark_staged(tag).unwrap();

        let claim = tags.claim_timeout(tag).unwrap();
        drop(claim);
        assert_eq!(tags.state(tag), Ok(RequestState::TimingOut));
        tags.finish_timeout_after_return(tag).unwrap();
        assert_eq!(tags.state(tag), Ok(RequestState::Terminal));
    }

    #[test]
    fn dispatch_boundary_excludes_timeout_and_cancel_until_driver_decides_ownership() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();
        tags.mark_staged(tag).unwrap();

        tags.begin_dispatch(tag).unwrap();

        assert_eq!(tags.state(tag), Ok(RequestState::Dispatching));
        assert_eq!(
            tags.claim_timeout(tag).err(),
            Some(TagError::InvalidTransition)
        );
        assert_eq!(
            tags.claim_cancel(tag).err(),
            Some(TagError::InvalidTransition)
        );

        tags.restore_after_rejection(tag).unwrap();
        assert_eq!(tags.state(tag), Ok(RequestState::Staged));
    }

    #[test]
    fn inline_driver_completion_can_finish_a_prepublished_reserved_tag() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();

        tags.claim_completion(tag).unwrap().finish().unwrap();

        assert_eq!(tags.state(tag), Ok(RequestState::Terminal));
    }

    #[test]
    fn timeout_does_not_publish_terminal_before_hardware_returns_ownership() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();
        tags.begin_dispatch(tag).unwrap();
        tags.mark_inflight(tag).unwrap();

        let claim = tags.claim_timeout(tag).unwrap();
        assert_eq!(tags.state(tag), Ok(RequestState::TimingOut));
        drop(claim);
        assert_eq!(tags.state(tag), Ok(RequestState::TimingOut));

        tags.finish_timeout_after_return(tag).unwrap();
        assert_eq!(tags.state(tag), Ok(RequestState::Terminal));
    }

    #[test]
    fn cancel_does_not_publish_terminal_before_inflight_dma_returns() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();
        tags.begin_dispatch(tag).unwrap();
        tags.mark_inflight(tag).unwrap();

        let claim = tags.claim_cancel(tag).unwrap();
        assert!(claim.requires_dma_quiesce());
        assert_eq!(tags.state(tag), Ok(RequestState::Canceling));
        drop(claim);
        assert_eq!(tags.state(tag), Ok(RequestState::Canceling));

        tags.finish_cancel_after_return(tag).unwrap();
        assert_eq!(tags.state(tag), Ok(RequestState::Terminal));
    }

    #[test]
    fn staged_cancel_can_return_ownership_without_controller_recovery() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();
        tags.mark_staged(tag).unwrap();

        let claim = tags.claim_cancel(tag).unwrap();
        assert!(!claim.requires_dma_quiesce());
        assert_eq!(tags.state(tag), Ok(RequestState::Canceling));
        tags.finish_cancel_after_return(tag).unwrap();
        assert_eq!(tags.state(tag), Ok(RequestState::Terminal));
    }

    #[test]
    fn request_id_round_trip_preserves_slot_and_generation() {
        let tags = RequestTagSet::<64>::new(64).unwrap();
        let tag = tags.reserve().unwrap();
        let id = tag.into_request_id().unwrap();

        assert_eq!(RequestTag::from_request_id(id), Ok(tag));
    }

    #[test]
    fn highest_encoded_tag_cannot_alias_the_inline_sentinel() {
        let tag = RequestTag {
            slot: 63,
            generation: (usize::MAX / 64) as u64,
        };

        assert_eq!(tag.into_request_id(), Err(TagError::IdOverflow));
    }

    #[test]
    fn unaccepted_staged_request_can_return_to_free() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();
        tags.mark_staged(tag).unwrap();

        tags.abandon_unaccepted(tag).unwrap();

        assert_eq!(tags.reserve().unwrap().slot(), tag.slot());
    }
}
