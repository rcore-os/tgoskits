use alloc::sync::Arc;
use core::time::Duration;

use axpoll::IoEvents;
#[cfg(test)]
use rdif_serial::ConfigError;
use rdif_serial::{Config, RxErrorFlags, RxFlag, RxSample, SerialEventSet};

use super::{
    RuntimeIrqBridge, RuntimeShared, RxItem,
    control::{CONTROL_QUEUE_CAPACITY, ControlCommand, ControlOp},
    deactivate_console,
    ingress::TxFrameCursor,
    log_mailbox::{LogReader, LogRecordCursor, LogRecordKind},
    spsc::{Consumer as SpscConsumer, Producer as SpscProducer},
};
use crate::{RuntimeError, RuntimeResult};

const RX_BUDGET: usize = 256;
const TX_BUDGET: usize = 64;

pub(super) struct SerialWorker {
    shared: Arc<RuntimeShared>,
    irq_rx: SpscConsumer<RxSample>,
    rx_output: SpscProducer<RxItem>,
    pending_rx: Option<PendingRx>,
    port_rx_ready: bool,
    pending_frame: Option<TxFrameCursor>,
    log_reader: LogReader,
    pending_log: Option<LogRecordCursor>,
    pending_control: Option<ControlCommand>,
    prefer_log: bool,
    pending_rearm: SerialEventSet,
    immediate_events: SerialEventSet,
    latched_rx_errors: RxErrorFlags,
}

impl SerialWorker {
    pub(super) fn new(
        shared: Arc<RuntimeShared>,
        irq_rx: SpscConsumer<RxSample>,
        rx_output: SpscProducer<RxItem>,
    ) -> Self {
        let log_reader = shared.log_mailbox.reader();
        Self {
            shared,
            irq_rx,
            rx_output,
            pending_rx: None,
            port_rx_ready: false,
            pending_frame: None,
            log_reader,
            pending_log: None,
            pending_control: None,
            prefer_log: false,
            pending_rearm: SerialEventSet::empty(),
            immediate_events: SerialEventSet::empty(),
            latched_rx_errors: RxErrorFlags::empty(),
        }
    }

    pub(super) fn run(mut self) {
        loop {
            self.shared.bridge.notify.drain();
            let force_service = self.process_control_commands();
            let mut events = core::mem::take(&mut self.immediate_events);

            if let Some(event) = self.shared.bridge.latch.take() {
                events |= event.events;
                self.pending_rearm |= event.rearm;
                self.latched_rx_errors |= event.rx_errors;
            }
            if self
                .shared
                .bridge
                .rx_overflow
                .swap(false, core::sync::atomic::Ordering::AcqRel)
            {
                self.latched_rx_errors |= RxErrorFlags::OVERRUN;
            }

            if events.contains(SerialEventSet::FAULT) {
                self.stop_faulted_port();
            }

            let rx_path = if let Some(pending) = self.pending_rx {
                Some(pending.path)
            } else if !self.shared.started() {
                None
            } else if force_service || self.shared.polling || self.port_rx_ready {
                Some(RxPath::Port)
            } else if events.has_rx() || !self.irq_rx.is_empty() {
                Some(RxPath::Irq)
            } else {
                None
            };
            let mut rx_blocked = false;
            if let Some(path) = rx_path {
                if path == RxPath::Port {
                    self.port_rx_ready = false;
                }
                let outcome = self.service_rx(path);
                rx_blocked = outcome.blocked;
                if outcome.budget_exhausted {
                    ax_task::yield_now();
                } else if !outcome.blocked && self.shared.bridge.latch.has_pending() {
                    continue;
                }
            }

            let tx_needed = self.pending_frame.is_some()
                || self.pending_log.is_some()
                || self.shared.ingress.has_pending()
                || self.logs_serviceable()
                || events.has_tx()
                || self.pending_rearm.has_tx();
            let mut budget_exhausted = false;
            let mut tx_blocked = false;
            if self.shared.started() && tx_needed {
                let outcome = self.service_tx();
                budget_exhausted |= outcome.budget_exhausted;
                tx_blocked = outcome.blocked;
            }

            self.update_tx_idle();
            if budget_exhausted {
                ax_task::yield_now();
                continue;
            }
            if self.port_rx_ready {
                continue;
            }

            if self.shared.started() && !self.shared.polling {
                self.rearm_sources();
                if !self.immediate_events.is_empty() {
                    continue;
                }
            }

            if self.shared.bridge.latch.has_pending()
                || self.shared.control.has_pending()
                || (self.pending_control.is_some() && !tx_blocked)
                || (!tx_blocked
                    && (self.pending_frame.is_some()
                        || self.pending_log.is_some()
                        || self.shared.ingress.has_pending()
                        || self.logs_serviceable()))
                || (!rx_blocked
                    && (self.pending_rx.is_some()
                        || (!self.shared.polling && !self.irq_rx.is_empty())))
            {
                continue;
            }

            if self.shared.polling {
                ax_task::sleep(Duration::from_millis(1));
            } else {
                self.shared.bridge.notify.wait();
            }
        }
    }

    fn process_control_commands(&mut self) -> bool {
        let mut force_service = false;
        if let Some(command) = self.pending_control.take() {
            if self.pending_log.is_some() {
                self.pending_control = Some(command);
                return true;
            }
            force_service |= self.execute_control(command);
        }
        for _ in 0..CONTROL_QUEUE_CAPACITY {
            let Some(command) = self.shared.control.try_pop() else {
                break;
            };
            if config_must_wait_for_pending_log(&command.op, self.pending_log.is_some()) {
                self.pending_control = Some(command);
                force_service = true;
                break;
            }
            force_service |= self.execute_control(command);
        }
        force_service
    }

    fn execute_control(&mut self, command: ControlCommand) -> bool {
        let mut force_service = false;
        let result = match &command.op {
            ControlOp::Start(config) => {
                let result = self.start_port(config);
                force_service |= result.is_ok();
                result
            }
            ControlOp::Shutdown => {
                self.shutdown_port();
                Ok(())
            }
            ControlOp::SetConfig(config) => {
                let result = self.set_config(config);
                force_service |= result.is_ok();
                result
            }
            ControlOp::DiscardRx => {
                self.discard_rx();
                Ok(())
            }
            ControlOp::DiscardTx => self.discard_tx(),
        };
        command.complete(result);
        force_service
    }

    fn start_port(&mut self, config: &Config) -> RuntimeResult {
        if self.shared.started() {
            return Ok(());
        }
        self.shared.with_port(|port| {
            port.startup(config)?;
            port.mask_all();
            Ok::<(), rdif_serial::ConfigError>(())
        })?;
        if let Err(err) = self.shared.enable_irq() {
            self.shared.with_port(|port| {
                port.mask_all();
                port.shutdown();
            });
            return Err(err);
        }
        self.shared.ingress.start_accepting();
        self.shared.set_started(true);
        self.pending_rearm = SerialEventSet::RX;
        Ok(())
    }

    fn shutdown_port(&mut self) {
        self.shared.disable_irq();
        self.shared.set_started(false);
        self.shared.ingress.stop_and_discard();
        self.pending_frame = None;
        self.pending_log = None;
        self.pending_rearm = SerialEventSet::empty();
        self.immediate_events = SerialEventSet::empty();
        self.latched_rx_errors = RxErrorFlags::empty();
        self.port_rx_ready = false;
        self.shared.with_port(|port| {
            port.mask_all();
            port.shutdown();
        });
        self.irq_rx.clear();
        self.pending_rx = None;
    }

    fn set_config(&mut self, config: &Config) -> RuntimeResult {
        if !self.shared.started() {
            return Err(RuntimeError::SerialNotStarted);
        }
        let result = self.shared.with_port(|port| {
            port.mask_all();
            port.set_config(config).map_err(RuntimeError::from)
        });
        self.pending_rearm |= SerialEventSet::RX;
        if self.pending_frame.is_some()
            || self.pending_log.is_some()
            || self.shared.ingress.has_pending()
            || self.logs_serviceable()
        {
            self.pending_rearm |= SerialEventSet::TX_SPACE;
        }
        result
    }

    fn stop_faulted_port(&mut self) {
        self.shutdown_port();
        deactivate_console(&self.shared);
        // SAFETY: the maintenance task is task context and publishes the
        // stopped state before waking poll waiters.
        unsafe {
            self.shared.rx_source.wake(IoEvents::ERR | IoEvents::HUP);
            self.shared.tx_source.wake(IoEvents::ERR | IoEvents::HUP);
        }
        self.shared.tx_progress.notify_all(true);
    }

    fn discard_tx(&mut self) -> RuntimeResult {
        let hardware_idle = self.shared.with_port(|port| {
            if !port.discard_tx() {
                return Err(RuntimeError::OperationNotSupported);
            }
            Ok(port.tx_idle())
        })?;
        self.shared.ingress.discard_pending();
        self.pending_frame = None;
        self.pending_rearm.remove(SerialEventSet::TX_SPACE);
        self.immediate_events.remove(SerialEventSet::TX_SPACE);
        if !hardware_idle && !self.shared.polling {
            self.pending_rearm.insert(SerialEventSet::TX_SPACE);
        }

        self.shared.publish_tx_space();
        if self.shared.ingress.mark_idle_if_empty(true, hardware_idle) {
            self.shared.publish_tx_idle();
        }
        Ok(())
    }

    fn discard_rx(&mut self) {
        self.pending_rx = None;
        self.port_rx_ready = false;
        self.latched_rx_errors = RxErrorFlags::empty();
        self.pending_rearm.remove(SerialEventSet::RX);
        self.immediate_events.remove(SerialEventSet::RX);

        let shared = self.shared.clone();
        shared.with_port(|port| {
            discard_rx_sources(port, &mut self.irq_rx, &self.shared.bridge);
        });

        if !self.shared.polling {
            self.pending_rearm.insert(SerialEventSet::RX);
        }
    }

    fn service_rx(&mut self, path: RxPath) -> RxServiceOutcome {
        let mut processed = 0;
        let mut published = false;
        let mut blocked = false;
        let mut source_drained = false;

        while processed < RX_BUDGET {
            let sample = if let Some(pending) = self.pending_rx.take() {
                debug_assert_eq!(pending.path, path);
                pending.sample
            } else {
                let next = match path {
                    RxPath::Irq => self.irq_rx.pop(),
                    RxPath::Port => self.shared.with_port(|port| port.read_rx()),
                };
                let Some(sample) = next else {
                    source_drained = true;
                    break;
                };
                sample
            };

            let normalized =
                match prepare_rx_output(&self.rx_output, sample, self.latched_rx_errors) {
                    Ok(normalized) => normalized,
                    Err(sample) => {
                        self.pending_rx = Some(PendingRx { path, sample });
                        let shared = self.shared.clone();
                        defer_rx_for_output_pressure(
                            self.shared.polling,
                            &mut self.pending_rearm,
                            |sources| shared.with_port(|port| port.mask(sources)),
                        );
                        blocked = true;
                        break;
                    }
                };

            self.latched_rx_errors = RxErrorFlags::empty();
            if normalized.flag != RxFlag::Normal {
                self.shared.stats.add_rx_errors(1);
            }
            if normalized.overrun {
                self.shared.stats.add_rx_errors(1);
            }
            if let Some(byte) = normalized.byte {
                self.shared.stats.add_rx_bytes(1);
                self.rx_output
                    .push(RxItem::Byte {
                        byte,
                        flag: normalized.flag,
                    })
                    .expect("RX output capacity was checked");
                published = true;
            }
            if normalized.overrun {
                self.rx_output
                    .push(RxItem::Overrun)
                    .expect("RX output capacity was checked");
                published = true;
            }
            processed += 1;
        }

        if published {
            self.shared.rx_progress.notify_all(true);
            // SAFETY: the worker Release-publishes ring entries before waking
            // task-context waiters.
            unsafe { self.shared.rx_source.wake(IoEvents::IN) };
        }

        if path == RxPath::Port && source_drained {
            let shared = self.shared.clone();
            let ready = shared.with_port(|port| {
                rearm_drained_rx(
                    true,
                    self.shared.polling,
                    &mut self.pending_rearm,
                    |sources| port.rearm(sources),
                )
            });
            if ready.has_rx() {
                self.port_rx_ready = true;
            }
        } else if path == RxPath::Port {
            self.port_rx_ready = true;
        }

        let source_pending = self.pending_rx.is_some()
            || match path {
                RxPath::Irq => !self.irq_rx.is_empty(),
                RxPath::Port => !source_drained,
            };
        RxServiceOutcome {
            blocked,
            budget_exhausted: !blocked && processed == RX_BUDGET && source_pending,
        }
    }

    fn service_tx(&mut self) -> TxServiceOutcome {
        let mut remaining_budget = TX_BUDGET;
        let mut woke_space = false;
        let mut blocked = false;
        while remaining_budget > 0 {
            let Some(source) = self.select_tx_source(&mut woke_space) else {
                break;
            };
            let remaining = match source {
                PendingTxSource::Tty => self.pending_frame.as_ref().unwrap().remaining(),
                PendingTxSource::Log => self.pending_log.as_ref().unwrap().remaining(),
            };
            let limit = remaining.len().min(remaining_budget);
            let shared = self.shared.clone();
            let written = shared.with_port(|port| port.write_tx(&remaining[..limit]));
            if written == 0 {
                self.pending_rearm |= SerialEventSet::TX_SPACE;
                blocked = true;
                break;
            }
            match source {
                PendingTxSource::Tty => {
                    let cursor = self.pending_frame.as_mut().unwrap();
                    cursor.advance(written);
                    if cursor.is_complete() {
                        self.pending_frame = None;
                    }
                }
                PendingTxSource::Log => {
                    let cursor = self.pending_log.as_mut().unwrap();
                    cursor.advance(written);
                    if cursor.is_complete() {
                        self.pending_log = None;
                    }
                }
            }
            remaining_budget -= written;
            self.shared.stats.add_tx_bytes(written);
        }
        if woke_space {
            self.shared.publish_tx_space();
        }

        TxServiceOutcome {
            blocked,
            budget_exhausted: !blocked
                && remaining_budget == 0
                && (self.pending_frame.is_some()
                    || self.pending_log.is_some()
                    || self.shared.ingress.has_pending()
                    || self.logs_serviceable()),
        }
    }

    fn select_tx_source(&mut self, woke_space: &mut bool) -> Option<PendingTxSource> {
        if self.pending_frame.is_some() {
            return Some(PendingTxSource::Tty);
        }
        if self.pending_log.is_some() {
            return Some(PendingTxSource::Log);
        }
        if self.prefer_log && self.try_load_log() {
            self.prefer_log = false;
            return Some(PendingTxSource::Log);
        }
        if let Some(frame) = self.shared.ingress.pop() {
            self.pending_frame = Some(TxFrameCursor::new(frame));
            self.prefer_log = true;
            *woke_space = true;
            return Some(PendingTxSource::Tty);
        }
        if self.try_load_log() {
            self.prefer_log = false;
            return Some(PendingTxSource::Log);
        }
        None
    }

    fn try_load_log(&mut self) -> bool {
        if !log_extraction_allowed(
            self.shared
                .log_barriers
                .load(core::sync::atomic::Ordering::Acquire),
            self.pending_control.is_some(),
        ) {
            return false;
        }
        let Some(consumed) = self.log_reader.take(self.shared.index) else {
            return false;
        };
        self.shared
            .stats
            .add_log_sequence_gaps(consumed.sequence_gap);
        self.shared.stats.observe_log_record(
            consumed.record.cpu_id(),
            consumed.record.timestamp_nanos(),
            consumed.record.task_id().is_some(),
            consumed.record.kind() == LogRecordKind::Log,
            consumed.record.is_truncated(),
        );
        self.pending_log = Some(LogRecordCursor::new(consumed.record));
        true
    }

    fn logs_serviceable(&self) -> bool {
        log_extraction_allowed(
            self.shared
                .log_barriers
                .load(core::sync::atomic::Ordering::Acquire),
            self.pending_control.is_some(),
        ) && self.shared.log_mailbox.has_pending_for(self.shared.index)
    }

    fn update_tx_idle(&mut self) {
        let drain_active = self
            .shared
            .log_barriers
            .load(core::sync::atomic::Ordering::Acquire)
            != 0;
        let worker_empty =
            self.pending_frame.is_none() && (!drain_active || self.pending_log.is_none());
        let hardware_idle = if !self.shared.started() {
            true
        } else {
            self.shared.with_port(|port| port.tx_idle())
        };
        if !hardware_idle && !self.shared.polling {
            self.pending_rearm |= SerialEventSet::TX_SPACE;
        }
        if self
            .shared
            .ingress
            .mark_idle_if_empty(worker_empty, hardware_idle)
        {
            self.shared.publish_tx_idle();
        }
    }

    fn rearm_sources(&mut self) {
        let mut sources = core::mem::take(&mut self.pending_rearm);
        if self.pending_frame.is_none()
            && self.pending_log.is_none()
            && !self.shared.ingress.has_pending()
            && !self.logs_serviceable()
            && self.shared.ingress.is_idle()
        {
            sources.remove(SerialEventSet::TX_SPACE);
        }
        if sources.is_empty() {
            return;
        }

        let ready = self.shared.with_port(|port| port.rearm(sources));
        self.pending_rearm |= ready;
        self.immediate_events |= ready;
    }
}

#[derive(Clone, Copy)]
enum PendingTxSource {
    Tty,
    Log,
}

fn config_must_wait_for_pending_log(op: &ControlOp, pending_log: bool) -> bool {
    pending_log && matches!(op, ControlOp::SetConfig(_))
}

const fn log_extraction_allowed(active_barriers: usize, pending_control: bool) -> bool {
    active_barriers == 0 && !pending_control
}

fn discard_rx_sources(
    port: &mut dyn rdif_serial::UartPort,
    irq_rx: &mut SpscConsumer<RxSample>,
    bridge: &RuntimeIrqBridge,
) {
    port.discard_rx();
    irq_rx.clear();
    bridge.latch.discard_rx();
    bridge
        .rx_overflow
        .store(false, core::sync::atomic::Ordering::Release);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RxPath {
    Irq,
    Port,
}

#[derive(Clone, Copy)]
struct PendingRx {
    path: RxPath,
    sample: RxSample,
}

struct NormalizedRx {
    byte: Option<u8>,
    flag: RxFlag,
    overrun: bool,
}

impl NormalizedRx {
    fn output_items(&self) -> usize {
        usize::from(self.byte.is_some()) + usize::from(self.overrun)
    }
}

fn normalize_rx(sample: RxSample, latched: RxErrorFlags) -> NormalizedRx {
    let flag = if sample.flag != RxFlag::Normal {
        sample.flag
    } else if latched.contains(RxErrorFlags::BREAK) {
        RxFlag::Break
    } else if latched.contains(RxErrorFlags::PARITY) {
        RxFlag::Parity
    } else if latched.contains(RxErrorFlags::FRAMING) {
        RxFlag::Framing
    } else {
        RxFlag::Normal
    };
    NormalizedRx {
        byte: sample.byte,
        flag,
        overrun: sample.overrun || latched.contains(RxErrorFlags::OVERRUN),
    }
}

fn prepare_rx_output(
    output: &SpscProducer<RxItem>,
    sample: RxSample,
    latched: RxErrorFlags,
) -> Result<NormalizedRx, RxSample> {
    let normalized = normalize_rx(sample, latched);
    if output.write_room() < normalized.output_items() {
        Err(sample)
    } else {
        Ok(normalized)
    }
}

struct RxServiceOutcome {
    blocked: bool,
    budget_exhausted: bool,
}

struct TxServiceOutcome {
    blocked: bool,
    budget_exhausted: bool,
}

fn rearm_drained_rx(
    drained: bool,
    polling: bool,
    pending_rearm: &mut SerialEventSet,
    rearm: impl FnOnce(SerialEventSet) -> SerialEventSet,
) -> SerialEventSet {
    if !drained || polling {
        return SerialEventSet::empty();
    }
    let sources = *pending_rearm & SerialEventSet::RX;
    if sources.is_empty() {
        return SerialEventSet::empty();
    }
    pending_rearm.remove(sources);
    let ready = rearm(sources);
    *pending_rearm |= ready;
    ready
}

fn defer_rx_for_output_pressure(
    polling: bool,
    pending_rearm: &mut SerialEventSet,
    mask: impl FnOnce(SerialEventSet),
) {
    if polling {
        return;
    }
    mask(SerialEventSet::RX);
    *pending_rearm |= SerialEventSet::RX;
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    struct SourcePort {
        source_pending: Arc<AtomicBool>,
    }

    impl rdif_serial::UartPort for SourcePort {
        fn startup(&mut self, _config: &Config) -> Result<(), ConfigError> {
            Ok(())
        }

        fn shutdown(&mut self) {}

        fn set_config(&mut self, _config: &Config) -> Result<(), ConfigError> {
            Ok(())
        }

        fn read_rx(&mut self) -> Option<RxSample> {
            self.source_pending
                .swap(false, Ordering::AcqRel)
                .then_some(RxSample {
                    byte: Some(b'h'),
                    ..RxSample::default()
                })
        }

        fn discard_rx(&mut self) {
            self.source_pending.store(false, Ordering::Release);
        }

        fn write_tx(&mut self, _bytes: &[u8]) -> usize {
            0
        }

        fn discard_tx(&mut self) -> bool {
            true
        }

        fn tx_idle(&mut self) -> bool {
            true
        }

        fn mask(&mut self, _sources: SerialEventSet) {}

        fn mask_all(&mut self) {}

        fn rearm(&mut self, _sources: SerialEventSet) -> SerialEventSet {
            SerialEventSet::empty()
        }
    }

    #[test]
    fn discard_rx_clears_hardware_and_irq_sources_before_returning() {
        let source_pending = Arc::new(AtomicBool::new(true));
        let mut port: Box<dyn rdif_serial::UartPort> = Box::new(SourcePort {
            source_pending: source_pending.clone(),
        });
        let (mut irq_tx, mut irq_rx) = super::super::spsc::channel(2);
        irq_tx
            .push(RxSample {
                byte: Some(b'i'),
                ..RxSample::default()
            })
            .unwrap();
        let bridge = RuntimeIrqBridge::new();
        bridge.rx_overflow.store(true, Ordering::Release);
        bridge.latch.publish(rdif_serial::SerialIrqEvent {
            events: SerialEventSet::RX_DATA | SerialEventSet::TX_SPACE,
            rx_errors: RxErrorFlags::OVERRUN,
            rearm: SerialEventSet::RX | SerialEventSet::TX_SPACE,
        });

        discard_rx_sources(&mut *port, &mut irq_rx, &bridge);

        assert!(!source_pending.load(Ordering::Acquire));
        assert!(port.read_rx().is_none());
        assert!(irq_rx.pop().is_none());
        assert!(!bridge.rx_overflow.load(Ordering::Acquire));
        let event = bridge.latch.take().unwrap();
        assert_eq!(event.events, SerialEventSet::TX_SPACE);
        assert!(event.rx_errors.is_empty());
        assert_eq!(event.rearm, SerialEventSet::TX_SPACE);
    }

    #[test]
    fn normalized_sample_reserves_byte_and_overrun_slots_together() {
        let normalized = normalize_rx(
            RxSample {
                byte: Some(b'x'),
                flag: RxFlag::Normal,
                overrun: false,
            },
            RxErrorFlags::PARITY | RxErrorFlags::OVERRUN,
        );
        assert_eq!(normalized.byte, Some(b'x'));
        assert_eq!(normalized.flag, RxFlag::Parity);
        assert!(normalized.overrun);
        assert_eq!(normalized.output_items(), 2);
    }

    #[test]
    fn full_subscription_ring_keeps_sample_pending_until_space_is_released() {
        let (mut output, mut subscription) = super::super::spsc::channel(1);
        output.push(RxItem::Overrun).unwrap();
        let sample = RxSample {
            byte: Some(b'x'),
            ..RxSample::default()
        };

        assert!(prepare_rx_output(&output, sample, RxErrorFlags::empty()).is_err());
        assert_eq!(subscription.pop(), Some(RxItem::Overrun));
        let prepared = prepare_rx_output(&output, sample, RxErrorFlags::empty()).unwrap();
        assert_eq!(prepared.byte, Some(b'x'));
    }

    #[test]
    fn full_subscription_ring_masks_rx_until_space_is_released() {
        let mut pending_rearm = SerialEventSet::empty();
        let mut masked = SerialEventSet::empty();

        defer_rx_for_output_pressure(false, &mut pending_rearm, |sources| {
            masked |= sources;
        });

        assert_eq!(masked, SerialEventSet::RX);
        assert_eq!(pending_rearm, SerialEventSet::RX);
    }

    #[test]
    fn exhausted_rx_budget_keeps_source_masked() {
        let mut rearm = SerialEventSet::RX;
        let mut called = false;

        let ready = rearm_drained_rx(false, false, &mut rearm, |_| {
            called = true;
            SerialEventSet::empty()
        });
        assert!(ready.is_empty());
        assert!(!called);
        assert_eq!(rearm, SerialEventSet::RX);
    }

    #[test]
    fn drained_rx_rearms_and_retains_immediately_ready_source() {
        let mut pending = SerialEventSet::RX | SerialEventSet::TX_SPACE;
        let ready = rearm_drained_rx(true, false, &mut pending, |sources| {
            assert_eq!(sources, SerialEventSet::RX);
            SerialEventSet::RX_DATA
        });
        assert_eq!(ready, SerialEventSet::RX_DATA);
        assert_eq!(pending, SerialEventSet::RX_DATA | SerialEventSet::TX_SPACE);
    }

    #[test]
    fn polling_rx_never_rearms_hardware_sources() {
        let mut pending = SerialEventSet::RX;
        let mut called = false;
        let ready = rearm_drained_rx(true, true, &mut pending, |_| {
            called = true;
            SerialEventSet::empty()
        });
        assert!(ready.is_empty());
        assert!(!called);
        assert_eq!(pending, SerialEventSet::RX);
    }

    #[test]
    fn configuration_waits_for_the_current_log_record() {
        assert!(config_must_wait_for_pending_log(
            &ControlOp::SetConfig(Config::new()),
            true
        ));
        assert!(!config_must_wait_for_pending_log(
            &ControlOp::SetConfig(Config::new()),
            false
        ));
        assert!(!config_must_wait_for_pending_log(
            &ControlOp::DiscardRx,
            true
        ));
    }

    #[test]
    fn termios_barrier_blocks_new_log_extraction_until_drop() {
        assert!(log_extraction_allowed(0, false));
        assert!(!log_extraction_allowed(1, false));
        assert!(!log_extraction_allowed(0, true));
    }
}
