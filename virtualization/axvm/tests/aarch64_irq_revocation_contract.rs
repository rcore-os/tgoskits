fn function_body<'a>(source: &'a str, signature: &str, next_signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let tail = &source[start..];
    let end = tail
        .find(next_signature)
        .unwrap_or_else(|| panic!("missing following function signature: {next_signature}"));
    &tail[..end]
}

fn assert_ordered(source: &str, steps: &[&str]) {
    let mut cursor = 0;
    for step in steps {
        let offset = source[cursor..]
            .find(step)
            .unwrap_or_else(|| panic!("missing ordered step: {step}"));
        cursor += offset + step.len();
    }
}

#[test]
fn aarch64_revocation_is_bounded_and_consumes_a_vgic_drain_proof() {
    let arch = include_str!("../src/arch/aarch64/mod.rs");
    let vm = include_str!("../src/arch/aarch64/vm.rs");

    assert!(arch.contains("fn revoke_guest_irq_routes"));
    assert!(arch.contains("vm::revoke_guest_irq_routes(vm)"));
    assert!(vm.contains("MAX_GICD_RWP_POLLS"));
    assert!(vm.contains("begin_assigned_spi_revocation"));
    assert!(vm.contains("SpiRevocationPoll::Pending"));
    assert!(vm.contains("SpiRevocationPoll::Complete(proof)"));
    assert!(vm.contains("crate::host::task::yield_now()"));
    assert!(!arch.contains("assigned_irqs"));
    assert!(!vm.contains("assigned_irqs"));
}

#[test]
fn vgic_hides_guest_access_before_quiesce_and_releases_after_sync() {
    let vgicd = include_str!("../../arm_vgic/src/v3/vgicd.rs");

    assert!(!vgicd.contains("pub assigned_irqs"));
    let begin = function_body(
        vgicd,
        "fn begin_assigned_spi_revocation_with",
        "impl BaseDeviceOps",
    );
    assert_ordered(
        begin,
        &[
            "let batch = self.irq_ownership.lock().begin_revocation()?",
            "control.begin_spi_quiesce(irq as u32)?",
        ],
    );
    let poll = function_body(
        vgicd,
        "fn poll_with",
        "/// Virtual Generic Interrupt Controller",
    );
    assert_ordered(
        poll,
        &[
            "control.poll_distributor_write_complete()?",
            "finish_revocation(self.batch)?",
        ],
    );
    assert!(vgicd.contains("Dropping this token does not restore guest access"));
}

#[test]
fn physical_gic_versions_disable_clear_and_poll_without_busy_waiting() {
    let host = include_str!("../src/arch/aarch64/gic.rs");
    let gicv2 = include_str!("../../../drivers/intc/arm-gic-driver/src/version/v2/mod.rs");
    let gicv3 = include_str!("../../../drivers/intc/arm-gic-driver/src/version/v3/mod.rs");

    assert!(host.contains("typed_mut::<arm_gic_driver::v2::Gic>()"));
    assert!(host.contains("typed_mut::<arm_gic_driver::v3::Gic>()"));

    for source in [gicv2, gicv3] {
        let begin = function_body(
            source,
            "pub fn begin_spi_quiesce",
            "pub fn poll_distributor_write_complete",
        );
        assert_ordered(
            begin,
            &[
                "set_irq_enable(id, false)",
                "set_pending(id, false)",
                "set_active(id, false)",
            ],
        );

        let poll = function_body(
            source,
            "pub fn poll_distributor_write_complete",
            "pub fn is_pending",
        );
        assert!(!poll.contains("while "));
        assert!(!poll.contains("loop {"));
    }
}

#[test]
fn physical_spi_target_comes_from_fixed_vcpu_placement_not_vm_identity() {
    let vm = include_str!("../src/arch/aarch64/vm.rs");
    let assignment = function_body(
        vm,
        "fn assign_passthrough_spis",
        "pub(crate) fn revoke_guest_irq_routes",
    );

    assert!(assignment.contains("passthrough_spi_target(config)?"));
    assert!(
        !assignment.contains("vm.id()"),
        "a VM identifier has no relationship to host CPU topology"
    );

    let target = function_body(
        vm,
        "fn passthrough_spi_target",
        "fn assign_passthrough_spis",
    );
    assert!(target.contains("get_vcpu_affinities_pcpu_ids"));
    assert!(target.contains("vcpu_id == 0"));
    assert!(target.contains("is_power_of_two"));
    assert!(target.contains("trailing_zeros"));
    for shift in ["mpidr >> 32", "mpidr >> 16", "mpidr >> 8"] {
        assert!(
            target.contains(shift),
            "missing MPIDR affinity extraction: {shift}"
        );
    }
}
