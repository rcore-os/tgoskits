use std::fs;

use ostool::build::config::LogLevel;

use super::{
    features::{
        c_compiler_features, c_config_features, c_defines, dynamic_pie_for_c_app,
        map_c_app_features,
    },
    flags::{CFlagsInput, cflags, pthread_mutex_header_contents},
    libc::{PIC_RUSTFLAG, append_pic_rustflag},
    link::{find_final_linker_script, find_link_scripts, find_linker_search_dirs},
};
use crate::build::ARCEOS_LINKER_SCRIPT;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

#[test]
fn c_config_features_skips_nested_cargo_only_features() {
    let features = c_config_features(&strings(&[
        "ax-libc/net",
        "ax-runtime/paging",
        "ax-driver/virtio-net",
        "ax-hal/custom-board",
        "some-crate/feature",
    ]));

    assert_eq!(
        features.into_iter().collect::<Vec<_>>(),
        vec!["net".to_string()]
    );
}

#[test]
fn c_config_features_ignore_removed_dynamic_platform_feature() {
    let features = c_config_features(&strings(&["plat-dyn", "multitask"]));

    assert!(features.contains("multitask"));
    assert!(!features.contains("plat-dyn"));
    assert!(!features.contains("smp"));
}

#[test]
fn c_config_features_skips_case_define_features() {
    let features = c_config_features(&strings(&["alloc", "c-define:ARCEOS_C_TEST_CASE_MEM"]));

    assert_eq!(
        features.into_iter().collect::<Vec<_>>(),
        vec!["alloc".to_string()]
    );
}

#[test]
fn c_defines_extracts_case_define_features() {
    let defines = c_defines(&strings(&[
        "alloc",
        "c-define:ARCEOS_C_TEST_CASE_MEM",
        "c-define:ARCEOS_C_TEST_CASE_NET_HTTP",
    ]));

    assert_eq!(
        defines.into_iter().collect::<Vec<_>>(),
        vec![
            "ARCEOS_C_TEST_CASE_MEM".to_string(),
            "ARCEOS_C_TEST_CASE_NET_HTTP".to_string()
        ]
    );
}

#[test]
fn c_compiler_features_keep_case_defines_for_cflags() {
    let features = c_compiler_features(
        &strings(&["alloc"]),
        &strings(&["c-define:ARCEOS_C_TEST_CASE_MEM"]),
    );
    let flags = cflags(CFlagsInput {
        workspace_root: std::path::Path::new("/workspace"),
        arch: "x86_64",
        mode: "release",
        generated_include_dir: std::path::Path::new("/generated"),
        include_dir: std::path::Path::new("/include"),
        features: &features,
        log: Some(LogLevel::Info),
        dynamic_pie: false,
    });

    assert!(flags.contains(&"-DAX_CONFIG_ALLOC".to_string()));
    assert!(flags.contains(&"-DARCEOS_C_TEST_CASE_MEM=1".to_string()));
}

#[test]
fn map_c_app_features_preserves_driver_features() {
    let features = map_c_app_features(&strings(&["net", "ax-driver/virtio-net"]), &[]).unwrap();

    assert!(features.contains(&"net".to_string()));
    assert!(features.contains(&"ax-driver/virtio-net".to_string()));
}

#[test]
fn map_c_app_features_does_not_forward_case_define_features_to_cargo() {
    let features =
        map_c_app_features(&strings(&["alloc", "c-define:ARCEOS_C_TEST_CASE_MEM"]), &[]).unwrap();

    assert_eq!(features, vec!["alloc".to_string()]);
}

#[test]
fn map_c_app_features_rejects_removed_platform_feature() {
    let err = map_c_app_features(&strings(&["alloc"]), &strings(&["plat-dyn"])).unwrap_err();

    assert!(err.to_string().contains("no longer supported"));
}

#[test]
fn c_apps_always_use_pie() {
    assert!(dynamic_pie_for_c_app(&[]));
    assert!(dynamic_pie_for_c_app(&strings(&["plat-dyn"])));
    assert!(dynamic_pie_for_c_app(&strings(&["ax-std/plat-dyn"])));
    assert!(dynamic_pie_for_c_app(&strings(&["smp"])));
}

#[test]
fn pic_rustflag_is_appended_to_axlibc_cargo_env() {
    let mut env = std::collections::HashMap::new();
    append_pic_rustflag(&mut env);
    assert_eq!(
        env.get("CARGO_ENCODED_RUSTFLAGS"),
        Some(&PIC_RUSTFLAG.to_string())
    );

    let mut env = std::collections::HashMap::from([(
        "CARGO_ENCODED_RUSTFLAGS".to_string(),
        "-Cforce-frame-pointers=yes".to_string(),
    )]);
    append_pic_rustflag(&mut env);
    assert_eq!(
        env.get("CARGO_ENCODED_RUSTFLAGS"),
        Some(&format!("-Cforce-frame-pointers=yes\x1f{PIC_RUSTFLAG}"))
    );

    let mut env = std::collections::HashMap::from([(
        "RUSTFLAGS".to_string(),
        "-Cforce-frame-pointers=yes".to_string(),
    )]);
    append_pic_rustflag(&mut env);
    assert_eq!(
        env.get("RUSTFLAGS"),
        Some(&format!("-Cforce-frame-pointers=yes {PIC_RUSTFLAG}"))
    );
}

#[test]
fn map_c_app_features_forwards_multitask_to_runtime_features() {
    let features = map_c_app_features(&strings(&["multitask"]), &[]).unwrap();

    assert!(features.contains(&"multitask".to_string()));
}

#[test]
fn map_c_app_features_preserves_paging_facade_feature() {
    let features = map_c_app_features(&strings(&["paging"]), &[]).unwrap();

    assert_eq!(features, vec!["paging".to_string()]);
}

#[test]
fn map_c_app_features_does_not_add_fd_for_higher_level_features() {
    let features = map_c_app_features(&strings(&["fs"]), &[]).unwrap();

    assert_eq!(features, strings(&["fs"]));
}

#[test]
fn pthread_mutex_header_matches_lockdep_smp_layout() {
    let header = pthread_mutex_header_contents(&strings(&["multitask", "lockdep", "smp"]));

    assert!(header.contains("long __l[10];"));
    assert!(header.contains("{-1, 0, 0, 0, 0, 0, 0, 0, 0, 0}"));
}

#[test]
fn pthread_mutex_header_matches_plain_smp_layout() {
    let header = pthread_mutex_header_contents(&strings(&["multitask", "smp"]));

    assert!(header.contains("long __l[6];"));
    assert!(header.contains("{0, 0, 8, 0, 0, 0}"));
}

#[test]
fn pthread_mutex_header_ignores_removed_dynamic_platform_feature() {
    let header = pthread_mutex_header_contents(&strings(&["multitask", "plat-dyn"]));

    assert!(header.contains("long __l[5];"));
    assert!(header.contains("{0, 8, 0, 0, 0}"));
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
fn linker_search_dirs_use_current_platform_script_owner() {
    let root = tempfile::tempdir().unwrap();
    let target_dir = root.path().join("target");
    let target = "loongarch64-unknown-none-softfloat";
    let mode = "release";
    let build_dir = target_dir.join(target).join(mode).join("build");
    let runtime_out = build_dir.join("ax-runtime-def/out");
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
        "plat-dyn",
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

#[test]
fn linker_search_dirs_use_axplat_dyn_for_generic_dynamic_platforms() {
    let root = tempfile::tempdir().unwrap();
    let target_dir = root.path().join("target");
    let target = "riscv64gc-unknown-none-elf";
    let mode = "release";
    let build_dir = target_dir.join(target).join(mode).join("build");
    let axplat_out = build_dir.join("axplat-dyn-abc/out");
    let runtime_out = build_dir.join("ax-runtime-def/out");
    fs::create_dir_all(&axplat_out).unwrap();
    fs::create_dir_all(&runtime_out).unwrap();
    fs::write(axplat_out.join("axplat.x"), "").unwrap();
    fs::write(runtime_out.join(ARCEOS_LINKER_SCRIPT), "").unwrap();

    let dirs = find_linker_search_dirs(&target_dir, target, mode, "riscv64-generic", &[]).unwrap();

    assert_eq!(dirs, vec![runtime_out, axplat_out]);
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
