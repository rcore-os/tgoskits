#![no_std]

#[macro_use]
extern crate alloc;
#[macro_use]
extern crate log;

pub use crate::{protocol::Xfer, shmem::Shmem};

mod err;
mod protocol;
mod shmem;
mod transport;

use alloc::sync::Arc;
use spin::Mutex;
pub use transport::Smc;
pub use transport::Transport;

type Data<T> = Arc<Mutex<ScmiData<T>>>;

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
