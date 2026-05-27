use ax_errno::AxError;

pub fn probe_all_devices() -> Result<(), AxError> {
    if !rdrive::is_initialized() {
        warn!("rdrive is not initialized; skip platform device probe");
        return Ok(());
    }
    rdrive::probe_all(false).map_err(|_| AxError::BadState)
}
