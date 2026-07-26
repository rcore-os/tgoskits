use std::{fs, path::PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sources() -> String {
    let root = crate_root().join("src");
    let mut pending = vec![root];
    let mut source = String::new();
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                source.push_str(&fs::read_to_string(path).unwrap());
            }
        }
    }
    source
}

#[test]
fn layout_and_access_api_are_unversioned_and_scoped() {
    let source = sources();
    for forbidden in [
        "PerCpuLayoutV1",
        "PerCpuLayoutInitV2",
        "BoundCpuPin",
        "LayoutIdentity",
        "layout_cookie",
        "generation",
        "current_ref_raw",
        "current_ref_mut_raw",
        "read_current_raw",
        "write_current_raw",
        "current_ptr_unchecked",
        "ObjectAccess",
        "PrimitiveAccess",
    ] {
        assert!(
            !source.contains(forbidden),
            "ax-percpu still contains obsolete surface {forbidden}"
        );
    }

    assert!(source.contains("pub struct PerCpuRegion"));
    assert!(source.contains("pub struct PerCpuLayout"));
    assert!(source.contains("with_current_mut"));
    assert!(source.contains("&ExclusiveCpu<'_>"));
}

#[test]
fn c_boundary_is_scalar_and_unversioned() {
    let ffi = fs::read_to_string(crate_root().join("src/ffi.rs")).unwrap();
    assert!(ffi.contains("fn __percpu_initialize_layout("));
    assert!(!ffi.contains("__percpu_initialize_layout_v2"));
    assert!(!ffi.contains("__percpu_image_register_mode_v1"));
    assert!(!ffi.contains("PerCpuError) ->"));
}
