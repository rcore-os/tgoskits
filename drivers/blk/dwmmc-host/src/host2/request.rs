//! Host2 request ownership and bounded bus-operation state.

use super::*;

pub struct DataRequest<'a> {
    pub(crate) id: RequestId,
    pub(crate) request: Option<BlockRequest>,
    pub(crate) slot: BlockRequestSlot,
    pub(crate) _buffer: PhantomData<&'a [u8]>,
}

pub struct TransactionRequest<'a> {
    pub(super) owner: usize,
    pub(crate) id: u64,
    pub(super) done: bool,
    pub(super) kind: TransactionRequestKind,
    pub(super) data: Option<DataRequest<'a>>,
}

pub(super) enum TransactionRequestKind {
    Command { response: sdio_host2::ResponseType },
    Data { response: sdio_host2::ResponseType },
}

impl<'a> TransactionRequest<'a> {
    pub(super) fn command(owner: usize, id: u64, response: sdio_host2::ResponseType) -> Self {
        Self {
            owner,
            id,
            done: false,
            kind: TransactionRequestKind::Command { response },
            data: None,
        }
    }

    pub(super) fn data(
        owner: usize,
        id: u64,
        request: DataRequest<'a>,
        response: sdio_host2::ResponseType,
    ) -> Self {
        Self {
            owner,
            id,
            done: false,
            kind: TransactionRequestKind::Data { response },
            data: Some(request),
        }
    }
}

pub struct BusRequest {
    pub(super) owner: usize,
    pub(crate) id: u64,
    pub(super) done: bool,
    pub(super) state: BusRequestState,
}

impl BusRequest {
    pub(super) fn pending(owner: usize, id: u64, state: BusRequestState) -> Self {
        Self {
            owner,
            id,
            done: false,
            state,
        }
    }
}

pub(super) enum BusRequestState {
    ResetAll(DwMmcResetState),
    ResetDataLine(DwMmcFifoResetState),
    PowerOn(DwMmcResetState),
    PowerOff,
    SetClock(DwMmcClockState),
    SetBusWidth(BusWidth),
    SetSignalVoltage(SignalVoltage),
}

impl BusRequestState {
    pub(super) fn wake_at_ns(&self) -> Option<u64> {
        match self {
            Self::ResetAll(DwMmcResetState::WaitReset { wait })
            | Self::PowerOn(DwMmcResetState::WaitReset { wait })
            | Self::ResetDataLine(DwMmcFifoResetState::WaitReset { wait })
            | Self::SetClock(DwMmcClockState::WaitDivider { wait })
            | Self::SetClock(DwMmcClockState::WaitEnable { wait }) => Some(wait.wake_at_ns()),
            Self::SetClock(DwMmcClockState::WaitGate { wait, .. }) => Some(wait.wake_at_ns()),
            _ => None,
        }
    }
}

pub(super) enum DwMmcResetState {
    Start,
    WaitReset { wait: Host2TimedWait },
}

pub(super) enum DwMmcFifoResetState {
    Start,
    WaitReset { wait: Host2TimedWait },
}

pub(super) enum DwMmcClockState {
    Start {
        speed: Option<ClockSpeed>,
        target_hz: u32,
        wait_prvdata_complete: bool,
    },
    ExternalSetClock {
        speed: Option<ClockSpeed>,
        target_hz: u32,
        wait_prvdata_complete: bool,
    },
    WaitGate {
        wait: Host2TimedWait,
        target_hz: u32,
    },
    ProgramDivider {
        target_hz: u32,
    },
    WaitDivider {
        wait: Host2TimedWait,
    },
    Enable,
    WaitEnable {
        wait: Host2TimedWait,
    },
}

#[derive(Clone, Copy)]
pub(super) struct Host2TimedWait {
    deadline_ns: u64,
    wake_at_ns: u64,
}

impl Host2TimedWait {
    pub(super) fn start(now_ns: u64) -> Self {
        Self {
            deadline_ns: now_ns.saturating_add(HOST2_TRANSITION_TIMEOUT_NS),
            wake_at_ns: next_check(now_ns, now_ns.saturating_add(HOST2_TRANSITION_TIMEOUT_NS)),
        }
    }

    pub(super) const fn wake_at_ns(self) -> u64 {
        self.wake_at_ns
    }

    pub(super) const fn expired(self, now_ns: u64) -> bool {
        now_ns >= self.deadline_ns
    }

    pub(super) fn defer(&mut self, now_ns: u64) {
        self.wake_at_ns = next_check(now_ns, self.deadline_ns);
    }
}

const HOST2_CHECK_INTERVAL_NS: u64 = 50_000;
const HOST2_TRANSITION_TIMEOUT_NS: u64 = 100_000_000;

fn next_check(now_ns: u64, deadline_ns: u64) -> u64 {
    now_ns
        .saturating_add(HOST2_CHECK_INTERVAL_NS)
        .min(deadline_ns)
}
