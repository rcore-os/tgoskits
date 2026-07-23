use alloc::sync::Arc;
use core::time::Duration;

use ax_errno::{AxError, AxResult};
use axpoll::IoEvents;
use rdif_serial::{Config, ConfigError, RxErrorFlags, RxFlag, RxSample, SerialEventSet};

use super::{
    RuntimeShared, RxItem,
    control::{CONTROL_QUEUE_CAPACITY, ControlOp},
    ingress::TxFrameCursor,
    spsc::{Consumer as SpscConsumer, Producer as SpscProducer},
};

const RX_BUDGET: usize = 256;
const TX_BUDGET: usize = 64;

pub(super) struct SerialWorker {
    shared: Arc<RuntimeShared>,
    irq_rx: SpscConsumer<RxSample>,
    rx_output: SpscProducer<RxItem>,
    pending_rx: Option<PendingRx>,
    port_rx_ready: bool,
    pending_frame: Option<TxFrameCursor>,
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
        Self {
            shared,
            irq_rx,
            rx_output,
            pending_rx: None,
            port_rx_ready: false,
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
                if outcome.budget_exhausted
                    || (!outcome.blocked && self.shared.bridge.latch.has_pending())
                {
                    continue;
                }
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
                || (!tx_blocked
                    && (self.pending_frame.is_some() || self.shared.ingress.has_pending()))
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
                ControlOp::SetConfig(config) => {
                    let result = self.set_config(config);
                    force_service |= result.is_ok();
                    result
                }
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
        self.port_rx_ready = false;
        {
            let mut port = self.shared.port.lock();
            port.mask_all();
            port.shutdown();
        }
        self.irq_rx.clear();
        self.pending_rx = None;
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
                    RxPath::Port => self.shared.port.lock().read_rx(),
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
            // SAFETY: the worker Release-publishes ring entries before waking
            // task-context waiters.
            unsafe { self.shared.rx_source.wake(IoEvents::IN) };
        }

        if path == RxPath::Port && source_drained {
            let ready = {
                let mut port = self.shared.port.lock();
                rearm_drained_rx(
                    true,
                    self.shared.polling,
                    &mut self.pending_rearm,
                    |sources| port.rearm(sources),
                )
            };
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
