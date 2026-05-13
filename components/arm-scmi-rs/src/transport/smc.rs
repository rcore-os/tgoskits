use tock_registers::interfaces::Readable;

use crate::{Shmem, Transport, Xfer, err::ScmiError};

/// SMC (Secure Monitor Call) transport.
///
/// Issues an `smc #0` with a configurable function ID to hand the
/// shared-memory buffer to the secure monitor / SCP and waits for
/// synchronous completion.
pub struct Smc {
    func_id: u32,
    irq: Option<u32>,
}

impl Smc {
    /// Create a new SMC transport.
    ///
    /// * `func_id` – the SMC/HVC function ID that the platform expects for
    ///   SCMI messages (e.g. `0x82000010`).
    /// * `irq` – optional completion interrupt number; when `None` the
    ///   transport reports `no_completion_irq() == true` and relies on
    ///   polling.
    pub fn new(func_id: u32, irq: Option<u32>) -> Self {
        Smc { func_id, irq }
    }

    pub(crate) fn call_sync(&self) {
        full_system_barrier();
        smc_call(self.func_id);
        full_system_barrier();
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
        shmem.tx_prepare(xfer)?;
        trace!("Sending SMC message {:?}", xfer.hdr);
        self.call_sync();
        Ok(())
    }

    const MAX_MSG: usize = 20;

    const MAX_MSG_SIZE: usize = 128;

    const SYNC_CMDS_COMPLETED_ON_RET: bool = true;

    fn fetch_response(&mut self, shmem: &mut Shmem, xfer: &mut Xfer) -> Result<(), ScmiError> {
        let len = shmem.header().length.get() as usize;
        let rx_len = response_payload_len(len, shmem.size, Self::MAX_MSG_SIZE)?;

        xfer.hdr.status = shmem.read_payload_u32(0)?;
        trace!(
            "Fetched SMC response rx_len = {rx_len}, header: {:?}",
            xfer.hdr
        );
        xfer.hdr.to_result()?;
        xfer.rx.resize(rx_len, 0);
        if rx_len > 0 {
            shmem.read_payload(&mut xfer.rx, 4)?;
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

fn response_payload_len(
    len: usize,
    shmem_size: usize,
    max_msg_size: usize,
) -> Result<usize, ScmiError> {
    const MSG_HEADER_SIZE: usize = size_of::<u32>();
    const STATUS_SIZE: usize = size_of::<u32>();
    const MIN_RESPONSE_LEN: usize = MSG_HEADER_SIZE + STATUS_SIZE;

    if len < MIN_RESPONSE_LEN || len > shmem_size {
        return Err(ScmiError::ProtocolError);
    }

    let rx_len = len
        .checked_sub(MIN_RESPONSE_LEN)
        .ok_or(ScmiError::ProtocolError)?;
    if rx_len > max_msg_size {
        return Err(ScmiError::ProtocolError);
    }
    Ok(rx_len)
}

#[cfg(target_arch = "aarch64")]
fn smc_call(func_id: u32) {
    let mut ret: usize;
    unsafe {
        core::arch::asm!(
            "smc #0",
            inlateout("x0") func_id as usize => ret,
            in("x1") 0usize,
            in("x2") 0usize,
            in("x3") 0usize,
        );
    }
    let _ = ret;
}

#[cfg(not(target_arch = "aarch64"))]
fn smc_call(_func_id: u32) {}

fn full_system_barrier() {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("dsb sy", "isb", options(nostack, preserves_flags));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_length_rejects_missing_status() {
        assert_eq!(
            response_payload_len(7, 64, Smc::MAX_MSG_SIZE),
            Err(ScmiError::ProtocolError)
        );
    }

    #[test]
    fn response_length_rejects_shmem_overflow() {
        assert_eq!(
            response_payload_len(65, 64, Smc::MAX_MSG_SIZE),
            Err(ScmiError::ProtocolError)
        );
    }

    #[test]
    fn response_length_rejects_oversized_payload() {
        assert_eq!(
            response_payload_len(8 + Smc::MAX_MSG_SIZE + 1, 256, Smc::MAX_MSG_SIZE),
            Err(ScmiError::ProtocolError)
        );
    }

    #[test]
    fn response_length_returns_payload_without_status() {
        assert_eq!(response_payload_len(12, 64, Smc::MAX_MSG_SIZE), Ok(4));
    }
}
