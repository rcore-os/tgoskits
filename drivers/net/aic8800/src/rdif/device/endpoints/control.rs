use alloc::{boxed::Box, sync::Arc};
use core::time::Duration;

use rdif_eth::{
    NetControlEndpoint, NetError, WifiControl, WifiControlProgress, WifiOperation, WifiTransaction,
};
use ringbuf::traits::{Consumer, Producer};

use crate::{
    AicError, ControlRequest, Entropy, Pmk, SdioFailure,
    rdif::device::{MacAddressState, WifiProgressReceiver, WifiProgressSignal, WifiRequestSender},
};

pub(super) struct AicNetControl {
    mac: Arc<MacAddressState>,
}

impl AicNetControl {
    pub(super) fn new(mac: Arc<MacAddressState>) -> Self {
        Self { mac }
    }
}

impl NetControlEndpoint for AicNetControl {
    fn mac_address(&mut self) -> Result<[u8; 6], NetError> {
        Ok(self.mac.load())
    }
}

pub(super) struct AicWifiControl {
    requests: WifiRequestSender,
    progress: WifiProgressReceiver,
    progress_signal: Arc<WifiProgressSignal>,
    startup: Option<WifiTransaction>,
    control_timeout: Duration,
    deadline_nanos: Option<u64>,
    active: bool,
    connect_retry: Option<ConnectRetry>,
}

// The bare-metal attachment has no supplicant to reconnect after an ordinary
// association refusal. Retry only unspecified association failure, with the
// initial supplicant backoffs, within the original control deadline.
const CONNECT_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_millis(100), Duration::from_millis(500)];

struct ConnectRetry {
    request: ControlRequest,
    attempts: usize,
    retry_at: Option<u64>,
}

impl AicWifiControl {
    pub(super) fn new(
        requests: WifiRequestSender,
        progress: WifiProgressReceiver,
        progress_signal: Arc<WifiProgressSignal>,
        startup: Option<WifiTransaction>,
        control_timeout: Duration,
    ) -> Self {
        Self {
            requests,
            progress,
            progress_signal,
            startup,
            control_timeout,
            deadline_nanos: None,
            active: false,
            connect_retry: None,
        }
    }
}

impl WifiControl for AicWifiControl {
    fn start(
        &mut self,
        operation: &WifiOperation,
        now_nanos: u64,
    ) -> Result<WifiControlProgress, NetError> {
        if self.active {
            return Err(NetError::Retry);
        }
        let request = map_wifi_operation(operation)?;
        let connect_retry =
            matches!(&request, ControlRequest::Connect { .. }).then(|| ConnectRetry {
                request: request.clone(),
                attempts: 0,
                retry_at: None,
            });
        self.requests
            .try_push(request)
            .map_err(|_| NetError::Retry)?;
        self.connect_retry = connect_retry;
        self.active = true;
        let timeout = u64::try_from(self.control_timeout.as_nanos()).unwrap_or(u64::MAX);
        let deadline_nanos = now_nanos.saturating_add(timeout);
        self.deadline_nanos = Some(deadline_nanos);
        Ok(WifiControlProgress::WaitForInterruptUntil { deadline_nanos })
    }

    fn advance(&mut self, now_nanos: u64) -> Result<WifiControlProgress, NetError> {
        if !self.active {
            return Err(NetError::InvalidParts);
        }
        let deadline_nanos = self.deadline_nanos.ok_or(NetError::InvalidParts)?;
        if now_nanos >= deadline_nanos {
            return Err(NetError::Other(Box::new(AicError::Sdio(
                SdioFailure::Timeout,
            ))));
        }
        if let Some(retry) = &mut self.connect_retry
            && let Some(retry_at) = retry.retry_at
        {
            if now_nanos < retry_at {
                return Ok(WifiControlProgress::RetryAt {
                    deadline_nanos: retry_at,
                });
            }
            self.requests
                .try_push(retry.request.clone())
                .map_err(|_| NetError::Retry)?;
            retry.retry_at = None;
            return Ok(WifiControlProgress::WaitForInterruptUntil { deadline_nanos });
        }
        let Some(progress) = self.progress.try_pop() else {
            return Ok(WifiControlProgress::WaitForInterruptUntil { deadline_nanos });
        };
        self.progress_signal.consume();
        if matches!(progress, Ok(WifiControlProgress::Complete) | Err(_)) {
            log::info!("[wifi] control result consumed by network runtime");
        }
        match progress {
            Ok(WifiControlProgress::Complete) => {
                self.active = false;
                self.deadline_nanos = None;
                self.connect_retry = None;
                Ok(WifiControlProgress::Complete)
            }
            Ok(WifiControlProgress::WaitForInterrupt) => {
                Ok(WifiControlProgress::WaitForInterruptUntil { deadline_nanos })
            }
            Ok(WifiControlProgress::WaitForInterruptUntil {
                deadline_nanos: inner_deadline,
            }) => Ok(WifiControlProgress::WaitForInterruptUntil {
                deadline_nanos: inner_deadline.min(deadline_nanos),
            }),
            Ok(WifiControlProgress::RetryAt {
                deadline_nanos: inner_deadline,
            }) => Ok(WifiControlProgress::RetryAt {
                deadline_nanos: inner_deadline.min(deadline_nanos),
            }),
            Err(error) => {
                if matches!(
                    error,
                    AicError::FirmwareRejected {
                        message_id: crate::lmac::SM_CONNECT_IND,
                        status: 1,
                    }
                ) && let Some(retry) = &mut self.connect_retry
                    && let Some(delay) = CONNECT_RETRY_DELAYS.get(retry.attempts)
                {
                    let retry_at = now_nanos.saturating_add(delay.as_nanos() as u64);
                    if retry_at < deadline_nanos {
                        retry.attempts += 1;
                        retry.retry_at = Some(retry_at);
                        log::warn!(
                            "[wifi] association refused; reconnect attempt {} in {:?}",
                            retry.attempts,
                            delay
                        );
                        return Ok(WifiControlProgress::RetryAt {
                            deadline_nanos: retry_at,
                        });
                    }
                }
                self.active = false;
                self.deadline_nanos = None;
                self.connect_retry = None;
                Err(NetError::Other(Box::new(error)))
            }
        }
    }

    fn cancel(&mut self) -> Result<(), NetError> {
        if self.active {
            let waiting_to_reconnect = self
                .connect_retry
                .as_ref()
                .is_some_and(|retry| retry.retry_at.is_some());
            if !waiting_to_reconnect {
                self.requests
                    .try_push(ControlRequest::Cancel)
                    .map_err(|_| NetError::Retry)?;
            }
            self.active = false;
            self.deadline_nanos = None;
            self.connect_retry = None;
        }
        Ok(())
    }

    fn startup_transaction(&self) -> Option<WifiTransaction> {
        self.startup.clone()
    }
}

fn map_wifi_operation(operation: &WifiOperation) -> Result<ControlRequest, NetError> {
    match operation {
        WifiOperation::Connect { ssid, pmk, entropy } => {
            if pmk.is_some() && entropy.is_none() {
                return Err(NetError::Other(Box::new(AicError::EntropyUnavailable)));
            }
            Ok(ControlRequest::Connect {
                ssid: ssid.as_bytes().to_vec(),
                pmk: pmk.as_ref().map(|pmk| Pmk::new(*pmk.bytes())),
                entropy: entropy.map(Entropy::new),
            })
        }
        WifiOperation::Disconnect => Ok(ControlRequest::Disconnect),
        WifiOperation::StartOpenAccessPoint { ssid, channel } => {
            Ok(ControlRequest::StartOpenAccessPoint {
                ssid: ssid.clone(),
                channel: *channel,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use ringbuf::traits::Consumer;

    use super::*;
    use crate::rdif::device::WifiChannels;

    #[test]
    fn transient_association_retries_are_delayed_bounded_and_preserve_the_request() {
        let wifi = WifiChannels::new();
        let mut requests = wifi.requests_rx;
        let mut progress = wifi.progress_tx;
        let mut control = AicWifiControl::new(
            wifi.requests_tx,
            wifi.progress_rx,
            wifi.progress_signal,
            None,
            Duration::from_secs(10),
        );
        let operation = WifiTransaction::connect_wpa2_pmk_with_entropy(
            "network",
            rdif_eth::Wpa2Pmk::new([0x21; 32]),
            [0x34; 32],
        );
        let rejected = AicError::FirmwareRejected {
            message_id: crate::lmac::SM_CONNECT_IND,
            status: 1,
        };
        control.start(operation.operation(), 0).unwrap();
        let request = requests.try_pop().unwrap();
        let mut now = 1;
        for _ in 0..2 {
            progress.try_push(Err(rejected.clone())).unwrap();
            let WifiControlProgress::RetryAt { deadline_nanos } = control.advance(now).unwrap()
            else {
                panic!("association failure must wait before reconnecting");
            };
            assert!(deadline_nanos > now);
            assert_eq!(
                control.advance(deadline_nanos - 1).unwrap(),
                WifiControlProgress::RetryAt { deadline_nanos }
            );
            assert!(requests.try_pop().is_none());
            assert!(matches!(
                control.advance(deadline_nanos).unwrap(),
                WifiControlProgress::WaitForInterruptUntil {
                    deadline_nanos: 10_000_000_000,
                }
            ));
            assert_eq!(requests.try_pop(), Some(request.clone()));
            now = deadline_nanos + 1;
        }
        progress.try_push(Err(rejected.clone())).unwrap();
        let Err(NetError::Other(error)) = control.advance(now) else {
            panic!("association retry budget must return the final failure");
        };
        assert_eq!(error.downcast_ref::<AicError>(), Some(&rejected));
        assert!(requests.try_pop().is_none());

        control.start(operation.operation(), now).unwrap();
        requests.try_pop().unwrap();
        progress
            .try_push(Ok(WifiControlProgress::Complete))
            .unwrap();
        assert_eq!(control.advance(now).unwrap(), WifiControlProgress::Complete);
    }

    #[test]
    fn connect_backoff_respects_cancellation_deadline_and_permanent_errors() {
        let wifi = WifiChannels::new();
        let mut requests = wifi.requests_rx;
        let mut progress = wifi.progress_tx;
        let mut control = AicWifiControl::new(
            wifi.requests_tx,
            wifi.progress_rx,
            wifi.progress_signal,
            None,
            Duration::from_secs(1),
        );
        let operation = WifiTransaction::connect_open("network");
        let rejected = AicError::FirmwareRejected {
            message_id: crate::lmac::SM_CONNECT_IND,
            status: 1,
        };
        control.start(operation.operation(), 0).unwrap();
        requests.try_pop().unwrap();
        progress.try_push(Err(rejected.clone())).unwrap();
        assert!(matches!(
            control.advance(1),
            Ok(WifiControlProgress::RetryAt { .. })
        ));
        control.cancel().unwrap();
        // The core already completed the rejected attempt. A Cancel sent now
        // would target no active operation and turn recovery into a core error.
        assert!(requests.try_pop().is_none());

        for error in [
            AicError::WpaMic,
            AicError::Sdio(SdioFailure::Crc),
            AicError::FirmwareRejected {
                message_id: crate::lmac::SM_CONNECT_IND,
                status: 13,
            },
        ] {
            control.start(operation.operation(), 0).unwrap();
            requests.try_pop().unwrap();
            progress.try_push(Err(error.clone())).unwrap();
            let Err(NetError::Other(actual)) = control.advance(1) else {
                panic!("non-transient errors must be returned without retrying");
            };
            assert_eq!(actual.downcast_ref::<AicError>(), Some(&error));
            assert!(requests.try_pop().is_none());
        }

        control.start(operation.operation(), 0).unwrap();
        requests.try_pop().unwrap();
        progress.try_push(Err(rejected)).unwrap();
        assert!(matches!(
            control.advance(999_999_999),
            Err(NetError::Other(_))
        ));
        assert!(requests.try_pop().is_none());
    }

    #[test]
    fn control_wait_is_bounded_by_the_injected_monotonic_deadline() {
        let wifi = WifiChannels::new();
        let mut requests = wifi.requests_rx;
        let mut control = AicWifiControl::new(
            wifi.requests_tx,
            wifi.progress_rx,
            wifi.progress_signal,
            None,
            Duration::from_nanos(10),
        );

        assert_eq!(
            control.start(&WifiOperation::Disconnect, 100).unwrap(),
            WifiControlProgress::WaitForInterruptUntil {
                deadline_nanos: 110,
            }
        );
        assert_eq!(
            control.advance(109).unwrap(),
            WifiControlProgress::WaitForInterruptUntil {
                deadline_nanos: 110,
            }
        );
        assert!(matches!(control.advance(110), Err(NetError::Other(_))));

        control.cancel().unwrap();
        assert_eq!(requests.try_pop(), Some(ControlRequest::Disconnect));
        assert_eq!(requests.try_pop(), Some(ControlRequest::Cancel));
    }
}
