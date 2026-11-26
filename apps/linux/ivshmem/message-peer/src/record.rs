use std::{
    fmt,
    num::NonZeroU32,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use arceos_ivshmem_pci::{MessageRecordEndpoint, RecordProgress};
use axivc::{IVC_RING_CAPACITY, IvshmemSessionState};

use crate::ivshmem::{MessageSession, WaitResult};

const EVENT_WAIT_MS: i32 = 25;

/// A terminal reset of the Message V1 session owned by this Linux process.
#[derive(Debug)]
pub(crate) struct SessionLost {
    detail: String,
}

pub(crate) fn session_lost(detail: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(SessionLost {
        detail: detail.into(),
    })
}

impl fmt::Display for SessionLost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for SessionLost {}

/// Linux compatibility adapter for one configured Message V1 endpoint.
///
/// Applications interact with records only; this type owns handshake polling,
/// UIO/MSI-X waits, peer doorbells, and session validation.
pub struct PendingEndpoint<'a> {
    session: MessageSession<'a>,
    local_incarnation: NonZeroU32,
    observed_events: usize,
    previous_state: Option<IvshmemSessionState>,
}

impl<'a> PendingEndpoint<'a> {
    pub(crate) const fn new(session: MessageSession<'a>, local_incarnation: NonZeroU32) -> Self {
        Self {
            session,
            local_incarnation,
            observed_events: 0,
            previous_state: None,
        }
    }

    pub const fn local_incarnation(&self) -> NonZeroU32 {
        self.local_incarnation
    }

    /// Advances the asymmetric handshake once without blocking.
    pub fn poll_ready(self) -> Result<PendingReady<'a>> {
        let Self {
            mut session,
            local_incarnation,
            observed_events,
            previous_state,
        } = self;
        session.observe_epochs()?;
        let state = session.poll_handshake()?;
        session.observe_epochs()?;
        match state {
            IvshmemSessionState::Ready { local, peer } => {
                if local != local_incarnation {
                    return Err(session_lost(format!(
                        "session ready with unexpected local incarnation {local}, expected \
                         {local_incarnation}"
                    )));
                }
                Ok(PendingReady::Connected(LinuxMessageEndpoint::connect(
                    session,
                    local,
                    peer,
                    observed_events,
                )?))
            }
            IvshmemSessionState::WaitingForPeer { .. }
            | IvshmemSessionState::WaitingForPeerAcknowledgement { .. } => {
                if previous_state != Some(state) {
                    session.ring_peer()?;
                }
                Ok(PendingReady::Waiting(Self {
                    session,
                    local_incarnation,
                    observed_events,
                    previous_state: Some(state),
                }))
            }
            state => Err(session_lost(format!(
                "ivshmem Message V1 handshake left the waiting path: {state:?}"
            ))),
        }
    }

    /// Waits for one UIO event while the handshake remains pending.
    pub fn wait_for_progress(&mut self, timeout: Duration) -> Result<bool> {
        let timeout_ms = duration_to_millis(timeout)?;
        let observed = self.session.wait_event(timeout_ms)? == WaitResult::Interrupt;
        if observed {
            self.observed_events += 1;
        }
        Ok(observed)
    }

    /// Completes the asymmetric Linux subscriber handshake and returns the
    /// connected logical-record endpoint.
    pub fn wait_ready(mut self, timeout: Duration) -> Result<LinuxMessageEndpoint<'a>> {
        let started = Instant::now();
        loop {
            match self.poll_ready()? {
                PendingReady::Connected(endpoint) => return Ok(endpoint),
                PendingReady::Waiting(pending) => self = pending,
            }
            if started.elapsed() >= timeout {
                let state = self.session.poll_handshake()?;
                bail!("session state/backpressure timeout after {timeout:?}: {state:?}");
            }
            self.wait_for_progress(EVENT_WAIT_MS_DURATION)?;
        }
    }
}

pub enum PendingReady<'a> {
    Waiting(PendingEndpoint<'a>),
    Connected(LinuxMessageEndpoint<'a>),
}

/// Connected Linux logical-record endpoint.
pub struct LinuxMessageEndpoint<'a> {
    sender: Option<axivc::IvcMessageSender<'a>>,
    receiver: Option<axivc::IvcMessageReceiver<'a>>,
    session: MessageSession<'a>,
    local_incarnation: NonZeroU32,
    peer_incarnation: NonZeroU32,
    observed_events: usize,
}

impl<'a> LinuxMessageEndpoint<'a> {
    fn connect(
        mut session: MessageSession<'a>,
        local_incarnation: NonZeroU32,
        peer_incarnation: NonZeroU32,
        observed_events: usize,
    ) -> Result<Self> {
        let endpoints = session.try_connect()?;
        let (sender, receiver) = endpoints.into_parts();
        Ok(Self {
            sender: Some(sender),
            receiver: Some(receiver),
            session,
            local_incarnation,
            peer_incarnation,
            observed_events,
        })
    }

    pub const fn local_incarnation(&self) -> NonZeroU32 {
        self.local_incarnation
    }

    pub const fn peer_incarnation(&self) -> NonZeroU32 {
        self.peer_incarnation
    }

    pub const fn observed_events(&self) -> usize {
        self.observed_events
    }

    pub fn session_is_current(&mut self) -> Result<bool> {
        self.session.observe_epochs()?;
        self.session
            .session_state()
            .map(|state| matches!(state, IvshmemSessionState::Ready { .. }))
    }

    pub fn complete_record_available(&self) -> Result<bool> {
        self.receiver
            .as_ref()
            .ok_or_else(|| anyhow!("Linux IVC receiver is closed"))?
            .complete_message_available()
            .context("failed to inspect a Linux IVC record")
    }

    pub fn abort_record(&mut self) -> Result<()> {
        self.sender
            .as_mut()
            .ok_or_else(|| anyhow!("Linux IVC sender is closed"))?
            .try_abort()
            .context("failed to abort a Linux IVC record")
    }

    pub fn wait_for_progress_timeout(&mut self, timeout: Duration) -> Result<bool> {
        self.session.observe_epochs()?;
        let state = self.session.session_state()?;
        if !matches!(state, IvshmemSessionState::Ready { .. }) {
            return Err(session_lost(format!(
                "ivshmem Message V1 session was lost: {state:?}"
            )));
        }
        let observed =
            self.session.wait_event(duration_to_millis(timeout)?)? == WaitResult::Interrupt;
        if observed {
            self.observed_events += 1;
        }
        self.session.observe_epochs()?;
        Ok(observed)
    }
}

impl Drop for LinuxMessageEndpoint<'_> {
    fn drop(&mut self) {
        // The Message V1 invalidation contract requires every endpoint derived
        // from the local output to be dropped before publishing the invalid
        // session header.
        self.sender = None;
        self.receiver = None;
        self.session.invalidate_session();
    }
}

impl MessageRecordEndpoint for LinuxMessageEndpoint<'_> {
    type Error = anyhow::Error;

    fn start_record(&mut self, record_len: u64) -> Result<(), Self::Error> {
        self.sender
            .as_mut()
            .ok_or_else(|| anyhow!("Linux IVC sender is closed"))?
            .start_message(record_len)
            .map(|_| ())
            .context("failed to start a Linux IVC record")
    }

    fn write_record(&mut self, input: &[u8]) -> Result<RecordProgress, Self::Error> {
        self.session.observe_epochs()?;
        let progress = self
            .sender
            .as_mut()
            .ok_or_else(|| anyhow!("Linux IVC sender is closed"))?
            .try_write(input)
            .context("failed to write a Linux IVC record")?;
        self.session.observe_epochs()?;
        let progressed = progress.published_slots() > 0;
        if progressed {
            self.session.ring_peer()?;
        }
        Ok(RecordProgress::new(
            progress.consumed(),
            progressed,
            progress.is_complete(),
        ))
    }

    fn peek_record_len(&mut self) -> Result<Option<u64>, Self::Error> {
        self.session.observe_epochs()?;
        let record_len = self
            .receiver
            .as_mut()
            .ok_or_else(|| anyhow!("Linux IVC receiver is closed"))?
            .peek_message_meta()
            .map(|meta| meta.map(|meta| meta.len()))
            .context("failed to inspect a Linux IVC record")?;
        self.session.observe_epochs()?;
        Ok(record_len)
    }

    fn read_record(&mut self, output: &mut [u8]) -> Result<RecordProgress, Self::Error> {
        self.session.observe_epochs()?;
        let progress = self
            .receiver
            .as_mut()
            .ok_or_else(|| anyhow!("Linux IVC receiver is closed"))?
            .try_read(output)
            .context("failed to read a Linux IVC record")?;
        let progressed = progress.consumed_slots() > 0;
        // A completed read already owns the copied data and released its slots.
        // If the publisher stops after publishing that credit, the epoch change
        // only makes the now-useless wakeup optional.
        if progressed
            && matches!(self.session.epochs_current(), Ok(true))
            && let Err(error) = self.session.ring_peer()
            && matches!(self.session.epochs_current(), Ok(true))
        {
            return Err(error);
        }
        Ok(RecordProgress::new(
            progress.written(),
            progressed,
            progress.is_complete(),
        ))
    }

    fn outbound_drained(&mut self) -> Result<bool, Self::Error> {
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| anyhow!("Linux IVC sender is closed"))?;
        if sender.available_slots() == IVC_RING_CAPACITY {
            return Ok(true);
        }
        self.session.observe_epochs()?;
        let state = self.session.session_state()?;
        if !matches!(state, IvshmemSessionState::Ready { .. }) {
            return Err(session_lost(format!(
                "ivshmem Message V1 session was lost: {state:?}"
            )));
        }
        Ok(false)
    }

    fn wait_for_progress(&mut self) -> Result<bool, Self::Error> {
        self.wait_for_progress_timeout(EVENT_WAIT_MS_DURATION)
    }
}

const EVENT_WAIT_MS_DURATION: Duration = Duration::from_millis(EVENT_WAIT_MS as u64);

fn duration_to_millis(timeout: Duration) -> Result<i32> {
    let millis = timeout.as_millis();
    i32::try_from(millis).context("ivshmem event timeout exceeds c_int")
}
