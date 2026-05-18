#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HartStartError {
    Failed(isize),
    NotSupported,
    InvalidParam,
    InvalidAddress,
    AlreadyAvailable,
    AlreadyStarted,
}

pub fn hart_start(hartid: usize, start_addr: usize, opaque: usize) -> Result<(), HartStartError> {
    let ret = sbi_rt::hart_start(hartid, start_addr, opaque);
    match ret.error {
        x if x == sbi_rt::SbiRet::success(0).error => Ok(()),
        x if x == sbi_rt::SbiRet::not_supported().error => Err(HartStartError::NotSupported),
        x if x == sbi_rt::SbiRet::invalid_param().error => Err(HartStartError::InvalidParam),
        x if x == sbi_rt::SbiRet::invalid_address().error => Err(HartStartError::InvalidAddress),
        x if x == sbi_rt::SbiRet::already_available().error => {
            Err(HartStartError::AlreadyAvailable)
        }
        x if x == sbi_rt::SbiRet::already_started().error => Err(HartStartError::AlreadyStarted),
        other => Err(HartStartError::Failed(other as isize)),
    }
}

pub fn set_timer(stime_value: u64) -> Result<(), isize> {
    let ret = sbi_rt::set_timer(stime_value);
    if ret.error == 0 {
        Ok(())
    } else {
        Err(ret.error as isize)
    }
}

pub fn system_reset_shutdown() -> Result<(), isize> {
    let ret = sbi_rt::system_reset(sbi_rt::Shutdown, sbi_rt::NoReason);
    if ret.error == 0 {
        Ok(())
    } else {
        Err(ret.error as isize)
    }
}

pub fn detect_timebase_frequency() -> Option<usize> {
    let fdt_ptr = crate::fdt::fdt_addr()?;
    let fdt = unsafe { fdt_raw::Fdt::from_ptr(fdt_ptr).ok()? };
    let cpus = fdt.find_by_path("/cpus")?;
    let prop = cpus.find_property("timebase-frequency")?;
    prop.as_u32().map(|value| value as usize)
}
