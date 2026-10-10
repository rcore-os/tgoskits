//! Narrow observation ports for network events.
//!
//! One [`ObservationPort`] is one typed function-pointer slot installed by the
//! OS adapter and never replaced or removed.  Entering it costs one
//! published-flag load while no consumer is active, and the slot load plus the
//! call when one is; it never allocates, reads a clock or takes a network lock,
//! so it is safe on the queue executor and protocol executor paths.  The caller
//! assembles the report value before entering, which the compiler may sink
//! behind the flag check but which the port itself does not guarantee.
//!
//! Every event owns one port instance and exposes it to the OS adapter through
//! `install_<event>_observer` / `publish_<event>_gate`, so the adapter never
//! touches the slot or the flag directly.  The module holds the ports of both
//! executors: the queue runtime's events and the protocol executor's.
//!
//! Queue reports carry [`crate::queue_runtime::NetQueueIdentity`] and the queue
//! runtime re-exports these ports, so the two modules refer to each other.  The
//! cycle is deliberate: one table of ports and report types keeps the event
//! boundary in a single place, and the identity type stays where it is owned.

use core::{
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

use crate::queue_runtime::NetQueueIdentity;

/// Result of one queue executor poll round.
///
/// The discriminants are the reported codes and are part of the event
/// contract: they must not be reordered.  The assertion below pins them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum QueuePollOutcome {
    /// The round found no further work.
    Idle    = 0,
    /// Work remains: a budget was exhausted or retryable RX work is pending.
    More    = 1,
    /// The round stopped because an SPSC ring could not accept a produced
    /// token.
    Blocked = 2,
    /// The round failed; the group is disabled.
    Failed  = 3,
}

const _: () = assert!(
    QueuePollOutcome::Idle as u32 == 0
        && QueuePollOutcome::More as u32 == 1
        && QueuePollOutcome::Blocked as u32 == 2
        && QueuePollOutcome::Failed as u32 == 3,
    "the reported outcome codes are part of the event contract"
);

/// One completed queue executor poll round.
///
/// The identity is the one fixed when the group was built, so `owner_cpu` is
/// the group's owner rather than a CPU sampled at report time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueuePollReport {
    pub identity: NetQueueIdentity,
    /// CPU work budget handed to this poll call.  It is the round's remaining
    /// budget, so it shrinks as the round serves earlier groups.
    pub budget: usize,
    /// Executor work units completed by this call.  A unit is one completed
    /// TX reclaim, TX submit, RX recycle, RX refill or RX reclaim; it is not a
    /// frame count.
    pub work_units: usize,
    pub outcome: QueuePollOutcome,
}

/// How a queue rearm ended, when it did not end in the plain idle case.
///
/// The discriminants are the reported codes and are part of the event
/// contract: they must not be reordered.  The assertion below pins them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum QueueRearmOutcome {
    /// An IRQ arrived while the round was polling, so rearm was skipped and
    /// the group was rescheduled.
    Race        = 0,
    /// Rearm found work already pending; the group is rescheduled.  This is
    /// the reported form of the group's `rearm_race` counter.
    WorkPending = 1,
    /// The device asked for a deferred retry instead of accepting rearm.
    RetryAt     = 2,
    /// Rearm failed; the group is disabled.
    Failed      = 3,
}

const _: () = assert!(
    QueueRearmOutcome::Race as u32 == 0
        && QueueRearmOutcome::WorkPending as u32 == 1
        && QueueRearmOutcome::RetryAt as u32 == 2
        && QueueRearmOutcome::Failed as u32 == 3,
    "the reported rearm codes are part of the event contract"
);

/// One queue rearm that did not end in the plain idle case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueRearmReport {
    pub identity: NetQueueIdentity,
    pub outcome: QueueRearmOutcome,
}

/// Where the queue executor could not proceed because the device asked to
/// wait for a hardware event.
///
/// The discriminants are the reported codes and are part of the event
/// contract: they must not be reordered.  The assertion below pins them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum QueueBackpressureStage {
    /// A frame was retained because the driver would not accept it yet.
    TxSubmit = 0,
    /// A replacement completion was retained because the device would not
    /// accept it yet.
    RxRefill = 1,
}

const _: () = assert!(
    QueueBackpressureStage::TxSubmit as u32 == 0 && QueueBackpressureStage::RxRefill as u32 == 1,
    "the reported backpressure stages are part of the event contract"
);

/// The retryable device outcomes a backpressure report can carry.
///
/// The discriminants are the reported codes and are part of the event
/// contract: they must not be reordered.  The assertion below pins them.
/// Not every reason is reachable from every stage: a link-down during RX
/// refill fails the round instead of retaining the replacement, so
/// [`QueueBackpressureStage::RxRefill`] only ever reports `Retry`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum QueueBackpressureReason {
    /// The device asked to be retried later.
    Retry    = 0,
    /// The device reported the link down; the frame stays retained.
    LinkDown = 1,
}

const _: () = assert!(
    QueueBackpressureReason::Retry as u32 == 0 && QueueBackpressureReason::LinkDown as u32 == 1,
    "the reported backpressure reasons are part of the event contract"
);

/// One retryable refusal to proceed, reported once per occurrence.
///
/// `reason` is the classified device outcome, not an accumulated cause code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueBackpressureReport {
    pub identity: NetQueueIdentity,
    pub stage: QueueBackpressureStage,
    pub reason: QueueBackpressureReason,
}

/// One frame accepted by the device.
///
/// Acceptance is not transmission: the driver took ownership of the frame,
/// and neither a completion nor a hardware send is implied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxSubmitReport {
    pub identity: NetQueueIdentity,
    /// Frame length in bytes, as handed to the driver.
    pub len: usize,
}

/// One received frame published to the protocol-side ring.
///
/// Publication means the protocol side can now observe the frame; a failed
/// push keeps the frame on the executor side and is not reported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RxPublishReport {
    pub identity: NetQueueIdentity,
    /// Frame length in bytes.
    pub len: usize,
}

/// Why the protocol executor's poll budget asked it to give up the CPU.
///
/// The discriminants are the reported codes and are part of the event
/// contract: they must not be reordered.  The assertion below pins them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ProtoYieldReason {
    /// The poll-count limit was reached.
    PollCount = 0,
    /// The elapsed-time limit was reached.
    Deadline  = 1,
    /// Both limits were reached at the same check.
    Both      = 2,
}

const _: () = assert!(
    ProtoYieldReason::PollCount as u32 == 0
        && ProtoYieldReason::Deadline as u32 == 1
        && ProtoYieldReason::Both as u32 == 2,
    "the reported yield reasons are part of the event contract"
);

/// One protocol executor yield.
///
/// The yield is a transition of the executor's own scheduling loop, not a
/// protocol poll round: the executor releases CPU ownership because its budget
/// asked for it, and it yields again on the next budget exhaustion whether or
/// not the previous work finished.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtoYieldReport {
    /// CPU the protocol executor runs on.  The executor is pinned to the
    /// protocol owner CPU, so this is its fixed owner rather than a CPU
    /// sampled at report time.
    pub owner_cpu: usize,
    pub reason: ProtoYieldReason,
    /// Whether protocol work was still pending when the budget asked for the
    /// yield.
    pub work_pending: bool,
}

/// Consumer of completed queue poll rounds.
pub type QueuePollObserver = fn(QueuePollReport);

/// Consumer of queue rearm results.
pub type QueueRearmObserver = fn(QueueRearmReport);

/// Consumer of queue backpressure reports.
pub type QueueBackpressureObserver = fn(QueueBackpressureReport);

/// Consumer of accepted TX submissions.
pub type TxSubmitObserver = fn(TxSubmitReport);

/// Consumer of published RX frames.
pub type RxPublishObserver = fn(RxPublishReport);

/// Consumer of protocol executor yields.
pub type ProtoYieldObserver = fn(ProtoYieldReport);

/// One event's observation port: a typed callback slot plus a published gate.
///
/// The port is one process-wide instance per event, installed by the OS adapter
/// after the network runtime is already running (rounds before installation are
/// not reported) and never replaced or removed.  A report costs one flag load
/// while no consumer is active; the flag and the slot are read separately, so a
/// report racing with a gate change may still reach the consumer, whose final
/// gate check remains authoritative.
pub struct ObservationPort<T> {
    observer: AtomicPtr<()>,
    enabled: AtomicBool,
    _marker: PhantomData<fn(T)>,
}

impl<T> ObservationPort<T> {
    pub const fn new() -> Self {
        Self {
            observer: AtomicPtr::new(core::ptr::null_mut()),
            enabled: AtomicBool::new(false),
            _marker: PhantomData,
        }
    }

    /// Installs this port's consumer.
    ///
    /// Reinstalling the same function is harmless; replacing a live consumer is
    /// an invariant violation because reports may concurrently execute it.
    pub fn install(&'static self, observer: fn(T)) {
        let observer = observer as *mut ();
        match self.observer.compare_exchange(
            core::ptr::null_mut(),
            observer,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(installed) => assert_eq!(installed, observer, "observation port already installed"),
        }
    }

    /// Publishes whether this event has active consumers.
    ///
    /// The tracepoint adapter owns the authoritative gate and mirrors it here,
    /// so the runtime can skip a report without querying the tracepoint
    /// registry.
    pub fn publish_gate(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    /// Reports one occurrence to the installed consumer, if any.
    pub(super) fn report(&self, report: T) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        let observer = self.observer.load(Ordering::Acquire);
        if observer.is_null() {
            return;
        }
        // SAFETY: `install` accepts exactly this function-pointer type, and the
        // slot is never replaced or removed.
        let observer = unsafe { core::mem::transmute::<*mut (), fn(T)>(observer) };
        observer(report);
    }
}

static QUEUE_POLL_PORT: ObservationPort<QueuePollReport> = ObservationPort::new();
static QUEUE_REARM_PORT: ObservationPort<QueueRearmReport> = ObservationPort::new();
static QUEUE_BACKPRESSURE_PORT: ObservationPort<QueueBackpressureReport> = ObservationPort::new();
static TX_SUBMIT_PORT: ObservationPort<TxSubmitReport> = ObservationPort::new();
static RX_PUBLISH_PORT: ObservationPort<RxPublishReport> = ObservationPort::new();
static PROTO_YIELD_PORT: ObservationPort<ProtoYieldReport> = ObservationPort::new();

/// Installs the process-wide queue poll consumer.
pub fn install_queue_poll_observer(observer: QueuePollObserver) {
    QUEUE_POLL_PORT.install(observer);
}

/// Publishes whether the queue poll event has active consumers.
pub fn publish_queue_poll_gate(enabled: bool) {
    QUEUE_POLL_PORT.publish_gate(enabled);
}

/// Installs the process-wide queue rearm consumer.
pub fn install_queue_rearm_observer(observer: QueueRearmObserver) {
    QUEUE_REARM_PORT.install(observer);
}

/// Publishes whether the queue rearm event has active consumers.
pub fn publish_queue_rearm_gate(enabled: bool) {
    QUEUE_REARM_PORT.publish_gate(enabled);
}

/// Installs the process-wide queue backpressure consumer.
pub fn install_queue_backpressure_observer(observer: QueueBackpressureObserver) {
    QUEUE_BACKPRESSURE_PORT.install(observer);
}

/// Publishes whether the queue backpressure event has active consumers.
pub fn publish_queue_backpressure_gate(enabled: bool) {
    QUEUE_BACKPRESSURE_PORT.publish_gate(enabled);
}

/// Installs the process-wide TX submit consumer.
pub fn install_tx_submit_observer(observer: TxSubmitObserver) {
    TX_SUBMIT_PORT.install(observer);
}

/// Publishes whether the TX submit event has active consumers.
pub fn publish_tx_submit_gate(enabled: bool) {
    TX_SUBMIT_PORT.publish_gate(enabled);
}

/// Installs the process-wide RX publish consumer.
pub fn install_rx_publish_observer(observer: RxPublishObserver) {
    RX_PUBLISH_PORT.install(observer);
}

/// Publishes whether the RX publish event has active consumers.
pub fn publish_rx_publish_gate(enabled: bool) {
    RX_PUBLISH_PORT.publish_gate(enabled);
}

pub(super) fn report_queue_poll(report: QueuePollReport) {
    QUEUE_POLL_PORT.report(report);
}

pub(super) fn report_queue_rearm(report: QueueRearmReport) {
    QUEUE_REARM_PORT.report(report);
}

pub(super) fn report_queue_backpressure(report: QueueBackpressureReport) {
    QUEUE_BACKPRESSURE_PORT.report(report);
}

pub(super) fn report_tx_submit(report: TxSubmitReport) {
    TX_SUBMIT_PORT.report(report);
}

pub(super) fn report_rx_publish(report: RxPublishReport) {
    RX_PUBLISH_PORT.report(report);
}

/// Installs the process-wide protocol yield consumer.
pub fn install_proto_yield_observer(observer: ProtoYieldObserver) {
    PROTO_YIELD_PORT.install(observer);
}

/// Publishes whether the protocol yield event has active consumers.
pub fn publish_proto_yield_gate(enabled: bool) {
    PROTO_YIELD_PORT.publish_gate(enabled);
}

pub(crate) fn report_proto_yield(report: ProtoYieldReport) {
    PROTO_YIELD_PORT.report(report);
}
