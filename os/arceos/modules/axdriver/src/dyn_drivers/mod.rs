use alloc::vec::Vec;

pub fn probe_all_devices() -> Vec<super::AxDeviceEnum> {
    #[cfg(target_os = "none")]
    {
        if let Err(err) = axplat_dyn::drivers::probe_all_devices() {
            error!("failed to probe dynamic platform devices: {err:?}");
            return Vec::new();
        }

        Vec::new()
    }
    #[cfg(not(target_os = "none"))]
    Vec::new()
}
