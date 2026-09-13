use alloc::{vec, vec::Vec};
use core::time::Duration;

use futures::{FutureExt, future::BoxFuture};
use usb_if::host::hub::Speed;

use crate::{
    backend::kmod::{
        Kernel,
        dwc2::reg::{Dwc2Registers, HPRT_PWR, HPRT_RST, HPRT_W1C_MASK, Hprt},
        hub::{HubInfo, HubOp, PortChangeInfo, PortEvent, PortState},
    },
    err::Result,
};

pub(crate) struct Dwc2RootHub {
    regs: Dwc2Registers,
    kernel: Kernel,
    port: PortState,
    last_logged_hprt: Option<u32>,
}
unsafe impl Send for Dwc2RootHub {}

impl Dwc2RootHub {
    pub(crate) fn new(regs: Dwc2Registers, kernel: Kernel) -> Self {
        Self {
            regs,
            kernel,
            port: PortState::Uninit,
            last_logged_hprt: None,
        }
    }

    async fn init_port(&mut self, mut info: HubInfo) -> Result<HubInfo> {
        info.speed = Speed::High;
        self.regs.hprt().update_safe(|value| value | HPRT_PWR);
        self.kernel.delay(Duration::from_millis(20));
        Ok(info)
    }

    async fn changed_ports_inner(&mut self) -> Result<Vec<PortEvent>> {
        if matches!(self.port, PortState::Probed) {
            if !self.regs.hprt().is_connected() {
                self.port = PortState::Uninit;
                self.regs.hprt().clear_connect_detect();
                return Ok(vec![PortEvent::Disconnected { port_id: 1 }]);
            }
            return Ok(Vec::new());
        }

        let hprt = self.regs.hprt();
        self.log_port_status("scan", &hprt);
        if !hprt.is_connected() {
            return Ok(Vec::new());
        }

        if matches!(self.port, PortState::Uninit) {
            self.reset_port();
            self.port = PortState::Reseted;
            self.log_port_status("reset", &self.regs.hprt());
        }

        if self.regs.hprt().is_connected() && self.regs.hprt().is_enabled() {
            self.port = PortState::Probed;
            Ok(vec![PortEvent::Connected(PortChangeInfo {
                root_port_id: 1,
                port_id: 1,
                port_speed: self.regs.hprt().speed(),
            })])
        } else {
            Ok(Vec::new())
        }
    }

    fn reset_port(&self) {
        self.regs.hprt().clear_connect_detect();
        self.regs
            .hprt()
            .write_safe((self.regs.hprt().raw() & !HPRT_W1C_MASK) | HPRT_PWR | HPRT_RST);
        self.kernel.delay(Duration::from_millis(60));
        self.regs
            .hprt()
            .write_safe(((self.regs.hprt().raw() & !HPRT_W1C_MASK) | HPRT_PWR) & !HPRT_RST);
        self.kernel.delay(Duration::from_millis(80));
    }

    fn log_port_status(&mut self, phase: &str, hprt: &Hprt) {
        if self.last_logged_hprt == Some(hprt.raw()) {
            return;
        }
        self.last_logged_hprt = Some(hprt.raw());
        log::info!(
            "dwc2: root port {phase} hprt0={:#010x} connected={} enabled={} speed={:?}",
            hprt.raw(),
            hprt.is_connected(),
            hprt.is_enabled(),
            hprt.speed()
        );
    }
}

impl HubOp for Dwc2RootHub {
    fn init<'a>(&'a mut self, info: HubInfo) -> BoxFuture<'a, Result<HubInfo>> {
        self.init_port(info).boxed()
    }

    fn changed_ports<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<PortEvent>>> {
        self.changed_ports_inner().boxed()
    }

    fn slot_id(&self) -> u8 {
        0
    }
}
