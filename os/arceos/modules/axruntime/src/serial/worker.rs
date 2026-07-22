use alloc::sync::Arc;
use core::time::Duration;

use ax_errno::{AxError, AxResult};
use axpoll::IoEvents;
use rdif_serial::{Config, ConfigError, RxErrorFlags, RxFlag, RxSample, SerialEventSet};

use super::{
    RuntimeShared, RxItem,
    control::{CONTROL_QUEUE_CAPACITY, ControlOp},
    ingress::TxFrameCursor,
};

const RX_BUDGET: usize = 256;
const TX_BUDGET: usize = 64;

pub(super) struct SerialWorker {
    shared: Arc<RuntimeShared>,
    pending_frame: Option<TxFrameCursor>,
    pending_rearm: SerialEventSet,
    immediate_events: SerialEventSet,
    latched_rx_errors: RxErrorFlags,
}

impl SerialWorker {
    pub(super) fn new(shared: Arc<RuntimeShared>) -> Self {
        Self {
            shared,
            pending_frame: None,
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
                self.shared
                    .stats
                    .add_rx_errors(event.rx_errors.bits().count_ones());
            }

            if events.contains(SerialEventSet::FAULT) {
                self.stop_faulted_port();
            }

            if self.shared.started()
                && (force_service || self.shared.polling || events.has_rx())
                && (self.service_rx() || self.shared.bridge.latch.has_pending())
            {
                continue;
            }

            let tx_needed = self.pending_frame.is_some()
                || self.shared.ingress.has_pending()
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
                || (!tx_blocked
                    && (self.pending_frame.is_some() || self.shared.ingress.has_pending()))
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
        for _ in 0..CONTROL_QUEUE_CAPACITY {
            let Some(command) = self.shared.control.try_pop() else {
                break;
            };
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
                ControlOp::SetConfig(config) => self.set_config(config),
            };
            command.complete(result);
        }
        force_service
    }

    fn start_port(&mut self, config: &Config) -> AxResult {
        if self.shared.started() {
            return Ok(());
        }
        {
            let mut port = self.shared.port.lock();
            port.startup(config).map_err(map_config_error)?;
            port.mask_all();
        }
        self.shared.ingress.start_accepting();
        self.shared.set_started(true);
        self.pending_rearm = SerialEventSet::RX;
        Ok(())
    }

    fn shutdown_port(&mut self) {
        self.shared.set_started(false);
        self.shared.ingress.stop_and_discard();
        self.pending_frame = None;
        self.pending_rearm = SerialEventSet::empty();
        self.immediate_events = SerialEventSet::empty();
        self.latched_rx_errors = RxErrorFlags::empty();
        let mut port = self.shared.port.lock();
        port.mask_all();
        port.shutdown();
    }

    fn set_config(&mut self, config: &Config) -> AxResult {
        if !self.shared.started() {
            return Err(AxError::BadState);
        }
        let result = {
            let mut port = self.shared.port.lock();
            port.mask_all();
            port.set_config(config).map_err(map_config_error)
        };
        self.pending_rearm |= SerialEventSet::RX;
        if self.pending_frame.is_some() || self.shared.ingress.has_pending() {
            self.pending_rearm |= SerialEventSet::TX_SPACE;
        }
        result
    }

    fn stop_faulted_port(&mut self) {
        self.shutdown_port();
        // SAFETY: the maintenance task is task context and publishes the
        // stopped state before waking poll waiters.
        unsafe {
            self.shared.rx_source.wake(IoEvents::ERR | IoEvents::HUP);
            self.shared.tx_source.wake(IoEvents::ERR | IoEvents::HUP);
        }
        self.shared.tx_progress.notify_all(true);
    }

    /// Returns whether RX service must continue before any slower TX work.
    fn service_rx(&mut self) -> bool {
        let mut samples = [RxSample::default(); RX_BUDGET];
        let mut count = 0;
        let (drained, ready) = {
            let mut port = self.shared.port.lock();
            while count < samples.len() {
                let Some(sample) = port.read_rx() else {
                    break;
                };
                samples[count] = sample;
                count += 1;
            }
            let drained = count < samples.len();
            let ready = rearm_drained_rx(
                drained,
                self.shared.polling,
                &mut self.pending_rearm,
                |sources| port.rearm(sources),
            );
            (drained, ready)
        };
        self.immediate_events |= ready;
        let rx = self.shared.rx.clone();
        let rx_source = self.shared.rx_source.clone();
        let mut publisher = RxBatchPublisher::new(rx.as_ref(), || {
            // SAFETY: the worker publishes ring entries before waking task-context waiters.
            unsafe { rx_source.wake(IoEvents::IN) };
        });
        for sample in samples.into_iter().take(count) {
            self.publish_rx_sample(sample, &mut publisher);
        }
        self.shared.stats.add_rx_dropped(publisher.finish());
        if !drained {
            // The source remains masked while the worker drains the next
            // budget. No edge is required to keep the service loop alive.
            self.immediate_events |= SerialEventSet::RX_DATA;
        }
        !drained || !ready.is_empty()
    }

    fn publish_rx_sample(
        &mut self,
        sample: RxSample,
        publisher: &mut RxBatchPublisher<'_, impl FnMut()>,
    ) {
        let latched = core::mem::take(&mut self.latched_rx_errors);
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
        let overrun = sample.overrun || latched.contains(RxErrorFlags::OVERRUN);

        if sample.flag != RxFlag::Normal {
            self.shared.stats.add_rx_errors(1);
        }
        if sample.overrun {
            self.shared.stats.add_rx_errors(1);
        }

        if let Some(byte) = sample.byte {
            self.shared.stats.add_rx_bytes(1);
            publisher.push(RxItem::Byte { byte, flag });
        }
        if overrun {
            publisher.push(RxItem::Overrun);
        }
    }

    fn service_tx(&mut self) -> TxServiceOutcome {
        let mut remaining_budget = TX_BUDGET;
        let mut woke_space = false;
        let mut port = self.shared.port.lock();

        while remaining_budget > 0 {
            if self.pending_frame.is_none() {
                let Some(frame) = self.shared.ingress.pop() else {
                    break;
                };
                self.pending_frame = Some(TxFrameCursor::new(frame));
                woke_space = true;
            }

            let cursor = self.pending_frame.as_mut().unwrap();
            let remaining = cursor.remaining();
            let limit = remaining.len().min(remaining_budget);
            let written = port.write_tx(&remaining[..limit]);
            if written == 0 {
                self.pending_rearm |= SerialEventSet::TX_SPACE;
                drop(port);
                if woke_space {
                    self.shared.publish_tx_space();
                }
                return TxServiceOutcome {
                    blocked: true,
                    budget_exhausted: false,
                };
            }
            cursor.advance(written);
            remaining_budget -= written;
            self.shared.stats.add_tx_bytes(written);
            if cursor.is_complete() {
                self.pending_frame = None;
            }
        }
        drop(port);
        if woke_space {
            self.shared.publish_tx_space();
        }

        TxServiceOutcome {
            blocked: false,
            budget_exhausted: remaining_budget == 0
                && (self.pending_frame.is_some() || self.shared.ingress.has_pending()),
        }
    }

    fn update_tx_idle(&mut self) {
        let worker_empty = self.pending_frame.is_none();
        let hardware_idle = if !self.shared.started() {
            true
        } else {
            self.shared.port.lock().tx_idle()
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
            && !self.shared.ingress.has_pending()
            && self.shared.ingress.is_idle()
        {
            sources.remove(SerialEventSet::TX_SPACE);
        }
        if sources.is_empty() {
            return;
        }

        let ready = self.shared.port.lock().rearm(sources);
        self.pending_rearm |= ready;
        self.immediate_events |= ready;
    }
}

struct RxBatchPublisher<'a, W> {
    channel: &'a super::ingress::RxChannel,
    wake: W,
    published: bool,
    dropped: usize,
}

impl<'a, W: FnMut()> RxBatchPublisher<'a, W> {
    fn new(channel: &'a super::ingress::RxChannel, wake: W) -> Self {
        Self {
            channel,
            wake,
            published: false,
            dropped: 0,
        }
    }

    fn push(&mut self, item: RxItem) {
        if self.channel.push(item).is_err() {
            self.dropped += 1;
            return;
        }
        self.published = true;
    }

    fn finish(mut self) -> usize {
        if self.published {
            (self.wake)();
        }
        self.dropped
    }
}

struct TxServiceOutcome {
    blocked: bool,
    budget_exhausted: bool,
}

fn map_config_error(error: ConfigError) -> AxError {
    match error {
        ConfigError::InvalidBaudrate
        | ConfigError::UnsupportedDataBits
        | ConfigError::UnsupportedStopBits
        | ConfigError::UnsupportedParity => AxError::InvalidInput,
        ConfigError::Timeout => AxError::TimedOut,
        ConfigError::RegisterError => AxError::Io,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rx_batch_wakes_a_reregistering_reader_once() {
        let channel = super::super::ingress::RxChannel::new();
        let mut wake_count = 0;
        let mut publisher = RxBatchPublisher::new(&channel, || wake_count += 1);
        for byte in 0..64 {
            publisher.push(RxItem::Byte {
                byte,
                flag: RxFlag::Normal,
            });
        }
        assert_eq!(publisher.finish(), 0);
        assert_eq!(
            wake_count, 1,
            "one hardware RX batch must publish one reader wakeup",
        );
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
}
