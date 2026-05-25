use ax_errno::AxError;

pub fn probe_all_devices() -> Result<(), AxError> {
    rdrive::probe_all(false).map_err(|_| AxError::BadState)
}
