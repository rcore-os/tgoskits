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
