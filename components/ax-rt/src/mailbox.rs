//! Bounded, typed RT ↔ host mailbox.
//!
//! The mailbox is split into two planes so it can serve both deployment models
//! for the RT executor:
//!
//! - **Data plane** — two fixed-capacity single-producer/single-consumer (SPSC)
//!   rings living in shared memory. One ring carries host→RT commands, the other
//!   RT→host events. The ring storage is `#[repr(C)]` with a magic/version header
//!   so that, in an asymmetric-multiprocessing (AMP) deployment where the RT side
//!   is a separate image (for example a Sophgo SG2002 "little" core talking to a
//!   "big" core running the host OS), both images can agree on the layout.
//! - **Notification plane** — a [`MailboxDoorbell`] capability, injected by the
//!   integrator, that raises an interrupt on the peer core so it drains its
//!   inbound ring. The peer's interrupt handler calls [`rt_mailbox_on_doorbell`]
//!   / [`host_mailbox_on_doorbell`], which do nothing but set a single atomic
//!   flag — safe to run in interrupt context. On symmetric SMP the doorbell is an
//!   IPI/SGI; on SG2002 it is the hardware mailbox doorbell register.
//!
//! Both RT-side operations are non-blocking and bounded: the RT executor never
//! spins waiting on the host. Like the rest of this crate, the mailbox assumes
//! the cooperative single-CPU RT executor for its RT endpoint and coherent
//! shared memory between the two cores.

use core::sync::atomic::{
    AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

use spin::Once;

/// Maximum payload length, in bytes, carried by a single [`RtMessage`].
pub const RT_MAILBOX_PAYLOAD_CAP: usize = 48;

/// Number of message slots in each direction's ring.
const RT_MAILBOX_RING_SLOTS: usize = 16;

/// Identifies a valid mailbox layout in shared memory (`"AXRM"`).
const RT_MAILBOX_MAGIC: u32 = 0x4158_524d;

/// Layout/protocol version. Bump on any incompatible change to the shared
/// `#[repr(C)]` layout so a mismatched peer image can be detected instead of
/// silently misreading the rings.
const RT_MAILBOX_ABI_VERSION: u32 = 1;

static RT_MAILBOX: RtMailbox = RtMailbox::new();
static TO_RT_DOORBELL: Once<&'static dyn MailboxDoorbell> = Once::new();
static TO_HOST_DOORBELL: Once<&'static dyn MailboxDoorbell> = Once::new();

/// A typed, fixed-capacity mailbox message.
#[derive(Clone, Copy)]
pub struct RtMessage {
    tag: u32,
    len: u16,
    payload: [u8; RT_MAILBOX_PAYLOAD_CAP],
}

impl RtMessage {
    /// Builds a message with a caller-defined `tag` and up to
    /// [`RT_MAILBOX_PAYLOAD_CAP`] payload bytes.
    pub fn new(tag: u32, payload: &[u8]) -> Result<Self, RtMailboxError> {
        if payload.len() > RT_MAILBOX_PAYLOAD_CAP {
            return Err(RtMailboxError::PayloadTooLarge);
        }
        let mut buffer = [0u8; RT_MAILBOX_PAYLOAD_CAP];
        buffer[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            tag,
            len: payload.len() as u16,
            payload: buffer,
        })
    }

    /// Returns the caller-defined message tag.
    pub fn tag(&self) -> u32 {
        self.tag
    }

    /// Returns the valid payload bytes.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.len as usize]
    }

    /// Returns the payload length in bytes.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns whether the payload is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Errors returned by mailbox send operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtMailboxError {
    /// The destination ring is full; the message was not enqueued.
    Full,
    /// The payload exceeds [`RT_MAILBOX_PAYLOAD_CAP`].
    PayloadTooLarge,
}

/// Snapshot of mailbox occupancy and drop/notification counters, for `rt status`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RtMailboxStats {
    /// Messages currently queued host→RT.
    pub to_rt_depth: usize,
    /// Messages currently queued RT→host.
    pub to_host_depth: usize,
    /// Messages dropped because the host→RT ring was full.
    pub to_rt_dropped: u64,
    /// Messages dropped because the RT→host ring was full.
    pub to_host_dropped: u64,
    /// Doorbell interrupts observed by the RT side.
    pub rt_notifications: u64,
    /// Doorbell interrupts observed by the host side.
    pub host_notifications: u64,
}

/// Platform capability that raises an interrupt on the peer core so it drains
/// its inbound ring.
///
/// The integrator implements this per platform: an IPI/SGI to a target CPU on
/// symmetric SMP, or a hardware mailbox doorbell register on an AMP SoC such as
/// the SG2002.
///
/// # Contract
///
/// - [`ring`](MailboxDoorbell::ring) must be non-blocking and safe to call from
///   RT task context; for the host→RT doorbell it must also be callable from
///   host context.
/// - The corresponding receive-side interrupt handler is the integrator's
///   responsibility. It must deassert the hardware interrupt itself and then
///   call [`rt_mailbox_on_doorbell`] / [`host_mailbox_on_doorbell`]. It must not
///   allocate, map memory, take sleepable locks, or disable its interrupt source
///   — only clear the source and set the mailbox pending flag.
pub trait MailboxDoorbell: Sync {
    /// Raises an interrupt on the peer core. Non-blocking.
    fn ring(&self);
}

/// Registers the doorbell used to notify the RT core after a host→RT send.
///
/// Idempotent: only the first registration takes effect.
pub fn set_rt_doorbell(doorbell: &'static dyn MailboxDoorbell) {
    TO_RT_DOORBELL.call_once(|| doorbell);
}

/// Registers the doorbell used to notify a host core after an RT→host send.
///
/// Idempotent: only the first registration takes effect.
pub fn set_host_doorbell(doorbell: &'static dyn MailboxDoorbell) {
    TO_HOST_DOORBELL.call_once(|| doorbell);
}

/// Sends a command to the RT core (host→RT) and rings the RT doorbell, if set.
///
/// Returns [`RtMailboxError::Full`] without enqueuing when the ring is full.
pub fn host_mailbox_send(msg: &RtMessage) -> Result<(), RtMailboxError> {
    RT_MAILBOX.to_rt.try_push(msg)?;
    if let Some(doorbell) = TO_RT_DOORBELL.get() {
        doorbell.ring();
    }
    Ok(())
}

/// Receives one RT→host event on the host side, or `None` if none are queued.
pub fn host_mailbox_recv() -> Option<RtMessage> {
    RT_MAILBOX.to_host.try_pop()
}

/// Sends an event to the host (RT→host) and rings the host doorbell, if set.
///
/// Non-blocking; returns [`RtMailboxError::Full`] without enqueuing when the
/// ring is full. Intended to be called from RT task context.
pub fn rt_mailbox_send(msg: &RtMessage) -> Result<(), RtMailboxError> {
    RT_MAILBOX.to_host.try_push(msg)?;
    if let Some(doorbell) = TO_HOST_DOORBELL.get() {
        doorbell.ring();
    }
    Ok(())
}

/// Receives one host→RT command on the RT side, or `None` if none are queued.
/// Non-blocking; intended to be called from RT task context.
pub fn rt_mailbox_recv() -> Option<RtMessage> {
    RT_MAILBOX.to_rt.try_pop()
}

/// Interrupt-context entry for the RT core's doorbell handler.
///
/// Does nothing but record the notification and set the RT pending flag; the RT
/// executor consumes the flag in task context. Safe to call from an ISR.
pub fn rt_mailbox_on_doorbell() {
    RT_MAILBOX.rt_notifications.fetch_add(1, Ordering::Relaxed);
    RT_MAILBOX.rt_pending.store(true, Ordering::Release);
}

/// Interrupt-context entry for a host core's doorbell handler. Safe to call from
/// an ISR.
pub fn host_mailbox_on_doorbell() {
    RT_MAILBOX
        .host_notifications
        .fetch_add(1, Ordering::Relaxed);
    RT_MAILBOX.host_pending.store(true, Ordering::Release);
}

/// Takes and clears the RT-side pending flag. Returns `true` if a doorbell was
/// pending since the last call. The RT executor polls this to decide whether to
/// drain the inbound ring.
pub fn rt_mailbox_take_pending() -> bool {
    RT_MAILBOX.rt_pending.swap(false, Ordering::AcqRel)
}

/// Takes and clears the host-side pending flag. Returns `true` if a doorbell was
/// pending since the last call.
pub fn host_mailbox_take_pending() -> bool {
    RT_MAILBOX.host_pending.swap(false, Ordering::AcqRel)
}

/// Returns a snapshot of mailbox occupancy and counters.
pub fn rt_mailbox_stats() -> RtMailboxStats {
    RtMailboxStats {
        to_rt_depth: RT_MAILBOX.to_rt.depth(),
        to_host_depth: RT_MAILBOX.to_host.depth(),
        to_rt_dropped: RT_MAILBOX.to_rt.dropped(),
        to_host_dropped: RT_MAILBOX.to_host.dropped(),
        rt_notifications: RT_MAILBOX.rt_notifications.load(Ordering::Relaxed),
        host_notifications: RT_MAILBOX.host_notifications.load(Ordering::Relaxed),
    }
}

/// Shared mailbox state. The `#[repr(C)]` header and rings are laid out so a
/// separate peer image can map and agree on the same structure; the doorbell and
/// pending flags are runtime-only and never part of the shared wire layout.
#[repr(C)]
struct RtMailbox {
    magic: u32,
    version: u32,
    payload_cap: u32,
    ring_slots: u32,
    to_rt: SpscRing,
    to_host: SpscRing,
    rt_pending: AtomicBool,
    host_pending: AtomicBool,
    rt_notifications: AtomicU64,
    host_notifications: AtomicU64,
}

impl RtMailbox {
    const fn new() -> Self {
        Self {
            magic: RT_MAILBOX_MAGIC,
            version: RT_MAILBOX_ABI_VERSION,
            payload_cap: RT_MAILBOX_PAYLOAD_CAP as u32,
            ring_slots: RT_MAILBOX_RING_SLOTS as u32,
            to_rt: SpscRing::new(),
            to_host: SpscRing::new(),
            rt_pending: AtomicBool::new(false),
            host_pending: AtomicBool::new(false),
            rt_notifications: AtomicU64::new(0),
            host_notifications: AtomicU64::new(0),
        }
    }
}

/// A single-producer/single-consumer ring of fixed-size message slots.
///
/// `write`/`read` are free-running slot counters; the difference gives the
/// occupancy and the modulo gives the slot index. The producer publishes a slot
/// by advancing `write` with `Release` after writing the payload; the consumer
/// observes it with an `Acquire` load of `write`, so the payload writes are
/// visible before it reads them. The consumer frees a slot by advancing `read`
/// with `Release`; the producer observes it with `Acquire` before reusing the
/// slot. This is sound for exactly one producer and one consumer per ring.
#[repr(C)]
struct SpscRing {
    write: AtomicUsize,
    read: AtomicUsize,
    dropped: AtomicU64,
    slots: [MessageSlot; RT_MAILBOX_RING_SLOTS],
}

impl SpscRing {
    const fn new() -> Self {
        Self {
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            slots: [const { MessageSlot::new() }; RT_MAILBOX_RING_SLOTS],
        }
    }

    fn try_push(&self, msg: &RtMessage) -> Result<(), RtMailboxError> {
        let write = self.write.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= RT_MAILBOX_RING_SLOTS {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return Err(RtMailboxError::Full);
        }
        let slot = &self.slots[write % RT_MAILBOX_RING_SLOTS];
        let len = msg.len as usize;
        for index in 0..len {
            slot.payload[index].store(msg.payload[index], Ordering::Relaxed);
        }
        slot.tag.store(msg.tag, Ordering::Relaxed);
        slot.len.store(msg.len, Ordering::Relaxed);
        self.write.store(write.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    fn try_pop(&self) -> Option<RtMessage> {
        let read = self.read.load(Ordering::Relaxed);
        let write = self.write.load(Ordering::Acquire);
        if read == write {
            return None;
        }
        let slot = &self.slots[read % RT_MAILBOX_RING_SLOTS];
        let tag = slot.tag.load(Ordering::Relaxed);
        let len = (slot.len.load(Ordering::Relaxed) as usize).min(RT_MAILBOX_PAYLOAD_CAP);
        let mut payload = [0u8; RT_MAILBOX_PAYLOAD_CAP];
        for (index, byte) in payload.iter_mut().enumerate().take(len) {
            *byte = slot.payload[index].load(Ordering::Relaxed);
        }
        self.read.store(read.wrapping_add(1), Ordering::Release);
        Some(RtMessage {
            tag,
            len: len as u16,
            payload,
        })
    }

    fn depth(&self) -> usize {
        self.write
            .load(Ordering::Acquire)
            .wrapping_sub(self.read.load(Ordering::Acquire))
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// One message slot's storage. Atomic fields keep the layout well-defined for a
/// peer image while allowing cross-core reads/writes without a lock.
#[repr(C)]
struct MessageSlot {
    tag: AtomicU32,
    len: AtomicU16,
    payload: [AtomicU8; RT_MAILBOX_PAYLOAD_CAP],
}

impl MessageSlot {
    const fn new() -> Self {
        Self {
            tag: AtomicU32::new(0),
            len: AtomicU16::new(0),
            payload: [const { AtomicU8::new(0) }; RT_MAILBOX_PAYLOAD_CAP],
        }
    }
}
