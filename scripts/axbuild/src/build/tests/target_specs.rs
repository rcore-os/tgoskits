use super::*;

#[test]
fn std_c_toolchain_env_does_not_require_installed_cross_compiler() {
    let env = std_c_toolchain_env("riscv64gc-unknown-linux-musl", "definitely-missing-musl");

    assert_eq!(
        env.get("CC_riscv64gc_unknown_linux_musl"),
        Some(&"definitely-missing-musl-cc".to_string())
    );
    assert_eq!(
        env.get("AR_riscv64gc_unknown_linux_musl"),
        Some(&"definitely-missing-musl-ar".to_string())
    );
    assert_eq!(
        env.get("CFLAGS_riscv64gc_unknown_linux_musl"),
        Some(&"-march=rv64gc -mabi=lp64d -mcmodel=medany -fno-stack-protector".to_string())
    );
    assert_eq!(
        env.get("CXXFLAGS_riscv64gc_unknown_linux_musl"),
        Some(&"-march=rv64gc -mabi=lp64d -mcmodel=medany -fno-stack-protector".to_string())
    );
    assert!(!env.contains_key("BINDGEN_EXTRA_CLANG_ARGS_riscv64gc_unknown_linux_musl"));
}

#[test]
fn std_c_toolchain_env_exports_loongarch_softfloat_abi_flags() {
    let env = std_c_toolchain_env("loongarch64-unknown-linux-musl", "loongarch64-linux-musl");

    assert_eq!(
        env.get("CFLAGS_loongarch64_unknown_linux_musl"),
        Some(&"-mabi=lp64s -msoft-float -fno-stack-protector".to_string())
    );
    assert_eq!(
        env.get("CXXFLAGS_loongarch64_unknown_linux_musl"),
        Some(&"-mabi=lp64s -msoft-float -fno-stack-protector".to_string())
    );
    if let Some(bindgen_args) = env.get("BINDGEN_EXTRA_CLANG_ARGS_loongarch64_unknown_linux_musl") {
        assert!(bindgen_args.contains("--target=loongarch64-linux-musl"));
        assert!(bindgen_args.contains("-mabi=lp64s"));
        assert!(bindgen_args.contains("-msoft-float"));
    }
}

#[test]
fn musl_toolchain_bindgen_args_pin_clang_to_musl_toolchain() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let root = dir.path();
    let cc = root.join("bin/loongarch64-linux-musl-cc");
    let sysroot = root.join("loongarch64-linux-musl");
    let libc_include = sysroot.join("include");
    let gcc_include = root.join("lib/gcc/loongarch64-linux-musl/13.2.0/include");
    fs::create_dir_all(cc.parent().unwrap())?;
    fs::create_dir_all(&libc_include)?;
    fs::create_dir_all(&gcc_include)?;
    fs::write(&cc, "")?;

    let args = musl_toolchain_bindgen_args(
        cc.to_str().unwrap(),
        sysroot.to_str().unwrap(),
        "loongarch64-linux-musl",
    );
    let joined = args.join(" ");

    assert!(joined.contains(&format!("--gcc-toolchain={}", root.display())));
    assert!(joined.contains(&libc_include.display().to_string()));
    assert!(joined.contains(&gcc_include.display().to_string()));
    Ok(())
}

#[test]
fn bare_build_targets_use_json_specs_for_all_architectures() {
    for target_name in [
        "x86_64-unknown-none",
        "aarch64-unknown-none-softfloat",
        "riscv64gc-unknown-none-elf",
        "loongarch64-unknown-none-softfloat",
    ] {
        let target = bare_build_target_for(target_name).unwrap();

        assert_eq!(
            target.target,
            format!("scripts/targets/bare/{target_name}.json")
        );
        assert_eq!(
            target.env.get("CARGO_UNSTABLE_JSON_TARGET_SPEC"),
            Some(&"true".to_string())
        );
        assert_eq!(
            target.cargo_args,
            ["-Z", "json-target-spec", "-Z", "build-std=core,alloc"]
        );
    }
}

#[test]
fn bare_target_specs_preserve_builtin_abi_and_isa_contracts() {
    for (target_name, arch, llvm_target, features, max_atomic_width) in [
        (
            "x86_64-unknown-none",
            "x86_64",
            "x86_64-unknown-none-elf",
            "-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2,+soft-float",
            64,
        ),
        (
            "aarch64-unknown-none-softfloat",
            "aarch64",
            "aarch64-unknown-none",
            "+v8a,+strict-align,-neon",
            128,
        ),
        (
            "riscv64gc-unknown-none-elf",
            "riscv64",
            "riscv64",
            "+m,+a,+f,+d,+c,+zicsr,+zifencei",
            64,
        ),
        (
            "loongarch64-unknown-none-softfloat",
            "loongarch64",
            "loongarch64-unknown-none",
            "-f,-d,-ual",
            64,
        ),
    ] {
        let path = crate::context::workspace_root_path()
            .unwrap()
            .join(format!("scripts/targets/bare/{target_name}.json"));
        let spec: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        assert_eq!(spec["arch"], arch);
        assert_eq!(spec["llvm-target"], llvm_target);
        assert_eq!(spec["features"], features);
        assert_eq!(spec["max-atomic-width"], max_atomic_width);
        assert_eq!(spec["panic-strategy"], "abort");
        assert_eq!(spec["metadata"]["std"], false);
        assert!(spec.get("os").is_none());
        assert!(spec.get("env").is_none());
        assert!(spec.get("target-family").is_none());
    }

    let loongarch = serde_json::from_str::<serde_json::Value>(
        &fs::read_to_string(
            crate::context::workspace_root_path()
                .unwrap()
                .join("scripts/targets/bare/loongarch64-unknown-none-softfloat.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(loongarch["abi"], "softfloat");
    assert_eq!(loongarch["llvm-abiname"], "lp64s");
}
