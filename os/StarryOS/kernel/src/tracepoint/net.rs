//! Network event adapters.
//!
//! The network runtime owns the facts and reports them through narrow
//! observation ports; this module owns the `net:*` tracepoint names, the record
//! layouts and the published gates that let the runtime skip reports while no
//! consumer is attached.

use ax_net::{
    ProtoYieldReport, QueueBackpressureReport, QueuePollReport, QueueRearmReport, RxPublishReport,
    TxSubmitReport,
};

/// Converts a runtime value to its recorded width.
///
/// The runtime bounds these values far below `u32::MAX` (device counts, CPU
/// ids, the round budget and its work units), so a saturated field means the
/// runtime contract changed rather than that a meaningful value was truncated.
fn field(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

ax_tracepoint::define_event_trace!(
    queue_poll_round,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(net),
    TP_PROTO(
        discovery_order: u32,
        group_id: u32,
        owner_cpu: u32,
        budget: u32,
        work_units: u32,
        outcome: u32,
    ),
    TP_STRUCT__entry {
        discovery_order: u32,
        group_id: u32,
        owner_cpu: u32,
        budget: u32,
        work_units: u32,
        outcome: u32,
    },
    TP_fast_assign {
        discovery_order: discovery_order,
        group_id: group_id,
        owner_cpu: owner_cpu,
        budget: budget,
        work_units: work_units,
        outcome: outcome,
    },
    TP_ident(__entry),
    TP_printk({
        alloc::format!(
            "discovery_order={} group_id={} owner_cpu={} budget={} work_units={} outcome={}",
            __entry.discovery_order,
            __entry.group_id,
            __entry.owner_cpu,
            __entry.budget,
            __entry.work_units,
            __entry.outcome,
        )
    })
);

/// Reports one completed queue executor poll round.
///
/// The runtime calls this from the queue executor thread.  The values are the
/// port's domain facts; the generated event function re-checks the gate and
/// only then builds the record.
fn on_queue_poll(report: QueuePollReport) {
    trace_queue_poll_round(
        field(report.identity.discovery_order),
        u32::from(report.identity.group_id.get()),
        field(report.identity.owner_cpu),
        field(report.budget),
        field(report.work_units),
        report.outcome as u32,
    );
}

ax_tracepoint::define_event_trace!(
    queue_rearm,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(net),
    TP_PROTO(
        discovery_order: u32,
        group_id: u32,
        owner_cpu: u32,
        outcome: u32,
    ),
    TP_STRUCT__entry {
        discovery_order: u32,
        group_id: u32,
        owner_cpu: u32,
        outcome: u32,
    },
    TP_fast_assign {
        discovery_order: discovery_order,
        group_id: group_id,
        owner_cpu: owner_cpu,
        outcome: outcome,
    },
    TP_ident(__entry),
    TP_printk({
        alloc::format!(
            "discovery_order={} group_id={} owner_cpu={} outcome={}",
            __entry.discovery_order,
            __entry.group_id,
            __entry.owner_cpu,
            __entry.outcome,
        )
    })
);

/// Reports one queue rearm that did not end in the plain idle case.
fn on_queue_rearm(report: QueueRearmReport) {
    trace_queue_rearm(
        field(report.identity.discovery_order),
        u32::from(report.identity.group_id.get()),
        field(report.identity.owner_cpu),
        report.outcome as u32,
    );
}

ax_tracepoint::define_event_trace!(
    queue_backpressure,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(net),
    TP_PROTO(
        discovery_order: u32,
        group_id: u32,
        owner_cpu: u32,
        stage: u32,
        reason: u32,
    ),
    TP_STRUCT__entry {
        discovery_order: u32,
        group_id: u32,
        owner_cpu: u32,
        stage: u32,
        reason: u32,
    },
    TP_fast_assign {
        discovery_order: discovery_order,
        group_id: group_id,
        owner_cpu: owner_cpu,
        stage: stage,
        reason: reason,
    },
    TP_ident(__entry),
    TP_printk({
        alloc::format!(
            "discovery_order={} group_id={} owner_cpu={} stage={} reason={}",
            __entry.discovery_order,
            __entry.group_id,
            __entry.owner_cpu,
            __entry.stage,
            __entry.reason,
        )
    })
);

/// Reports one retryable refusal to proceed.
fn on_queue_backpressure(report: QueueBackpressureReport) {
    trace_queue_backpressure(
        field(report.identity.discovery_order),
        u32::from(report.identity.group_id.get()),
        field(report.identity.owner_cpu),
        report.stage as u32,
        report.reason as u32,
    );
}

ax_tracepoint::define_event_trace!(
    tx_submit,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(net),
    TP_PROTO(
        discovery_order: u32,
        group_id: u32,
        owner_cpu: u32,
        frame_len: u32,
    ),
    TP_STRUCT__entry {
        discovery_order: u32,
        group_id: u32,
        owner_cpu: u32,
        frame_len: u32,
    },
    TP_fast_assign {
        discovery_order: discovery_order,
        group_id: group_id,
        owner_cpu: owner_cpu,
        frame_len: frame_len,
    },
    TP_ident(__entry),
    TP_printk({
        alloc::format!(
            "discovery_order={} group_id={} owner_cpu={} frame_len={}",
            __entry.discovery_order,
            __entry.group_id,
            __entry.owner_cpu,
            __entry.frame_len,
        )
    })
);

/// Reports one frame accepted by the device.
fn on_tx_submit(report: TxSubmitReport) {
    trace_tx_submit(
        field(report.identity.discovery_order),
        u32::from(report.identity.group_id.get()),
        field(report.identity.owner_cpu),
        field(report.len),
    );
}

ax_tracepoint::define_event_trace!(
    rx_publish,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(net),
    TP_PROTO(
        discovery_order: u32,
        group_id: u32,
        owner_cpu: u32,
        frame_len: u32,
    ),
    TP_STRUCT__entry {
        discovery_order: u32,
        group_id: u32,
        owner_cpu: u32,
        frame_len: u32,
    },
    TP_fast_assign {
        discovery_order: discovery_order,
        group_id: group_id,
        owner_cpu: owner_cpu,
        frame_len: frame_len,
    },
    TP_ident(__entry),
    TP_printk({
        alloc::format!(
            "discovery_order={} group_id={} owner_cpu={} frame_len={}",
            __entry.discovery_order,
            __entry.group_id,
            __entry.owner_cpu,
            __entry.frame_len,
        )
    })
);

/// Reports one received frame published to the protocol side.
fn on_rx_publish(report: RxPublishReport) {
    trace_rx_publish(
        field(report.identity.discovery_order),
        u32::from(report.identity.group_id.get()),
        field(report.identity.owner_cpu),
        field(report.len),
    );
}

ax_tracepoint::define_event_trace!(
    proto_yield,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(net),
    TP_PROTO(
        owner_cpu: u32,
        reason: u32,
        work_pending: u32,
    ),
    TP_STRUCT__entry {
        owner_cpu: u32,
        reason: u32,
        work_pending: u32,
    },
    TP_fast_assign {
        owner_cpu: owner_cpu,
        reason: reason,
        work_pending: work_pending,
    },
    TP_ident(__entry),
    TP_printk({
        alloc::format!(
            "owner_cpu={} reason={} work_pending={}",
            __entry.owner_cpu,
            __entry.reason,
            __entry.work_pending,
        )
    })
);

/// Reports one protocol executor yield.
fn on_proto_yield(report: ProtoYieldReport) {
    trace_proto_yield(
        field(report.owner_cpu),
        report.reason as u32,
        u32::from(report.work_pending),
    );
}

/// Installs the observation ports and publishes their initial gates.
///
/// The network runtime is already running here, so rounds that completed
/// before this call are not reported.  Each gate starts from its tracepoint's
/// own state, which is "no callbacks" until a consumer attaches.
pub(super) fn install() {
    ax_net::install_queue_poll_observer(on_queue_poll);
    super::gate::register(&__queue_poll_round, ax_net::publish_queue_poll_gate);
    ax_net::install_queue_rearm_observer(on_queue_rearm);
    super::gate::register(&__queue_rearm, ax_net::publish_queue_rearm_gate);
    ax_net::install_queue_backpressure_observer(on_queue_backpressure);
    super::gate::register(
        &__queue_backpressure,
        ax_net::publish_queue_backpressure_gate,
    );
    ax_net::install_tx_submit_observer(on_tx_submit);
    super::gate::register(&__tx_submit, ax_net::publish_tx_submit_gate);
    ax_net::install_rx_publish_observer(on_rx_publish);
    super::gate::register(&__rx_publish, ax_net::publish_rx_publish_gate);
    ax_net::install_proto_yield_observer(on_proto_yield);
    super::gate::register(&__proto_yield, ax_net::publish_proto_yield_gate);
}
