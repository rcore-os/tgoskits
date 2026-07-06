use alloc::sync::Arc;

use ax_kspin::SpinRaw as Mutex;
use rdrive::{DriverGeneric, KError, probe::OnProbeError};
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

    fn enable_clock(&self, id: u32) -> Result<(), OnProbeError> {
        let id = ClkId::from(id);
        let mut inner = self.inner.lock();
        if inner.clk_is_enabled(id).unwrap_or(false) {
            return Ok(());
        }

        inner.clk_enable(id).map_err(|err| {
            OnProbeError::other(alloc::format!("failed to enable clock {id}: {err}"))
        })
    }

    fn set_clock_rate(&self, id: u32, rate: u64) -> Result<(), OnProbeError> {
        self.inner
            .lock()
            .clk_set_rate(ClkId::from(id), rate)
            .map_err(|err| {
                OnProbeError::other(alloc::format!("failed to set clock {id}: {err}"))
            })?;
        Ok(())
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

fn with_clk_drv<T>(
    f: impl FnOnce(&mut ClkDrv) -> Result<T, OnProbeError>,
) -> Result<T, OnProbeError> {
    let clk = rdrive::get_one::<rdif_clk::Clk>()
        .ok_or_else(|| OnProbeError::other("Rockchip CRU clock device not registered"))?;
    let mut clk = clk.lock().map_err(|err| {
        OnProbeError::other(alloc::format!(
            "failed to lock Rockchip CRU clock device: {err}"
        ))
    })?;
    let drv = clk
        .typed_mut::<ClkDrv>()
        .ok_or_else(|| OnProbeError::other("Rockchip CRU clock device type mismatch"))?;

    f(drv)
}

pub fn rk3588_enable_clock(id: u32) -> Result<(), OnProbeError> {
    with_clk_drv(|drv| drv.enable_clock(id))
}

pub fn rk3588_set_clock_rate(id: u32, rate: u64) -> Result<(), OnProbeError> {
    with_clk_drv(|drv| drv.set_clock_rate(id, rate))
}
