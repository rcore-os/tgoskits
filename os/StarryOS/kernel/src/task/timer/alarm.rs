use super::*;

static NEXT_ALARM_SLOT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub(crate) struct AlarmSlot {
    state: Arc<AlarmSlotState>,
}

#[derive(Debug)]
struct AlarmSlotState {
    id: u64,
    generation_and_armed: AtomicU64,
}

#[derive(Clone, Debug)]
pub(crate) struct AlarmToken {
    slot: AlarmSlot,
    generation: u64,
}

impl AlarmSlot {
    pub(crate) fn new() -> Self {
        let id = NEXT_ALARM_SLOT_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .unwrap_or_else(|_| panic!("alarm slot identity space exhausted"));
        Self {
            state: Arc::new(AlarmSlotState {
                id,
                generation_and_armed: AtomicU64::new(0),
            }),
        }
    }

    pub(crate) fn replace(&self, delay: Option<Duration>) -> AlarmChange {
        let armed = delay.is_some();
        let previous = self
            .state
            .generation_and_armed
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                let generation = state >> 1;
                generation
                    .checked_add(1)
                    .filter(|next| *next <= u64::MAX >> 1)
                    .map(|next| (next << 1) | u64::from(armed))
            })
            .unwrap_or_else(|_| panic!("alarm generation space exhausted"));
        let token = AlarmToken {
            slot: self.clone(),
            generation: (previous >> 1) + 1,
        };
        match delay {
            Some(delay) => AlarmChange::Schedule { delay, token },
            None => AlarmChange::Cancel(self.clone()),
        }
    }

    pub(crate) fn matches(&self, token: &AlarmToken) -> bool {
        self.id() == token.slot_id() && token.is_current_generation()
    }

    pub(super) fn id(&self) -> u64 {
        self.state.id
    }
}

impl Default for AlarmSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl AlarmToken {
    fn slot_id(&self) -> u64 {
        self.slot.id()
    }

    fn is_current_generation(&self) -> bool {
        self.slot.state.generation_and_armed.load(Ordering::Acquire) >> 1 == self.generation
    }

    fn is_armed(&self) -> bool {
        self.slot.state.generation_and_armed.load(Ordering::Acquire) == (self.generation << 1) | 1
    }
}

#[derive(Clone, Debug)]
pub enum AlarmTarget {
    Process(Pid),
}

struct Entry<T> {
    deadline: Duration,
    token: AlarmToken,
    target: T,
}

impl<T> PartialEq for Entry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.token.slot_id() == other.token.slot_id()
    }
}
impl<T> Eq for Entry<T> {}
impl<T> PartialOrd for Entry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Entry<T> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| other.token.slot_id().cmp(&self.token.slot_id()))
    }
}

struct AlarmQueue<T> {
    entries: BinaryHeap<Entry<T>>,
}

enum AlarmQueueAction<T> {
    Empty,
    Wait(Duration),
    Fire(Entry<T>),
}

impl<T> AlarmQueue<T> {
    const fn new() -> Self {
        Self {
            entries: BinaryHeap::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn earliest_deadline(&self) -> Option<Duration> {
        self.entries.peek().map(|entry| entry.deadline)
    }

    fn schedule(&mut self, deadline: Duration, token: AlarmToken, target: T) {
        if !token.is_armed() {
            return;
        }
        let slot_id = token.slot_id();
        self.entries
            .retain(|entry| entry.token.slot_id() != slot_id);
        if token.is_armed() {
            self.entries.push(Entry {
                deadline,
                token,
                target,
            });
        }
    }

    fn cancel(&mut self, slot: &AlarmSlot) {
        self.entries
            .retain(|entry| entry.token.slot_id() != slot.id());
    }

    fn pop_expired(&mut self, now: Duration) -> Option<Entry<T>> {
        loop {
            let entry = self.entries.peek()?;
            if !entry.token.is_armed() {
                self.entries.pop();
                continue;
            }
            if entry.deadline > now {
                return None;
            }
            return self.entries.pop();
        }
    }

    fn next_action(&mut self, now: Duration) -> AlarmQueueAction<T> {
        loop {
            let Some(deadline) = self.earliest_deadline() else {
                return AlarmQueueAction::Empty;
            };
            if deadline > now {
                return AlarmQueueAction::Wait(deadline);
            }
            if let Some(entry) = self.pop_expired(now) {
                return AlarmQueueAction::Fire(entry);
            }
        }
    }
}

static ALARM_LIST: LazyLock<PiMutex<AlarmQueue<AlarmTarget>>> =
    LazyLock::new(|| PiMutex::new(AlarmQueue::new()));
static EVENT_NEW_TIMER: LazyLock<Event> = LazyLock::new(Event::new);

include!("alarm/change.rs");
include!("alarm/worker.rs");
include!("alarm/tests.rs");
