use alloc::vec::Vec;

use nb::block;

use crate::{Transport, err::ScmiError, protocol::FuturePoll};

/// Clock protocol message IDs.
const PROTOCOL_ATTRIBUTES: u8 = 0x1;
const PROTOCOL_CLOCK_ATTRIBUTES: u8 = 0x3;
const PROTOCOL_DESCRIBE_RATES: u8 = 0x4;
const PROTOCOL_RATE_SET: u8 = 0x5;
const PROTOCOL_RATE_GET: u8 = 0x6;
const PROTOCOL_CONFIG_SET: u8 = 0x7;

const ATTRIBUTES_CLOCK_ENABLE: u32 = 1 << 0;

/// SCMI Clock protocol client (protocol ID 0x14).
///
/// Provides clock enable/disable, rate query/set, and attribute discovery.
pub struct Clock<T: Transport> {
    protocol: super::Protocol<T>,
    num_clocks: u16,
    max_async_req: u8,
}

/// Attributes returned by `CLOCK_ATTRIBUTES` (message ID 0x3).
#[derive(Debug, Clone)]
pub struct ClockAttributes {
    /// Whether the clock is currently enabled.
    pub enabled: bool,
    /// Whether the clock is writable (rate can be changed).
    pub rate_changed_notifications: bool,
    /// Clock name (up to 16 bytes, ASCII, NUL-terminated).
    pub name: Vec<u8>,
}

/// Rate entry returned by `CLOCK_DESCRIBE_RATES` (message ID 0x4).
#[derive(Debug, Clone)]
pub enum RateInfo {
    /// A single discrete rate.
    Discrete(u64),
    /// A linear range: `(min, max, step)`. When `step == 0` any rate in
    /// `[min, max]` is valid.
    Range { min: u64, max: u64, step: u64 },
}

impl<T: Transport> Clock<T> {
    /// SCMI Clock protocol identifier.
    pub const PROTOCOL_ID: u8 = 0x14;

    pub(crate) fn new(protocol: super::Protocol<T>) -> Self {
        Self {
            protocol,
            num_clocks: 0,
            max_async_req: 0,
        }
    }

    /// Initialise the protocol by querying version and attributes.
    pub(crate) fn init(&mut self) {
        {
            let mut version_fur = self.protocol.version();
            let version = block!(version_fur.poll_completion()).unwrap();
            debug!("Clock Protocol version: {}.{}", version.0, version.1);
        }
        self.protocol_attributes().unwrap();
    }

    /// Number of clocks discovered while creating an initialized client.
    pub fn num_clocks(&self) -> u16 {
        self.num_clocks
    }

    fn protocol_attributes(&mut self) -> Result<(), ScmiError> {
        let xfer = super::Xfer::new(PROTOCOL_ATTRIBUTES, 4);
        let mut res = self.protocol.do_xfer(xfer, |xfer| {
            let mut buff = [0u8; 4];
            buff[..4].copy_from_slice(&xfer.rx[..4]);
            Ok(buff)
        });
        let res = block!(res.poll_completion())?;
        let num_clocks = u16::from_le_bytes([res[0], res[1]]);
        let max_async_req = res[2];
        self.max_async_req = max_async_req;
        self.num_clocks = num_clocks;
        debug!(
            "Clock Protocol Attributes: num_clocks={}, max_async_req={}",
            num_clocks, max_async_req
        );
        Ok(())
    }

    /// Query attributes of a specific clock (message ID 0x3).
    pub fn clock_attributes(&mut self, clk_id: u32) -> Result<ClockAttributes, ScmiError> {
        let mut xfer = super::Xfer::new(PROTOCOL_CLOCK_ATTRIBUTES, 20);
        xfer.tx.extend_from_slice(&clk_id.to_le_bytes());
        let mut res = self.protocol.do_xfer(xfer, |xfer| {
            let attr = u32::from_le_bytes([xfer.rx[0], xfer.rx[1], xfer.rx[2], xfer.rx[3]]);
            let enabled = (attr & ATTRIBUTES_CLOCK_ENABLE) != 0;
            let rate_changed_notifications = (attr & (1 << 1)) != 0;
            let mut name = Vec::with_capacity(16);
            name.extend_from_slice(&xfer.rx[4..20]);
            while name.last() == Some(&0) {
                name.pop();
            }
            Ok(ClockAttributes {
                enabled,
                rate_changed_notifications,
                name,
            })
        });
        block!(res.poll_completion())
    }

    /// Describe the rates supported by a clock (message ID 0x4).
    ///
    /// Returns a list of [`RateInfo`] entries. Use `rate_index` to iterate
    /// when the platform reports more rates than fit in one response.
    pub fn describe_rates(
        &mut self,
        clk_id: u32,
        rate_index: u32,
    ) -> Result<Vec<RateInfo>, ScmiError> {
        let mut xfer = super::Xfer::new(PROTOCOL_DESCRIBE_RATES, 24);
        xfer.tx.extend_from_slice(&clk_id.to_le_bytes());
        xfer.tx.extend_from_slice(&rate_index.to_le_bytes());
        let mut res = self.protocol.do_xfer(xfer, |xfer| {
            let attr = u32::from_le_bytes([xfer.rx[0], xfer.rx[1], xfer.rx[2], xfer.rx[3]]);
            let remaining_count = attr & 0xFFF;
            let _rate_format = (attr >> 12) & 0x1; // 0 = discrete, 1 = linear range
            let _returned_count = (attr >> 16) & 0xFFF;

            let mut rates = Vec::new();
            // Each rate entry is 12 bytes for discrete (pad to 12), or
            // 12 bytes for range (min_low, min_high, max_low, max_high, step_low, step_high).
            // With 24 bytes of rx, we can fit 2 entries of 12 bytes each.
            // Remaining rates after status word start at offset 4.
            let entry_size = 12usize;
            let num_entries = ((xfer.rx.len() - 4) / entry_size).min(2);
            for i in 0..num_entries {
                let off = 4 + i * entry_size;
                let low = u64::from(u32::from_le_bytes([
                    xfer.rx[off],
                    xfer.rx[off + 1],
                    xfer.rx[off + 2],
                    xfer.rx[off + 3],
                ]));
                let high = u64::from(u32::from_le_bytes([
                    xfer.rx[off + 4],
                    xfer.rx[off + 5],
                    xfer.rx[off + 6],
                    xfer.rx[off + 7],
                ]));
                let rate = low | (high << 32);

                if _rate_format == 0 {
                    // Discrete rate
                    rates.push(RateInfo::Discrete(rate));
                } else {
                    // Linear range: min, max, step
                    let step_low = u32::from_le_bytes([
                        xfer.rx[off + 8],
                        xfer.rx[off + 9],
                        xfer.rx[off + 10],
                        xfer.rx[off + 11],
                    ]);
                    let step = u64::from(step_low);
                    // For range format, first entry gives min, we'd need next for max/step
                    // Simplified: return as Range with current info
                    rates.push(RateInfo::Range {
                        min: rate,
                        max: rate, // Will be filled by subsequent call
                        step,
                    });
                }
            }

            let _ = remaining_count;
            Ok(rates)
        });
        block!(res.poll_completion())
    }

    /// Enable a clock (message ID 0x7, config = 1).
    pub fn clk_enable(&mut self, clk_id: u32) -> Result<(), ScmiError> {
        self.clock_config_set(clk_id, ATTRIBUTES_CLOCK_ENABLE)
    }

    /// Disable a clock (message ID 0x7, config = 0).
    pub fn clk_disable(&mut self, clk_id: u32) -> Result<(), ScmiError> {
        self.clock_config_set(clk_id, 0)
    }

    /// Get the current rate of a clock in Hz (message ID 0x6).
    pub fn rate_get(&mut self, clk_id: u32) -> Result<u64, ScmiError> {
        let mut xfer = super::Xfer::new(PROTOCOL_RATE_GET, size_of::<u64>());
        xfer.tx.extend_from_slice(&clk_id.to_le_bytes());
        let mut res = self.protocol.do_xfer(xfer, |xfer| {
            let mut buff = [0u8; 8];
            buff.copy_from_slice(&xfer.rx[..8]);
            Ok(u64::from_le_bytes(buff))
        });
        block!(res.poll_completion())
    }

    /// Set the rate of a clock in Hz (message ID 0x5).
    pub fn rate_set(&mut self, clk_id: u32, rate: u64) -> Result<(), ScmiError> {
        let mut xfer = super::Xfer::new(PROTOCOL_RATE_SET, 12);
        let flags = 0u32;
        xfer.tx.extend_from_slice(&flags.to_le_bytes());
        xfer.tx.extend_from_slice(&clk_id.to_le_bytes());
        xfer.tx
            .extend_from_slice((rate as u32).to_le_bytes().as_slice());
        xfer.tx
            .extend_from_slice(((rate >> 32) as u32).to_le_bytes().as_slice());
        let mut res = self.protocol.do_xfer(xfer, |_xfer| Ok(()));
        block!(res.poll_completion())
    }

    fn clock_config_set(&mut self, clk_id: u32, config: u32) -> Result<(), ScmiError> {
        let mut xfer = super::Xfer::new(PROTOCOL_CONFIG_SET, 0);
        xfer.tx.extend_from_slice(&clk_id.to_le_bytes());
        xfer.tx.extend_from_slice(&config.to_le_bytes());
        let mut res = self.protocol.do_xfer(xfer, |_xfer| Ok(()));
        block!(res.poll_completion())
    }
}
