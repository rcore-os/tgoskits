#![feature(used_with_arg)]

use ax_driver::{
    probe::OnProbeError,
    register::{ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
};
ax_driver::model_register!(
    name: "ax-driver model register test",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,model-register"],
        on_probe: probe,
    }],
);

fn probe(_probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    Ok(())
}

#[test]
fn model_register_is_usable_from_ax_driver_only() {
    let _ = core::mem::size_of::<ax_driver::register::DriverRegister>();
}
