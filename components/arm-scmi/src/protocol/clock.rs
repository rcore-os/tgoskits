use nb::block;

use crate::{Transport, err::ScmiError, protocol::FuturePoll};

const PROTOCOL_RATE_SET: u8 = 0x5;
const PROTOCOL_RATE_GET: u8 = 0x6;
const PROTOCOL_CONFIG_SET: u8 = 0x7;

const ATTRIBUTES_CLOCK_ENABLE: u32 = 1 << 0;

pub struct Clock<T: Transport> {
    protocol: super::Protocal<T>,
    num_clocks: u16,
    max_async_req: u8,
}

impl<T: Transport> Clock<T> {
    pub const PROTOCOL_ID: u8 = 0x14;

    pub(crate) fn new(protocol: super::Protocal<T>) -> Self {
        Self {
            protocol,
            num_clocks: 0,
            max_async_req: 0,
        }
    }

    pub(crate) fn init(&mut self) {
        {
            // Initialization code if needed
            let mut version_fur = self.protocol.version();
            let version = block!(version_fur.poll_completion()).unwrap();
            debug!("Clock Protocol version: {}.{}", version.0, version.1);
        }
        self.attributes().unwrap();
    }

    fn attributes(&mut self) -> Result<(), ScmiError> {
        let xfer = super::Xfer::new(super::PROTOCOL_ATTRIBUTES, 4);
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

    pub fn clk_enable(&mut self, clk_id: u32) -> Result<(), ScmiError> {
        self.clock_config_set(clk_id, ATTRIBUTES_CLOCK_ENABLE)
    }

    pub fn clk_disable(&mut self, clk_id: u32) -> Result<(), ScmiError> {
        self.clock_config_set(clk_id, 0)
    }

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

    pub fn rate_set(&mut self, clk_id: u32, rate: u64) -> Result<(), ScmiError> {
        let mut xfer = super::Xfer::new(PROTOCOL_RATE_SET, 12);
        let flags = 0u32;
        xfer.tx.extend_from_slice(&flags.to_le_bytes());
        xfer.tx.extend_from_slice(&clk_id.to_le_bytes());
        xfer.tx
            .extend_from_slice(((rate & 0xffffffff) as u32).to_le_bytes().as_slice());
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
