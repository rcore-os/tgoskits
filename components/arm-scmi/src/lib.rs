#![no_std]

#[macro_use]
extern crate alloc;
#[macro_use]
extern crate log;

pub use crate::{err::ScmiError, protocol::Xfer, shmem::Shmem};

mod err;
mod protocol;
mod shmem;
mod transport;

use alloc::sync::Arc;

use spin::Mutex;
pub use transport::{Smc, Transport};

type Data<T> = Arc<Mutex<ScmiData<T>>>;

const SCMI_CLOCK_PROTOCOL: u32 = 0x14;
const SCMI_CLOCK_RATE_SET: u8 = 0x05;
const SCMI_CLOCK_RATE_GET: u8 = 0x06;
const SHMEM_PAYLOAD_OFFSET: usize = 0x1c;

pub struct Scmi<T: Transport> {
    data: Data<T>,
}

impl<T: Transport> Scmi<T> {
    pub fn new(kind: T, mut shmem: Shmem) -> Self {
        shmem.reset();
        let data = ScmiData {
            transport: kind,
            shmem,
        };
        Scmi {
            data: Arc::new(Mutex::new(data)),
        }
    }

    pub fn protocol_clk(&self) -> protocol::Clock<T> {
        let data = self.data.clone();
        let mut clk = protocol::Clock::new(protocol::Protocal::new(
            data,
            protocol::Clock::<T>::PROTOCOL_ID,
        ));
        clk.init();
        clk
    }

    pub fn protocol_clk_no_init(&self) -> protocol::Clock<T> {
        protocol::Clock::new(protocol::Protocal::new(
            self.data.clone(),
            protocol::Clock::<T>::PROTOCOL_ID,
        ))
    }
}

impl Scmi<Smc> {
    pub fn clock_rate_get_direct(&self, clock_id: u32) -> Result<u64, ScmiError> {
        let mut data = self.data.lock();
        if data.shmem.size < SHMEM_PAYLOAD_OFFSET + 12 {
            return Err(ScmiError::ProtocolError);
        }
        data.shmem
            .write_message_header(SCMI_CLOCK_PROTOCOL, SCMI_CLOCK_RATE_GET, 4);
        data.shmem.write_payload_u32(0, clock_id);
        data.transport.call_sync();
        let status = data.shmem.read_payload_i32(0);
        if let Err(err) = ScmiError::from_status(status) {
            data.shmem.reset();
            return Err(err);
        }
        let low = data.shmem.read_payload_u32(4) as u64;
        let high = data.shmem.read_payload_u32(8) as u64;
        data.shmem.reset();
        Ok(low | (high << 32))
    }

    pub fn clock_rate_set_direct(&self, clock_id: u32, rate: u64) -> Result<(), ScmiError> {
        let mut data = self.data.lock();
        if data.shmem.size < SHMEM_PAYLOAD_OFFSET + 16 {
            return Err(ScmiError::ProtocolError);
        }
        data.shmem
            .write_message_header(SCMI_CLOCK_PROTOCOL, SCMI_CLOCK_RATE_SET, 16);
        data.shmem.write_payload_u32(0, 0);
        data.shmem.write_payload_u32(4, clock_id);
        data.shmem.write_payload_u32(8, rate as u32);
        data.shmem.write_payload_u32(12, (rate >> 32) as u32);
        data.transport.call_sync();
        let status = data.shmem.read_payload_i32(0);
        let result = ScmiError::from_status(status);
        data.shmem.reset();
        result
    }
}

struct ScmiData<T: Transport> {
    transport: T,
    shmem: Shmem,
}

impl<T: Transport> ScmiData<T> {
    pub fn send_message(&mut self, xfer: &mut Xfer) -> Result<(), crate::err::ScmiError> {
        self.transport.send_message(&mut self.shmem, xfer)
    }

    pub fn fetch_response(&mut self, xfer: &mut Xfer) -> Result<(), crate::err::ScmiError> {
        self.transport.fetch_response(&mut self.shmem, xfer)
    }
}
