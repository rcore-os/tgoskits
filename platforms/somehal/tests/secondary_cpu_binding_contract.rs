use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("somehal must live below the workspace platforms directory")
        .to_path_buf()
}

fn source(relative: &str) -> String {
    fs::read_to_string(workspace_root().join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

#[test]
fn secondary_cpu_binds_its_anchor_before_hal_or_irq_state() {
    let setup = source("platforms/somehal/src/setup.rs");
    assert!(
        setup.contains("fn bind_current_cpu(&self, binding: CpuBindingV1)"),
        "the platform runtime must expose an early CPU-anchor binding capability"
    );
    assert!(
        setup.contains("pub use ax_cpu_local::CpuBindingV1")
            && setup.contains("CpuAreaHeader) }.binding()"),
        "early binding must reuse the frozen CPU-local ABI value"
    );

    let somehal = source("platforms/somehal/src/lib.rs");
    let secondary_start = somehal
        .find("fn secondary_entry() -> !")
        .expect("somehal must define its secondary entry");
    let secondary = &somehal[secondary_start..];
    let bind = secondary
        .find("bind_current_cpu(binding)")
        .expect("secondary entry must bind its CPU-local anchor");
    let arch = secondary
        .find("arch::Plat::secondary_init()")
        .expect("secondary architecture initialization must remain explicit");
    let irq = secondary
        .find("irq::init_secondary_boot_irqs(meta.cpu_idx)")
        .expect("secondary IRQ initialization must remain explicit");

    assert!(
        bind < arch && bind < irq,
        "TPIDR/GS/scratch binding must precede every HAL or IRQ path that can use CPU-local locks"
    );

    let platform = source("platforms/axplat-dyn/src/boot.rs");
    let kernel_impl = platform
        .find("impl KernelOp for Kernel")
        .expect("dynamic platform must implement the somehal kernel boundary");
    assert!(
        platform[kernel_impl..].contains("fn bind_current_cpu(&self, binding: CpuBindingV1)"),
        "the selected platform must implement the early CPU-anchor binding capability"
    );
}
