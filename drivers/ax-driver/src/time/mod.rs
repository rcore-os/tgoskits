use log::{debug, info};
use rdrive::probe::OnProbeError;

#[cfg(target_arch = "x86_64")]
mod cmos;
#[cfg(any(test, target_arch = "x86_64"))]
mod cmos_decode;
#[cfg(any(
    test,
    target_arch = "loongarch64",
    target_arch = "riscv64",
    target_arch = "x86_64"
))]
mod datetime;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
mod fdt;
#[cfg(target_arch = "riscv64")]
mod goldfish;
#[cfg(target_arch = "loongarch64")]
mod loongson;
#[cfg(any(test, target_arch = "loongarch64"))]
mod loongson_decode;
#[cfg(target_arch = "aarch64")]
mod pl031;
#[cfg(target_arch = "riscv64")]
mod starfive;
#[cfg(any(test, target_arch = "riscv64"))]
mod starfive_decode;

fn init_epoch_offset(node_name: &str, unix_timestamp: u64) -> Result<(), OnProbeError> {
    if unix_timestamp == 0 {
        return Err(OnProbeError::other(alloc::format!(
            "[{node_name}] returned zero unix timestamp"
        )));
    }

    let epoch_time_nanos = unix_timestamp * 1_000_000_000;
    if axklib::time::try_init_epoch_offset(epoch_time_nanos) {
        info!("Initialized wall clock from {node_name}");
    } else {
        debug!("Skipping RTC {node_name} because epoch offset is already initialized",);
    }

    Ok(())
}
