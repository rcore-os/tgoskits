use std::{
    cmp,
    ffi::{c_int, c_void},
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
    slice,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use arceos_ivshmem_pci::MessageRecordEndpoint;
use axivc::{IVC_SLOT_FRAGMENT_CAPACITY, IvcMessageError};

use crate::{
    ivshmem::{AdapterError, ExclusiveDevice, WriterLockBusy},
    record::{LinuxMessageEndpoint, PendingEndpoint, PendingReady, SessionLost},
};

const MAX_RECORD_BYTES: usize = 48;
const MAX_TIMEOUT_MS: i32 = 60_000;
const EVENT_WAIT: Duration = Duration::from_millis(25);
const _: () = assert!(MAX_RECORD_BYTES <= IVC_SLOT_FRAGMENT_CAPACITY);

/// One process-local Linux ivshmem Message V1 connection.
///
/// The C header exposes this type as `struct ivc`. A handle owns the device
/// mappings until the endpoint or pending session above them has been dropped
/// and its Message V1 incarnation has been invalidated.
pub struct LinuxIvc {
    session: Option<Session>,
    lost: bool,
}

enum Session {
    Pending(OwnedPending),
    Connected(OwnedConnected),
}

struct OwnedPending {
    pending: PendingEndpoint<'static>,
    _device: StableDevice,
}

struct OwnedConnected {
    endpoint: LinuxMessageEndpoint<'static>,
    _device: StableDevice,
}

enum OwnedReady {
    Waiting(OwnedPending),
    Connected(OwnedConnected),
}

/// Keeps an `ExclusiveDevice` allocation at a stable address while endpoints
/// with a `'static` phantom borrow use its non-moving BAR mappings.
struct StableDevice(NonNull<ExclusiveDevice>);

impl StableDevice {
    fn new(device: ExclusiveDevice) -> Self {
        // Box::leak transfers the sole owning allocation to this StableDevice;
        // NonNull::from stores its address without retaining a reference.
        Self(NonNull::from(Box::leak(Box::new(device))))
    }

    fn take_endpoint(&mut self) -> Result<PendingEndpoint<'static>> {
        // SAFETY: StableDevice is the sole owner of this Box allocation and
        // never moves the pointee. This temporary mutable reference ends after
        // take_endpoint returns; the derived endpoint retains only a phantom
        // lifetime over the stable mappings, not the reference itself.
        let device: &'static mut ExclusiveDevice = unsafe { &mut *self.0.as_ptr() };
        device.take_endpoint()
    }
}

impl Drop for StableDevice {
    fn drop(&mut self) {
        // SAFETY: new() leaked exactly one Box allocation into this sole
        // owning pointer, which is reclaimed here exactly once. The enclosing
        // session declares its derived endpoint before this owner, so that
        // endpoint is dropped before the parent mappings.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

impl OwnedPending {
    fn open() -> Result<Self> {
        let mut device = StableDevice::new(ExclusiveDevice::open()?);
        let pending = device.take_endpoint()?;
        Ok(Self {
            pending,
            _device: device,
        })
    }

    fn poll_ready(self) -> Result<OwnedReady> {
        let Self {
            pending,
            _device: device,
        } = self;
        match pending.poll_ready()? {
            PendingReady::Waiting(pending) => Ok(OwnedReady::Waiting(Self {
                pending,
                _device: device,
            })),
            PendingReady::Connected(endpoint) => Ok(OwnedReady::Connected(OwnedConnected {
                endpoint,
                _device: device,
            })),
        }
    }
}

impl LinuxIvc {
    fn open() -> Result<Self> {
        Ok(Self {
            session: Some(Session::Pending(OwnedPending::open()?)),
            lost: false,
        })
    }

    fn ensure_ready(&mut self, deadline: OperationDeadline) -> c_int {
        if self.lost {
            return -libc::EPIPE;
        }
        loop {
            let Some(session) = self.session.take() else {
                return -libc::ENODEV;
            };
            match session {
                Session::Pending(pending) => match pending.poll_ready() {
                    Ok(OwnedReady::Connected(endpoint)) => {
                        self.session = Some(Session::Connected(endpoint));
                        return 0;
                    }
                    Ok(OwnedReady::Waiting(mut pending)) => {
                        let remaining = deadline.remaining();
                        if remaining.is_zero() {
                            self.session = Some(Session::Pending(pending));
                            return -libc::EAGAIN;
                        }
                        let waited = pending
                            .pending
                            .wait_for_progress(cmp::min(remaining, EVENT_WAIT));
                        if let Err(error) = waited {
                            return self.fail(&error);
                        }
                        self.session = Some(Session::Pending(pending));
                    }
                    Err(error) => return self.fail(&error),
                },
                Session::Connected(mut endpoint) => {
                    let result = endpoint.endpoint.session_is_current();
                    match result {
                        Ok(true) => {
                            self.session = Some(Session::Connected(endpoint));
                            return 0;
                        }
                        Ok(false) => return self.lose(),
                        Err(error) => return self.fail(&error),
                    }
                }
            }
        }
    }

    fn send(&mut self, input: &[u8], deadline: OperationDeadline) -> c_int {
        let ready = self.ensure_ready(deadline);
        if ready != 0 {
            return ready;
        }
        let outcome = match self.session.as_mut() {
            Some(Session::Connected(connected)) => {
                send_record(&mut connected.endpoint, input, deadline)
            }
            _ => return -libc::ENODEV,
        };
        match outcome {
            SendOutcome::Complete => 0,
            SendOutcome::Error(error) => error,
            SendOutcome::Fatal(error) => self.fail(&error),
        }
    }

    fn recv(&mut self, output: &mut [u8], deadline: OperationDeadline) -> c_int {
        let ready = self.ensure_ready(deadline);
        if ready != 0 {
            return ready;
        }
        let outcome = match self.session.as_mut() {
            Some(Session::Connected(connected)) => {
                receive_record(&mut connected.endpoint, output, deadline)
            }
            _ => return -libc::ENODEV,
        };
        match outcome {
            ReceiveOutcome::Length(length) => length,
            ReceiveOutcome::Error(error) => error,
            ReceiveOutcome::Fatal(error) => self.fail(&error),
        }
    }

    fn lose(&mut self) -> c_int {
        self.session = None;
        self.lost = true;
        -libc::EPIPE
    }

    fn fail(&mut self, error: &anyhow::Error) -> c_int {
        let error_no = operation_errno(error);
        self.session = None;
        self.lost = true;
        error_no
    }
}

enum SendOutcome {
    Complete,
    Error(c_int),
    Fatal(anyhow::Error),
}

enum ReceiveOutcome {
    Length(c_int),
    Error(c_int),
    Fatal(anyhow::Error),
}

fn send_record(
    endpoint: &mut LinuxMessageEndpoint<'_>,
    input: &[u8],
    deadline: OperationDeadline,
) -> SendOutcome {
    if let Err(error) = endpoint
        .start_record(input.len() as u64)
        .context("failed to start a Linux IVC message")
    {
        return SendOutcome::Fatal(error);
    }
    let mut consumed = 0;
    loop {
        let progress = match endpoint.write_record(&input[consumed..]) {
            Ok(progress) => progress,
            Err(error) => return SendOutcome::Fatal(error),
        };
        consumed = match consumed.checked_add(progress.transferred()) {
            Some(consumed) if consumed <= input.len() => consumed,
            _ => {
                return SendOutcome::Fatal(anyhow!(
                    "Linux IVC sender reported invalid progress: consumed={consumed} \
                     transferred={} total={}",
                    progress.transferred(),
                    input.len()
                ));
            }
        };
        if progress.is_complete() {
            return if consumed == input.len() {
                SendOutcome::Complete
            } else {
                SendOutcome::Fatal(anyhow!(
                    "Linux IVC sender completed {consumed} of {} bytes",
                    input.len()
                ))
            };
        }

        let remaining = deadline.remaining();
        if remaining.is_zero() {
            return match endpoint
                .abort_record()
                .context("failed to abort a timed-out Linux IVC message")
            {
                Ok(()) => SendOutcome::Error(-libc::EAGAIN),
                Err(error) => SendOutcome::Fatal(error),
            };
        }
        if let Err(error) = endpoint.wait_for_progress_timeout(cmp::min(remaining, EVENT_WAIT)) {
            return SendOutcome::Fatal(error);
        }
    }
}

fn receive_record(
    endpoint: &mut LinuxMessageEndpoint<'_>,
    output: &mut [u8],
    deadline: OperationDeadline,
) -> ReceiveOutcome {
    loop {
        let record_len = match endpoint.peek_record_len() {
            Ok(Some(record_len)) => Some(record_len),
            Ok(None) => None,
            Err(error) => return ReceiveOutcome::Fatal(error),
        };
        if let Some(record_len) = record_len {
            let record_len = match usize::try_from(record_len) {
                Ok(record_len) if (1..=MAX_RECORD_BYTES).contains(&record_len) => record_len,
                // Keep the record at the ring head so the caller can diagnose
                // or close the session without silently consuming it.
                _ => return ReceiveOutcome::Error(-libc::EMSGSIZE),
            };
            if record_len > output.len() {
                return ReceiveOutcome::Error(-libc::ENOSPC);
            }
            match endpoint.complete_record_available() {
                Ok(true) => {}
                Ok(false) => {
                    let remaining = deadline.remaining();
                    if remaining.is_zero() {
                        return ReceiveOutcome::Error(-libc::EAGAIN);
                    }
                    if let Err(error) =
                        endpoint.wait_for_progress_timeout(cmp::min(remaining, EVENT_WAIT))
                    {
                        return ReceiveOutcome::Fatal(error);
                    }
                    continue;
                }
                Err(error) => return ReceiveOutcome::Fatal(error),
            }
            let progress = match endpoint.read_record(&mut output[..record_len]) {
                Ok(progress) => progress,
                Err(error) => return ReceiveOutcome::Fatal(error),
            };
            return if progress.is_complete() && progress.transferred() == record_len {
                ReceiveOutcome::Length(record_len as c_int)
            } else {
                ReceiveOutcome::Fatal(anyhow!(
                    "Linux IVC receiver completed={} transferred={} expected={record_len}",
                    progress.is_complete(),
                    progress.transferred()
                ))
            };
        }

        let remaining = deadline.remaining();
        if remaining.is_zero() {
            return ReceiveOutcome::Error(-libc::EAGAIN);
        }
        if let Err(error) = endpoint.wait_for_progress_timeout(cmp::min(remaining, EVENT_WAIT)) {
            return ReceiveOutcome::Fatal(error);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct OperationDeadline {
    end: Instant,
}

impl OperationDeadline {
    fn new(timeout_ms: i32) -> Result<Self, c_int> {
        if !(0..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
            return Err(-libc::EINVAL);
        }
        Ok(Self {
            end: Instant::now() + Duration::from_millis(timeout_ms as u64),
        })
    }

    fn remaining(self) -> Duration {
        self.end.saturating_duration_since(Instant::now())
    }
}

fn record_input_len(bytes: *const c_void, len: usize) -> Result<usize, c_int> {
    if len == 0 {
        return Err(-libc::EINVAL);
    }
    if len > MAX_RECORD_BYTES {
        return Err(-libc::EMSGSIZE);
    }
    if bytes.is_null() {
        return Err(-libc::EINVAL);
    }
    Ok(len)
}

fn receive_buffer_len(buffer: *mut c_void, cap: usize) -> Result<usize, c_int> {
    if cap == 0 || buffer.is_null() {
        return Err(-libc::EINVAL);
    }
    Ok(cap.min(MAX_RECORD_BYTES))
}

/// Opens the provisioned native ivshmem-pci endpoint.
///
/// # Safety
///
/// `out` must point to writable storage for one handle pointer. The caller
/// must eventually pass a successful handle to exactly one `ivc_close`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ivc_open(out: *mut *mut LinuxIvc) -> c_int {
    if out.is_null() {
        return -libc::EINVAL;
    }
    // SAFETY: the caller supplied one writable pointer slot.
    unsafe { out.write(ptr::null_mut()) };

    match catch_unwind(AssertUnwindSafe(LinuxIvc::open)) {
        Ok(Ok(handle)) => {
            // SAFETY: the caller supplied one writable pointer slot.
            unsafe { out.write(Box::into_raw(Box::new(handle))) };
            0
        }
        Ok(Err(error)) => open_errno(&error),
        Err(_) => -libc::EIO,
    }
}

/// Sends one complete Message V1 record.
///
/// # Safety
///
/// `handle` must be a live handle from `ivc_open`. `bytes` must reference
/// `len` readable bytes that remain valid and immutable for the duration of
/// this potentially blocking call. Calls on one handle must be serialized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ivc_send(
    handle: *mut LinuxIvc,
    bytes: *const c_void,
    len: usize,
    timeout_ms: c_int,
) -> c_int {
    let deadline = match OperationDeadline::new(timeout_ms) {
        Ok(deadline) => deadline,
        Err(error) => return error,
    };
    let input_len = match record_input_len(bytes, len) {
        Ok(input_len) => input_len,
        Err(error) => return error,
    };
    if handle.is_null() {
        return -libc::EINVAL;
    }
    // SAFETY: the caller supplies `input_len` readable bytes and keeps them
    // immutable until this call returns. `record_input_len` bounds it to 48.
    let input = unsafe { slice::from_raw_parts(bytes.cast::<u8>(), input_len) };

    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the caller guarantees a live, exclusively used handle for
        // this serialized call.
        unsafe { (&mut *handle).send(input, deadline) }
    })) {
        Ok(result) => result,
        Err(_) => {
            // SAFETY: the caller supplied a live handle. Its partially updated
            // session cannot be reused after an internal panic, so poison it
            // before returning the one-time internal error.
            unsafe { (&mut *handle).lose() };
            -libc::EIO
        }
    }
}

/// Receives one complete Message V1 record.
///
/// # Safety
///
/// `handle` must be a live handle from `ivc_open`. `buffer` must reference
/// `min(cap, 48)` writable bytes that remain valid until this potentially
/// blocking call returns. Calls on one handle must be serialized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ivc_recv(
    handle: *mut LinuxIvc,
    buffer: *mut c_void,
    cap: usize,
    timeout_ms: c_int,
) -> c_int {
    let deadline = match OperationDeadline::new(timeout_ms) {
        Ok(deadline) => deadline,
        Err(error) => return error,
    };
    let output_len = match receive_buffer_len(buffer, cap) {
        Ok(output_len) => output_len,
        Err(error) => return error,
    };
    if handle.is_null() {
        return -libc::EINVAL;
    }
    // SAFETY: the caller keeps at least `min(cap, 48)` writable bytes valid and
    // exclusively borrowed until this call returns.
    let output = unsafe { slice::from_raw_parts_mut(buffer.cast::<u8>(), output_len) };

    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the caller guarantees a live, exclusively used handle for
        // this serialized call.
        unsafe { (&mut *handle).recv(output, deadline) }
    })) {
        Ok(result) => result,
        Err(_) => {
            // SAFETY: the caller supplied a live handle. Its partially updated
            // session cannot be reused after an internal panic, so poison it
            // before returning the one-time internal error.
            unsafe { (&mut *handle).lose() };
            -libc::EIO
        }
    }
}

/// Drops one endpoint before its device mappings and invalidates its session.
///
/// # Safety
///
/// `handle` must be a live handle from `ivc_open`, and no other call may
/// be using it. The handle is invalid when this function returns, including
/// after an error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ivc_close(handle: *mut LinuxIvc) -> c_int {
    if handle.is_null() {
        return -libc::EINVAL;
    }

    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the caller transfers the single raw ownership created by
        // ivc_open and guarantees no concurrent use.
        drop(unsafe { Box::from_raw(handle) });
    })) {
        Ok(()) => 0,
        Err(_) => -libc::EIO,
    }
}

fn open_errno(error: &anyhow::Error) -> c_int {
    if error.downcast_ref::<WriterLockBusy>().is_some() {
        return -libc::EBUSY;
    }
    if let Some(error) = error.downcast_ref::<io::Error>() {
        return match error.raw_os_error() {
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => -libc::EBUSY,
            Some(code) if code > 0 => -code,
            _ => -libc::EIO,
        };
    }
    if let Some(error) = error.downcast_ref::<AdapterError>() {
        return match error.code() {
            -1 | -2 | -3 | -6 => -libc::ENODEV,
            -7 => -libc::ETIMEDOUT,
            -8 => -libc::ENOMEM,
            -9 => -libc::EINVAL,
            _ => -libc::EIO,
        };
    }
    -libc::EIO
}

fn operation_errno(error: &anyhow::Error) -> c_int {
    let session_lost = error.chain().any(|cause| {
        cause.downcast_ref::<SessionLost>().is_some()
            || cause
                .downcast_ref::<IvcMessageError>()
                .is_some_and(|error| *error == IvcMessageError::SessionLost)
    });
    if session_lost {
        -libc::EPIPE
    } else {
        -libc::EIO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_length_policy_accepts_only_nonempty_records_within_limit() {
        let bytes = NonNull::<u8>::dangling().as_ptr().cast();

        assert_eq!(record_input_len(bytes, 0), Err(-libc::EINVAL));
        assert_eq!(record_input_len(bytes, 1), Ok(1));
        assert_eq!(
            record_input_len(bytes, MAX_RECORD_BYTES),
            Ok(MAX_RECORD_BYTES)
        );
        assert_eq!(
            record_input_len(bytes, MAX_RECORD_BYTES + 1),
            Err(-libc::EMSGSIZE)
        );
        assert_eq!(record_input_len(ptr::null(), 1), Err(-libc::EINVAL));
    }

    #[test]
    fn receive_capacity_is_nonzero_and_capped_before_slice_creation() {
        let buffer = NonNull::<u8>::dangling().as_ptr().cast();

        assert_eq!(receive_buffer_len(buffer, 0), Err(-libc::EINVAL));
        assert_eq!(receive_buffer_len(ptr::null_mut(), 1), Err(-libc::EINVAL));
        assert_eq!(receive_buffer_len(buffer, 1), Ok(1));
        assert_eq!(
            receive_buffer_len(buffer, MAX_RECORD_BYTES + 1),
            Ok(MAX_RECORD_BYTES)
        );
        assert_eq!(receive_buffer_len(buffer, usize::MAX), Ok(MAX_RECORD_BYTES));
    }

    #[test]
    fn timeout_policy_accepts_zero_through_sixty_seconds() {
        assert_eq!(OperationDeadline::new(-1).unwrap_err(), -libc::EINVAL);
        assert!(OperationDeadline::new(0).is_ok());
        assert!(OperationDeadline::new(MAX_TIMEOUT_MS).is_ok());
        assert_eq!(
            OperationDeadline::new(MAX_TIMEOUT_MS + 1).unwrap_err(),
            -libc::EINVAL
        );
    }

    #[test]
    fn lost_handle_returns_epipe_for_every_operation() {
        let deadline = OperationDeadline::new(0).unwrap();
        let mut handle = LinuxIvc {
            session: None,
            lost: true,
        };
        let mut output = [0; 1];

        assert_eq!(handle.ensure_ready(deadline), -libc::EPIPE);
        assert_eq!(handle.send(b"x", deadline), -libc::EPIPE);
        assert_eq!(handle.recv(&mut output, deadline), -libc::EPIPE);
        assert_eq!(handle.send(b"x", deadline), -libc::EPIPE);
    }

    #[test]
    fn only_confirmed_session_loss_maps_to_epipe() {
        assert_eq!(
            operation_errno(&crate::record::session_lost("session reset")),
            -libc::EPIPE
        );
        assert_eq!(
            operation_errno(&anyhow::Error::msg("device failure")),
            -libc::EIO
        );
    }
}
