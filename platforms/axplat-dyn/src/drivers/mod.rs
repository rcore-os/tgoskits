/// Errors owned by dynamic-platform device discovery.
#[derive(Debug, thiserror::Error)]
pub enum PlatformProbeError {
    /// A registered platform driver failed while probing devices.
    #[error(transparent)]
    Probe(#[from] rdrive::ProbeError),
}

/// Probes every device registered with the dynamic platform.
pub fn probe_all_devices() -> Result<(), PlatformProbeError> {
    if !rdrive::is_initialized() {
        warn!("rdrive is not initialized; skip platform device probe");
        return Ok(());
    }
    rdrive::probe_all(false)?;
    Ok(())
}
