use alloc::{
    boxed::Box,
    collections::VecDeque,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use ax_std::os::arceos::task::sync::WaitQueue;
use rdif_audio::PcmDirection;

use super::{
    DirectionProfile,
    protocol::{ControlCommand, Request, SessionId, SoundError},
    session::PcmSession,
};
use crate::{StarryError, StarryResult, sync::Mutex};

const MAX_PENDING_COMMANDS: usize = 64;

#[derive(Clone, Copy)]
pub(crate) struct PublishedCard {
    pub(crate) playback: Option<DirectionProfile>,
    pub(crate) capture: Option<DirectionProfile>,
}

enum Publication {
    Discovering,
    Ready(Box<PublishedCard>),
    Failed(SoundError),
}

struct SessionSlot {
    id: SessionId,
    direction: PcmDirection,
    session: Weak<PcmSession>,
}

pub(crate) enum Command {
    Open {
        id: SessionId,
        direction: PcmDirection,
        session: Weak<PcmSession>,
    },
    Control {
        session: Weak<PcmSession>,
        command: ControlCommand,
    },
}

/// The single owner of the worker command queue and direction admission.
pub(crate) struct SoundService {
    publication: Mutex<Publication>,
    fault: Mutex<Option<SoundError>>,
    slots: Mutex<[Option<SessionSlot>; 2]>,
    commands: Mutex<VecDeque<Command>>,
    discovery: WaitQueue,
    park: WaitQueue,
    work_epoch: AtomicU64,
    next_session_id: AtomicU64,
}

impl SoundService {
    pub(crate) fn new() -> Self {
        Self {
            publication: Mutex::new(Publication::Discovering),
            fault: Mutex::new(None),
            slots: Mutex::new([None, None]),
            commands: Mutex::new(VecDeque::new()),
            discovery: WaitQueue::new(),
            park: WaitQueue::new(),
            work_epoch: AtomicU64::new(0),
            next_session_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn publication(&self) -> Option<PublishedCard> {
        match &*self.publication.lock() {
            Publication::Ready(card) => Some(**card),
            Publication::Discovering | Publication::Failed(_) => None,
        }
    }

    pub(crate) fn publish_ready(
        &self,
        playback: Option<DirectionProfile>,
        capture: Option<DirectionProfile>,
    ) {
        let card = PublishedCard { playback, capture };
        *self.publication.lock() = Publication::Ready(Box::new(card));
        self.discovery.notify_all();
        self.notify_worker();
    }

    pub(crate) fn publish_failed(&self, error: SoundError) {
        let mut publication = self.publication.lock();
        if matches!(&*publication, Publication::Discovering) {
            *publication = Publication::Failed(error);
        }
        drop(publication);
        self.discovery.notify_all();
        self.notify_worker();
    }

    pub(crate) fn wait_for_discovery(&self, timeout: Duration) -> bool {
        if !matches!(&*self.publication.lock(), Publication::Discovering) {
            return true;
        }
        !self.discovery.wait_timeout_until(timeout, || {
            !matches!(&*self.publication.lock(), Publication::Discovering)
        })
    }

    pub(crate) fn discovery_error(&self) -> Option<SoundError> {
        match &*self.publication.lock() {
            Publication::Failed(error) => Some(*error),
            Publication::Discovering | Publication::Ready(_) => None,
        }
    }

    pub(crate) fn open_session(
        self: &Arc<Self>,
        direction: PcmDirection,
    ) -> StarryResult<Arc<PcmSession>> {
        if let Some(error) = *self.fault.lock() {
            return Err(error.into_starry());
        }
        let card = self.publication().ok_or(StarryError::NoSuchDevice)?;
        let profile = match direction {
            PcmDirection::Playback => card.playback,
            PcmDirection::Capture => card.capture,
        }
        .ok_or(StarryError::NoSuchDevice)?;
        let index = slot_index(direction);
        let (id, session) = {
            let mut slots = self.slots.lock();
            if slots[index].is_some() {
                return Err(StarryError::ResourceBusy);
            }
            let id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
            let session = Arc::new(PcmSession::new(
                id,
                direction,
                profile,
                Arc::downgrade(self),
            ));
            slots[index] = Some(SessionSlot {
                id,
                direction,
                session: Arc::downgrade(&session),
            });
            (id, session)
        };
        if let Err(error) = self.enqueue(Command::Open {
            id,
            direction,
            session: Arc::downgrade(&session),
        }) {
            self.release_slot(id);
            return Err(error);
        }
        Ok(session)
    }

    pub(crate) fn enqueue_control(
        &self,
        session: Weak<PcmSession>,
        command: ControlCommand,
    ) -> StarryResult<()> {
        self.enqueue(Command::Control { session, command })
    }

    pub(crate) fn complete_control(&self, request: &Request, result: Result<(), SoundError>) {
        request.complete(result);
    }

    pub(crate) fn take_command(&self) -> Option<Command> {
        self.commands.lock().pop_front()
    }

    pub(crate) fn sessions(&self) -> Vec<(SessionId, PcmDirection, Weak<PcmSession>)> {
        let slots = self.slots.lock();
        slots
            .iter()
            .filter_map(|slot| {
                let slot = slot.as_ref()?;
                Some((slot.id, slot.direction, slot.session.clone()))
            })
            .collect()
    }

    pub(crate) fn release_slot(&self, id: SessionId) {
        let mut slots = self.slots.lock();
        for slot in slots.iter_mut() {
            if slot.as_ref().is_some_and(|slot| slot.id == id) {
                *slot = None;
            }
        }
        drop(slots);
        self.notify_worker();
    }

    pub(crate) fn is_busy(&self, direction: PcmDirection) -> bool {
        self.slots.lock()[slot_index(direction)].is_some()
    }

    pub(crate) fn fail(&self, error: SoundError) {
        {
            let mut fault = self.fault.lock();
            if fault.is_none() {
                *fault = Some(error);
            }
        }
        let sessions: Vec<Arc<PcmSession>> = self
            .slots
            .lock()
            .iter()
            .filter_map(|slot| slot.as_ref()?.session.upgrade())
            .collect();
        for session in sessions {
            session.fail(error);
        }
        self.publish_failed(error);
        self.notify_worker();
        self.park.notify_all();
    }

    pub(crate) fn notify_worker(&self) {
        self.work_epoch.fetch_add(1, Ordering::AcqRel);
        self.park.notify_one();
    }

    pub(crate) fn work_epoch(&self) -> u64 {
        self.work_epoch.load(Ordering::Acquire)
    }

    pub(crate) fn wait_for_work(&self, timeout: Duration, observed_epoch: u64) {
        let _timed_out = self
            .park
            .wait_timeout_until(timeout, || self.work_epoch() != observed_epoch);
    }

    fn enqueue(&self, command: Command) -> StarryResult<()> {
        {
            let mut commands = self.commands.lock();
            if commands.len() >= MAX_PENDING_COMMANDS {
                return Err(StarryError::NoMemory);
            }
            commands.push_back(command);
        }
        self.notify_worker();
        Ok(())
    }
}

const fn slot_index(direction: PcmDirection) -> usize {
    match direction {
        PcmDirection::Playback => 0,
        PcmDirection::Capture => 1,
    }
}
