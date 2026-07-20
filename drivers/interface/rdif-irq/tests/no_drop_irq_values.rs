use std::{fs, path::PathBuf};

#[test]
fn portable_irq_results_cannot_carry_drop_state() {
    let source = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("read rdif-irq public surface");

    assert!(source.contains("type Event: Copy + Send + 'static;"));
    assert!(source.contains("type Fault: Copy + Error + Send + Sync + 'static;"));
}
