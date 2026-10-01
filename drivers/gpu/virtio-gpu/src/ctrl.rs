//! Asynchronous control-queue submission machinery.
//!
//! Implements the Linux virtio_gpu submission model (Linux
//! `drivers/gpu/drm/virtio/virtgpu_vq.c`) on top of a plain split-ring
//! virtqueue:
//!
//! - **Fire-and-forget commands** ([`ControlQueue::enqueue`]) are added to the
//!   ring and return immediately. Strict virtqueue ordering guarantees the
//!   host applies them in submission order, so create → attach-backing →
//!   transfer → flush → scanout sequences need no per-command fence. Device
//!   error responses are logged by [`ControlQueue::pump_completions`] but not
//!   returned to the caller — commands that must observe the device's answer
//!   go through [`ControlQueue::request_sync`] instead.
//! - **Deferred delivery at the consumer boundary** (Linux
//!   `virtio_gpu_notify()`, virtgpu_vq.c): enqueued commands are counted but
//!   *never* kicked at submit time. Delivery happens when the consumer calls
//!   [`ControlQueue::notify`] at its transaction boundary — the exact
//!   `pending_commands` counter + `virtqueue_kick_prepare` gate shape: the
//!   batch is delivered with at most one MMIO write, and none at all when the
//!   device has suppressed notifications (`RING_EVENT_IDX`). A synchronous
//!   request or [`ControlQueue::wait_fence`] forces a delivery decision when
//!   it must wait, because it cannot return to the boundary first — the same
//!   exception Linux makes for its cursor queue. This crate therefore
//!   *requires* the boundary contract: every consumer must end a transaction
//!   that enqueued commands with [`ControlQueue::notify`] (the workspace's
//!   card0 ioctls all do, mirroring the DRM ioctl structure this model
//!   assumes).
//! - **Bounded memory**: command/response bytes live in preallocated,
//!   address-stable vbuf slots — the equivalent of Linux's `virtio_gpu_vbuf`
//!   slab (`kmem_cache`), one slot per ring entry plus the parking-FIFO cap —
//!   so a host that drains slower than the guest produces cannot grow
//!   guest-side buffering without bound.

use alloc::{boxed::Box, collections::VecDeque, vec, vec::Vec};
use core::{hint::spin_loop, mem::size_of};

use virtio_drivers::{queue::VirtQueue, transport::Transport};
use zerocopy::FromBytes;

use super::wire::{Command, CtrlHeader};
use crate::Error;

/// Number of descriptors on the control virtqueue.
///
/// With `RING_INDIRECT_DESC` a whole command collapses into one queue slot, so
/// this is effectively the number of commands that can be in flight at once.
/// 64 matches what QEMU advertises for the control queue (Linux also uses the
/// device-negotiated size).
pub(crate) const CTRL_QUEUE_SIZE: u16 = 64;
/// Cap for the parking FIFO ([`ControlQueue::pending_commands`]). Above this
/// the driver degrades to the bounded wait, so a host that drains far slower
/// than the guest produces cannot grow guest-side buffering without bound.
const PENDING_FIFO_CAP: usize = 128;
/// Maximum inline size of an async control-command buffer, mirroring Linux
/// `MAX_INLINE_CMD_SIZE` (virtgpu_vq.c): the largest control command is
/// `CmdCtxCreate` at 96 bytes, and every command submitted fire-and-forget
/// (create/attach/transfer/flush/scanout/unref/submit_3d) fits well within it.
const INLINE_CMD_SIZE: usize = 96;
/// Size in bytes of the device-writable response buffer of every enqueued
/// fire-and-forget command (the largest response this driver cares about is
/// the plain [`CtrlHeader`]).
const RESP_SIZE: usize = size_of::<CtrlHeader>();
/// Maximum number of live fire-and-forget commands: one per control-queue
/// slot plus the parking-FIFO cap. Bounds the vbuf arenas below.
const MAX_INFLIGHT: usize = CTRL_QUEUE_SIZE as usize + PENDING_FIFO_CAP;

/// Timeout for the bounded blocking waits ([`ControlQueue::wait_fence`] and
/// [`ControlQueue::wait_idle`]), in nanoseconds of the monotonic clock.
///
/// These waits run with IRQs disabled under the consumer's global lock, so an
/// unbounded wait on a stalled or dead host wedges the whole guest (the same
/// hazard `enqueue` avoids with its bounded [`Error::QueueBusy`]). Five
/// seconds is ~15000 frame-times at 3000 fps — far beyond any legitimate
/// completion, including a host paused for migration or snapshot — while
/// still recovering the guest from an unrecoverable host within a bounded
/// time.
const WAIT_TIMEOUT_NS: u64 = 5_000_000_000;

/// A fire-and-forget command in flight on the control queue (or parked while
/// the queue is full): a reference to its vbuf-arena slot plus the metadata
/// that only the driver cares about.
struct PendingSubmit {
    /// Index into the vbuf arenas holding this command's bytes.
    slot: usize,
    /// Number of valid bytes in this command's `vbuf_cmds[slot]`.
    cmd_len: usize,
    /// Optional extra device-readable payload (e.g. the virgl command stream).
    /// Heap-allocated (stable address) — Linux `vmemdup_user`s this too.
    data: Option<Vec<u8>>,
    /// Monotonic fence id assigned by the upper layer (SUBMIT_3D only).
    fence_id: u64,
}

/// The GPU control queue: a [`VirtQueue`] plus the state needed to run it in
/// the Linux async style (in-flight tracking, parking FIFO, kick suppression,
/// fence bookkeeping). See the [module docs](self) for the model.
///
/// The command/response bytes live in two parallel boxed arenas
/// ([`ControlQueue::vbuf_cmds`] / [`ControlQueue::vbuf_resps`]) that never
/// move, because the device DMAs from these addresses at arbitrary later
/// times: storing the bytes inline in a `PendingSubmit` that later gets moved
/// (into `pending[token]` or the parking FIFO) would leave the descriptors
/// pointing at a dead stack frame. Only the slot's *index* travels around the
/// driver; the bytes stay put until the used entry is popped.
pub(crate) struct ControlQueue<H: virtio_drivers::Hal> {
    queue: VirtQueue<H, { CTRL_QUEUE_SIZE as usize }>,
    /// Virtqueue index of the control queue on this device.
    queue_idx: u16,
    /// Stable-address arena of command bytes, one slot per possible in-flight
    /// or parked command.
    vbuf_cmds: Box<[[u8; INLINE_CMD_SIZE]]>,
    /// Parallel arena of device-writable response buffers, indexed like
    /// [`ControlQueue::vbuf_cmds`] (kept separate so `add_pending` can borrow
    /// command and response from `self` simultaneously).
    vbuf_resps: Box<[[u8; RESP_SIZE]]>,
    /// Indices of unused vbuf slots (a set, not a queue — any free slot works,
    /// no ordering requirement on buffer storage).
    vbuf_free: Vec<usize>,
    /// In-flight commands, keyed by descriptor token. `add` always returns a
    /// token in `0..CTRL_QUEUE_SIZE` (the free-list head is a descriptor table
    /// index), so the token can index this array directly.
    pending: [Option<PendingSubmit>; CTRL_QUEUE_SIZE as usize],
    /// Fire-and-forget commands parked while the ring is full. Re-added
    /// in-order by [`ControlQueue::flush_pending`], which runs from the next
    /// enqueue, from [`ControlQueue::pump_completions`], and therefore also
    /// from the consumer's IRQ path.
    pending_commands: VecDeque<PendingSubmit>,
    /// Number of fire-and-forget commands enqueued since the last kick
    /// decision (Linux `pending_commands` in `virtgpu_vq.c`). Cleared by
    /// [`ControlQueue::notify`], which is what delivers the batch.
    ctrl_pending: u32,
    /// Highest fence id whose completion has been observed (implicit ordering:
    /// fence N done ⇒ all ≤ N done).
    ///
    /// INVARIANT: only [`ControlQueue::pump_completions`] advances this
    /// counter, when it pops a used entry that carries a fence id. Submits
    /// are fire-and-forget (`enqueue` with a fence id); there is no blocking
    /// fenced submission path, so a fence id the host never completes keeps
    /// [`ControlQueue::wait_fence`] waiting until its timeout.
    completed_fence_id: u64,
    /// Monotonic nanosecond clock injected at construction; bounds the
    /// blocking waits. See [`WAIT_TIMEOUT_NS`].
    clock: fn() -> u64,
    /// Descriptor token of the in-flight blocking request, if any: its used
    /// entry has no `pending` record (the buffers live in the caller's
    /// frame), so the completion pump must leave it for the waiter instead
    /// of treating the missing record as a foreign completion.
    sync_token: Option<u16>,
    /// Set when the queue was invalidated (device reset, [`ControlQueue::invalidate`])
    /// or the device reported a used entry that belongs to no submission on
    /// this queue (see [`Error::QueueBroken`]). The FIFO used ring can never
    /// make progress past such an entry, and an invalidated queue has no
    /// device behind it, so every further operation fails fast instead of
    /// wedging or silently losing completions.
    broken: bool,
}

impl<H: virtio_drivers::Hal> ControlQueue<H> {
    /// Creates the control queue on the given transport.
    ///
    /// `clock` must return monotonically increasing nanoseconds; it bounds
    /// the blocking waits so a stalled host unwedges the caller (see
    /// [`WAIT_TIMEOUT_NS`]).
    pub(crate) fn new(
        transport: &mut impl Transport,
        queue_idx: u16,
        indirect: bool,
        event_idx: bool,
        clock: fn() -> u64,
    ) -> Result<Self, Error> {
        let queue = VirtQueue::new(transport, queue_idx, indirect, event_idx)?;
        Ok(Self {
            queue,
            queue_idx,
            vbuf_cmds: vec![[0; INLINE_CMD_SIZE]; MAX_INFLIGHT].into_boxed_slice(),
            vbuf_resps: vec![[0; RESP_SIZE]; MAX_INFLIGHT].into_boxed_slice(),
            vbuf_free: (0..MAX_INFLIGHT).collect(),
            pending: [const { None }; { CTRL_QUEUE_SIZE as usize }],
            pending_commands: VecDeque::new(),
            ctrl_pending: 0,
            completed_fence_id: 0,
            clock,
            sync_token: None,
            broken: false,
        })
    }

    /// Enqueues a fire-and-forget control command and returns its queue token
    /// immediately, without waiting for the device.
    ///
    /// The request bytes (`req`) and optional second device-readable payload
    /// (`data`) are copied into a vbuf slot (plus a heap box for a large
    /// payload), because the device may DMA from them long after this call
    /// returns and no caller stack survives to hold them.
    ///
    /// Enqueued commands are counted (Linux `pending_commands`) but never
    /// kicked here — delivery is the consumer's boundary
    /// [`ControlQueue::notify`], like Linux's DRM ioctl end.
    ///
    /// If the ring is full, finished entries are reclaimed once and the
    /// command is retried; if it still doesn't fit it parks in
    /// `pending_commands` and `Ok(None)` is returned — a submit never blocks
    /// on host drain. Like Linux's queue-full path, the accumulated batch is
    /// notified first so the host starts draining. Parked commands are
    /// re-added in order by [`ControlQueue::flush_pending`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::QueueBusy`] when every vbuf slot is live (ring full
    /// *and* FIFO at cap) and a reclaim pass freed nothing. Linux blocks at
    /// this boundary (`virtio_gpu_queue_ctrl_sgs` sleeps on `ctrlq.ackq`
    /// until vbufs free); this crate cannot sleep and its callers may hold a
    /// global lock, so exhaustion is reported and the consumer retries —
    /// it must not spin, or a stalled host wedges the CPU (observed as a
    /// whole-guest freeze on a single-vCPU build).
    ///
    /// Returns the queue token (`0..CTRL_QUEUE_SIZE`) for an enqueued command,
    /// or `None` if it was parked. Callers must not wait on a parked command.
    pub(crate) fn enqueue<Req: zerocopy::IntoBytes + zerocopy::Immutable>(
        &mut self,
        transport: &mut impl Transport,
        req: &Req,
        data: Option<&[u8]>,
        fence_id: u64,
    ) -> Result<Option<u16>, Error> {
        if self.broken {
            return Err(Error::QueueBroken);
        }
        // Put any parked commands onto the virtqueue first, so strict
        // submission order is preserved relative to this command.
        self.flush_pending(transport)?;

        let cmd_bytes = req.as_bytes();
        if cmd_bytes.len() > INLINE_CMD_SIZE {
            return Err(Error::InvalidParam);
        }
        // The payload box is moved into the `PendingSubmit`, not cloned — for
        // SUBMIT_3D it can be hundreds of kilobytes.
        let mut data: Option<Vec<u8>> = data.map(<[u8]>::to_vec);

        // Acquire a stable-address slot. Every path below that consumes an
        // entry (`pop_used` or this error return) gives its slot back, so
        // slots are only ever live in the ring, the parking FIFO, or `entry`.
        let slot = match self.vbuf_free.pop() {
            Some(slot) => slot,
            None => {
                // Ring and parking FIFO are both full and nothing has
                // completed: reclaim finished entries once and report
                // exhaustion if that frees nothing. Spinning here would hold
                // the consumer's lock (and IRQs) for as long as the host
                // stays stalled — see [`Error::QueueBusy`].
                self.pump_completions(transport)?;
                self.notify(transport);
                let Some(slot) = self.vbuf_free.pop() else {
                    return Err(Error::QueueBusy);
                };
                slot
            }
        };
        self.vbuf_cmds[slot][..cmd_bytes.len()].copy_from_slice(cmd_bytes);
        self.vbuf_resps[slot] = [0; RESP_SIZE];
        let mut entry = PendingSubmit {
            slot,
            cmd_len: cmd_bytes.len(),
            data: data.take(),
            fence_id,
        };

        match self.add_pending(&mut entry) {
            Ok(token) => {
                self.ctrl_pending += 1;
                self.pending[token as usize] = Some(entry);
                Ok(Some(token))
            }
            Err(err) if is_queue_full(&err) => {
                // Never busy-spin on host drain: reclaim finished entries and
                // try once more, then either park the owned command in the
                // FIFO or report exhaustion.
                self.pump_completions(transport)?;
                match self.add_pending(&mut entry) {
                    Ok(token) => {
                        self.ctrl_pending += 1;
                        self.pending[token as usize] = Some(entry);
                        Ok(Some(token))
                    }
                    Err(err)
                        if is_queue_full(&err)
                            && self.pending_commands.len() < PENDING_FIFO_CAP =>
                    {
                        // Park only after delivering everything queued so
                        // far (Linux's queue-full `virtio_gpu_notify`).
                        self.notify(transport);
                        self.pending_commands.push_back(entry);
                        Ok(None)
                    }
                    Err(err) if is_queue_full(&err) => {
                        // FIFO at cap too: the submit cannot be parked and
                        // must not spin — report exhaustion, slot returned.
                        self.vbuf_free.push(entry.slot);
                        Err(Error::QueueBusy)
                    }
                    Err(err) => {
                        self.vbuf_free.push(entry.slot);
                        Err(err)
                    }
                }
            }
            Err(err) => {
                self.vbuf_free.push(entry.slot);
                Err(err)
            }
        }
    }

    /// Synchronous, zero-copy control request: adds the caller's borrowed
    /// buffers to the control queue, kicks, waits for the used entry, and pops
    /// it back into the same buffers, which live in the caller's frame
    /// across the entire call.
    ///
    /// The buffers' addresses are handed to the device through
    /// [`virtio_drivers::Hal::share`], so callers must pass memory whose
    /// device-visible address that translation resolves faithfully — an
    /// allocator-owned buffer, never an arbitrary stack frame (a vmap'd task
    /// stack can mistranslate under a linear-offset `share` implementation).
    ///
    /// Parked commands are flushed first and earlier in-flight entries are
    /// drained while waiting, so strict submission order is preserved
    /// end-to-end: the used ring is FIFO, so this command's entry completes
    /// only after every older one.
    ///
    /// The wait busy-polls with `spin_loop`, exactly as upstream
    /// `add_notify_wait_pop` does, bounded by the wait timeout like every
    /// other wait. Expiry cannot simply return an error: the ring entry
    /// would still reference the caller's returned stack frame, which the
    /// device could DMA into, so the queue is marked broken instead — the
    /// caller's failure path then resets the device, stopping all DMA (the
    /// same contract as `add_sync`'s queue-full loop below). It is only
    /// reachable from the init-time and capset queries, and a completion
    /// pump marks the queue broken long before it if the device reports
    /// foreign completions.
    pub(crate) fn request_sync<'a: 'b, 'b>(
        &mut self,
        transport: &mut impl Transport,
        inputs: &'a [&'b [u8]],
        outputs: &'a mut [&'b mut [u8]],
    ) -> Result<u32, Error> {
        let token = self.add_sync(transport, inputs, outputs)?;
        self.wait_sync(token, transport, inputs, outputs)
    }

    /// Adds a synchronous request's buffers and kicks the host exactly as
    /// upstream `add_notify_wait_pop` does: notify only when the device has
    /// not suppressed kicks (`should_notify` ≡ `virtqueue_kick_prepare`; with
    /// event-index the host is only asked when it is waiting). If the host is
    /// still draining earlier commands it will reach this one without a kick —
    /// the standard virtio no-lost-wakeup protocol covers it.
    fn add_sync<'a, 'b>(
        &mut self,
        transport: &mut impl Transport,
        inputs: &'a [&'b [u8]],
        outputs: &'a mut [&'b mut [u8]],
    ) -> Result<u16, Error> {
        self.flush_pending(transport)?;

        let deadline = (self.clock)().saturating_add(WAIT_TIMEOUT_NS);
        let token = loop {
            // SAFETY: the borrowed buffers live in the caller's frame until the
            // matching `pop_used` in `wait_sync`, exactly as
            // `add_notify_wait_pop` requires.
            match unsafe { self.queue.add(inputs, outputs) } {
                Ok(t) => break t,
                Err(virtio_drivers::Error::QueueFull) => {
                    // The ring is full of commands the host has not drained
                    // yet: kick so it makes progress, reclaim finished entries
                    // and retry. Unlike the fire-and-forget path this cannot
                    // report exhaustion — the borrowed buffers live in the
                    // caller's frame and a returned error would leave the ring
                    // entry dangling — so a stalled host breaks the queue
                    // instead (the caller's failure path resets the device).
                    // Reachable from the init-time queries only; moving the
                    // wait out of the caller's frame is the remaining
                    // alignment work.
                    transport.notify(self.queue_idx);
                    self.pump_completions(transport)?;
                    if (self.clock)() >= deadline {
                        return Err(self.mark_broken(
                            "the ring never drained a queue-full synchronous request",
                        ));
                    }
                    spin_loop();
                }
                Err(err) => return Err(err.into()),
            }
        };
        self.sync_token = Some(token);

        // Unconditional kick — same wrap-safety reasoning as `notify` (the
        // crate's `should_notify()` is not reliable across the index wrap).
        transport.notify(self.queue_idx);
        Ok(token)
    }

    /// Waits until the synchronous request `token` is at the head of the used
    /// ring, then pops it back into the caller's buffers.
    fn wait_sync<'a: 'b, 'b>(
        &mut self,
        token: u16,
        transport: &mut impl Transport,
        inputs: &'a [&'b [u8]],
        outputs: &'a mut [&'b mut [u8]],
    ) -> Result<u32, Error> {
        let deadline = (self.clock)().saturating_add(WAIT_TIMEOUT_NS);
        loop {
            // Reclaim earlier in-flight entries (fire-and-forget commands
            // submitted before this one) so the whole queue keeps making
            // progress.
            self.pump_completions(transport)?;
            if self.queue.peek_used() == Some(token) {
                // SAFETY: same buffers as the `add` in `add_sync`; still alive
                // in the caller's frame.
                let popped = unsafe { self.queue.pop_used(token, inputs, outputs) };
                self.sync_token = None;
                return match popped {
                    Ok(len) => Ok(len),
                    // The device replaced its own used entry between the peek
                    // and the pop: from here on the ring cannot be trusted.
                    Err(err) => Err(self.mark_broken(err)),
                };
            }
            if (self.clock)() >= deadline {
                // The ring entry still references the caller's stack frame,
                // so it can never be reclaimed on this path: break the queue
                // (every further operation fails fast) and let the caller's
                // failure path reset the device. Bounded, like every other
                // wait.
                return Err(self.mark_broken("the host never completed the synchronous request"));
            }
            spin_loop();
        }
    }

    /// Adds the buffers of a [`PendingSubmit`] to the ring WITHOUT notifying
    /// the host. Callers must eventually call [`ControlQueue::notify`], or the
    /// command sits in the avail ring undelivered.
    fn add_pending(&mut self, entry: &mut PendingSubmit) -> Result<u16, Error> {
        let (cmd, data, resp) = entry_bufs(entry, &self.vbuf_cmds, &mut self.vbuf_resps);
        match data {
            // SAFETY: the entry's buffers are owned by the vbuf arenas and the
            // entry, and stay alive (untouched) until the used entry is popped
            // with them.
            Some(d) => unsafe { self.queue.add(&[cmd, d], &mut [resp]) },
            // SAFETY: as above; there is no extra payload buffer.
            None => unsafe { self.queue.add(&[cmd], &mut [resp]) },
        }
        .map_err(Error::from)
    }

    /// Delivers the accumulated batch with at most one MMIO write — Linux
    /// `virtio_gpu_notify()`, virtgpu_vq.c, verbatim minus the multi-submitter
    /// lock (this queue has a single `&mut self` owner).
    ///
    /// Call at the end of a transaction that enqueued fire-and-forget commands
    /// (the workspace's card0 ioctls all do, mirroring the DRM ioctl
    /// structure); this is the *only* thing that delivers them. Also before
    /// submitting a command on a *different* queue that references resources
    /// created by control commands (the queues have no mutual ordering
    /// guarantee), and from the queue-full paths so the host starts draining.
    /// Commands parked because the ring was full are *not* delivered by this —
    /// they re-enter the ring via [`ControlQueue::flush_pending`] once the
    /// host drains. No-op when the accumulator is empty.
    pub(crate) fn notify(&mut self, transport: &mut impl Transport) {
        if self.broken || self.ctrl_pending == 0 {
            return;
        }
        self.ctrl_pending = 0;
        // Always delivered as a real MMIO write, deliberately NOT gated on
        // `should_notify()`: virtio-drivers 0.13.0 implements the
        // RING_EVENT_IDX decision as `avail_idx >= avail_event + 1` with a
        // plain non-wrapping u16 `>=`, but `vring_need_event` requires
        // wrapping arithmetic. A batch published across the 65536 index
        // wrap leaves `avail_idx` small while a behind host still holds a
        // large `avail_event`, so every later boundary kick decision
        // returns false and the host is never told about the batch —
        // observed (QMP `x-query-virtio-queue-status`) as an idle host main
        // loop with `last-avail-idx` frozen at 65534 and `inuse: 0` while
        // the guest exhausted the queue. Extra kicks are always safe per
        // the virtio spec and cost one MMIO write per transaction boundary.
        transport.notify(self.queue_idx);
    }

    /// Pops and reclaims every used control-queue entry currently available.
    ///
    /// For each reclaimed entry, recycles the descriptors (`H::unshare` with
    /// the persisted vbuf buffers), advances `completed_fence_id` (implicit
    /// ordering: fence N popped ⇒ all ≤ N done), and logs device-side error
    /// responses (fire-and-forget callers have no other way to learn about
    /// them; Linux `virtio_gpu_dequeue_ctrl_func` logs them too). This is the
    /// counterpart of Linux's IRQ-driven dequeue func; call it from the IRQ
    /// handler and/or from the polling wait paths.
    ///
    /// Entries belonging to an in-flight [`ControlQueue::request_sync`] have
    /// no pending record (their buffers live in the caller's frame); the used
    /// ring is strictly ordered, so once one is reached nothing behind it can
    /// be reclaimed either and this returns, leaving it for its waiter.
    pub(crate) fn pump_completions(&mut self, transport: &mut impl Transport) -> Result<(), Error> {
        if self.broken {
            return Err(Error::QueueBroken);
        }
        while self.queue.can_pop() {
            let Some(token) = self.queue.peek_used() else {
                break;
            };
            if self.sync_token == Some(token) {
                // The in-flight blocking request's entry: its buffers live in
                // the caller's frame, so the waiter pops it (and the used ring
                // is strictly ordered, so nothing behind it can be reclaimed
                // either).
                break;
            }
            // The used id is device-written. An id outside the pending table,
            // or one with neither a pending record nor an in-flight blocking
            // request, was never submitted by this driver: `pop_used` can
            // never consume it (its token check fails for our real tokens),
            // and the FIFO used ring means nothing behind it can ever be
            // reclaimed either. Fail the queue loudly instead of wedging.
            let entry = if (token as usize) < self.pending.len() {
                self.pending[token as usize].take()
            } else {
                None
            };
            let Some(entry) = entry else {
                return Err(self.mark_broken(virtio_drivers::Error::WrongToken));
            };
            {
                let (cmd, data, resp) = entry_bufs(&entry, &self.vbuf_cmds, &mut self.vbuf_resps);
                let popped = match data {
                    // SAFETY: the vbuf arena slot and the data box are the
                    // exact buffers `add` saw; they are owned, address-stable,
                    // and untouched since enqueue, so unshare gets valid
                    // buffers.
                    Some(d) => unsafe { self.queue.pop_used(token, &[cmd, d], &mut [resp]) },
                    // SAFETY: as above; there is no extra payload buffer.
                    None => unsafe { self.queue.pop_used(token, &[cmd], &mut [resp]) },
                };
                if let Err(err) = popped {
                    // The entry never completed, and a pop that fails after
                    // its own `peek` means the used ring disagrees with the
                    // driver's records: give the slot back and break the
                    // queue rather than wedge on this entry forever.
                    self.vbuf_free.push(entry.slot);
                    return Err(self.mark_broken(err));
                }
            }
            if entry.fence_id > self.completed_fence_id {
                self.completed_fence_id = entry.fence_id;
            }
            // Fire-and-forget: the response is dropped here (no waiter), but
            // surface device-side errors on the log.
            let rsp_hdr = CtrlHeader::read_from_bytes(&self.vbuf_resps[entry.slot])
                .expect("response buffer is exactly one CtrlHeader");
            if rsp_hdr.hdr_type.0 >= Command::ERR_UNSPEC.0 {
                let cmd_hdr = CtrlHeader::read_from_bytes(&self.vbuf_cmds[entry.slot][..RESP_SIZE])
                    .expect("command buffer holds at least one CtrlHeader");
                log::warn!(
                    "virtio-gpu: control command 0x{:x} (token {}) failed with error response \
                     0x{:x}",
                    cmd_hdr.hdr_type.0,
                    token,
                    rsp_hdr.hdr_type.0
                );
            }
            self.vbuf_free.push(entry.slot);
        }
        // Slots freed by the pops: put any parked commands back on the ring in
        // submission order (this drains the FIFO as the host makes progress,
        // even when the guest is between transactions — e.g. driven by the IRQ
        // path).
        self.flush_pending(transport)
    }

    /// Synchronously discards all in-flight state after the device has been
    /// reset (or is about to be): parked commands, pending records and the
    /// kick accumulator. The caller must perform the transport reset around
    /// this call; after the queue memory is unregistered nothing can complete
    /// those commands, so their bookkeeping is dropped rather than waited
    /// for. Every further queue operation fails fast with
    /// [`Error::QueueBroken`] — the terminal state; there is no un-invalidate.
    pub(crate) fn invalidate(&mut self) {
        if self.broken
            && self.pending_commands.is_empty()
            && self.pending.iter().all(|entry| entry.is_none())
            && self.sync_token.is_none()
        {
            return;
        }
        let in_flight = self.pending_commands.len()
            + self.pending.iter().filter(|entry| entry.is_some()).count()
            + usize::from(self.sync_token.is_some());
        self.broken = true;
        self.pending = [const { None }; { CTRL_QUEUE_SIZE as usize }];
        self.pending_commands.clear();
        self.vbuf_free.clear();
        self.ctrl_pending = 0;
        self.sync_token = None;
        log::warn!(
            "virtio-gpu: control queue invalidated; {in_flight} in-flight command(s) dropped"
        );
    }

    /// Records a foreign completion and returns the error every further
    /// operation on this queue reports. The `cause` is only logged (once, at
    /// the first trip); the typed result is [`Error::QueueBroken`] so callers
    /// fail fast instead of wedging on the unusable used ring.
    fn mark_broken(&mut self, cause: impl core::fmt::Debug) -> Error {
        if !self.broken {
            self.broken = true;
            log::error!(
                "virtio-gpu: control queue broken by a foreign completion ({cause:?}); all \
                 further operations fail until the device is reset"
            );
        }
        Error::QueueBroken
    }

    /// Moves parked commands onto the ring in strict FIFO order, as long as
    /// there are free slots.
    ///
    /// Re-added commands count toward the accumulator; if any were re-added,
    /// one notify at the end delivers them (the host may be idle between
    /// transactions and nothing else would kick). No-op when the FIFO is
    /// empty.
    fn flush_pending(&mut self, transport: &mut impl Transport) -> Result<(), Error> {
        let mut re_added = false;
        while let Some(mut cmd) = self.pending_commands.pop_front() {
            // `cmd` is moved out of the FIFO first, so no borrow of
            // `pending_commands` outlives the `add_pending` call.
            match self.add_pending(&mut cmd) {
                Ok(token) => {
                    self.pending[token as usize] = Some(cmd);
                    self.ctrl_pending = self.ctrl_pending.saturating_add(1);
                    re_added = true;
                }
                Err(err) if is_queue_full(&err) => {
                    // Ring saturated again: park it back at the front and wait
                    // for another drain trigger (next enqueue / pump / IRQ).
                    self.pending_commands.push_front(cmd);
                    break;
                }
                Err(err) => {
                    self.pending_commands.push_front(cmd);
                    return Err(err);
                }
            }
        }
        if re_added {
            self.notify(transport);
        }
        Ok(())
    }

    /// Blocks until the fence identified by `fence_id` (and everything
    /// enqueued before it) has been popped from the ring.
    ///
    /// Delivers the fire-and-forget commands accumulated since the last kick
    /// before waiting: the fenced entry itself may still be sitting in the
    /// kick accumulator, so a wait must force delivery (the same invariant
    /// Linux guarantees with its `virtio_gpu_notify()` before
    /// `virtio_gpu_wait_ioctl`).
    ///
    /// The wait busy-polls with `spin_loop`, like
    /// [`ControlQueue::request_sync`], and is bounded by
    /// [`WAIT_TIMEOUT_NS`]: a stalled host returns [`Error::TimedOut`] with
    /// the queue still usable instead of wedging the caller (which holds its
    /// global lock with IRQs disabled) forever.
    pub(crate) fn wait_fence(
        &mut self,
        transport: &mut impl Transport,
        fence_id: u64,
    ) -> Result<(), Error> {
        let deadline = (self.clock)().saturating_add(WAIT_TIMEOUT_NS);
        while self.completed_fence_id < fence_id {
            // Force a kick decision: the fenced entry may not have been
            // delivered to the host yet (the consumer's boundary notify may
            // from a full ring, `enqueue` performs no kick on its own).
            self.notify(transport);
            self.pump_completions(transport)?;
            if self.completed_fence_id >= fence_id {
                break;
            }
            if (self.clock)() >= deadline {
                return Err(Error::TimedOut);
            }
            spin_loop();
        }
        Ok(())
    }

    /// Blocks until every enqueued command — fire-and-forget and synchronous
    /// alike — has been popped from the ring. Used by teardown paths that
    /// hand device-accessible memory back to their owner right after issuing
    /// the commands that stop the device from touching it (unref, detach,
    /// scanout-off): the pop is the proof the host is done.
    ///
    /// Delivers the accumulated batch first (a never-notified command would
    /// otherwise spin here forever), then pumps; the busy-wait is bounded by
    /// the host actually draining, which every preceding kick guarantees.
    ///
    /// The wait is additionally bounded by [`WAIT_TIMEOUT_NS`]: a stalled
    /// host returns [`Error::TimedOut`] instead of wedging the caller forever.
    /// A caller that releases device-accessible memory after this error may
    /// race a device that is still (or was never) done — only reachable when
    /// the host is unrecoverably stalled, where guest recovery beats waiting.
    pub(crate) fn wait_idle(&mut self, transport: &mut impl Transport) -> Result<(), Error> {
        let deadline = (self.clock)().saturating_add(WAIT_TIMEOUT_NS);
        loop {
            self.notify(transport);
            self.pump_completions(transport)?;
            let drained = self.pending_commands.is_empty()
                && self.pending.iter().all(|entry| entry.is_none())
                && !self.queue.can_pop();
            if drained {
                return Ok(());
            }
            if (self.clock)() >= deadline {
                return Err(Error::TimedOut);
            }
            spin_loop();
        }
    }

    /// Non-blocking fence query: has `fence_id` (and everything enqueued
    /// before it) already been popped, i.e. has its virgl fence fired?
    ///
    /// Only reflects batches that have actually been delivered to the host; a
    /// consumer that only polls must ensure delivery itself (e.g.
    /// [`ControlQueue::notify`] at the transaction boundary, or a
    /// [`ControlQueue::wait_fence`]). The high-water mark only advances when
    /// completed entries are popped, so without an IRQ handler calling
    /// [`ControlQueue::pump_completions`] the poll loop must call it itself.
    /// The counterpart of Linux `dma_resv_test_signaled` in the NOWAIT probe
    /// of `virtio_gpu_wait_ioctl` (virtgpu_ioctl.c).
    pub(crate) fn fence_completed(&self, fence_id: u64) -> bool {
        self.completed_fence_id >= fence_id
    }
}

/// Whether `err` is the virtqueue's ring-full condition, which the submission
/// paths treat as flow control rather than failure.
fn is_queue_full(err: &Error) -> bool {
    matches!(err, Error::VirtIo(virtio_drivers::Error::QueueFull))
}

/// Slices a pending entry's device-visible buffers out of the arenas: the
/// command bytes, the optional extra payload, and the device-writable response
/// buffer.
fn entry_bufs<'a>(
    entry: &'a PendingSubmit,
    cmds: &'a [[u8; INLINE_CMD_SIZE]],
    resps: &'a mut [[u8; RESP_SIZE]],
) -> (&'a [u8], Option<&'a [u8]>, &'a mut [u8]) {
    (
        &cmds[entry.slot][..entry.cmd_len],
        entry.data.as_deref(),
        &mut resps[entry.slot],
    )
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec};
    use core::{
        ptr::{NonNull, slice_from_raw_parts_mut},
        sync::atomic::{AtomicU16, AtomicU64, Ordering},
    };
    use std::sync::Mutex;

    use virtio_drivers::{
        BufferDirection, PhysAddr,
        transport::{DeviceStatus, DeviceType, InterruptStatus},
    };
    use zerocopy::{FromBytes, Immutable, IntoBytes};

    use super::*;

    const Q: u16 = 0;
    const QSIZE: usize = CTRL_QUEUE_SIZE as usize;
    const RSP: u32 = RESP_SIZE as u32;

    #[repr(C)]
    #[derive(Clone, Copy, FromBytes, Immutable, IntoBytes)]
    struct TestCmd {
        kind: u32,
        seq: u32,
    }

    /// A command code the completion pump classifies as an error response.
    const ERR_KIND: u32 = Command::ERR_UNSPEC.0;

    // --- Fake device plumbing (virtio-drivers 0.13.0 keeps its own fakes
    // behind `#[cfg(test)]`, so this crate ships a minimal equivalent). ---

    /// Split-ring layouts the driver writes; replicated here so the fake
    /// device can walk the rings through the addresses `queue_set` reported.
    #[repr(C)]
    struct AvailRing {
        _flags: u16,
        idx: AtomicU16,
        ring: [u16; QSIZE],
        _used_event: u16,
    }

    #[repr(C)]
    struct UsedElem {
        id: u32,
        len: u32,
    }

    #[repr(C)]
    struct UsedRing {
        _flags: u16,
        idx: AtomicU16,
        ring: [UsedElem; QSIZE],
        _avail_event: u16,
    }

    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    struct Desc {
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    }

    const DESC_NEXT: u16 = 1;
    const DESC_WRITE: u16 = 2;
    const DESC_INDIRECT: u16 = 4;

    /// Test HAL: `share` copies the buffer into an exact-size heap allocation
    /// and uses its address as the "physical" address, so the fake device
    /// reads a stable copy and `unshare` copies device writes back.
    struct TestHal;

    // SAFETY: DMA allocations are whole zeroed pages owned by the value;
    // `share`d buffers are exact-size heap copies that stay stable until
    // `unshare` reclaims them, and virtual addresses double as physical ones
    // for the in-process fake device.
    unsafe impl virtio_drivers::Hal for TestHal {
        fn dma_alloc(pages: usize, _direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
            assert_ne!(pages, 0);
            let layout = std::alloc::Layout::from_size_align(
                pages * virtio_drivers::PAGE_SIZE,
                virtio_drivers::PAGE_SIZE,
            )
            .unwrap();
            let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
            match NonNull::new(ptr) {
                Some(ptr) => (ptr.as_ptr() as PhysAddr, ptr),
                None => std::alloc::handle_alloc_error(layout),
            }
        }

        unsafe fn dma_dealloc(_paddr: PhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
            assert_ne!(pages, 0);
            let layout = std::alloc::Layout::from_size_align(
                pages * virtio_drivers::PAGE_SIZE,
                virtio_drivers::PAGE_SIZE,
            )
            .unwrap();
            unsafe { std::alloc::dealloc(vaddr.as_ptr(), layout) };
            0
        }

        unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
            NonNull::new(paddr as _).unwrap()
        }

        unsafe fn share(buffer: NonNull<[u8]>, direction: BufferDirection) -> PhysAddr {
            assert_ne!(buffer.len(), 0);
            let mut shared = vec![0u8; buffer.len()].into_boxed_slice();
            if matches!(
                direction,
                BufferDirection::DriverToDevice | BufferDirection::Both
            ) {
                shared.copy_from_slice(unsafe { buffer.as_ref() });
            }
            let vaddr = Box::into_raw(shared) as *mut u8 as usize;
            vaddr as PhysAddr
        }

        unsafe fn unshare(paddr: PhysAddr, buffer: NonNull<[u8]>, direction: BufferDirection) {
            assert_ne!(buffer.len(), 0);
            assert_ne!(paddr, 0);
            let shared =
                unsafe { Box::from_raw(slice_from_raw_parts_mut(paddr as *mut u8, buffer.len())) };
            if matches!(
                direction,
                BufferDirection::DeviceToDriver | BufferDirection::Both
            ) {
                let bytes: &[u8] = &shared;
                let dst = buffer.as_ptr() as *mut u8;
                unsafe { dst.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len()) };
            }
        }
    }

    #[derive(Default)]
    struct QueueStatus {
        descriptors: PhysAddr,
        driver_area: PhysAddr,
        device_area: PhysAddr,
        notified: bool,
    }

    struct State {
        queues: Vec<QueueStatus>,
        /// `seq` values of every command the fake device processed, in order.
        processed: Vec<u32>,
        /// Raw input bytes of the most recently processed command.
        last_input: Vec<u8>,
    }

    struct FakeTransport {
        state: Arc<Mutex<State>>,
    }

    impl virtio_drivers::transport::Transport for FakeTransport {
        fn device_type(&self) -> DeviceType {
            DeviceType::GPU
        }

        fn read_device_features(&mut self) -> u64 {
            0
        }

        fn write_driver_features(&mut self, _driver_features: u64) {}

        fn max_queue_size(&mut self, _queue: u16) -> u32 {
            QSIZE as u32
        }

        fn notify(&mut self, queue: u16) {
            self.state.lock().unwrap().queues[queue as usize].notified = true;
        }

        fn get_status(&self) -> DeviceStatus {
            DeviceStatus::empty()
        }

        fn set_status(&mut self, _status: DeviceStatus) {}

        fn set_guest_page_size(&mut self, _guest_page_size: u32) {}

        fn requires_legacy_layout(&self) -> bool {
            false
        }

        fn queue_set(
            &mut self,
            queue: u16,
            _size: u32,
            descriptors: PhysAddr,
            driver_area: PhysAddr,
            device_area: PhysAddr,
        ) {
            let q = &mut self.state.lock().unwrap().queues[queue as usize];
            q.descriptors = descriptors;
            q.driver_area = driver_area;
            q.device_area = device_area;
        }

        fn queue_unset(&mut self, queue: u16) {
            let q = &mut self.state.lock().unwrap().queues[queue as usize];
            *q = QueueStatus::default();
        }

        fn queue_used(&mut self, queue: u16) -> bool {
            self.state.lock().unwrap().queues[queue as usize].descriptors != 0
        }

        fn ack_interrupt(&mut self) -> InterruptStatus {
            InterruptStatus::empty()
        }

        fn read_config_generation(&self) -> u32 {
            0
        }

        fn read_config_space<T: FromBytes>(
            &self,
            _offset: usize,
        ) -> Result<T, virtio_drivers::Error> {
            Ok(T::new_zeroed())
        }

        fn write_config_space<T: Immutable + IntoBytes>(
            &mut self,
            _offset: usize,
            _value: T,
        ) -> Result<(), virtio_drivers::Error> {
            Ok(())
        }
    }

    fn make_ctrl() -> (ControlQueue<TestHal>, Arc<Mutex<State>>, FakeTransport) {
        // A frozen clock never reaches the wait deadline, so the bounded
        // waits behave exactly like the historical unbounded ones.
        make_ctrl_with(frozen_clock)
    }

    /// Clock that never advances: the wait deadline is never reached.
    fn frozen_clock() -> u64 {
        0
    }

    fn make_ctrl_with(
        clock: fn() -> u64,
    ) -> (ControlQueue<TestHal>, Arc<Mutex<State>>, FakeTransport) {
        let state = Arc::new(Mutex::new(State {
            queues: vec![QueueStatus::default()],
            processed: Vec::new(),
            last_input: Vec::new(),
        }));
        let mut transport = FakeTransport {
            state: state.clone(),
        };
        let ctrl = ControlQueue::new(&mut transport, Q, true, false, clock).unwrap();
        (ctrl, state, transport)
    }

    /// Monotonic clock advancing one second per read, for deterministic
    /// timeout tests: `WAIT_TIMEOUT_NS` elapses after six reads.
    fn ticking_clock() -> u64 {
        static NOW: AtomicU64 = AtomicU64::new(0);
        NOW.fetch_add(1_000_000_000, Ordering::Relaxed)
    }

    /// Simulates a broken device: reports a used entry for `id`, a descriptor
    /// chain this driver never submitted, without touching the real rings.
    fn inject_used_entry(state: &Mutex<State>, id: u16) {
        let st = state.lock().unwrap();
        let device_addr = st.queues[Q as usize].device_area;
        // SAFETY: the device area was allocated and zeroed by `TestHal` and
        // registered via `queue_set`; only this fn (under the lock) and the
        // driver's atomically-ordered ring accesses touch it.
        unsafe {
            let used = device_addr as *mut UsedRing;
            let slot = (*used).idx.load(Ordering::Acquire) & (QSIZE as u16 - 1);
            (*used).ring[slot as usize] = UsedElem {
                id: u32::from(id),
                len: 0,
            };
            (*used).idx.fetch_add(1, Ordering::Release);
        }
    }

    /// Simulates the device processing exactly one descriptor chain (FIFO):
    /// reads the command, records its `seq`, writes the `respond` output into
    /// the device-writable buffers, and marks the entry used. Returns whether
    /// a chain was available.
    fn complete_one(state: &Mutex<State>, respond: impl FnOnce(&[u8]) -> Vec<u8>) -> bool {
        let mut st = state.lock().unwrap();
        let (desc_addr, driver_addr, device_addr) = {
            let q = &st.queues[Q as usize];
            (q.descriptors, q.driver_area, q.device_area)
        };
        // SAFETY: the queue areas were allocated and zeroed by `TestHal` and
        // registered via `queue_set`; only this fn (under the lock) and the
        // driver's atomically-ordered ring accesses touch them.
        unsafe {
            let avail = driver_addr as *const AvailRing;
            let used = device_addr as *mut UsedRing;
            let descs = desc_addr as *const Desc;
            if (*avail).idx.load(Ordering::Acquire) == (*used).idx.load(Ordering::Acquire) {
                return false;
            }
            let slot = (*used).idx.load(Ordering::Acquire) & (QSIZE as u16 - 1);
            let head = (*avail).ring[slot as usize];

            let mut input: Vec<u8> = Vec::new();
            let mut outputs: Vec<(u64, u32)> = Vec::new();
            let head_desc = *descs.add(head as usize);
            if head_desc.flags & DESC_INDIRECT != 0 {
                let table = core::slice::from_raw_parts(
                    head_desc.addr as *const Desc,
                    (head_desc.len as usize) / size_of::<Desc>(),
                );
                for d in table {
                    if d.flags & DESC_WRITE != 0 {
                        outputs.push((d.addr, d.len));
                    } else {
                        input.extend_from_slice(core::slice::from_raw_parts(
                            d.addr as *const u8,
                            d.len as usize,
                        ));
                    }
                }
            } else {
                let mut d = head_desc;
                loop {
                    if d.flags & DESC_WRITE != 0 {
                        outputs.push((d.addr, d.len));
                    } else {
                        input.extend_from_slice(core::slice::from_raw_parts(
                            d.addr as *const u8,
                            d.len as usize,
                        ));
                    }
                    if d.flags & DESC_NEXT == 0 {
                        break;
                    }
                    d = *descs.add(d.next as usize);
                }
            }

            if input.len() >= size_of::<TestCmd>() {
                let cmd = TestCmd::read_from_bytes(&input[..size_of::<TestCmd>()]).unwrap();
                st.processed.push(cmd.seq);
            }
            st.last_input = input.clone();

            let output = respond(&input);
            let mut remaining = output.as_slice();
            let mut written = 0usize;
            for (addr, len) in outputs {
                let n = remaining.len().min(len as usize);
                core::ptr::copy_nonoverlapping(remaining.as_ptr(), addr as *mut u8, n);
                remaining = &remaining[n..];
                written += n;
            }
            assert!(
                remaining.is_empty(),
                "response did not fit the output buffers"
            );

            (*used).ring[slot as usize] = UsedElem {
                id: u32::from(head),
                len: (input.len() + written) as u32,
            };
            (*used).idx.fetch_add(1, Ordering::Release);
            true
        }
    }

    /// Standard OK response echoing the command type.
    fn ok_responder(input: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; RESP_SIZE];
        out[..4].copy_from_slice(&input[..4]);
        out
    }

    fn poll_queue_notified(state: &Mutex<State>) -> bool {
        let mut st = state.lock().unwrap();
        core::mem::take(&mut st.queues[Q as usize].notified)
    }

    // --- Tests ---

    #[test]
    fn enqueued_commands_stay_undelivered_until_the_boundary_notify() {
        // Linux `virtio_gpu_notify()` semantics: enqueue only bumps the
        // pending counter; the transaction boundary's notify delivers the
        // whole batch with one kick; a second notify with an empty counter
        // must NOT kick.
        let (mut ctrl, state, mut transport) = make_ctrl();

        for i in 0..8 {
            ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: i }, None, 0)
                .unwrap();
        }
        assert!(!poll_queue_notified(&state));

        // The boundary notify delivers the whole batch with one kick.
        ctrl.notify(&mut transport);
        assert!(poll_queue_notified(&state));

        // The counter was cleared: a notify with an empty accumulator does
        // not kick.
        ctrl.notify(&mut transport);
        assert!(!poll_queue_notified(&state));

        // The host drains one command; nothing new to notify until the next
        // enqueue.
        assert!(complete_one(&state, ok_responder));
        ctrl.pump_completions(&mut transport).unwrap();
        assert!(!poll_queue_notified(&state));
    }

    #[test]
    fn notify_delivers_accumulated_batch_early() {
        // A transaction-boundary consumer: notify after a small batch delivers
        // it immediately, well before the threshold.
        let (mut ctrl, state, mut transport) = make_ctrl();

        for i in 0..3 {
            ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: i }, None, 0)
                .unwrap();
        }
        assert!(!poll_queue_notified(&state));
        ctrl.notify(&mut transport);
        assert!(poll_queue_notified(&state));
    }

    #[test]
    fn sync_request_roundtrip_is_zero_copy() {
        let (mut ctrl, state, mut transport) = make_ctrl();

        let req = [7u8; 16];
        let mut resp = [0u8; RESP_SIZE];
        let token = ctrl
            .add_sync(&mut transport, &[&req], &mut [&mut resp])
            .unwrap();

        // A response is owed, so the add kicks unconditionally.
        assert!(poll_queue_notified(&state));

        // The device reads the command bytes (shared copy) and writes the
        // response into the shared copy of `resp`.
        assert!(complete_one(&state, |input| {
            assert_eq!(&input[..16], &[7u8; 16]);
            let mut out = vec![0u8; RESP_SIZE];
            out[0] = 0xAA;
            out
        }));

        let used_len = ctrl
            .wait_sync(token, &mut transport, &[&req], &mut [&mut resp])
            .unwrap();
        assert_eq!(used_len, 16 + RSP);
        // `unshare` copied the device-written bytes back into our buffer.
        assert_eq!(resp[0], 0xAA);
    }

    #[test]
    fn pump_advances_fence_high_water_with_implicit_ordering() {
        let (mut ctrl, state, mut transport) = make_ctrl();

        ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 1 }, None, 5)
            .unwrap();
        assert!(!ctrl.fence_completed(5));

        assert!(complete_one(&state, ok_responder));
        ctrl.pump_completions(&mut transport).unwrap();
        assert!(ctrl.fence_completed(5));
        // Implicit ordering: everything ≤ 5 is complete too.
        assert!(ctrl.fence_completed(4));
        assert_eq!(state.lock().unwrap().processed, vec![1]);
    }

    #[test]
    fn wait_fence_returns_once_the_host_completes_the_entry() {
        let (mut ctrl, state, mut transport) = make_ctrl();

        ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 1 }, None, 9)
            .unwrap();
        assert!(complete_one(&state, ok_responder));
        ctrl.wait_fence(&mut transport, 9).unwrap();
        assert!(ctrl.fence_completed(9));
    }

    #[test]
    fn error_response_pop_does_not_wedge_the_queue() {
        // The pump has no caller to return an error response to; it logs one
        // (not observable here) but must pop cleanly and keep the queue
        // usable.
        let (mut ctrl, state, mut transport) = make_ctrl();

        ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 1 }, None, 0)
            .unwrap();
        assert!(complete_one(&state, |_input| {
            let mut out = vec![0u8; RESP_SIZE];
            out[..4].copy_from_slice(&ERR_KIND.to_ne_bytes());
            out
        }));
        ctrl.pump_completions(&mut transport).unwrap();
        assert_eq!(state.lock().unwrap().processed, vec![1]);

        // The queue is reusable afterwards.
        ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 2 }, None, 0)
            .unwrap();
        assert!(complete_one(&state, ok_responder));
        ctrl.pump_completions(&mut transport).unwrap();
        assert_eq!(state.lock().unwrap().processed, vec![1, 2]);
    }

    #[test]
    fn queue_full_parks_commands_in_fifo_order() {
        let (mut ctrl, state, mut transport) = make_ctrl();

        // Fill the whole ring; none delivered yet (below the kick threshold
        // and the host has not drained).
        for i in 0..(CTRL_QUEUE_SIZE as u32) {
            ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: i }, None, 0)
                .unwrap();
        }
        // The ring is full: these two park in the FIFO.
        ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 64 }, None, 0)
            .unwrap();
        ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 65 }, None, 0)
            .unwrap();

        // Drain the ring; each pump re-adds parked commands in order.
        for _ in 0..(CTRL_QUEUE_SIZE + 2) {
            assert!(complete_one(&state, ok_responder));
            ctrl.pump_completions(&mut transport).unwrap();
        }
        assert_eq!(
            state.lock().unwrap().processed,
            (0..=65).collect::<Vec<u32>>()
        );
    }

    /// Regression (single-vCPU guest freeze): with the ring and the parking
    /// FIFO both full and the host stalled, `enqueue` must fail in bounded
    /// time instead of spinning forever under the consumer's global lock. The
    /// predecessor of this fix wedged the whole guest here. After the failure
    /// the queue must recover without losing or reordering the parked
    /// commands as soon as the host drains.
    #[test]
    fn enqueue_returns_busy_when_every_slot_is_live_and_host_is_stalled() {
        let (mut ctrl, state, mut transport) = make_ctrl();
        let capacity = CTRL_QUEUE_SIZE as u32 + PENDING_FIFO_CAP as u32;

        // Saturate the ring, then the parking FIFO. The fake device never
        // completes anything, so no slot is reclaimed along the way.
        for seq in 0..capacity {
            ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq }, None, 0)
                .unwrap();
        }

        // One past total capacity with a stalled host: a bounded, typed
        // failure — never an infinite spin.
        let err = ctrl
            .enqueue(
                &mut transport,
                &TestCmd {
                    kind: 0,
                    seq: capacity,
                },
                None,
                0,
            )
            .unwrap_err();
        assert!(
            matches!(err, Error::QueueBusy),
            "expected QueueBusy, got {err:?}"
        );

        // The queue recovers: drain the host side until everything (including
        // the parked FIFO) has been processed, then submissions work again.
        for _ in 0..capacity {
            if !complete_one(&state, ok_responder) {
                break;
            }
            ctrl.pump_completions(&mut transport).unwrap();
        }
        assert_eq!(
            state.lock().unwrap().processed,
            (0..capacity).collect::<Vec<u32>>()
        );

        ctrl.enqueue(
            &mut transport,
            &TestCmd {
                kind: 0,
                seq: capacity,
            },
            None,
            0,
        )
        .unwrap();
        assert!(complete_one(&state, ok_responder));
        ctrl.pump_completions(&mut transport).unwrap();
        assert_eq!(
            state.lock().unwrap().processed,
            (0..=capacity).collect::<Vec<u32>>()
        );
    }

    /// Regression (stalled-host wedge): a bounded wait must fail in bounded
    /// time when the host never completes the fenced entry, and the queue
    /// must remain usable afterwards — a timeout is not a broken queue.
    #[test]
    fn wait_fence_times_out_when_the_host_never_completes() {
        let (mut ctrl, state, mut transport) = make_ctrl_with(ticking_clock);

        ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 1 }, None, 9)
            .unwrap();

        let err = ctrl.wait_fence(&mut transport, 9).unwrap_err();
        assert!(
            matches!(err, Error::TimedOut),
            "expected TimedOut, got {err:?}"
        );

        // Once the host resumes, the fence completes and the queue keeps
        // working: wait_fence's failure did not poison it.
        assert!(complete_one(&state, ok_responder));
        ctrl.pump_completions(&mut transport).unwrap();
        assert!(ctrl.fence_completed(9));

        ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 2 }, None, 0)
            .unwrap();
        assert!(complete_one(&state, ok_responder));
        ctrl.pump_completions(&mut transport).unwrap();
        assert_eq!(state.lock().unwrap().processed, vec![1, 2]);
    }

    /// A synchronous request whose host never answers must fail in bounded
    /// time and break the queue: the used-ring entry still references the
    /// caller's frame, so no later operation may trust the ring — the
    /// caller's failure path resets the device.
    #[test]
    fn sync_request_timeout_breaks_the_queue() {
        let (mut ctrl, _state, mut transport) = make_ctrl_with(ticking_clock);

        let req = [1u8; 16];
        let mut resp = [0u8; RESP_SIZE];
        let token = ctrl
            .add_sync(&mut transport, &[&req], &mut [&mut resp])
            .unwrap();

        let err = ctrl
            .wait_sync(token, &mut transport, &[&req], &mut [&mut resp])
            .unwrap_err();
        assert!(
            matches!(err, Error::QueueBroken),
            "expected QueueBroken, got {err:?}"
        );

        // Every further operation fails fast instead of trusting the ring.
        assert!(matches!(
            ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 1 }, None, 0),
            Err(Error::QueueBroken)
        ));
    }

    /// The queue-full retry of a synchronous add is bounded too: a host that
    /// never drains the ring breaks the queue instead of spinning forever
    /// under the caller's lock.
    #[test]
    fn sync_request_queue_full_timeout_breaks_the_queue() {
        let (mut ctrl, _state, mut transport) = make_ctrl_with(ticking_clock);
        let capacity = CTRL_QUEUE_SIZE as u32 + PENDING_FIFO_CAP as u32;

        // Saturate the ring and the parking FIFO with a stalled host.
        for seq in 0..capacity {
            ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq }, None, 0)
                .unwrap();
        }

        let req = [1u8; 16];
        let mut resp = [0u8; RESP_SIZE];
        let err = ctrl
            .add_sync(&mut transport, &[&req], &mut [&mut resp])
            .unwrap_err();
        assert!(
            matches!(err, Error::QueueBroken),
            "expected QueueBroken, got {err:?}"
        );
    }

    /// Regression (silent wedge / index panic): a used entry whose id is
    /// outside the descriptor table can never be consumed, so the queue must
    /// fail fast and loudly instead of wedging every later pump.
    #[test]
    fn foreign_used_entry_outside_the_table_breaks_the_queue() {
        let (mut ctrl, state, mut transport) = make_ctrl();

        // 200 is beyond the 64-entry pending table; the predecessor of this
        // fix indexed `pending` with it (panic) or wedged on it forever.
        inject_used_entry(&state, 200);

        let err = ctrl.pump_completions(&mut transport).unwrap_err();
        assert!(
            matches!(err, Error::QueueBroken),
            "expected QueueBroken, got {err:?}"
        );

        // Every further operation reports the same typed error.
        assert!(matches!(
            ctrl.pump_completions(&mut transport),
            Err(Error::QueueBroken)
        ));
        assert!(matches!(
            ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 1 }, None, 0),
            Err(Error::QueueBroken)
        ));
    }

    /// Same foreign-completion condition with an in-range id that carries no
    /// pending record and no in-flight blocking request.
    #[test]
    fn foreign_used_entry_inside_the_table_breaks_the_queue() {
        let (mut ctrl, state, mut transport) = make_ctrl();

        // Nothing is in flight, so no in-range id can be a legitimate
        // completion (a blocking request would own the token it waits on).
        inject_used_entry(&state, 5);

        let err = ctrl.pump_completions(&mut transport).unwrap_err();
        assert!(
            matches!(err, Error::QueueBroken),
            "expected QueueBroken, got {err:?}"
        );
    }

    /// A blocking request's own used entry must be left for its waiter, not
    /// treated as a foreign completion.
    #[test]
    fn pump_leaves_the_blocking_requests_entry_for_its_waiter() {
        let (mut ctrl, state, mut transport) = make_ctrl();

        let req = [7u8; 16];
        let mut resp = [0u8; RESP_SIZE];

        // A fire-and-forget command, then a blocking request whose entry sits
        // behind it on the used ring.
        ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 1 }, None, 0)
            .unwrap();
        let token = ctrl
            .add_sync(&mut transport, &[&req], &mut [&mut resp])
            .unwrap();
        // The fake device completes both entries in order.
        assert!(complete_one(&state, ok_responder));
        assert!(complete_one(&state, ok_responder));
        ctrl.pump_completions(&mut transport).unwrap();
        // The pump observed the async completion and stopped at the sync
        // entry without consuming it.
        assert_eq!(ctrl.queue.peek_used(), Some(token));

        // The waiter still pops its own entry.
        let used_len = ctrl
            .wait_sync(token, &mut transport, &[&req], &mut [&mut resp])
            .unwrap();
        assert_eq!(used_len, 16 + RSP);
    }

    /// D3 reset×async guard: after the device reset invalidates the queue,
    /// every further operation must fail fast (or no-op for `notify`) instead
    /// of programming the unregistered virtqueue. Without the broken gate an
    /// enqueue would reuse freed descriptor bookkeeping or write the notify
    /// register of a device whose queues were already unset.
    #[test]
    fn invalidated_queue_fails_every_operation_fast() {
        let (mut ctrl, state, mut transport) = make_ctrl();
        ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 1 }, None, 3)
            .unwrap();

        ctrl.invalidate();

        assert!(matches!(
            ctrl.enqueue(&mut transport, &TestCmd { kind: 0, seq: 2 }, None, 0),
            Err(Error::QueueBroken)
        ));
        assert!(matches!(
            ctrl.pump_completions(&mut transport),
            Err(Error::QueueBroken)
        ));
        assert!(matches!(
            ctrl.wait_fence(&mut transport, 3),
            Err(Error::QueueBroken)
        ));
        assert!(matches!(
            ctrl.wait_idle(&mut transport),
            Err(Error::QueueBroken)
        ));
        // notify is infallible: it must silently no-op, never kick the reset
        // device (the fake transport would still record the kick).
        ctrl.notify(&mut transport);
        assert!(!poll_queue_notified(&state));
        // The fence of the dropped command never completes: the high-water
        // mark is frozen and a poll reports "not done" rather than pretending.
        assert!(!ctrl.fence_completed(3));

        // Invalidate is idempotent and needs no transport access.
        ctrl.invalidate();
    }
}
