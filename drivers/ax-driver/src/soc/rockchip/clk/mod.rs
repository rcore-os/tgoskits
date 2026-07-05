use rdrive::{DriverGeneric, KError, probe::OnProbeError};
use rockchip_soc::{ClkId, Cru, CruOp};

mod rk3568;
mod rk3588;

pub struct ClkDrv {
    name: &'static str,
    inner: Cru,
}

impl ClkDrv {
    pub const fn new(name: &'static str, cru: Cru) -> Self {
        Self { name, inner: cru }
    }

    fn enable_clock(&mut self, id: u32) -> Result<(), OnProbeError> {
        let id = ClkId::from(id);
        if self.inner.clk_is_enabled(id).unwrap_or(false) {
            return Ok(());
        }

        self.inner.clk_enable(id).map_err(|err| {
            OnProbeError::other(alloc::format!("failed to enable clock {id}: {err}"))
        })
    }

    fn set_clock_rate(&mut self, id: u32, rate: u64) -> Result<(), OnProbeError> {
        self.inner
            .clk_set_rate(ClkId::from(id), rate)
            .map_err(|err| {
                OnProbeError::other(alloc::format!("failed to set clock {id}: {err}"))
            })?;
        Ok(())
    }

    fn reset_assert(&mut self, id: u64) {
        self.inner.reset_assert(id.into());
    }

    fn reset_deassert(&mut self, id: u64) {
        self.inner.reset_deassert(id.into());
    }
}

unsafe impl Send for ClkDrv {}

impl DriverGeneric for ClkDrv {
    fn name(&self) -> &str {
        self.name
    }
}

impl rdif_clk::Interface for ClkDrv {
    fn perper_enable(&mut self) {}

    fn enable(&mut self, id: rdif_clk::ClockId) -> Result<(), KError> {
        self.inner
            .clk_enable(clock_id(id))
            .map_err(|_| KError::InvalidArg { name: "clock_id" })
    }

    fn get_rate(&self, id: rdif_clk::ClockId) -> Result<u64, KError> {
        self.inner
            .clk_get_rate(clock_id(id))
            .map_err(|_| KError::InvalidArg { name: "clock_id" })
    }

    fn set_rate(&mut self, id: rdif_clk::ClockId, rate: u64) -> Result<(), KError> {
        self.inner
            .clk_set_rate(clock_id(id), rate)
            .map_err(|_| KError::InvalidArg { name: "clock_id" })?;
        Ok(())
    }
}

fn clock_id(id: rdif_clk::ClockId) -> ClkId {
    let id: usize = id.into();
    ClkId::from(id)
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

pub fn rk3588_reset_assert(id: u64) -> Result<(), OnProbeError> {
    with_clk_drv(|drv| {
        drv.reset_assert(id);
        Ok(())
    })
}

pub fn rk3588_reset_deassert(id: u64) -> Result<(), OnProbeError> {
    with_clk_drv(|drv| {
        drv.reset_deassert(id);
        Ok(())
    })
}
