//! Source contracts for PCI xHCI IRQ routing and endpoint-gate ownership.

use std::{fs, path::PathBuf};

fn source(relative: &str) -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

#[test]
fn pci_xhci_retains_required_intx_route_and_endpoint_gate() {
    let source = source("src/usb/xhci_pci.rs");

    for required in [
        "binding_info_from_pci_endpoint",
        "PciIrqRequirement::Required",
        "probe.take_endpoint()",
        "PciIntxIrqLease::new",
        "register_usb_host_with_irq_lease",
    ] {
        assert!(
            source.contains(required),
            "PCI xHCI discovery is missing IRQ ownership step `{required}`"
        );
    }

    let resolve = source
        .find("binding_info_from_pci_endpoint")
        .expect("PCI xHCI must resolve its route");
    let take = source
        .find("probe.take_endpoint()")
        .expect("PCI xHCI must retain its endpoint");
    assert!(
        resolve < take,
        "the ACPI/FDT INTx route and BAR resources must be captured before endpoint ownership \
         moves"
    );
    assert!(
        !source.contains("BindingInfo::empty()"),
        "PCI xHCI must not erase its resolved IRQ binding"
    );
}

#[test]
fn usb_host_keeps_irq_metadata_and_gate_in_one_private_owner() {
    let source = source("src/usb/mod.rs");

    for required in [
        "enum UsbIrqBindingOwner",
        "EndpointGate",
        "lease.binding_info()",
        "enable_usb_irq_transaction",
        "disable_usb_irq_transaction",
    ] {
        assert!(
            source.contains(required),
            "USB host IRQ lifecycle is missing ownership contract `{required}`"
        );
    }

    let enable = function_body(&source, "fn enable_usb_irq_transaction");
    assert_ordered(enable, &["enable_device_irq", "enable_binding_irq"]);

    let disable = function_body(&source, "fn disable_usb_irq_transaction");
    assert_ordered(disable, &["disable_device_irq", "disable_binding_irq"]);

    assert!(
        !source.contains("trait ProbePciUsbHost"),
        "PCI USB registration must not expose a metadata-only path that loses endpoint ownership"
    );
    assert!(
        !source.contains("trait PlatformDeviceUsbHost"),
        "raw platform registration must not let a PCI caller bypass its endpoint gate lease"
    );
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let tail = &source[start..];
    let open = tail.find('{').expect("function must have a body");
    let mut depth = 0usize;
    for (offset, byte) in tail[open..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &tail[..open + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function `{signature}`")
}

fn assert_ordered(source: &str, needles: &[&str]) {
    let mut cursor = 0usize;
    for needle in needles {
        let offset = source[cursor..]
            .find(needle)
            .unwrap_or_else(|| panic!("missing ordered step `{needle}`"));
        cursor += offset + needle.len();
    }
}
