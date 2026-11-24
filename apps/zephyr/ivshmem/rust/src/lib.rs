#![no_std]

//! Private Rust implementation of the TGOSKits Zephyr ivshmem Message V1
//! device interface.
//!
//! Message encoding, incarnation handling, and slot ownership stay in
//! `axivc`. The C backend only supplies permanent PCI/MMU mappings, MSI-X
//! notification, raw epoch loads, and Zephyr synchronization primitives.

use core::{
    cell::UnsafeCell,
    ffi::{c_int, c_void},
    ptr, slice,
};

use axivc::{
    IvcMessageError, IvcMessageReceiver, IvcMessageSender, IvshmemMessagePort, IvshmemSessionState,
};

const EIO: c_int = 5;
const EAGAIN: c_int = 11;
const EBUSY: c_int = 16;
const ENODEV: c_int = 19;
const EINVAL: c_int = 22;
const ENOSPC: c_int = 28;
const EPIPE: c_int = 32;
const EOVERFLOW: c_int = 75;
const EMSGSIZE: c_int = 90;

const MAX_RECORD_BYTES: usize = 48;
const MESSAGE_CHANNEL_BYTES: usize = 0x2100;
const PAYLOAD_OFFSET: usize = 0x3000;
const PAGE_SIZE: usize = 0x1000;

#[repr(C)]
#[derive(Clone, Copy)]
struct PlatformView {
    local_output: usize,
    peer_output: usize,
    local_epoch: usize,
    peer_epoch: usize,
    output_size: usize,
}

impl PlatformView {
    const fn empty() -> Self {
        Self {
            local_output: 0,
            peer_output: 0,
            local_epoch: 0,
            peer_epoch: 0,
            output_size: 0,
        }
    }
}

unsafe extern "C" {
    fn tgos_ivc_backend_lock() -> c_int;
    fn tgos_ivc_backend_unlock();
    fn tgos_ivc_backend_prepare(view: *mut PlatformView) -> c_int;
    fn tgos_ivc_backend_enable_interrupts() -> c_int;
    fn tgos_ivc_backend_disable_interrupts();
    fn tgos_ivc_backend_reset_event();
    fn tgos_ivc_backend_take_events();
    fn tgos_ivc_backend_wait_event(timeout_ms: i32) -> c_int;
    fn tgos_ivc_backend_now_ms() -> i64;
    fn tgos_ivc_backend_epochs(local: *mut u64, peer: *mut u64) -> c_int;
    fn tgos_ivc_backend_ring_peer() -> c_int;
    fn tgos_ivc_backend_abort() -> !;
}

struct Session {
    view: PlatformView,
    port: Option<IvshmemMessagePort<'static>>,
    endpoints: Option<(IvcMessageSender<'static>, IvcMessageReceiver<'static>)>,
    previous_session: Option<IvshmemSessionState>,
    local_epoch: u64,
    peer_epoch: u64,
    lost: bool,
}

impl Session {
    fn check_epochs(&mut self) -> c_int {
        if self.lost {
            return -EPIPE;
        }

        let mut local = 0;
        let mut peer = 0;
        // SAFETY: both output pointers reference initialized locals, and the
        // backend only writes one u64 through each valid pointer.
        let result = unsafe { tgos_ivc_backend_epochs(&mut local, &mut peer) };
        if result < 0 {
            return self.lose();
        }
        if local != self.local_epoch || peer != self.peer_epoch {
            // Host-published epochs are terminal evidence for this handle:
            // `lost` remains set and every later operation returns EPIPE
            // until close() releases the session generation.
            return self.lose();
        }
        0
    }

    fn advance(&mut self) -> c_int {
        let result = self.check_epochs();
        if result != 0 {
            return result;
        }

        if self.endpoints.is_some() {
            let state = self.port.as_mut().map(|port| port.session_state());
            let Some(state) = state else {
                return self.lose();
            };
            return match state {
                Ok(IvshmemSessionState::Ready { .. }) => 0,
                _ => self.lose(),
            };
        }

        let next = self.port.as_mut().map(|port| port.poll_handshake());
        let Some(next) = next else {
            return self.lose();
        };

        match next {
            Ok(state) if peer_session_regressed(self.previous_session, state) => self.lose(),
            Ok(state @ IvshmemSessionState::Ready { .. }) => {
                self.previous_session = Some(state);
                let endpoints = self.port.as_mut().map(|port| port.try_connect());
                let Some(endpoints) = endpoints else {
                    return self.lose();
                };
                match endpoints {
                    Ok(endpoints) => self.endpoints = Some(endpoints.into_parts()),
                    Err(_) => return self.lose(),
                }
                if unsafe { tgos_ivc_backend_ring_peer() } < 0 {
                    return self.lose();
                }
                0
            }
            Ok(
                state @ (IvshmemSessionState::WaitingForPeer { .. }
                | IvshmemSessionState::WaitingForPeerAcknowledgement { .. }),
            ) => {
                let progressed = self.previous_session != Some(state);
                self.previous_session = Some(state);
                if progressed && unsafe { tgos_ivc_backend_ring_peer() } < 0 {
                    return self.lose();
                }
                -EAGAIN
            }
            Ok(_) | Err(_) => self.lose(),
        }
    }

    fn ensure_ready(&mut self, deadline: Option<i64>) -> c_int {
        loop {
            let result = self.advance();
            if result == 0 {
                return 0;
            }
            if result != -EAGAIN {
                return result;
            }

            let result = wait_for_event(deadline);
            if result != 0 {
                return result;
            }
        }
    }

    fn lose(&mut self) -> c_int {
        if self.lost {
            return -EPIPE;
        }
        self.lost = true;
        self.endpoints = None;
        if let Some(mut port) = self.port.take() {
            // SAFETY: all endpoints derived from this port were dropped just
            // above, and the public API mutex makes this the sole local
            // protocol access for this session generation.
            unsafe { port.invalidate_session() };
            let _ = unsafe { tgos_ivc_backend_ring_peer() };
        }
        -EPIPE
    }

    fn shutdown(&mut self) {
        self.lost = true;
        self.endpoints = None;
        if let Some(mut port) = self.port.take() {
            // SAFETY: endpoints are dropped above and the public API mutex
            // excludes every other local access to this output section.
            unsafe { port.invalidate_session() };
            let _ = unsafe { tgos_ivc_backend_ring_peer() };
        }
    }

    fn send(&mut self, input: &[u8], timeout_ms: i32) -> c_int {
        let deadline = deadline_from(timeout_ms);
        let result = self.ensure_ready(deadline);
        if result != 0 {
            return result;
        }
        unsafe { tgos_ivc_backend_take_events() };

        let mut started = false;
        loop {
            let result = self.advance();
            if result == -EAGAIN {
                let result = wait_for_event(deadline);
                if result != 0 {
                    abort_unsent(self);
                    return result;
                }
                continue;
            }
            if result != 0 {
                return result;
            }

            let step = {
                match self.endpoints.as_mut() {
                    None => SendStep::Failed,
                    Some((sender, _)) => {
                        if !started && sender.available_slots() == 0 {
                            SendStep::Wait
                        } else {
                            if !started {
                                if sender.start_message(input.len() as u64).is_err() {
                                    SendStep::Failed
                                } else {
                                    started = true;
                                    SendStep::Try
                                }
                            } else {
                                SendStep::Try
                            }
                        }
                    }
                }
            };

            match step {
                SendStep::Wait => {}
                SendStep::Failed => return self.lose(),
                SendStep::Try => {
                    let progress = {
                        match self.endpoints.as_mut() {
                            None => Err(-EIO),
                            Some((sender, _)) => match sender.try_write(input) {
                                Ok(progress) => Ok(progress),
                                Err(IvcMessageError::SlotFull) => Err(-EAGAIN),
                                Err(_) => Err(-EIO),
                            },
                        }
                    };
                    match progress {
                        Ok(progress)
                            if progress.is_complete() && progress.consumed() == input.len() =>
                        {
                            if unsafe { tgos_ivc_backend_ring_peer() } < 0 {
                                return self.lose();
                            }
                            return 0;
                        }
                        Ok(_) => {}
                        Err(error) if error == -EAGAIN => {}
                        Err(_) => return self.lose(),
                    }
                }
            }

            let result = wait_for_event(deadline);
            if result != 0 {
                abort_unsent(self);
                return result;
            }
        }
    }

    fn receive(&mut self, data: *mut u8, cap: usize, timeout_ms: i32) -> c_int {
        let deadline = deadline_from(timeout_ms);
        let result = self.ensure_ready(deadline);
        if result != 0 {
            return result;
        }
        unsafe { tgos_ivc_backend_take_events() };

        loop {
            let result = self.advance();
            if result == -EAGAIN {
                let result = wait_for_event(deadline);
                if result != 0 {
                    return result;
                }
                continue;
            }
            if result != 0 {
                return result;
            }

            let step = {
                match self.endpoints.as_mut() {
                    None => ReceiveStep::Failed,
                    Some((_, receiver)) => {
                        match receiver.complete_message_available() {
                            Ok(false) => ReceiveStep::Wait,
                            Ok(true) => match receiver.peek_message_meta() {
                                Ok(Some(meta)) => match usize::try_from(meta.len()) {
                                    Err(_) => ReceiveStep::Failed,
                                    Ok(length) if length > MAX_RECORD_BYTES => {
                                        ReceiveStep::Immediate(-EMSGSIZE)
                                    }
                                    Ok(length) if length > cap => ReceiveStep::Immediate(-ENOSPC),
                                    Ok(length) => {
                                        let output = if length == 0 {
                                            // SAFETY: a zero-length slice may
                                            // use the canonical dangling
                                            // pointer.
                                            unsafe {
                                                slice::from_raw_parts_mut(
                                                    ptr::NonNull::<u8>::dangling().as_ptr(),
                                                    0,
                                                )
                                            }
                                        } else {
                                            // SAFETY: the caller owns `cap`
                                            // accessible bytes at `data`, and
                                            // 0 < length <= cap.
                                            unsafe { slice::from_raw_parts_mut(data, length) }
                                        };
                                        match receiver.try_read(output) {
                                            Ok(progress)
                                                if progress.is_complete()
                                                    && progress.written() == length =>
                                            {
                                                ReceiveStep::Complete(length)
                                            }
                                            Err(IvcMessageError::BufferTooSmall { .. }) => {
                                                ReceiveStep::Immediate(-ENOSPC)
                                            }
                                            Ok(_) | Err(_) => ReceiveStep::Failed,
                                        }
                                    }
                                },
                                Ok(None) => ReceiveStep::Wait,
                                Err(_) => ReceiveStep::Failed,
                            },
                            Err(_) => ReceiveStep::Failed,
                        }
                    }
                }
            };

            match step {
                ReceiveStep::Complete(length) => {
                    if unsafe { tgos_ivc_backend_ring_peer() } < 0 {
                        return self.lose();
                    }
                    return length as c_int;
                }
                ReceiveStep::Immediate(error) => return error,
                ReceiveStep::Wait => {}
                ReceiveStep::Failed => return self.lose(),
            }

            let result = wait_for_event(deadline);
            if result != 0 {
                return result;
            }
        }
    }

    fn payload_capacity(&mut self) -> usize {
        if self.check_epochs() != 0 {
            return 0;
        }
        self.view.output_size.saturating_sub(PAYLOAD_OFFSET)
    }

    fn write_payload(&mut self, offset: usize, data: *const u8, len: usize) -> c_int {
        let result = self.ensure_ready(Some(unsafe { tgos_ivc_backend_now_ms() }));
        if result != 0 {
            return result;
        }

        let capacity = self.payload_capacity();
        let Some(end) = offset.checked_add(len) else {
            return -EOVERFLOW;
        };
        if end > capacity {
            return -ENOSPC;
        }
        if len != 0 && data.is_null() {
            return -EINVAL;
        }

        let Some(payload_start) = self.view.local_output.checked_add(PAYLOAD_OFFSET) else {
            return -EOVERFLOW;
        };
        let Some(destination) = payload_start.checked_add(offset) else {
            return -EOVERFLOW;
        };
        if len != 0 {
            // SAFETY: the backend exposed an output-sized writable mapping at
            // `local_output`. The checked application range ends inside the
            // payload subrange at `PAYLOAD_OFFSET`, so the destination is
            // within the mapping. `ptr::copy` permits an application buffer
            // that overlaps this destination.
            unsafe {
                ptr::copy(data, destination as *mut u8, len);
            }
        }

        self.check_epochs()
    }

    fn read_payload(&mut self, offset: usize, data: *mut u8, len: usize) -> c_int {
        let result = self.ensure_ready(Some(unsafe { tgos_ivc_backend_now_ms() }));
        if result != 0 {
            return result;
        }

        let capacity = self.payload_capacity();
        let Some(end) = offset.checked_add(len) else {
            return -EOVERFLOW;
        };
        if end > capacity {
            return -ENOSPC;
        }
        if len != 0 && data.is_null() {
            return -EINVAL;
        }

        let Some(payload_start) = self.view.peer_output.checked_add(PAYLOAD_OFFSET) else {
            return -EOVERFLOW;
        };
        let Some(source) = payload_start.checked_add(offset) else {
            return -EOVERFLOW;
        };
        if len != 0 {
            // SAFETY: `peer_output` starts a read-only mapping of `output_size`
            // bytes. The checked application range ends inside the payload
            // subrange at `PAYLOAD_OFFSET`, so the source is within that
            // mapping, and the caller owns `len` output bytes.
            unsafe {
                ptr::copy(source as *const u8, data, len);
            }
        }

        self.check_epochs()
    }
}

enum SendStep {
    Wait,
    Try,
    Failed,
}

enum ReceiveStep {
    Wait,
    Complete(usize),
    Immediate(c_int),
    Failed,
}

struct GlobalState(UnsafeCell<Option<Session>>);

// SAFETY: every access to the inner session occurs while the backend mutex is
// held. The backend mutex is the single serialization boundary for send,
// receive, close, payload access, and session replacement.
unsafe impl Sync for GlobalState {}

static STATE: GlobalState = GlobalState(UnsafeCell::new(None));

fn peer_session_regressed(
    previous: Option<IvshmemSessionState>,
    next: IvshmemSessionState,
) -> bool {
    let Some(IvshmemSessionState::WaitingForPeerAcknowledgement {
        peer: previous_peer,
        ..
    }) = previous
    else {
        return false;
    };

    match next {
        IvshmemSessionState::WaitingForPeer { .. } => true,
        IvshmemSessionState::WaitingForPeerAcknowledgement { peer, .. } => peer != previous_peer,
        _ => false,
    }
}

fn deadline_from(timeout_ms: i32) -> Option<i64> {
    if timeout_ms < 0 {
        None
    } else {
        let now = unsafe { tgos_ivc_backend_now_ms() };
        Some(now.saturating_add(i64::from(timeout_ms)))
    }
}

fn wait_for_event(deadline: Option<i64>) -> c_int {
    let timeout_ms = match deadline {
        None => -1,
        Some(deadline) => {
            let now = unsafe { tgos_ivc_backend_now_ms() };
            if now >= deadline {
                return -EAGAIN;
            }
            (deadline - now).min(i64::from(i32::MAX)) as i32
        }
    };

    if timeout_ms == 0 {
        return -EAGAIN;
    }

    let result = unsafe { tgos_ivc_backend_wait_event(timeout_ms) };
    if result == 0 {
        unsafe { tgos_ivc_backend_take_events() };
        0
    } else if result == -EAGAIN {
        -EAGAIN
    } else {
        -EIO
    }
}

fn abort_unsent(session: &mut Session) {
    let result = match session.endpoints.as_mut() {
        Some((sender, _)) => sender.try_abort(),
        None => return,
    };
    if matches!(
        result,
        Err(IvcMessageError::SessionLost) | Err(IvcMessageError::CursorOutOfRange { .. })
    ) {
        let _ = session.lose();
    }
}

fn validate_view(view: &PlatformView) -> c_int {
    if view.local_output == 0
        || view.peer_output == 0
        || view.local_epoch == 0
        || view.peer_epoch == 0
    {
        return -ENODEV;
    }
    if view.output_size < MESSAGE_CHANNEL_BYTES
        || !view.output_size.is_multiple_of(PAGE_SIZE)
        || !view.local_output.is_multiple_of(PAGE_SIZE)
        || !view.peer_output.is_multiple_of(PAGE_SIZE)
        || !view.local_epoch.is_multiple_of(8)
        || !view.peer_epoch.is_multiple_of(8)
    {
        return -EINVAL;
    }
    let output_distance = view.local_output.abs_diff(view.peer_output);
    if output_distance < view.output_size {
        return -EINVAL;
    }
    0
}

fn open_locked(slot: &mut Option<Session>) -> c_int {
    if slot.is_some() {
        return -EBUSY;
    }

    let mut view = PlatformView::empty();
    let result = unsafe { tgos_ivc_backend_prepare(&mut view) };
    if result != 0 {
        return result;
    }

    let result = validate_view(&view);
    if result != 0 {
        return result;
    }

    let mut local_epoch = 0;
    let mut peer_epoch = 0;
    let result = unsafe { tgos_ivc_backend_epochs(&mut local_epoch, &mut peer_epoch) };
    if result != 0 {
        return -EIO;
    }

    // SAFETY: the backend owns permanent page-aligned mappings of the local
    // and peer output sections. The public API mutex gives this session sole
    // local protocol ownership, and peer access is read-only. Both mappings
    // remain valid until the Zephyr device is reset.
    let mut port: IvshmemMessagePort<'static> = match unsafe {
        IvshmemMessagePort::attach(
            view.local_output as *mut u8,
            view.output_size,
            view.peer_output as *const u8,
            view.output_size,
        )
    } {
        Ok(port) => port,
        Err(_) => return -EIO,
    };

    // SAFETY: no endpoint from this output exists yet, and the public API
    // mutex excludes every other local session initializer.
    if unsafe { port.begin_session() }.is_err() {
        return -EIO;
    }

    let result = unsafe { tgos_ivc_backend_enable_interrupts() };
    if result != 0 {
        unsafe { port.invalidate_session() };
        let _ = unsafe { tgos_ivc_backend_ring_peer() };
        unsafe { tgos_ivc_backend_disable_interrupts() };
        return result;
    }

    unsafe { tgos_ivc_backend_reset_event() };
    if unsafe { tgos_ivc_backend_ring_peer() } < 0 {
        unsafe { port.invalidate_session() };
        unsafe { tgos_ivc_backend_disable_interrupts() };
        return -EIO;
    }

    *slot = Some(Session {
        view,
        port: Some(port),
        endpoints: None,
        previous_session: None,
        local_epoch,
        peer_epoch,
        lost: false,
    });
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn tgos_ivc_open() -> c_int {
    let result = unsafe { tgos_ivc_backend_lock() };
    if result != 0 {
        return result;
    }

    // SAFETY: the backend mutex is held, so this is the only reference to the
    // global session slot for the duration of the call.
    let result = unsafe { open_locked(&mut *STATE.0.get()) };
    unsafe { tgos_ivc_backend_unlock() };
    result
}

#[unsafe(no_mangle)]
pub extern "C" fn tgos_ivc_wait_ready(timeout_ms: i32) -> c_int {
    let result = unsafe { tgos_ivc_backend_lock() };
    if result != 0 {
        return result;
    }

    // SAFETY: the backend mutex is held, giving this call exclusive access to
    // the session state and its V1 handshake.
    let result = unsafe {
        match (&mut *STATE.0.get()).as_mut() {
            Some(session) => session.ensure_ready(deadline_from(timeout_ms)),
            None => -ENODEV,
        }
    };
    unsafe { tgos_ivc_backend_unlock() };
    result
}

#[unsafe(no_mangle)]
pub extern "C" fn tgos_ivc_close() {
    let result = unsafe { tgos_ivc_backend_lock() };
    if result != 0 {
        return;
    }

    // SAFETY: the backend mutex is held for the complete session teardown.
    let session = unsafe { (&mut *STATE.0.get()).take() };
    if let Some(mut session) = session {
        session.shutdown();
    }
    unsafe {
        tgos_ivc_backend_disable_interrupts();
        tgos_ivc_backend_reset_event();
        tgos_ivc_backend_unlock();
    }
}

/// Send one bounded Message V1 record.
///
/// # Safety
///
/// When `len` is nonzero, `data` must point to `len` readable bytes that stay
/// valid until this potentially blocking call returns. The caller must not
/// mutate those bytes concurrently with the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tgos_ivc_send(data: *const c_void, len: usize, timeout_ms: i32) -> c_int {
    if len > MAX_RECORD_BYTES {
        return -EMSGSIZE;
    }
    if len != 0 && data.is_null() {
        return -EINVAL;
    }

    let input = if len == 0 {
        // SAFETY: the canonical dangling pointer is valid for a zero-length
        // slice and is never dereferenced.
        unsafe { slice::from_raw_parts(ptr::NonNull::<u8>::dangling().as_ptr(), 0) }
    } else {
        // SAFETY: the caller provides `len <= 48` readable bytes at `data`.
        unsafe { slice::from_raw_parts(data.cast::<u8>(), len) }
    };

    let result = unsafe { tgos_ivc_backend_lock() };
    if result != 0 {
        return result;
    }

    // SAFETY: the backend mutex is held, giving this call exclusive access to
    // the session state and its V1 sender/receiver pair.
    let result = unsafe {
        match (&mut *STATE.0.get()).as_mut() {
            Some(session) => session.send(input, timeout_ms),
            None => -ENODEV,
        }
    };
    unsafe { tgos_ivc_backend_unlock() };
    result
}

/// Receive one bounded Message V1 record.
///
/// # Safety
///
/// When `cap` is nonzero, `data` must point to `cap` writable bytes that stay
/// valid until this potentially blocking call returns. The caller must not
/// access those bytes concurrently with the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tgos_ivc_recv(data: *mut c_void, cap: usize, timeout_ms: i32) -> c_int {
    if cap != 0 && data.is_null() {
        return -EINVAL;
    }

    let result = unsafe { tgos_ivc_backend_lock() };
    if result != 0 {
        return result;
    }

    // SAFETY: the backend mutex is held, giving this call exclusive access to
    // the session state and its V1 sender/receiver pair.
    let result = unsafe {
        match (&mut *STATE.0.get()).as_mut() {
            Some(session) => session.receive(data.cast::<u8>(), cap, timeout_ms),
            None => -ENODEV,
        }
    };
    unsafe { tgos_ivc_backend_unlock() };
    result
}

#[unsafe(no_mangle)]
pub extern "C" fn tgos_ivc_payload_capacity() -> usize {
    let result = unsafe { tgos_ivc_backend_lock() };
    if result != 0 {
        return 0;
    }

    // SAFETY: the backend mutex is held for the complete state access.
    let capacity = unsafe {
        match (&mut *STATE.0.get()).as_mut() {
            Some(session) => session.payload_capacity(),
            None => 0,
        }
    };
    unsafe { tgos_ivc_backend_unlock() };
    capacity
}

/// Copy bytes into the local owner-only payload window.
///
/// # Safety
///
/// When `len` is nonzero, `data` must point to `len` readable bytes for the
/// duration of the call. The caller must not mutate those bytes concurrently.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tgos_ivc_write_payload(
    offset: usize,
    data: *const c_void,
    len: usize,
) -> c_int {
    if len != 0 && data.is_null() {
        return -EINVAL;
    }

    let result = unsafe { tgos_ivc_backend_lock() };
    if result != 0 {
        return result;
    }

    // SAFETY: the backend mutex is held for the complete state and mapping
    // access.
    let result = unsafe {
        match (&mut *STATE.0.get()).as_mut() {
            Some(session) => session.write_payload(offset, data.cast::<u8>(), len),
            None => -ENODEV,
        }
    };
    unsafe { tgos_ivc_backend_unlock() };
    result
}

/// Copy bytes from the peer-owned read-only payload window.
///
/// # Safety
///
/// When `len` is nonzero, `data` must point to `len` writable bytes for the
/// duration of the call. The caller must not access those bytes concurrently.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tgos_ivc_read_payload(
    offset: usize,
    data: *mut c_void,
    len: usize,
) -> c_int {
    if len != 0 && data.is_null() {
        return -EINVAL;
    }

    let result = unsafe { tgos_ivc_backend_lock() };
    if result != 0 {
        return result;
    }

    // SAFETY: the backend mutex is held for the complete state and mapping
    // access.
    let result = unsafe {
        match (&mut *STATE.0.get()).as_mut() {
            Some(session) => session.read_payload(offset, data.cast::<u8>(), len),
            None => -ENODEV,
        }
    };
    unsafe { tgos_ivc_backend_unlock() };
    result
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    unsafe { tgos_ivc_backend_abort() }
}
