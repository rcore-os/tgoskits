#![feature(used_with_arg)]

use ax_driver::{PlatformDevice, probe::OnProbeError};

ax_driver::model_register!(
    name: "ax-driver model register test",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe,
    }],
);

fn probe(_plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    Ok(())
}

#[test]
fn model_register_is_usable_from_ax_driver_only() {
    let _ = core::mem::size_of::<ax_driver::register::DriverRegister>();
}
