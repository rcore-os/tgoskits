use std::{fs, path::PathBuf};

#[test]
fn controller_sources_have_no_temporary_debug_logs() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for relative in ["src/controller/mod.rs", "src/controller/physical.rs"] {
        let source = fs::read_to_string(root.join(relative)).unwrap();
        assert!(
            !source.contains("DBG "),
            "temporary debug log remains in {relative}"
        );
    }
}
