use alloc::sync::Arc;

use ax_kspin::SpinRaw as Mutex;
use rdrive::{DriverGeneric, KError};
use rockchip_soc::{ClkId, ClockOp, Cru, ResetOp, RstId};

mod rk3568;
mod rk3588;

type SharedCru = Arc<Mutex<Cru>>;

pub struct ClkDrv {
    name: &'static str,
    inner: SharedCru,
}

impl ClkDrv {
    pub fn new(name: &'static str, cru: SharedCru) -> Self {
        Self { name, inner: cru }
    }
}

pub struct ResetDrv {
    name: &'static str,
    inner: SharedCru,
}

impl ResetDrv {
    pub fn new(name: &'static str, cru: SharedCru) -> Self {
        Self { name, inner: cru }
    }
}

impl DriverGeneric for ResetDrv {
    fn name(&self) -> &str {
        self.name
    }
}

unsafe impl Send for ClkDrv {}
unsafe impl Send for ResetDrv {}

impl DriverGeneric for ClkDrv {
    fn name(&self) -> &str {
        self.name
    }
}

impl rdif_clk::Interface for ClkDrv {
    fn perper_enable(&mut self) {}

    fn enable(&mut self, id: rdif_clk::ClockId) -> Result<(), KError> {
        self.inner
            .lock()
            .clk_enable(clock_id(id))
            .map_err(|_| KError::InvalidArg { name: "clock_id" })
    }

    fn get_rate(&self, id: rdif_clk::ClockId) -> Result<u64, KError> {
        self.inner
            .lock()
            .clk_get_rate(clock_id(id))
            .map_err(|_| KError::InvalidArg { name: "clock_id" })
    }

    fn set_rate(&mut self, id: rdif_clk::ClockId, rate: u64) -> Result<(), KError> {
        self.inner
            .lock()
            .clk_set_rate(clock_id(id), rate)
            .map_err(|_| KError::InvalidArg { name: "clock_id" })?;
        Ok(())
    }
}

impl rdif_reset::Interface for ResetDrv {
    fn assert(&mut self, id: rdif_reset::ResetId) -> Result<(), rdif_reset::ResetError> {
        self.inner.lock().reset_assert(reset_id(id));
        Ok(())
    }

    fn deassert(&mut self, id: rdif_reset::ResetId) -> Result<(), rdif_reset::ResetError> {
        self.inner.lock().reset_deassert(reset_id(id));
        Ok(())
    }
}

fn clock_id(id: rdif_clk::ClockId) -> ClkId {
    let id: usize = id.into();
    ClkId::from(id)
}

fn reset_id(id: rdif_reset::ResetId) -> RstId {
    RstId::from(id.raw())
}
