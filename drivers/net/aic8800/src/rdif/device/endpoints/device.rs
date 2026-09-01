use alloc::{boxed::Box, sync::Arc, vec};
use core::time::Duration;

use rdif_eth::{
    DriverGeneric, NetDevice, NetDeviceInfo, NetDeviceParts, NetError, NetHardIrqEndpoint,
    NetIrqSourceId, NetPollGroupId, NetPollGroupParts, NetQueuePairParts, QueueConfig,
    WifiTransaction,
};
use sdmmc_protocol::sdio::CompletionIrqRearmHost;

use super::{
    control::{AicNetControl, AicWifiControl},
    irq::AicHardIrq,
    startup::{AicOwnerStartup, AicPollIrqControl},
};
use crate::rdif::{
    device::{MacAddressState, OwnerChannels, WifiChannels, queues::queue_parts, shared_irq_latch},
    error::AicRdifError,
    owner::AicOwner,
};

const GROUP_ID: NetPollGroupId = NetPollGroupId::new(0);
const DEFAULT_QUEUE_SIZE: usize = 32;
const DEFAULT_FRAME_SIZE: usize = 2048;
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

/// Construction policy for one portable AIC RDIF device.
#[derive(Clone, Debug)]
pub struct AicRdifOptions {
    /// Driver-local physical IRQ source identity.
    pub irq_source: NetIrqSourceId,
    /// Optional board-selected operation executed after device startup.
    pub startup_transaction: Option<WifiTransaction>,
    /// Number of move-only tokens accepted by each SPSC queue.
    pub queue_size: usize,
    /// Maximum Ethernet frame size accepted by the adapter.
    pub frame_size: usize,
    /// SoC reset-settle interval observed before the first card command.
    pub startup_delay: Duration,
    /// End-to-end deadline covering card enumeration, firmware, and FDRV startup.
    pub startup_timeout: Duration,
    /// End-to-end deadline for one scan/connect/AP/disconnect transaction.
    pub control_timeout: Duration,
}

impl AicRdifOptions {
    /// Creates the default bounded queue policy for one IRQ source.
    pub const fn new(irq_source: NetIrqSourceId) -> Self {
        Self {
            irq_source,
            startup_transaction: None,
            queue_size: DEFAULT_QUEUE_SIZE,
            frame_size: DEFAULT_FRAME_SIZE,
            startup_delay: Duration::ZERO,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            control_timeout: DEFAULT_CONTROL_TIMEOUT,
        }
    }

    /// Selects one startup transaction without executing it during probe.
    pub fn with_startup_transaction(mut self, transaction: WifiTransaction) -> Self {
        self.startup_transaction = Some(transaction);
        self
    }

    /// Delays the first card command until the owner-provided deadline.
    pub const fn with_startup_delay(mut self, delay: Duration) -> Self {
        self.startup_delay = delay;
        self
    }

    /// Sets the end-to-end startup deadline.
    pub const fn with_startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Sets the end-to-end deadline for each Wi-Fi control transaction.
    pub const fn with_control_timeout(mut self, timeout: Duration) -> Self {
        self.control_timeout = timeout;
        self
    }
}

/// Move-only portable AIC device before it is split into RDIF endpoints.
pub struct AicRdifDevice<H: CompletionIrqRearmHost + 'static> {
    host: H,
    options: AicRdifOptions,
    dma_mask: u64,
}

impl<H: CompletionIrqRearmHost + Send + 'static> AicRdifDevice<H> {
    /// Creates a portable adapter without issuing card or firmware commands.
    ///
    /// # Errors
    ///
    /// Returns an error when the host lacks DMA or the queue policy is invalid.
    pub fn new(host: H, options: AicRdifOptions) -> Result<Self, AicRdifError> {
        if options.queue_size < 2
            || options.frame_size == 0
            || options.startup_timeout.is_zero()
            || options.control_timeout.is_zero()
        {
            return Err(AicRdifError::QueueUnavailable);
        }
        let dma_mask = host
            .device_dma()
            .map_err(|_| AicRdifError::DmaUnavailable)?
            .info()
            .constraints()
            .addr_mask;
        Ok(Self {
            host,
            options,
            dma_mask,
        })
    }
}

impl<H: CompletionIrqRearmHost + Send + 'static> DriverGeneric for AicRdifDevice<H> {
    fn name(&self) -> &str {
        "aic8800"
    }
}

impl<H: CompletionIrqRearmHost + Send + 'static> NetDevice for AicRdifDevice<H> {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        let Self {
            host,
            options,
            dma_mask,
        } = *self;
        let queue_config = QueueConfig {
            dma_mask,
            align: 4,
            buf_size: options.frame_size,
            ring_size: options.queue_size,
        };
        let (tx, rx, queue_ports) = queue_parts(queue_config);
        let irq_latch = shared_irq_latch();
        let mac = Arc::new(MacAddressState::new([0; 6]));
        let parts = host.into_parts();
        let wifi_channels = WifiChannels::new();
        let progress_signal = Arc::clone(&wifi_channels.progress_signal);
        let (owner, wifi_requests, wifi_progress) = AicOwner::new(
            parts.bus,
            parts.card_irq,
            queue_ports,
            wifi_channels,
            Arc::clone(&irq_latch),
            Arc::clone(&mac),
        );
        let OwnerChannels {
            sender: owner_sender,
            receiver: owner_receiver,
        } = OwnerChannels::new();

        Ok(NetDeviceParts {
            info: NetDeviceInfo::new("aic8800", [0; 6]),
            control: Box::new(AicNetControl::new(Arc::clone(&mac))),
            wifi_control: Some(Box::new(AicWifiControl::new(
                wifi_requests,
                wifi_progress,
                Arc::clone(&progress_signal),
                options.startup_transaction,
                options.control_timeout,
            ))),
            poll_groups: vec![NetPollGroupParts {
                id: GROUP_ID,
                queues: NetQueuePairParts {
                    tx: Box::new(tx),
                    rx: Box::new(rx),
                },
                irq_control: Box::new(AicPollIrqControl::new(owner_receiver, progress_signal)),
                owner_startup: Some(Box::new(AicOwnerStartup::new(
                    owner,
                    owner_sender,
                    options.startup_delay,
                    options.startup_timeout,
                ))),
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    options.irq_source,
                    Box::new(AicHardIrq::new(parts.irq, irq_latch)),
                )],
            }],
        })
    }
}
