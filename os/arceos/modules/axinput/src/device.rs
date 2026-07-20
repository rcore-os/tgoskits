//! Immutable input publication and bounded owner-to-consumer event channel.

use alloc::{collections::VecDeque, string::String, sync::Arc, vec::Vec};
use core::{cell::Cell, marker::PhantomData, task::Waker};

use ax_sync::SpinMutex;
use axpoll::{IoEvents, PollSet};

use crate::{AbsInfo, Event, EventType, InputDeviceId};

/// Maximum Linux-compatible absolute axis index represented in a snapshot.
pub const ABS_AXIS_COUNT: usize = EventType::Absolute.bits_count();
/// Number of input property bits retained from the portable driver.
pub const INPUT_PROPERTY_COUNT: usize = 0x20;
/// Bounded events retained when userspace is slower than the owner thread.
pub const INPUT_EVENT_CAPACITY: usize = 256;

pub type InputResult<T = ()> = Result<T, InputError>;

/// Stable facade or runtime service failure.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum InputError {
    #[error("no input event is available")]
    Again,
    #[error("the input runtime is not available")]
    NotAvailable,
    #[error("the input runtime is in an invalid state")]
    BadState,
}

/// Immutable controller metadata captured after owner-side initialization.
pub struct InputDeviceSnapshot {
    name: String,
    physical_location: String,
    unique_id: String,
    device_id: InputDeviceId,
    event_bits: [Vec<u8>; EventType::COUNT as usize],
    property_bits: Vec<u8>,
    absolute_info: [Option<AbsInfo>; ABS_AXIS_COUNT],
}

impl InputDeviceSnapshot {
    /// Builds a complete snapshot from final owner-thread observations.
    pub fn new(
        name: String,
        physical_location: String,
        unique_id: String,
        device_id: InputDeviceId,
        event_bits: [Vec<u8>; EventType::COUNT as usize],
        property_bits: Vec<u8>,
        absolute_info: [Option<AbsInfo>; ABS_AXIS_COUNT],
    ) -> Self {
        Self {
            name,
            physical_location,
            unique_id,
            device_id,
            event_bits,
            property_bits,
            absolute_info,
        }
    }

    /// Returns the stable driver-provided name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the stable physical topology path.
    pub fn physical_location(&self) -> &str {
        &self.physical_location
    }

    /// Returns the stable unique input identity.
    pub fn unique_id(&self) -> &str {
        &self.unique_id
    }

    /// Returns the bus/vendor/product/version identity.
    pub const fn device_id(&self) -> InputDeviceId {
        self.device_id
    }

    /// Returns the exact event-code bitmap captured for `event_type`.
    pub fn event_bits(&self, event_type: EventType) -> &[u8] {
        &self.event_bits[event_type as usize]
    }

    /// Reports whether the device advertised this event type.
    pub fn supports_event(&self, event_type: EventType) -> bool {
        !self.event_bits(event_type).is_empty()
    }

    /// Returns the portable input-property bitmap.
    pub fn property_bits(&self) -> &[u8] {
        &self.property_bits
    }

    /// Returns cached absolute-axis metadata when the axis was advertised.
    pub fn absolute_info(&self, axis: u8) -> Option<AbsInfo> {
        self.absolute_info.get(axis as usize).copied().flatten()
    }
}

/// Runtime lifetime and shutdown boundary retained by a public input facade.
pub trait InputRuntimeService: Send + Sync {
    /// Requests the explicit owner-thread close protocol.
    fn request_shutdown(&self) -> InputResult;
}

struct InputEventBuffer {
    events: SpinMutex<VecDeque<Event>>,
    waiters: PollSet,
}

impl InputEventBuffer {
    fn new() -> Self {
        Self {
            events: SpinMutex::new(VecDeque::with_capacity(INPUT_EVENT_CAPACITY)),
            waiters: PollSet::new(),
        }
    }
}

/// Move-only owner capability for publishing serviced input events.
pub struct InputEventPublisher {
    buffer: Arc<InputEventBuffer>,
    _not_sync: PhantomData<Cell<()>>,
}

impl InputEventPublisher {
    /// Publishes a bounded task-context batch and wakes read waiters.
    pub fn publish(&self, events: &[Event]) -> InputPublishResult {
        let mut queue = self.buffer.events.lock();
        let mut dropped = 0;
        for &event in events {
            if queue.len() == INPUT_EVENT_CAPACITY {
                queue.pop_front();
                dropped += 1;
            }
            queue.push_back(event);
        }
        let published = events.len();
        drop(queue);
        if published != 0 {
            // SAFETY: the maintenance owner invokes this in ordinary task
            // context after publishing queue contents and holds no queue lock.
            unsafe { self.buffer.waiters.wake(IoEvents::IN) };
        }
        InputPublishResult { published, dropped }
    }
}

/// Result of one bounded owner publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputPublishResult {
    published: usize,
    dropped: usize,
}

impl InputPublishResult {
    /// Number of newly published events.
    pub const fn published(self) -> usize {
        self.published
    }

    /// Number of oldest events displaced by this publication.
    pub const fn dropped(self) -> usize {
        self.dropped
    }
}

/// One fully activated input device exposed without mutable driver state.
pub struct InputDeviceFacade {
    snapshot: InputDeviceSnapshot,
    buffer: Arc<InputEventBuffer>,
    runtime: Arc<dyn InputRuntimeService>,
}

impl InputDeviceFacade {
    /// Creates a private event channel before spawning the maintenance owner.
    pub fn event_channel() -> InputEventChannel {
        InputEventChannel {
            buffer: Arc::new(InputEventBuffer::new()),
        }
    }

    /// Publishes a ready facade after driver initialization and IRQ activation.
    pub fn new(
        snapshot: InputDeviceSnapshot,
        receiver: InputEventReceiver,
        runtime: Arc<dyn InputRuntimeService>,
    ) -> Self {
        Self {
            snapshot,
            buffer: receiver.buffer,
            runtime,
        }
    }

    /// Returns the immutable activation snapshot.
    pub const fn snapshot(&self) -> &InputDeviceSnapshot {
        &self.snapshot
    }

    /// Pops one already-serviced event without accessing the driver.
    pub fn read_event(&self) -> InputResult<Event> {
        self.buffer
            .events
            .lock()
            .pop_front()
            .ok_or(InputError::Again)
    }

    /// Reports whether at least one serviced event is ready.
    pub fn has_events(&self) -> bool {
        !self.buffer.events.lock().is_empty()
    }

    /// Registers a task-context read waker with a post-registration recheck.
    pub fn register_read_waker(&self, waker: &Waker) {
        // SAFETY: VFS poll registration is task context, and no input-buffer
        // lock is held while PollSet clones or wakes this waker.
        unsafe { self.buffer.waiters.register(waker, IoEvents::IN) };
        if self.has_events() {
            waker.wake_by_ref();
        }
    }

    /// Requests explicit runtime shutdown without exposing its owner handle.
    pub fn request_shutdown(&self) -> InputResult {
        self.runtime.request_shutdown()
    }
}

/// Unpublished half of a newly allocated event channel.
pub struct InputEventChannel {
    buffer: Arc<InputEventBuffer>,
}

impl InputEventChannel {
    /// Consumes the unpublished channel into its unique producer and receiver.
    pub fn split(self) -> (InputEventPublisher, InputEventReceiver) {
        let publisher = InputEventPublisher {
            buffer: Arc::clone(&self.buffer),
            _not_sync: PhantomData,
        };
        let receiver = InputEventReceiver {
            buffer: self.buffer,
        };
        (publisher, receiver)
    }
}

/// Consumer half accepted only by [`InputDeviceFacade::new`].
pub struct InputEventReceiver {
    buffer: Arc<InputEventBuffer>,
}
