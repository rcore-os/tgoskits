use super::*;

#[test]
fn std_build_target_maps_arceos_targets_to_dynamic_linux_musl_specs() {
    let cases = [
        (
            "x86_64-unknown-none",
            "scripts/targets/std/pie/x86_64-unknown-linux-musl.json",
        ),
        (
            "aarch64-unknown-none-softfloat",
            "scripts/targets/std/pie/aarch64-unknown-linux-musl.json",
        ),
        (
            "riscv64gc-unknown-none-elf",
            "scripts/targets/std/pie/riscv64gc-unknown-linux-musl.json",
        ),
        (
            "loongarch64-unknown-none-softfloat",
            "scripts/targets/std/pie/loongarch64-unknown-linux-musl.json",
        ),
    ];

    for (bare_target, expected_path) in cases {
        let mapped = std_build_target_for(bare_target).unwrap();
        assert!(mapped.target.ends_with(expected_path));
        assert!(
            mapped
                .cargo_args
                .windows(2)
                .any(|pair| pair == ["-Z", "json-target-spec"])
        );
        assert_eq!(
            mapped.env.get("CARGO_UNSTABLE_JSON_TARGET_SPEC"),
            Some(&"true".to_string())
        );
    }

    let riscv = std_build_target_for("riscv64gc-unknown-none-elf").unwrap();
    assert_eq!(
        riscv.env.get("CC_riscv64gc_unknown_linux_musl"),
        Some(&"riscv64-linux-musl-cc".to_string())
    );
    assert_eq!(
        riscv.env.get("AR_riscv64gc_unknown_linux_musl"),
        Some(&"riscv64-linux-musl-ar".to_string())
    );
    if let Some(bindgen_args) = riscv
        .env
        .get("BINDGEN_EXTRA_CLANG_ARGS_riscv64gc_unknown_linux_musl")
    {
        assert!(bindgen_args.contains("--target=riscv64-linux-musl"));
        assert!(bindgen_args.contains("--sysroot="));
        assert!(bindgen_args.contains("-march=rv64gc"));
        assert!(bindgen_args.contains("-mabi=lp64d"));
    }
}
