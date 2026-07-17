use std::{fs, path::PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn filesystem_has_no_block_driver_runtime_dependencies() {
    let root = crate_root();
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();

    for dependency in [
        "rdif-block",
        "irq-framework",
        "dma-api",
        "ax-runtime",
        "ax-task",
    ] {
        assert!(
            !manifest.contains(dependency),
            "ax-fs-ng must not depend on {dependency}"
        );
    }

    let legacy_runtime = root.join("src/block_runtime");
    let has_sources = fs::read_dir(legacy_runtime)
        .ok()
        .is_some_and(|mut entries| entries.next().is_some());
    assert!(
        !has_sources,
        "block request/IRQ scheduling belongs to ax-runtime"
    );
}

#[test]
fn filesystem_sources_do_not_contain_completion_polling_runtime() {
    let root = crate_root().join("src");
    let mut pending = vec![root];
    let forbidden = [
        "BlockCompletionMode",
        "RequestPoller",
        "poll_request",
        "poll_completions",
        "RequestFlags::POLLED",
        "RequestId",
        "irq_driven",
        "completion_wait",
        "drain_worker",
        "notify_drain",
        "wait_for_drain_notification",
        "release_block_irqs_for_passthrough",
        "VOLUME_METADATA_READ_RETRIES",
        "core::hint::spin_loop",
    ];

    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let source = fs::read_to_string(&path).unwrap();
                for symbol in forbidden {
                    assert!(
                        !source.contains(symbol),
                        "{} still contains legacy symbol {symbol}",
                        path.display()
                    );
                }
            }
        }
    }
}
