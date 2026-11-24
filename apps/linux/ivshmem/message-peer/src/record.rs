use std::{
    num::NonZeroU32,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use arceos_ivshmem_pci::{MessageRecordEndpoint, RecordProgress};
use axivc::{IVC_RING_CAPACITY, IvshmemSessionState};

use crate::ivshmem::{MessageSession, WaitResult};

const EVENT_WAIT_MS: i32 = 25;

/// Linux compatibility adapter for one configured Message V1 endpoint.
///
/// Applications interact with records only; this type owns handshake polling,
/// UIO/MSI-X waits, peer doorbells, and session validation.
pub struct PendingEndpoint<'a> {
    session: MessageSession<'a>,
    local_incarnation: NonZeroU32,
    observed_events: usize,
}

impl<'a> PendingEndpoint<'a> {
    pub(crate) const fn new(session: MessageSession<'a>, local_incarnation: NonZeroU32) -> Self {
        Self {
            session,
            local_incarnation,
            observed_events: 0,
        }
    }

    pub const fn local_incarnation(&self) -> NonZeroU32 {
        self.local_incarnation
    }

    /// Completes the asymmetric Linux subscriber handshake and returns the
    /// connected logical-record endpoint.
    pub fn wait_ready(mut self, timeout: Duration) -> Result<LinuxMessageEndpoint<'a>> {
        let started = Instant::now();
        let mut previous_state = None;
        loop {
            self.session.observe_peer_epoch()?;
            let state = self.session.poll_handshake()?;
            self.session.observe_peer_epoch()?;
            match state {
                IvshmemSessionState::Ready { local, peer } => {
                    if local != self.local_incarnation {
                        bail!(
                            "session ready with unexpected local incarnation {local}, expected {}",
                            self.local_incarnation
                        );
                    }
                    let observed_events = self.observed_events;
                    return LinuxMessageEndpoint::connect(
                        self.session,
                        local,
                        peer,
                        observed_events,
                    );
                }
                IvshmemSessionState::WaitingForPeer { .. }
                | IvshmemSessionState::WaitingForPeerAcknowledgement { .. } => {}
                state => {
                    bail!("ivshmem Message V1 handshake left the waiting path: {state:?}")
                }
            }

            if previous_state != Some(state) {
                self.session.ring_peer()?;
                previous_state = Some(state);
            }
            if self.session.wait_event(EVENT_WAIT_MS)? == WaitResult::Interrupt {
                self.observed_events += 1;
            }
            if started.elapsed() >= timeout {
                let state = self.session.poll_handshake()?;
                bail!("session state/backpressure timeout after {timeout:?}: {state:?}");
            }
        }
    }
}

/// Connected Linux logical-record endpoint.
pub struct LinuxMessageEndpoint<'a> {
    sender: axivc::IvcMessageSender<'a>,
    receiver: axivc::IvcMessageReceiver<'a>,
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
            sender,
            receiver,
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
}

impl MessageRecordEndpoint for LinuxMessageEndpoint<'_> {
    type Error = anyhow::Error;

    fn start_record(&mut self, record_len: u64) -> Result<(), Self::Error> {
        self.sender
            .start_message(record_len)
            .map(|_| ())
            .context("failed to start a Linux IVC record")
    }

    fn write_record(&mut self, input: &[u8]) -> Result<RecordProgress, Self::Error> {
        self.session.observe_peer_epoch()?;
        let progress = self
            .sender
            .try_write(input)
            .context("failed to write a Linux IVC record")?;
        self.session.observe_peer_epoch()?;
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
        self.receiver
            .peek_message_meta()
            .map(|meta| meta.map(|meta| meta.len()))
            .context("failed to inspect a Linux IVC record")
    }

    fn read_record(&mut self, output: &mut [u8]) -> Result<RecordProgress, Self::Error> {
        self.session.observe_peer_epoch()?;
        let progress = self
            .receiver
            .try_read(output)
            .context("failed to read a Linux IVC record")?;
        let progressed = progress.consumed_slots() > 0;
        // A completed read already owns the copied data and released its slots.
        // If the publisher stops after publishing that credit, the epoch change
        // only makes the now-useless wakeup optional.
        if progressed
            && matches!(self.session.peer_epoch_is_current(), Ok(true))
            && let Err(error) = self.session.ring_peer()
            && matches!(self.session.peer_epoch_is_current(), Ok(true))
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
        if self.sender.available_slots() == IVC_RING_CAPACITY {
            return Ok(true);
        }
        self.session.observe_peer_epoch()?;
        let state = self.session.session_state()?;
        if !matches!(state, IvshmemSessionState::Ready { .. }) {
            bail!("ivshmem Message V1 session was lost: {state:?}");
        }
        Ok(false)
    }

    fn wait_for_progress(&mut self) -> Result<bool, Self::Error> {
        self.session.observe_peer_epoch()?;
        let state = self.session.session_state()?;
        if !matches!(state, IvshmemSessionState::Ready { .. }) {
            bail!("ivshmem Message V1 session was lost: {state:?}");
        }
        let observed = self.session.wait_event(EVENT_WAIT_MS)? == WaitResult::Interrupt;
        if observed {
            self.observed_events += 1;
        }
        self.session.observe_peer_epoch()?;
        Ok(observed)
    }
}
