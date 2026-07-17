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
    ResetAll(PhytiumResetState),
    ResetDataLine(PhytiumFifoResetState),
    PowerOn,
    PowerOff,
    SetClock(PhytiumClockState),
    SetBusWidth(BusWidth),
    SetSignalVoltage(PhytiumVoltageState),
}

impl BusRequestState {
    pub(super) fn wake_at_ns(&self) -> Option<u64> {
        match self {
            Self::ResetAll(PhytiumResetState::WaitReset { wait })
            | Self::ResetDataLine(PhytiumFifoResetState::WaitReset { wait })
            | Self::SetClock(PhytiumClockState::WaitEnable { wait })
            | Self::SetSignalVoltage(PhytiumVoltageState::WaitUpdate { wait }) => {
                Some(wait.wake_at_ns())
            }
            Self::ResetAll(PhytiumResetState::InitClock(clock)) | Self::SetClock(clock) => {
                clock.wake_at_ns()
            }
            _ => None,
        }
    }
}

pub(super) enum PhytiumResetState {
    Start,
    WaitReset { wait: Host2TimedWait },
    InitClock(PhytiumClockState),
}

pub(super) enum PhytiumFifoResetState {
    Start,
    WaitReset { wait: Host2TimedWait },
}

pub(super) enum PhytiumClockState {
    Start {
        timing: timing::TimingTable,
    },
    WaitExternalClock {
        wait: Host2TimedWait,
        timing: timing::TimingTable,
    },
    WaitDisable {
        wait: Host2TimedWait,
        timing: timing::TimingTable,
    },
    ProgramDivider {
        timing: timing::TimingTable,
    },
    WaitEnable {
        wait: Host2TimedWait,
    },
}

impl PhytiumClockState {
    fn wake_at_ns(&self) -> Option<u64> {
        match self {
            Self::WaitExternalClock { wait, .. }
            | Self::WaitDisable { wait, .. }
            | Self::WaitEnable { wait } => Some(wait.wake_at_ns()),
            _ => None,
        }
    }
}

pub(super) enum PhytiumVoltageState {
    Start(SignalVoltage),
    WaitUpdate { wait: Host2TimedWait },
}

#[derive(Clone, Copy)]
pub(super) struct Host2TimedWait {
    deadline_ns: u64,
    wake_at_ns: u64,
}

impl Host2TimedWait {
    pub(super) fn start(now_ns: u64) -> Self {
        let deadline_ns = now_ns.saturating_add(HOST2_TRANSITION_TIMEOUT_NS);
        Self {
            deadline_ns,
            wake_at_ns: next_check(now_ns, deadline_ns),
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
