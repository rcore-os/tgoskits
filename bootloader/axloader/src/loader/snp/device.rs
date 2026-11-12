//! SNP owns submitted packet storage until GetStatus returns the same pointer.

use alloc::{boxed::Box, vec, vec::Vec};

use httpboot_protocol::MacAddress;
use smoltcp::{
    phy,
    time::{Duration, Instant},
};
use uefi::{
    Status,
    boot::{self, ScopedProtocol},
    proto::network::snp::{NetworkState, ReceiveFlags, SimpleNetwork},
};

use super::{admission::SynBackpressure, clock::Clock};

const MAX_FRAME_BYTES: usize = 2048;
const MAX_TX_BUFFERS: usize = 8;
const RX_BUDGET: usize = 16;

struct TransmitBuffer {
    bytes: Box<[u8]>,
    submitted: bool,
}

pub(super) struct Device {
    snp: ScopedProtocol<SimpleNetwork>,
    receive: Box<[u8]>,
    transmit: Vec<TransmitBuffer>,
    frame_bytes: usize,
    receive_budget: usize,
    transmit_budget: usize,
    failure: Option<Status>,
    syn_backpressure: Option<SynBackpressure>,
    active: bool,
    clock: Clock,
}

impl Device {
    pub(super) fn open(clock: Clock) -> uefi::Result<Self> {
        for handle in boot::find_handles::<SimpleNetwork>()? {
            let snp = match boot::open_protocol_exclusive::<SimpleNetwork>(handle) {
                Ok(snp) => snp,
                Err(error) => {
                    crate::logln!("loader_snp_open_failed: handle={handle:?} error={error:?}");
                    continue;
                }
            };
            let mode = snp.mode();
            if mode.hw_address_size != 6 || mode.media_header_size != 14 {
                continue;
            }
            let frame_bytes = usize::try_from(mode.max_packet_size)
                .ok()
                .and_then(|bytes| bytes.checked_add(14))
                .filter(|bytes| *bytes <= MAX_FRAME_BYTES && *bytes >= 590);
            let Some(frame_bytes) = frame_bytes else {
                continue;
            };
            if !matches!(
                mode.state,
                NetworkState::STOPPED | NetworkState::STARTED | NetworkState::INITIALIZED
            ) {
                continue;
            }
            let count = if snp.mode().multiple_tx_supported.into() {
                MAX_TX_BUFFERS
            } else {
                1
            };
            let mut device = Self {
                snp,
                receive: vec![0; MAX_FRAME_BYTES].into_boxed_slice(),
                transmit: (0..count)
                    .map(|_| TransmitBuffer {
                        bytes: vec![0; MAX_FRAME_BYTES].into_boxed_slice(),
                        submitted: false,
                    })
                    .collect(),
                frame_bytes,
                receive_budget: RX_BUDGET,
                transmit_budget: MAX_TX_BUFFERS,
                failure: None,
                syn_backpressure: None,
                active: true,
                clock,
            };
            // Establish the cleanup owner before the first state-changing
            // firmware call. Every failed start/reset/init path then reaches
            // the same stop-or-reset policy while guards and buffers are live.
            device.initialize()?;
            device.configure_filters()?;
            device.reap()?;
            return Ok(device);
        }
        Err(Status::NOT_FOUND.into())
    }

    fn initialize(&mut self) -> uefi::Result<()> {
        match self.snp.mode().state {
            NetworkState::STOPPED => self
                .snp
                .start()
                .inspect_err(|error| crate::logln!("loader_snp_start_failed: {error:?}"))?,
            NetworkState::INITIALIZED => self.snp.shutdown().inspect_err(|error| {
                crate::logln!("loader_snp_shutdown_before_initialize_failed: {error:?}")
            })?,
            NetworkState::STARTED => {}
            _ => return Err(Status::UNSUPPORTED.into()),
        }
        self.snp
            .initialize(0, 0)
            .inspect_err(|error| crate::logln!("loader_snp_initialize_failed: {error:?}"))
    }

    fn configure_filters(&self) -> uefi::Result<()> {
        let mask = self.snp.mode().receive_filter_mask;
        if mask == 0 {
            // A zero mask exposes no configurable hardware filters. Keep the
            // firmware receive policy; smoltcp checks Ethernet destinations.
            return Ok(());
        }
        let supported = ReceiveFlags::from_bits_retain(mask);
        let enable = (ReceiveFlags::UNICAST | ReceiveFlags::BROADCAST) & supported;
        self.snp
            .receive_filters(enable, ReceiveFlags::empty(), false, None)
            .inspect_err(|error| crate::logln!("loader_snp_receive_filters_failed: {error:?}"))
    }

    pub(super) fn addresses(&self) -> uefi::Result<(MacAddress, MacAddress)> {
        let mode = self.snp.mode();
        let current = MacAddress::new(
            mode.current_address.0[..6]
                .try_into()
                .map_err(|_| Status::UNSUPPORTED)?,
        );
        let permanent = MacAddress::new(
            mode.permanent_address.0[..6]
                .try_into()
                .map_err(|_| Status::UNSUPPORTED)?,
        );
        let stable = if permanent.is_zero() {
            current
        } else {
            permanent
        };
        if current.is_zero()
            || stable.is_zero()
            || current.octets()[0] & 1 != 0
            || stable.octets()[0] & 1 != 0
        {
            crate::logln!("loader_snp_mac_invalid: permanent={permanent} current={current}");
            return Err(Status::UNSUPPORTED.into());
        }
        Ok((stable, current))
    }

    pub(super) fn now(&self) -> Instant {
        self.clock.now()
    }

    pub(super) fn begin_poll(&mut self) -> uefi::Result<()> {
        self.health()?;
        self.receive_budget = RX_BUDGET;
        self.transmit_budget = MAX_TX_BUFFERS;
        self.reap()
    }

    pub(super) fn health(&self) -> uefi::Result<()> {
        self.failure.map_or(Ok(()), |status| Err(status.into()))
    }

    pub(super) fn set_syn_backpressure(&mut self, backpressure: Option<SynBackpressure>) {
        self.syn_backpressure = backpressure;
    }

    fn reap(&mut self) -> uefi::Result<()> {
        for _ in 0..=self.transmit.len() {
            match self.snp.get_recycled_transmit_buffer_status() {
                Ok(Some(pointer)) => {
                    let Some(buffer) = self.transmit.iter_mut().find(|buffer| {
                        buffer.submitted && buffer.bytes.as_ptr() == pointer.as_ptr()
                    }) else {
                        self.failure = Some(Status::COMPROMISED_DATA);
                        return self.health();
                    };
                    buffer.submitted = false;
                }
                Ok(None) => return Ok(()),
                Err(error) => {
                    crate::logln!("loader_snp_status_failed: {error:?}");
                    self.failure = Some(error.status());
                    return Err(error);
                }
            }
        }
        self.failure = Some(Status::COMPROMISED_DATA);
        self.health()
    }

    pub(super) fn close(&mut self) -> uefi::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.health()?;
        if self.snp.mode().state == NetworkState::STOPPED {
            self.active = false;
            return Ok(());
        }
        if self.snp.mode().state == NetworkState::STARTED {
            if let Err(error) = self.snp.stop() {
                self.failure = Some(error.status());
                return Err(error);
            }
            self.active = false;
            return Ok(());
        }
        if self.snp.mode().state != NetworkState::INITIALIZED {
            self.failure = Some(Status::DEVICE_ERROR);
            return self.health();
        }
        let deadline = self.now() + Duration::from_secs(3);
        while self.transmit.iter().any(|buffer| buffer.submitted) {
            self.reap()?;
            if self.now() >= deadline {
                self.failure = Some(Status::TIMEOUT);
                return self.health();
            }
            boot::stall(core::time::Duration::from_millis(1));
        }
        if let Err(error) = self.snp.shutdown().and_then(|()| self.snp.stop()) {
            self.failure = Some(error.status());
            return Err(error);
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            // All protocol guards and submitted boxes are still fields of this
            // live owner. Reset is terminal: no destructor can free DMA storage.
            crate::logln!("loader_snp_stop_failed: {error:?}; resetting");
            uefi::runtime::reset(uefi::runtime::ResetType::COLD, error.status(), None);
        }
    }
}

pub(super) struct ReceiveToken<'a>(&'a [u8]);
impl phy::RxToken for ReceiveToken<'_> {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(self.0)
    }
}

pub(super) struct TransmitToken<'a> {
    snp: &'a SimpleNetwork,
    buffer: &'a mut TransmitBuffer,
    failure: &'a mut Option<Status>,
    frame_bytes: usize,
    budget: &'a mut usize,
}
impl phy::TxToken for TransmitToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        // smoltcp derives frame lengths from the advertised MTU. A violation is
        // an internal contract failure, before the firmware owns this buffer.
        assert!(len <= self.frame_bytes);
        *self.budget -= 1;
        let result = f(&mut self.buffer.bytes[..len]);
        match self
            .snp
            .transmit(0, &self.buffer.bytes[..len], None, None, None)
        {
            Ok(()) => self.buffer.submitted = true,
            Err(error) if error.status() == Status::NOT_READY => {}
            Err(error) => *self.failure = Some(error.status()),
        }
        result
    }
}

impl phy::Device for Device {
    type RxToken<'a> = ReceiveToken<'a>;
    type TxToken<'a> = TransmitToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.receive_budget == 0 || self.transmit_budget == 0 || self.failure.is_some() {
            return None;
        }
        let buffer = self.transmit.iter_mut().find(|buffer| !buffer.submitted)?;
        while self.receive_budget != 0 {
            self.receive_budget -= 1;
            let size = match self.snp.receive(&mut self.receive, None, None, None, None) {
                Ok(size) if size <= self.frame_bytes => size,
                Ok(_) => {
                    self.failure = Some(Status::BAD_BUFFER_SIZE);
                    return None;
                }
                Err(error) if error.status() == Status::NOT_READY => return None,
                Err(error) => {
                    self.failure = Some(error.status());
                    return None;
                }
            };
            if self
                .syn_backpressure
                .as_ref()
                .is_some_and(|backpressure| backpressure.defer(&self.receive[..size]))
            {
                // No TCP state changes while discarding a new SYN, so the
                // per-frame snapshot remains valid. Every discarded frame still
                // consumes the original finite receive budget.
                continue;
            }
            return Some((
                ReceiveToken(&self.receive[..size]),
                TransmitToken {
                    snp: &self.snp,
                    buffer,
                    failure: &mut self.failure,
                    frame_bytes: self.frame_bytes,
                    budget: &mut self.transmit_budget,
                },
            ));
        }
        None
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        if self.transmit_budget == 0 || self.failure.is_some() {
            return None;
        }
        let buffer = self.transmit.iter_mut().find(|buffer| !buffer.submitted)?;
        Some(TransmitToken {
            snp: &self.snp,
            buffer,
            failure: &mut self.failure,
            frame_bytes: self.frame_bytes,
            budget: &mut self.transmit_budget,
        })
    }

    fn capabilities(&self) -> phy::DeviceCapabilities {
        let mut capabilities = phy::DeviceCapabilities::default();
        capabilities.medium = phy::Medium::Ethernet;
        capabilities.max_transmission_unit = self.frame_bytes;
        capabilities.max_burst_size = Some(self.transmit.len());
        capabilities
    }
}
