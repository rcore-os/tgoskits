use std::{fs, path::PathBuf};

#[test]
fn upper_layers_cannot_control_irq_lines_by_raw_irq_id() {
    let forbidden = [
        (
            "platforms/somehal/src/irq.rs",
            &[
                "pub fn irq_set_enable(irq: IrqId",
                "pub fn irq_set_affinity(irq: IrqId",
            ][..],
        ),
        (
            "platforms/somehal/src/common.rs",
            &["fn irq_set_enable(", "fn irq_set_affinity("][..],
        ),
        (
            "platforms/ax-plat/src/irq.rs",
            &["fn set_enable(irq: IrqId", "fn set_affinity("][..],
        ),
        (
            "platforms/axplat-dyn/src/irq.rs",
            &[
                "fn set_enable(irq: IrqId",
                "fn set_affinity(irq: IrqId",
                "somehal::irq::irq_set_enable(irq",
                "somehal::irq::irq_set_affinity(irq",
            ][..],
        ),
        (
            "os/arceos/modules/axhal/src/irq.rs",
            &["run_on_cpu_sync, set_enable, set_run_on_cpu_sync"][..],
        ),
        (
            "os/arceos/modules/axhal/src/dummy.rs",
            &["fn set_enable(_irq: IrqId", "fn set_affinity("][..],
        ),
        (
            "components/axklib/src/lib.rs",
            &["fn irq_set_enable(", "irq_set_enable as set_enable"][..],
        ),
        (
            "os/arceos/modules/axruntime/src/klib.rs",
            &["fn irq_set_enable(", "ax_hal::irq::set_enable("][..],
        ),
        (
            "drivers/ax-driver/src/test_klib.rs",
            &["fn irq_set_enable("][..],
        ),
        (
            "drivers/ax-driver/tests/binding_info.rs",
            &["fn irq_set_enable("][..],
        ),
        (
            "drivers/ax-driver/tests/model_register.rs",
            &["fn irq_set_enable("][..],
        ),
    ];

    for (relative, tokens) in forbidden {
        let source = read_workspace_source(relative);
        for token in tokens {
            assert!(
                !source.contains(token),
                "{relative} still exposes raw IRQ-line control through `{token}`"
            );
        }
    }
}

fn read_workspace_source(relative: &str) -> String {
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|platforms| platforms.parent())
        .expect("ax-plat must live under the workspace platforms directory")
        .to_path_buf();
    fs::read_to_string(project_root.join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}
