use alloc::sync::Arc;

use mbarrier::wmb;
use spin::{Mutex, RwLock};
use usb_if::err::TransferError;
use xhci::{
    registers::doorbell,
    ring::trb::{command, event::CommandCompletion},
};

use super::{reg::XhciRegisters, ring::SendRing};
use crate::{err::ConvertXhciError, osal::Kernel, queue::Finished};

#[derive(Clone)]
pub struct CommandRing(Arc<Mutex<Inner>>);

impl CommandRing {
    pub fn new(
        direction: crate::osal::DmaDirection,
        dma: &Kernel,
        reg: Arc<RwLock<XhciRegisters>>,
    ) -> crate::err::Result<Self> {
        let ring = SendRing::new(direction, dma)?;
        let inner = Inner { ring, reg };
        Ok(Self(Arc::new(Mutex::new(inner))))
    }

    pub fn bus_addr(&self) -> crate::BusAddr {
        let inner = self.0.lock();
        inner.ring.bus_addr()
    }

    pub fn cycle(&self) -> bool {
        let inner = self.0.lock();
        inner.ring.cycle()
    }

    pub fn finished_handle(&self) -> Finished<CommandCompletion> {
        let inner = self.0.lock();
        inner.ring.finished_handle()
    }

    pub async fn cmd_request(
        &mut self,
        trb: command::Allowed,
    ) -> Result<CommandCompletion, TransferError> {
        let fur = {
            let mut inner = self.0.lock();
            let trb_addr = inner.ring.enque_command(trb);
            let fur = inner.ring.take_finished_future(trb_addr);
            wmb();
            inner
                .reg
                .write()
                .doorbell
                .write_volatile_at(0, doorbell::Register::default());
            fur
        };

        let res = fur.await;

        match res.completion_code() {
            Ok(code) => code.to_result()?,
            Err(e) => Err(TransferError::Other(anyhow!("Command failed: {e:?}")))?,
        }

        Ok(res)
    }
}

struct Inner {
    ring: SendRing<CommandCompletion>,
    reg: Arc<RwLock<XhciRegisters>>,
}
