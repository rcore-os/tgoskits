//! Task-scoped fault injection into real resource providers for QEMU tests.

use core::{
    marker::PhantomData,
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
};

use ax_task::thread::{TaskError, current};

static OWNER: AtomicU64 = AtomicU64::new(0);
static FAILURE: AtomicU8 = AtomicU8::new(0);
static EVENTS: AtomicU64 = AtomicU64::new(0);

/// Recoverable resource stages before a newly created task is published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CreationStage {
    Stack   = 1,
    Tls     = 2,
    Context = 3,
    Bind    = 4,
    Fp      = 8,
    Mm      = 9,
}

/// Actual provider entry and destruction events in one creation transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CreationEvent {
    Stack       = 1,
    Tls         = 2,
    Context     = 3,
    Bind        = 4,
    DropContext = 5,
    DropTls     = 6,
    DropStack   = 7,
    Fp          = 8,
    Mm          = 9,
}

/// One creator-task probe; other tasks and interrupt work remain unaffected.
pub struct ThreadCreationProbe {
    _not_send: PhantomData<*mut ()>,
}

impl ThreadCreationProbe {
    /// Fails the next occurrence of the selected stage for this task.
    pub fn fail_at(stage: CreationStage) -> Result<Self, TaskError> {
        current::validate_blocking_context()?;
        let owner = current::current_thread_id()?.as_u64();
        OWNER
            .compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| TaskError::ThreadBusy)?;
        EVENTS.store(0, Ordering::Relaxed);
        FAILURE.store(stage as u8, Ordering::Release);
        Ok(Self {
            _not_send: PhantomData,
        })
    }

    /// Returns the provider event sequence, in execution order.
    pub fn events(&self) -> alloc::vec::Vec<CreationEvent> {
        let mut encoded = EVENTS.load(Ordering::Acquire);
        let mut events = alloc::vec::Vec::new();
        while encoded != 0 {
            events.push(match encoded & 15 {
                1 => CreationEvent::Stack,
                2 => CreationEvent::Tls,
                3 => CreationEvent::Context,
                4 => CreationEvent::Bind,
                5 => CreationEvent::DropContext,
                6 => CreationEvent::DropTls,
                7 => CreationEvent::DropStack,
                8 => CreationEvent::Fp,
                9 => CreationEvent::Mm,
                _ => unreachable!("invalid creation probe event"),
            });
            encoded >>= 4;
        }
        events.reverse();
        events
    }
}

impl Drop for ThreadCreationProbe {
    fn drop(&mut self) {
        FAILURE.store(0, Ordering::Relaxed);
        OWNER.store(0, Ordering::Release);
    }
}

pub(super) fn record(event: CreationEvent) -> bool {
    let owner = OWNER.load(Ordering::Acquire);
    if owner == 0
        || ax_task::runtime::task_runtime::in_hard_irq()
        || current::current_thread_id().map(|id| id.as_u64()) != Ok(owner)
    {
        return false;
    }
    EVENTS
        .try_update(Ordering::AcqRel, Ordering::Acquire, |events| {
            (events >> 60 == 0).then_some((events << 4) | event as u64)
        })
        .expect("creation probe event capacity exhausted");
    FAILURE
        .compare_exchange(event as u8, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

static MM_SWITCHES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Samples actual MM preparation paths: KK, KU, UK, UU.
pub fn mm_switch_counts() -> [u64; 4] {
    core::array::from_fn(|index| MM_SWITCHES[index].load(Ordering::Acquire))
}

pub(super) fn record_mm_switch(previous_user: bool, next_user: bool) {
    MM_SWITCHES[usize::from(previous_user) * 2 + usize::from(next_user)]
        .fetch_add(1, Ordering::Release);
}
