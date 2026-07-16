struct RiscvPlatformIrqInjector;

#[ax_crate_interface::impl_interface]
impl axvm::irq::RiscvPlatformIrqIf for RiscvPlatformIrqInjector {
    fn register_sink(sink: axvm::irq::RiscvHardIrqSink) -> bool {
        // SAFETY: the opaque capability can only be constructed by a caller
        // that acknowledged the hard-IRQ lifetime and execution contract.
        unsafe { axplat_dyn::register_virtual_irq_sink(sink.callback()) }
    }

    fn claim_and_mask(vector: usize) -> Option<axvm::irq::RiscvPhysicalIrqClaim> {
        let claim = axplat_dyn::claim_and_mask_virtual_irq(vector)?;
        axvm::irq::RiscvPhysicalIrqClaim::try_new(claim.source(), claim.generation())
    }

    fn unmask(claim: axvm::irq::RiscvPhysicalIrqClaim, current_cpu: usize) -> bool {
        axplat_dyn::RiscvForwardedIrq::try_new(claim.source(), claim.generation())
            .is_some_and(|claim| axplat_dyn::unmask_virtual_irq(claim, current_cpu))
    }

    fn prepare_virtual_irq_targets(
        cpu_id: usize,
        irq_sources: &[u32],
        cpu_pin: &ax_cpu_local::CpuPin,
    ) -> axvm::irq::RiscvPlatformIrqRouteResult {
        map_route_result(axplat_dyn::prepare_virtual_irq_targets(
            cpu_id,
            irq_sources,
            cpu_pin,
        ))
    }

    fn activate_virtual_irq_targets(
        cpu_id: usize,
        irq_sources: &[u32],
        cpu_pin: &ax_cpu_local::CpuPin,
    ) -> axvm::irq::RiscvPlatformIrqRouteResult {
        map_route_result(axplat_dyn::activate_virtual_irq_targets(
            cpu_id,
            irq_sources,
            cpu_pin,
        ))
    }
}

fn map_route_result(
    result: axplat_dyn::RiscvVirtualIrqRouteResult,
) -> axvm::irq::RiscvPlatformIrqRouteResult {
    let status = match result.status() {
        axplat_dyn::RiscvVirtualIrqRouteStatus::Prepared => {
            axvm::irq::RiscvPlatformIrqRouteStatus::Prepared
        }
        axplat_dyn::RiscvVirtualIrqRouteStatus::Activated => {
            axvm::irq::RiscvPlatformIrqRouteStatus::Activated
        }
        axplat_dyn::RiscvVirtualIrqRouteStatus::InvalidSource => {
            axvm::irq::RiscvPlatformIrqRouteStatus::InvalidSource
        }
        axplat_dyn::RiscvVirtualIrqRouteStatus::ConflictingTarget => {
            axvm::irq::RiscvPlatformIrqRouteStatus::ConflictingTarget
        }
        axplat_dyn::RiscvVirtualIrqRouteStatus::DomainUnavailable => {
            axvm::irq::RiscvPlatformIrqRouteStatus::DomainUnavailable
        }
        axplat_dyn::RiscvVirtualIrqRouteStatus::LeaseFailed => {
            axvm::irq::RiscvPlatformIrqRouteStatus::LeaseFailed
        }
        axplat_dyn::RiscvVirtualIrqRouteStatus::EndpointConflict => {
            axvm::irq::RiscvPlatformIrqRouteStatus::EndpointConflict
        }
        axplat_dyn::RiscvVirtualIrqRouteStatus::TransactionBusy => {
            axvm::irq::RiscvPlatformIrqRouteStatus::TransactionBusy
        }
        axplat_dyn::RiscvVirtualIrqRouteStatus::RouteConflict => {
            axvm::irq::RiscvPlatformIrqRouteStatus::RouteConflict
        }
    };
    axvm::irq::RiscvPlatformIrqRouteResult {
        status,
        source: result.source(),
    }
}
