use smccc::{error::success_or_error_64, smc64};
use tock_registers::interfaces::Readable;

use crate::{Shmem, Transport, Xfer, err::ScmiError};

pub struct Smc {
    func_id: u32,
    irq: Option<u32>,
}

impl Smc {
    pub fn new(func_id: u32, irq: Option<u32>) -> Self {
        Smc { func_id, irq }
    }

    fn call(&self) -> Result<(), smccc::psci::Error> {
        success_or_error_64(smc64(self.func_id, [0; 17])[0])
    }
}

impl Transport for Smc {
    fn chan_available(&self, _idx: usize) -> bool {
        true
    }

    fn no_completion_irq(&self) -> bool {
        self.irq.is_none()
    }

    fn send_message(&mut self, shmem: &mut Shmem, xfer: &Xfer) -> Result<(), ScmiError> {
        shmem.tx_prepare(xfer);
        trace!("Sending SMC message {:?}", xfer.hdr);
        self.call().unwrap();

        Ok(())
    }

    const MAX_MSG: usize = 20;

    const MAX_MSG_SIZE: usize = 128;

    const SYNC_CMDS_COMPLETED_ON_RET: bool = true;

    fn fetch_response(&mut self, shmem: &mut Shmem, xfer: &mut Xfer) -> Result<(), ScmiError> {
        let len = shmem.header().length.get() as usize;
        let rx_len = len.saturating_sub(8);

        xfer.hdr.status = unsafe { (shmem.payload_ptr() as *const u32).read_volatile() };
        trace!(
            "Fetched SMC response rx_len = {rx_len}, header: {:?}",
            xfer.hdr
        );
        xfer.hdr.to_result()?;
        xfer.rx.resize(rx_len, 0);
        if rx_len > 0 {
            shmem.read_payload(&mut xfer.rx, 4);
        }
        trace!(
            "Fetched response: hdr={:?}, rx_len={}, buff={:?}",
            xfer.hdr,
            xfer.rx.len(),
            xfer.rx
        );

        Ok(())
    }
}
