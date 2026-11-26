use core::{cell::Cell, cmp, mem, num::NonZeroU32};

use ax_std::{
    thread,
    time::{Duration, Instant},
};
use axivc::{
    IVC_RING_CAPACITY, IvcMessageError, IvcMessageReceiver, IvcMessageSender, IvshmemMessageError,
    IvshmemMessagePort, IvshmemSessionState,
};
use thiserror::Error;

use crate::{
    EVENT_VECTOR_0, IvshmemDevice, IvshmemError, IvshmemLayout,
    record::{MessageRecordEndpoint, RecordProgress},
};

const EXPECTED_MAX_PEERS: u32 = 2;
const PUBLISHER_PEER_ID: u32 = 0;
const SUBSCRIBER_PEER_ID: u32 = 1;
const DOORBELL_VECTOR: u16 = 0;
const HANDSHAKE_POLL_DELAY: Duration = Duration::from_millis(1);
const RECORD_POLL_DELAY: Duration = Duration::from_millis(1);

/// Maximum payload carried by one unified IVC message.
pub const IVC_MAX_RECORD_BYTES: usize = 48;
/// Maximum accepted timeout for a unified IVC operation.
pub const IVC_MAX_TIMEOUT_MS: i32 = 60_000;

const _: () = assert!(IVC_MAX_RECORD_BYTES <= axivc::IVC_SLOT_FRAGMENT_CAPACITY);

/// Logical role associated with one peer id.
///
/// The unified message API derives this value from discovery. The explicit
/// constructor remains available for the controlled publisher/subscriber
/// fragment regression.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IvcPeerRole {
    /// Request producer at peer slot zero.
    Publisher,
    /// Request consumer and response producer at peer slot one.
    Subscriber,
}

impl IvcPeerRole {
    const fn peer_id(self) -> u32 {
        match self {
            Self::Publisher => PUBLISHER_PEER_ID,
            Self::Subscriber => SUBSCRIBER_PEER_ID,
        }
    }

    const fn remote_peer_id(self) -> u32 {
        match self {
            Self::Publisher => SUBSCRIBER_PEER_ID,
            Self::Subscriber => PUBLISHER_PEER_ID,
        }
    }

    /// Stable role name used in runtime diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Publisher => "publisher",
            Self::Subscriber => "subscriber",
        }
    }

    const fn from_peer_id(peer_id: u32) -> Self {
        if peer_id == SUBSCRIBER_PEER_ID {
            Self::Subscriber
        } else {
            Self::Publisher
        }
    }
}

/// Ownership state of the single Message V1 session exposed by one device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionState {
    Unused,
    Active,
    Published,
    Closed,
}

/// Releases a device's single-session slot without invalidating shared state.
///
/// A published session becomes permanently closed when its owner drops. The
/// shared channel remains readable so the peer can consume the last published
/// ACK; a later device generation is detected through the host reset epoch.
struct SessionLease<'a> {
    state: &'a Cell<SessionState>,
}

impl<'a> SessionLease<'a> {
    fn acquire(state: &'a Cell<SessionState>) -> Result<Self, IvcDeviceError> {
        match state.get() {
            SessionState::Unused => {
                state.set(SessionState::Active);
                Ok(Self { state })
            }
            SessionState::Active | SessionState::Published => {
                Err(IvcDeviceError::SessionAlreadyOpen)
            }
            SessionState::Closed => Err(IvcDeviceError::SessionClosed),
        }
    }

    fn mark_published(&self) {
        self.state.set(SessionState::Published);
    }

    fn mark_reusable(&self) {
        self.state.set(SessionState::Unused);
    }
}

impl Drop for SessionLease<'_> {
    fn drop(&mut self) {
        self.state.set(match self.state.get() {
            SessionState::Unused => SessionState::Unused,
            SessionState::Active => SessionState::Unused,
            SessionState::Published | SessionState::Closed => SessionState::Closed,
        });
    }
}

/// Message V1 port plus the device-session ownership lease.
struct IvcPortSession<'a> {
    port: IvshmemMessagePort<'a>,
    _lease: SessionLease<'a>,
}

impl<'a> IvcPortSession<'a> {
    const fn new(port: IvshmemMessagePort<'a>, lease: SessionLease<'a>) -> Self {
        Self {
            port,
            _lease: lease,
        }
    }

    fn port(&self) -> &IvshmemMessagePort<'_> {
        &self.port
    }

    fn port_mut(&mut self) -> &mut IvshmemMessagePort<'a> {
        &mut self.port
    }
}

/// Observes the host-owned peer epoch while tolerating a peer that has not
/// attached yet.
#[derive(Clone, Copy, Default)]
struct PeerEpoch {
    baseline: Option<u64>,
}

impl PeerEpoch {
    fn observe(&mut self, device: &IvcDevice) -> Result<(), IvcDeviceError> {
        let observed = device.peer_epoch()?;
        // The hardware contract reserves zero for an epoch that has not been
        // published yet, so a late peer may attach after this endpoint. Keep
        // waiting for the first stable nonzero value instead of treating zero
        // as a generation change.
        if observed == 0 {
            return Ok(());
        }
        match self.baseline {
            Some(expected) if observed != expected => {
                Err(IvcDeviceError::PeerEpochChanged { expected, observed })
            }
            Some(_) => Ok(()),
            None => {
                self.baseline = Some(observed);
                Ok(())
            }
        }
    }

    fn is_current(&self, device: &IvcDevice) -> Result<bool, IvcDeviceError> {
        let observed = device.peer_epoch()?;
        Ok(match self.baseline {
            Some(expected) => observed == 0 || observed == expected,
            None => true,
        })
    }
}

/// Configured ArceOS guest device for one IVC peer.
pub struct IvcDevice {
    device: IvshmemDevice,
    role: IvcPeerRole,
    session_state: Cell<SessionState>,
}

impl IvcDevice {
    /// Discovers the configured two-peer ivshmem endpoint and derives the
    /// logical role from the hardware-assigned peer id.
    pub fn discover() -> Result<Self, IvcDeviceError> {
        let device = Self::discover_two_peer()?;
        let role = IvcPeerRole::from_peer_id(device.peer_id());
        Ok(Self {
            device,
            role,
            session_state: Cell::new(SessionState::Unused),
        })
    }

    /// Discovers the configured two-peer ivshmem endpoint for `role`.
    ///
    /// The adapter validates the hardware peer assignment, so applications do
    /// not select or interpret raw peer slots.
    pub fn open(role: IvcPeerRole) -> Result<Self, IvcDeviceError> {
        let device = Self::discover_two_peer()?;
        if device.peer_id() != role.peer_id() {
            return Err(IvcDeviceError::RoleMismatch {
                role,
                expected: role.peer_id(),
                actual: device.peer_id(),
            });
        }
        Ok(Self {
            device,
            role,
            session_state: Cell::new(SessionState::Unused),
        })
    }

    fn discover_two_peer() -> Result<IvshmemDevice, IvcDeviceError> {
        let device = IvshmemDevice::discover(IvshmemLayout::two_peer())?;
        if device.max_peers() != EXPECTED_MAX_PEERS {
            return Err(IvcDeviceError::UnexpectedPeerCount {
                expected: EXPECTED_MAX_PEERS,
                actual: device.max_peers(),
            });
        }
        Ok(device)
    }

    /// Returns the configured logical device profile.
    pub const fn info(&self) -> IvcDeviceInfo {
        IvcDeviceInfo {
            role: self.role,
            peer_id: self.device.peer_id(),
            max_peers: self.device.max_peers(),
        }
    }

    /// Creates the device's single Message V1 endpoint and publishes its
    /// incarnation.
    pub fn open_endpoint(&mut self) -> Result<IvcPendingEndpoint<'_>, IvcDeviceError> {
        let lease = SessionLease::acquire(&self.session_state)?;

        let local_output = self.device.own_output()?;
        let peer_output = self.device.output(self.role.remote_peer_id())?;
        // SAFETY: `IvcDevice` owns the BAR2 mapping and exposes exactly one
        // protocol session. The local range is writable and exclusively owned
        // by this guest; the opposite peer section is consumed read-only. Both
        // returned sections stay alive for `'a`, and no endpoint from an older
        // execution can survive a guest reset.
        let mut port = unsafe {
            IvshmemMessagePort::attach(
                local_output.as_ptr(),
                local_output.len(),
                peer_output.as_const_ptr(),
                peer_output.len(),
            )
        }?;
        // SAFETY: the attach contract above established exclusive local output
        // ownership and no endpoint exists for this device session yet.
        let local_incarnation = unsafe { port.begin_session() }?;
        lease.mark_published();
        Ok(IvcPendingEndpoint {
            device: self,
            session: Some(IvcPortSession::new(port, lease)),
            local_incarnation,
            observed_event: false,
            peer_epoch: PeerEpoch::default(),
            previous_state: None,
        })
    }

    fn ring_peer(&self) -> Result<(), IvshmemError> {
        self.device
            .ring_doorbell(self.role.remote_peer_id(), DOORBELL_VECTOR)
    }

    fn take_events(&self, mask: u32) -> Result<u32, IvshmemError> {
        self.device.take_events(mask)
    }

    fn peer_epoch(&self) -> Result<u64, IvshmemError> {
        self.device.peer_epoch(self.role.remote_peer_id())
    }
}

/// Device identity visible to the application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcDeviceInfo {
    role: IvcPeerRole,
    peer_id: u32,
    max_peers: u32,
}

impl IvcDeviceInfo {
    /// Configured asymmetric role.
    pub const fn role(self) -> IvcPeerRole {
        self.role
    }

    /// Provisioned peer slot selected by the adapter.
    pub const fn peer_id(self) -> u32 {
        self.peer_id
    }

    /// Number of peers in the configured profile.
    pub const fn max_peers(self) -> u32 {
        self.max_peers
    }
}

/// Endpoint metadata while the incarnation handshake is in progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcPendingInfo {
    device: IvcDeviceInfo,
    local_incarnation: NonZeroU32,
}

impl IvcPendingInfo {
    /// Configured device profile.
    pub const fn device(self) -> IvcDeviceInfo {
        self.device
    }

    /// Local incarnation published by this endpoint.
    pub const fn local_incarnation(self) -> NonZeroU32 {
        self.local_incarnation
    }
}

/// Endpoint metadata after both peers acknowledged the same incarnations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcEndpointInfo {
    device: IvcDeviceInfo,
    local_incarnation: NonZeroU32,
    peer_incarnation: NonZeroU32,
    observed_event: bool,
}

impl IvcEndpointInfo {
    /// Configured device profile.
    pub const fn device(self) -> IvcDeviceInfo {
        self.device
    }

    /// Local incarnation acknowledged by the peer.
    pub const fn local_incarnation(self) -> NonZeroU32 {
        self.local_incarnation
    }

    /// Peer incarnation acknowledged by this endpoint.
    pub const fn peer_incarnation(self) -> NonZeroU32 {
        self.peer_incarnation
    }

    /// Returns whether an event was observed during the handshake.
    pub const fn observed_event(self) -> bool {
        self.observed_event
    }
}

/// Message V1 endpoint carrying one in-progress incarnation handshake.
pub struct IvcPendingEndpoint<'a> {
    device: &'a IvcDevice,
    session: Option<IvcPortSession<'a>>,
    local_incarnation: NonZeroU32,
    observed_event: bool,
    peer_epoch: PeerEpoch,
    previous_state: Option<IvshmemSessionState>,
}

impl<'a> IvcPendingEndpoint<'a> {
    /// Returns the waiting-state metadata used by application diagnostics.
    pub const fn info(&self) -> IvcPendingInfo {
        IvcPendingInfo {
            device: self.device.info(),
            local_incarnation: self.local_incarnation,
        }
    }

    /// Polls the handshake, device events, and peer wakeups until both sides
    /// share a ready session or `timeout` expires.
    pub fn wait_ready(mut self, timeout: Duration) -> Result<IvcEndpoint<'a>, IvcDeviceError> {
        let started = Instant::now();
        loop {
            if let Some(endpoint) = self.poll_ready_once()? {
                return Ok(endpoint);
            }
            if started.elapsed() >= timeout {
                return Err(IvcDeviceError::SessionTimeout);
            }
            thread::sleep(HANDSHAKE_POLL_DELAY);
        }
    }

    fn poll_ready_once(&mut self) -> Result<Option<IvcEndpoint<'a>>, IvcDeviceError> {
        self.peer_epoch.observe(self.device)?;
        let state = self.port_mut()?.poll_handshake()?;
        self.peer_epoch.observe(self.device)?;
        match state {
            IvshmemSessionState::Ready { local, peer } => {
                if local != self.local_incarnation {
                    return Err(IvcDeviceError::IncarnationMismatch {
                        expected: self.local_incarnation,
                        actual: local,
                    });
                }
                let mut session = self.session.take().ok_or(IvcDeviceError::SessionReset)?;
                let endpoints = match session.port_mut().try_connect() {
                    Ok(endpoints) => endpoints,
                    Err(error) => {
                        // SAFETY: no Message V1 endpoint was created, and this
                        // pending session still owns the sole local port.
                        unsafe { session.port_mut().invalidate_session() };
                        session._lease.mark_reusable();
                        let _ = self.device.ring_peer();
                        return Err(error.into());
                    }
                };
                let (sender, receiver) = endpoints.into_parts();
                Ok(Some(IvcEndpoint::new(
                    self.device,
                    session,
                    sender,
                    receiver,
                    IvcEndpointInfo {
                        device: self.device.info(),
                        local_incarnation: local,
                        peer_incarnation: peer,
                        observed_event: self.observed_event,
                    },
                    self.peer_epoch,
                )))
            }
            IvshmemSessionState::WaitingForPeer { .. }
            | IvshmemSessionState::WaitingForPeerAcknowledgement { .. } => {
                if self.previous_state != Some(state) {
                    self.device.ring_peer()?;
                    self.previous_state = Some(state);
                }
                self.observed_event |= self.device.take_events(EVENT_VECTOR_0)? != 0;
                Ok(None)
            }
            state => Err(IvcDeviceError::UnexpectedSessionState { state }),
        }
    }

    fn close_session(mut self) -> Result<(), IvcDeviceError> {
        let Some(mut session) = self.session.take() else {
            return Ok(());
        };
        // SAFETY: a pending session has not created any Message V1 endpoint,
        // and `close_session` consumes its sole local protocol handle.
        unsafe { session.port_mut().invalidate_session() };
        session._lease.mark_reusable();
        self.device.ring_peer()?;
        Ok(())
    }

    fn port_mut(&mut self) -> Result<&mut IvshmemMessagePort<'a>, IvcDeviceError> {
        self.session
            .as_mut()
            .map(IvcPortSession::port_mut)
            .ok_or(IvcDeviceError::SessionReset)
    }
}

/// Connected logical Message V1 endpoint.
///
/// Fields drop in declaration order: local sender and receiver endpoints are
/// released before the session lease. The lease closes the device slot without
/// invalidating the shared channel, allowing the peer to consume the last ACK.
pub struct IvcEndpoint<'a> {
    sender: Option<IvcMessageSender<'a>>,
    receiver: Option<IvcMessageReceiver<'a>>,
    session: IvcPortSession<'a>,
    device: &'a IvcDevice,
    info: IvcEndpointInfo,
    peer_epoch: PeerEpoch,
}

impl<'a> IvcEndpoint<'a> {
    const fn new(
        device: &'a IvcDevice,
        session: IvcPortSession<'a>,
        sender: IvcMessageSender<'a>,
        receiver: IvcMessageReceiver<'a>,
        info: IvcEndpointInfo,
        peer_epoch: PeerEpoch,
    ) -> Self {
        Self {
            sender: Some(sender),
            receiver: Some(receiver),
            session,
            device,
            info,
            peer_epoch,
        }
    }

    /// Returns the ready-session metadata used by application diagnostics.
    pub const fn info(&self) -> IvcEndpointInfo {
        self.info
    }

    fn wake_peer(&self) -> Result<(), IvcDeviceError> {
        self.device.ring_peer()?;
        Ok(())
    }

    fn observe_event(&self) -> Result<bool, IvcDeviceError> {
        Ok(self.device.take_events(EVENT_VECTOR_0)? != 0)
    }

    fn ensure_session_current(&mut self) -> Result<(), IvcDeviceError> {
        self.peer_epoch.observe(self.device)?;
        if !self.session.port().is_session_current() {
            return Err(IvcDeviceError::SessionReset);
        }
        self.peer_epoch.observe(self.device)
    }

    fn try_send_complete(&mut self, input: &[u8]) -> Result<bool, IvcError> {
        self.ensure_session_current().map_err(map_device_error)?;
        let sender = self.sender.as_mut().ok_or(IvcError::SessionReset)?;
        // The compile-time limit above keeps every unified message within one
        // slot. Checking capacity before start_message means a timeout cannot
        // leave a partial logical record behind for the next send.
        if sender.available_slots() == 0 {
            return Ok(false);
        }

        if let Err(error) = sender.start_message(input.len() as u64) {
            let _ = sender.try_abort();
            return Err(map_message_error(error));
        }
        let progress = match sender.try_write(input) {
            Ok(progress) => progress,
            Err(IvcMessageError::SlotFull) => {
                let _ = sender.try_abort();
                return Ok(false);
            }
            Err(error) => {
                let _ = sender.try_abort();
                return Err(map_message_error(error));
            }
        };
        if !progress.is_complete() || progress.consumed() != input.len() {
            let _ = sender.try_abort();
            return Err(IvcError::SessionReset);
        }
        self.wake_peer().map_err(map_device_error)?;
        Ok(true)
    }

    fn try_receive_complete(&mut self, output: &mut [u8]) -> Result<Option<usize>, IvcError> {
        self.ensure_session_current().map_err(map_device_error)?;
        let Some(message_len) = self
            .receiver
            .as_mut()
            .ok_or(IvcError::SessionReset)?
            .peek_message_meta()
            .map_err(map_message_error)?
            .map(|meta| meta.len())
        else {
            return Ok(None);
        };
        if message_len > IVC_MAX_RECORD_BYTES as u64 {
            return Err(IvcError::MessageTooLarge { len: message_len });
        }
        if message_len == 0 {
            return Err(IvcError::InvalidMessageLength { len: 0 });
        }
        let message_len = usize::try_from(message_len)
            .map_err(|_| IvcError::MessageTooLarge { len: message_len })?;
        if message_len > output.len() {
            return Err(IvcError::BufferTooSmall {
                required: message_len,
                available: output.len(),
            });
        }

        let progress = self
            .receiver
            .as_mut()
            .ok_or(IvcError::SessionReset)?
            .try_read(&mut output[..message_len])
            .map_err(map_message_error)?;
        if !progress.is_complete() || progress.written() != message_len {
            return Err(IvcError::SessionReset);
        }

        if progress.consumed_slots() > 0
            && matches!(self.peer_epoch.is_current(self.device), Ok(true))
            && let Err(wake_error) = self.wake_peer()
            && matches!(self.peer_epoch.is_current(self.device), Ok(true))
        {
            return Err(map_device_error(wake_error));
        }
        Ok(Some(message_len))
    }

    fn close_session(mut self) -> Result<(), IvcDeviceError> {
        self.sender = None;
        self.receiver = None;
        // SAFETY: both Message V1 endpoints derived from this port were dropped
        // above, and the session owns the sole local protocol handle. No safe
        // borrow of this endpoint remains because `close_session` consumes it.
        unsafe { self.session.port_mut().invalidate_session() };
        self.session._lease.mark_reusable();
        self.wake_peer()
    }
}

impl MessageRecordEndpoint for IvcEndpoint<'_> {
    type Error = IvcDeviceError;

    fn start_record(&mut self, record_len: u64) -> Result<(), Self::Error> {
        self.peer_epoch.observe(self.device)?;
        self.sender
            .as_mut()
            .ok_or(IvcDeviceError::SessionReset)?
            .start_message(record_len)?;
        Ok(())
    }

    fn write_record(&mut self, input: &[u8]) -> Result<RecordProgress, Self::Error> {
        self.peer_epoch.observe(self.device)?;
        let progress = self
            .sender
            .as_mut()
            .ok_or(IvcDeviceError::SessionReset)?
            .try_write(input)?;
        self.peer_epoch.observe(self.device)?;
        let progressed = progress.published_slots() > 0;
        if progressed {
            self.wake_peer()?;
        }
        Ok(RecordProgress::new(
            progress.consumed(),
            progressed,
            progress.is_complete(),
        ))
    }

    fn peek_record_len(&mut self) -> Result<Option<u64>, Self::Error> {
        self.peer_epoch.observe(self.device)?;
        let record_len = self
            .receiver
            .as_mut()
            .ok_or(IvcDeviceError::SessionReset)?
            .peek_message_meta()?
            .map(|meta| meta.len());
        self.peer_epoch.observe(self.device)?;
        Ok(record_len)
    }

    fn read_record(&mut self, output: &mut [u8]) -> Result<RecordProgress, Self::Error> {
        self.peer_epoch.observe(self.device)?;
        let progress = self
            .receiver
            .as_mut()
            .ok_or(IvcDeviceError::SessionReset)?
            .try_read(output)?;
        let progressed = progress.consumed_slots() > 0;
        // A successful read has already copied and released its slots. If the
        // peer stops after publishing that credit, the read linearizes before
        // the epoch change. Post-read epoch failure therefore skips only the
        // now-useless wakeup and does not invalidate the completed read.
        if progressed
            && matches!(self.peer_epoch.is_current(self.device), Ok(true))
            && let Err(wake_error) = self.wake_peer()
            && matches!(self.peer_epoch.is_current(self.device), Ok(true))
        {
            return Err(wake_error);
        }
        Ok(RecordProgress::new(
            progress.written(),
            progressed,
            progress.is_complete(),
        ))
    }

    fn outbound_drained(&mut self) -> Result<bool, Self::Error> {
        if self
            .sender
            .as_ref()
            .ok_or(IvcDeviceError::SessionReset)?
            .available_slots()
            == IVC_RING_CAPACITY
        {
            // Recovered credit is the delivery linearization point. A peer may
            // stop immediately after its final read, so this evidence outranks
            // a later epoch change.
            return Ok(true);
        }
        self.peer_epoch.observe(self.device)?;
        if !self.session.port().is_session_current() {
            return Err(IvcMessageError::SessionLost.into());
        }
        Ok(false)
    }

    fn wait_for_progress(&mut self) -> Result<bool, Self::Error> {
        self.peer_epoch.observe(self.device)?;
        if !self.session.port().is_session_current() {
            return Err(IvcMessageError::SessionLost.into());
        }
        let observed = self.observe_event()?;
        thread::sleep(RECORD_POLL_DELAY);
        self.peer_epoch.observe(self.device)?;
        Ok(observed)
    }
}

/// Opaque unified IVC session.
///
/// The session owns either the in-progress handshake or the connected Message
/// V1 endpoint. Methods requiring `&mut self` serialize all calls on the
/// session. The device must outlive the session; `ivc_close` consumes the
/// session by value.
pub struct IvcSession<'a> {
    state: SessionProgress<'a>,
    failed: bool,
}

enum SessionProgress<'a> {
    Pending(IvcPendingEndpoint<'a>),
    Ready(IvcEndpoint<'a>),
    Reset,
}

impl<'a> IvcSession<'a> {
    fn ensure_ready(&mut self, deadline: OperationDeadline) -> Result<(), IvcError> {
        if self.failed {
            return Err(IvcError::SessionReset);
        }
        loop {
            let state = mem::replace(&mut self.state, SessionProgress::Reset);
            match state {
                SessionProgress::Reset => {
                    self.failed = true;
                    return Err(IvcError::SessionReset);
                }
                SessionProgress::Pending(mut pending) => match pending.poll_ready_once() {
                    Ok(None) => {
                        self.state = SessionProgress::Pending(pending);
                    }
                    Ok(Some(endpoint)) => {
                        self.state = SessionProgress::Ready(endpoint);
                        return Ok(());
                    }
                    Err(error) => {
                        let error = map_device_error(error);
                        let _ = pending.close_session();
                        self.failed = true;
                        self.state = SessionProgress::Reset;
                        return Err(error);
                    }
                },
                SessionProgress::Ready(mut endpoint) => {
                    let current = endpoint.ensure_session_current();
                    self.state = SessionProgress::Ready(endpoint);
                    match current {
                        Ok(()) => return Ok(()),
                        Err(error) if is_session_reset(&error) => {
                            self.failed = true;
                            return Err(IvcError::SessionReset);
                        }
                        Err(error) => return Err(IvcError::Device(error)),
                    }
                }
            }

            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Err(IvcError::TimedOut);
            }
            thread::sleep(cmp::min(remaining, HANDSHAKE_POLL_DELAY));
        }
    }

    fn endpoint_mut(&mut self) -> Result<&mut IvcEndpoint<'a>, IvcError> {
        if self.failed {
            return Err(IvcError::SessionReset);
        }
        match &mut self.state {
            SessionProgress::Ready(endpoint) => Ok(endpoint),
            SessionProgress::Pending(_) | SessionProgress::Reset => Err(IvcError::SessionReset),
        }
    }

    fn record_error(&mut self, error: IvcError) -> IvcError {
        if matches!(&error, IvcError::SessionReset) {
            self.failed = true;
        }
        error
    }

    fn close(self) -> Result<(), IvcError> {
        match self.state {
            SessionProgress::Pending(pending) => pending.close_session().map_err(map_device_error),
            SessionProgress::Ready(endpoint) => endpoint.close_session().map_err(map_device_error),
            SessionProgress::Reset => Ok(()),
        }
    }
}

/// Opens the single unified message session without waiting for the peer.
///
/// The returned pending session completes its handshake during the first
/// `ivc_send` or `ivc_recv` call. A zero timeout performs one nonblocking
/// progress attempt; a positive timeout is bounded and preserves the pending
/// handshake when it expires.
pub fn ivc_open(device: &mut IvcDevice) -> Result<IvcSession<'_>, IvcError> {
    let pending = device.open_endpoint().map_err(map_device_error)?;
    Ok(IvcSession {
        state: SessionProgress::Pending(pending),
        failed: false,
    })
}

/// Sends one complete 1..=48-byte message.
///
/// `timeout_ms` includes handshake and outbound ring-capacity waits. The call
/// never leaves a partial record active after returning `TimedOut`.
pub fn ivc_send(
    session: &mut IvcSession<'_>,
    message: &[u8],
    timeout_ms: i32,
) -> Result<(), IvcError> {
    if message.is_empty() {
        return Err(IvcError::InvalidArgument {
            argument: "empty outgoing message",
        });
    }
    if message.len() > IVC_MAX_RECORD_BYTES {
        return Err(IvcError::MessageTooLarge {
            len: message.len() as u64,
        });
    }
    let deadline = OperationDeadline::new(timeout_ms)?;
    session.ensure_ready(deadline)?;

    loop {
        let outcome = session.endpoint_mut()?.try_send_complete(message);
        match outcome {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return Err(session.record_error(error)),
        }
        if deadline.remaining().is_zero() {
            return Err(IvcError::TimedOut);
        }
        let wait = session.endpoint_mut()?.wait_for_progress();
        if let Err(error) = wait {
            return Err(session.record_error(map_device_error(error)));
        }
    }
}

/// Receives one complete 1..=48-byte message.
///
/// The buffer may be larger than 48 bytes. An oversized inbound record or a
/// record that does not fit the buffer is left at the receive head.
pub fn ivc_recv(
    session: &mut IvcSession<'_>,
    output: &mut [u8],
    timeout_ms: i32,
) -> Result<usize, IvcError> {
    if output.is_empty() {
        return Err(IvcError::InvalidArgument {
            argument: "empty receive buffer",
        });
    }
    let deadline = OperationDeadline::new(timeout_ms)?;
    session.ensure_ready(deadline)?;

    loop {
        let outcome = session.endpoint_mut()?.try_receive_complete(output);
        match outcome {
            Ok(Some(message_len)) => return Ok(message_len),
            Ok(None) if deadline.remaining().is_zero() => return Err(IvcError::TimedOut),
            Ok(None) => {
                let wait = session.endpoint_mut()?.wait_for_progress();
                if let Err(error) = wait {
                    return Err(session.record_error(map_device_error(error)));
                }
            }
            Err(error) => return Err(session.record_error(error)),
        }
    }
}

/// Closes and consumes one unified IVC session.
///
/// Safe Rust makes a null or reused handle unrepresentable. The consumed
/// endpoint invalidates the shared session and releases the device slot for a
/// later open. A doorbell failure is reported after the handle is consumed.
pub fn ivc_close(session: IvcSession<'_>) -> Result<(), IvcError> {
    session.close()
}

#[derive(Clone, Copy)]
struct OperationDeadline {
    end: Instant,
}

impl OperationDeadline {
    fn new(timeout_ms: i32) -> Result<Self, IvcError> {
        if !(0..=IVC_MAX_TIMEOUT_MS).contains(&timeout_ms) {
            return Err(IvcError::InvalidArgument {
                argument: "timeout outside 0..=60000 ms",
            });
        }
        Ok(Self {
            end: Instant::now() + Duration::from_millis(timeout_ms as u64),
        })
    }

    fn remaining(self) -> Duration {
        // ArceOS `Instant::duration_since` saturates to zero for future instants.
        self.end.duration_since(Instant::now())
    }
}

/// Errors reported by the configured ArceOS IVC device adapter.
#[derive(Debug, Error)]
pub enum IvcDeviceError {
    /// PCI discovery or BAR access failed.
    #[error("ivshmem device error: {0}")]
    Device(#[from] IvshmemError),
    /// Message V1 attach, handshake, or connection failed.
    #[error("ivshmem message error: {0}")]
    MessagePort(#[from] IvshmemMessageError),
    /// Logical Message V1 send or receive failed.
    #[error("IVC message error: {0}")]
    Message(#[from] IvcMessageError),
    /// The configured role does not match the guest peer slot.
    #[error("IVC role {role:?} expects peer {expected}, guest is peer {actual}")]
    RoleMismatch {
        /// Configured asymmetric role.
        role: IvcPeerRole,
        /// Peer slot selected by the role.
        expected: u32,
        /// Peer slot advertised by the device.
        actual: u32,
    },
    /// The device profile has an unexpected peer count.
    #[error("ivshmem profile has {actual} peers, expected {expected}")]
    UnexpectedPeerCount {
        /// Required peer count.
        expected: u32,
        /// Advertised peer count.
        actual: u32,
    },
    /// The device already created its single endpoint.
    #[error("the configured ivshmem endpoint is already open")]
    SessionAlreadyOpen,
    /// A previous endpoint closed without invalidating its peer-readable
    /// session, so this device instance cannot start another session.
    #[error("the configured ivshmem endpoint is closed")]
    SessionClosed,
    /// The shared Message V1 incarnation no longer belongs to this endpoint.
    #[error("the configured ivshmem endpoint session was reset")]
    SessionReset,
    /// The peer did not join before the session timeout.
    #[error("peer did not join the ivshmem Message V1 session")]
    SessionTimeout,
    /// Handshake left the waiting path.
    #[error("ivshmem Message V1 handshake is not ready: {state:?}")]
    UnexpectedSessionState {
        /// Observed session state.
        state: IvshmemSessionState,
    },
    /// Ready state disagreed with the local incarnation.
    #[error("ivshmem session incarnation is {actual}, expected {expected}")]
    IncarnationMismatch {
        /// Local incarnation created by this endpoint.
        expected: NonZeroU32,
        /// Incarnation reported ready.
        actual: NonZeroU32,
    },
    /// The host reset/detach epoch changed after the first peer attach.
    #[error("peer reset epoch changed from {expected} to {observed}")]
    PeerEpochChanged {
        /// First stable nonzero epoch observed for the peer.
        expected: u64,
        /// Later observed peer epoch.
        observed: u64,
    },
}

/// Typed failures from the unified IVC message interface.
#[derive(Debug, Error)]
pub enum IvcError {
    /// An argument violates the IVC message contract (C `EINVAL`).
    #[error("invalid IVC argument: {argument}")]
    InvalidArgument {
        /// Rejected argument.
        argument: &'static str,
    },
    /// A record exceeds the unified message limit (C `EMSGSIZE`).
    #[error("IVC message length {len} exceeds {IVC_MAX_RECORD_BYTES} bytes")]
    MessageTooLarge {
        /// Declared or supplied record length.
        len: u64,
    },
    /// An inbound record does not fit the caller's receive buffer (C `ENOSPC`).
    #[error("IVC receive buffer has {available} bytes but the record requires {required}")]
    BufferTooSmall {
        /// Number of bytes required by the next record.
        required: usize,
        /// Number of writable bytes supplied by the caller.
        available: usize,
    },
    /// The next inbound record is empty, outside the 1..=48-byte contract.
    #[error("invalid IVC message length {len}")]
    InvalidMessageLength {
        /// Declared complete-message length.
        len: u64,
    },
    /// The peer or ring capacity was unavailable before timeout (C `EAGAIN`).
    #[error("IVC operation timed out")]
    TimedOut,
    /// The session incarnation changed and must be closed before reopening
    /// (C `EPIPE`).
    #[error("IVC session was reset")]
    SessionReset,
    /// This device already owns an open session (C `EBUSY`).
    #[error("IVC session is already open")]
    SessionAlreadyOpen,
    /// Device discovery, BAR access, or doorbell operation failed.
    #[error(transparent)]
    Device(#[from] IvcDeviceError),
    /// Message V1 framing or transport state failed.
    #[error(transparent)]
    Message(#[from] IvcMessageError),
}

fn is_session_reset(error: &IvcDeviceError) -> bool {
    matches!(
        error,
        IvcDeviceError::SessionClosed
            | IvcDeviceError::SessionReset
            | IvcDeviceError::PeerEpochChanged { .. }
            | IvcDeviceError::Message(
                IvcMessageError::SessionLost | IvcMessageError::CursorOutOfRange { .. }
            )
    )
}

fn map_device_error(error: IvcDeviceError) -> IvcError {
    if is_session_reset(&error) {
        IvcError::SessionReset
    } else {
        match error {
            IvcDeviceError::SessionAlreadyOpen => IvcError::SessionAlreadyOpen,
            IvcDeviceError::SessionTimeout => IvcError::TimedOut,
            error => IvcError::Device(error),
        }
    }
}

fn map_message_error(error: IvcMessageError) -> IvcError {
    match error {
        IvcMessageError::SessionLost | IvcMessageError::CursorOutOfRange { .. } => {
            IvcError::SessionReset
        }
        error => IvcError::Message(error),
    }
}
