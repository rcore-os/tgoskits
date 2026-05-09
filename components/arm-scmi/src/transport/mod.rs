use crate::{Shmem, err::ScmiError, protocol::Xfer};

mod smc;

pub use smc::Smc;

pub trait Transport {
    const MAX_MSG: usize;
    const MAX_MSG_SIZE: usize;
    const SYNC_CMDS_COMPLETED_ON_RET: bool;

    fn chan_available(&self, idx: usize) -> bool;
    fn no_completion_irq(&self) -> bool;
    // fn chan_setup(&mut self, info: ChannelInfo);
    // fn chan_free(&mut self, idx: usize);
    fn send_message(&mut self, shmem: &mut Shmem, xfer: &Xfer) -> Result<(), ScmiError>;

    fn fetch_response(&mut self, shmem: &mut Shmem, xfer: &mut Xfer) -> Result<(), ScmiError>;
}
