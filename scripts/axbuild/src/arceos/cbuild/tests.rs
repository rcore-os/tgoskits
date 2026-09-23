use std::fs;

use super::{
    features::map_c_app_features,
    link::{find_final_linker_script, find_link_scripts},
};
use crate::build::ARCEOS_LINKER_SCRIPT;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

#[test]
fn map_c_app_features_rejects_removed_platform_feature() {
    let err = map_c_app_features(&strings(&["alloc"]), &strings(&["plat-dyn"])).unwrap_err();

    assert!(err.to_string().contains("no longer supported"));
}

#[test]
fn final_linker_script_comes_from_axruntime_build_out_dir() {
    let root = tempfile::tempdir().unwrap();
    let target_dir = root.path().join("target");
    let target = "x86_64-unknown-none";
    let mode = "release";
    let stable_dir = target_dir.join(target).join(mode);
    let out_dir = stable_dir.join("build/ax-runtime-abc/out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::create_dir_all(&stable_dir).unwrap();
    fs::write(stable_dir.join(ARCEOS_LINKER_SCRIPT), "stable").unwrap();
    fs::write(out_dir.join(ARCEOS_LINKER_SCRIPT), "runtime").unwrap();

    let linker = find_final_linker_script(&target_dir, target, mode).unwrap();

    assert_eq!(linker, out_dir.join(ARCEOS_LINKER_SCRIPT));
}

#[test]
fn linker_scripts_support_split_build_directory_layout() {
    let root = tempfile::tempdir().unwrap();
    let target_dir = root.path().join("target");
    let target = "loongarch64-unknown-none-softfloat";
    let mode = "release";
    let build_dir = target_dir.join(target).join(mode).join("build");
    let runtime_out = build_dir.join("ax-runtime/runtime-hash/out");
    let axplat_out = build_dir.join("axplat-dyn/axplat-hash/out");
    let somehal_out = build_dir.join("somehal/somehal-hash/out");
    let someboot_out = build_dir.join("someboot/someboot-hash/out");
    fs::create_dir_all(&runtime_out).unwrap();
    fs::create_dir_all(&axplat_out).unwrap();
    fs::create_dir_all(&somehal_out).unwrap();
    fs::create_dir_all(&someboot_out).unwrap();
    fs::write(runtime_out.join(ARCEOS_LINKER_SCRIPT), "").unwrap();
    fs::write(axplat_out.join("axplat.x"), "").unwrap();
    fs::write(somehal_out.join("link.x"), "").unwrap();
    fs::write(someboot_out.join("someboot.x"), "").unwrap();

    let link_scripts = find_link_scripts(&target_dir, target, mode, "loongarch64", &[]).unwrap();

    assert_eq!(link_scripts.script, runtime_out.join(ARCEOS_LINKER_SCRIPT));
    assert!(link_scripts.search_dirs.contains(&runtime_out));
    assert!(link_scripts.search_dirs.contains(&axplat_out));
    assert!(link_scripts.search_dirs.contains(&somehal_out));
    assert!(link_scripts.search_dirs.contains(&someboot_out));
}

#[test]
fn dynamic_link_scripts_use_runtime_script_as_entrypoint() {
    let root = tempfile::tempdir().unwrap();
    let target_dir = root.path().join("target");
    let target = "aarch64-unknown-none-softfloat";
    let mode = "release";
    let build_dir = target_dir.join(target).join(mode).join("build");
    let runtime_out = build_dir.join("ax-runtime-abc/out");
    let axplat_out = build_dir.join("axplat-dyn-def/out");
    let somehal_out = build_dir.join("somehal-ghi/out");
    let someboot_out = build_dir.join("someboot-jkl/out");
    fs::create_dir_all(&runtime_out).unwrap();
    fs::create_dir_all(&axplat_out).unwrap();
    fs::create_dir_all(&somehal_out).unwrap();
    fs::create_dir_all(&someboot_out).unwrap();
    fs::write(runtime_out.join(ARCEOS_LINKER_SCRIPT), "").unwrap();
    fs::write(axplat_out.join("axplat.x"), "").unwrap();
    fs::write(somehal_out.join("link.x"), "").unwrap();
    fs::write(someboot_out.join("someboot.x"), "").unwrap();

    let link_scripts = find_link_scripts(
        &target_dir,
        target,
        mode,
        "aarch64-generic",
        &strings(&["plat-dyn"]),
    )
    .unwrap();

    assert_eq!(link_scripts.script, runtime_out.join(ARCEOS_LINKER_SCRIPT));
    assert!(link_scripts.pie);
    assert!(link_scripts.search_dirs.contains(&runtime_out));
    assert!(link_scripts.search_dirs.contains(&axplat_out));
    assert!(link_scripts.search_dirs.contains(&somehal_out));
    assert!(link_scripts.search_dirs.contains(&someboot_out));
}
