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
        self.requests
            .try_push(request)
            .map_err(|_| NetError::Retry)?;
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
                self.active = false;
                self.deadline_nanos = None;
                Err(NetError::Other(Box::new(error)))
            }
        }
    }

    fn cancel(&mut self) -> Result<(), NetError> {
        if self.active {
            self.requests
                .try_push(ControlRequest::Cancel)
                .map_err(|_| NetError::Retry)?;
            self.active = false;
            self.deadline_nanos = None;
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
