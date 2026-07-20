use std::{fs, path::PathBuf};

fn workspace_file(relative: &str) -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .and_then(|path| path.parent())
        .expect("ax-driver must remain under the workspace drivers directory");
    fs::read_to_string(workspace.join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

#[test]
fn pci_display_irq_contains_only_its_endpoint_source() {
    let display = workspace_file("drivers/ax-driver/src/virtio/display.rs");

    assert!(display.contains("Option<crate::pci::PciIntxIrqLease>"));
    assert!(display.contains("crate::pci::PciIntxIrqLease::take_source_mask"));
    assert!(display.contains("source_mask.mask_from_irq()"));
    assert!(display.contains("lease.rearm_source(source)"));
    assert!(display.contains("ok_or(DisplayIrqFault::Uncontained)"));
}

#[test]
fn display_owner_rearms_only_the_generation_token_published_by_capture() {
    let rdif = workspace_file("drivers/interface/rdif-display/src/interface.rs");
    let runtime = workspace_file("os/arceos/modules/axruntime/src/display.rs");

    assert!(rdif.contains("fn rearm_irq(&mut self, _source: MaskedSource)"));
    assert!(runtime.contains("device.rearm_irq(source)"));
    assert!(runtime.contains("IrqReturn::DisableActionAndWake"));
    assert!(runtime.contains("IrqReturn::MaskLineAndWake"));
}

#[test]
fn stale_display_irq_generation_cannot_reenable_pci_intx() {
    let intx = workspace_file("drivers/ax-driver/src/pci/intx.rs");
    let rearm_start = intx
        .find("fn rearm(&self, source: MaskedSource)")
        .expect("PCI INTx lease must expose its generation-checked rearm transition");
    let rearm_tail = &intx[rearm_start..];
    let rearm_end = rearm_tail
        .find("\n    fn advance_generation")
        .expect("PCI INTx rearm transition must end before generation advance");
    let rearm = &rearm_tail[..rearm_end];

    let generation_check = rearm
        .find("source.generation().get()")
        .expect("rearm must validate the captured generation");
    let masked_claim = rearm
        .find("compare_exchange(true, false")
        .expect("rearm must consume the active masked-source claim once");
    let hardware_unmask = rearm
        .find("update_command(unmask_intx_command)")
        .expect("matching rearm must unmask the endpoint");

    assert!(generation_check < masked_claim);
    assert!(masked_claim < hardware_unmask);
}
