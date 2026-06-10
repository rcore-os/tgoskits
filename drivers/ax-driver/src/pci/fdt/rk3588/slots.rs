use rdrive::{
    probe::OnProbeError,
    register::{ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
};

use super::resources::probe_rk3588;

mod rk3588_pcie_slot0 {
    use super::*;

    crate::model_register!(
        name: "Rockchip RK3588 PCIe host slot0",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[
            ProbeKind::Fdt {
                compatibles: &["rockchip,rk3588-pcie"],
                on_probe: probe
            }
        ],
    );

    fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
        probe_rk3588(probe)
    }
}

mod rk3588_pcie_slot1 {
    use super::*;

    crate::model_register!(
        name: "Rockchip RK3588 PCIe host slot1",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[
            ProbeKind::Fdt {
                compatibles: &["rockchip,rk3588-pcie"],
                on_probe: probe
            }
        ],
    );

    fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
        probe_rk3588(probe)
    }
}

mod rk3588_pcie_slot2 {
    use super::*;

    crate::model_register!(
        name: "Rockchip RK3588 PCIe host slot2",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[
            ProbeKind::Fdt {
                compatibles: &["rockchip,rk3588-pcie"],
                on_probe: probe
            }
        ],
    );

    fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
        probe_rk3588(probe)
    }
}

mod rk3588_pcie_slot3 {
    use super::*;

    crate::model_register!(
        name: "Rockchip RK3588 PCIe host slot3",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[
            ProbeKind::Fdt {
                compatibles: &["rockchip,rk3588-pcie"],
                on_probe: probe
            }
        ],
    );

    fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
        probe_rk3588(probe)
    }
}

mod rk3588_pcie_slot4 {
    use super::*;

    crate::model_register!(
        name: "Rockchip RK3588 PCIe host slot4",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[
            ProbeKind::Fdt {
                compatibles: &["rockchip,rk3588-pcie"],
                on_probe: probe
            }
        ],
    );

    fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
        probe_rk3588(probe)
    }
}
