use std::{fs, path::Path};

fn source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read VirtIO source {path:?}: {error}"))
}

fn production_virtio_sources() -> Vec<(String, String)> {
    fn visit(root: &Path, sources: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(root).unwrap_or_else(|error| {
            panic!("failed to list VirtIO source directory {root:?}: {error}")
        }) {
            let path = entry.expect("VirtIO source entry must be readable").path();
            if path.is_dir() {
                if path.file_name().is_none_or(|name| name != "tests") {
                    visit(&path, sources);
                }
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read VirtIO source {path:?}: {error}"));
            sources.push((path.display().to_string(), source));
        }
    }

    let mut sources = Vec::new();
    visit(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src/virtio"),
        &mut sources,
    );
    sources
}

#[test]
fn every_registered_virtio_transport_is_send() {
    let root = source("src/virtio/mod.rs");
    assert!(
        root.contains("pub trait VirtIoTransport: Transport + Send + 'static"),
        "the local transport capability must make cross-thread ownership explicit"
    );
    assert!(
        root.contains("impl<T: Transport + Send + 'static> VirtIoTransport for T"),
        "all upstream transports may enter the driver boundary only after proving Send"
    );

    for (path, driver) in production_virtio_sources() {
        if path.ends_with("/src/virtio/mod.rs") {
            continue;
        }
        assert!(
            !driver.contains("T: Transport + 'static"),
            "{path} accepts a transport without proving it is safe to move"
        );
        assert!(
            !driver.contains("T: Transport"),
            "{path} bypasses the common VirtIoTransport ownership capability"
        );
        assert!(
            !driver.contains("virtio_drivers::transport::Transport"),
            "{path} uses the upstream trait directly instead of the local capability"
        );
    }
}

#[test]
fn transport_producers_preserve_the_send_capability() {
    let pci = source("src/pci/mod.rs");
    assert!(
        !pci.contains("impl Transport + 'static"),
        "PCI transport erasure must not discard the Send capability"
    );
    assert!(
        pci.contains("impl VirtIoTransport"),
        "PCI transport producers must expose the local capability boundary"
    );
}

#[test]
fn send_boundary_does_not_require_transport_sync() {
    let root = source("src/virtio/mod.rs");
    assert!(
        !root.contains("Transport + Send + Sync"),
        "VirtIO transports are serialized behind mutable/exclusive endpoints; Sync is not required"
    );

    for relative in [
        "src/virtio/block/device.rs",
        "src/virtio/net.rs",
        "src/virtio/display.rs",
        "src/virtio/input.rs",
        "src/virtio/vsock.rs",
    ] {
        assert!(
            !source(relative).contains("VirtIoTransport + Sync"),
            "{relative} widens the transport capability to Sync without a shared-access contract"
        );
    }
}
