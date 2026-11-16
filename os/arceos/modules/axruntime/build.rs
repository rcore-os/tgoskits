use std::{
    env, fs,
    io::{Error, ErrorKind, Result},
    path::PathBuf,
};

use quote::quote;

const LINKER_TEMPLATE_NAME: &str = "runtime.ld";
const FINAL_LINKER_SCRIPT_NAME: &str = "linker.x";
const EXT_LINKER_SCRIPT_NAME: &str = "runtime.x";
const BUILD_INFO_NAME: &str = "build_info.rs";
const AXTEST_COVERAGE_RUNTIME_SECTIONS_PLACEHOLDER: &str = "%AXTEST_COVERAGE_RUNTIME_SECTIONS%";
const AXTEST_COVERAGE_OUTPUT_SECTIONS_PLACEHOLDER: &str = "%AXTEST_COVERAGE_OUTPUT_SECTIONS%";
const HOST_TEST_LINKER_SCRIPT_NAME: &str = "host-test.ld";

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rustc-check-cfg=cfg(has_builtin_initramfs)");
    println!("cargo:rerun-if-env-changed=AX_BUILTIN_INITRAMFS");
    if let Some(path) = env::var_os("AX_BUILTIN_INITRAMFS") {
        let path = PathBuf::from(path).canonicalize()?;
        if !path.is_file() || fs::metadata(&path)?.len() == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "built-in initramfs must be a nonempty regular file",
            )
            .into());
        }
        println!("cargo:rerun-if-changed={}", path.display());
        println!("cargo::rustc-env=AX_BUILTIN_INITRAMFS={}", path.display());
        println!("cargo::rustc-cfg=has_builtin_initramfs");
    }
    let kernel_tls = std::env::var_os("CARGO_FEATURE_TLS").is_some()
        && std::env::var_os("CARGO_FEATURE_USPACE").is_none();
    println!("cargo::rustc-check-cfg=cfg(kernel_tls)");
    if kernel_tls {
        println!("cargo::rustc-cfg=kernel_tls");
    }

    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EXT_LD");
    println!("cargo:rerun-if-env-changed=DWARF");
    println!("cargo:rerun-if-env-changed=AXTEST_COVERAGE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_HOST_TEST");
    let config = ax_config::load_build_env()?;

    if cfg!(target_os = "linux") && env::var_os("CARGO_FEATURE_HOST_TEST").is_some() {
        let host_linker_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
            .join(HOST_TEST_LINKER_SCRIPT_NAME);
        println!("cargo:rerun-if-changed={}", host_linker_path.display());
        println!("cargo:rustc-link-arg=-T{}", host_linker_path.display());
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ld_content = fs::read_to_string(LINKER_TEMPLATE_NAME)?
        .replace(
            AXTEST_COVERAGE_RUNTIME_SECTIONS_PLACEHOLDER,
            axtest_coverage_runtime_sections(),
        )
        .replace(
            AXTEST_COVERAGE_OUTPUT_SECTIONS_PLACEHOLDER,
            axtest_coverage_output_sections(),
        );
    let linker_script_name = if env::var_os("CARGO_FEATURE_EXT_LD").is_some() {
        EXT_LINKER_SCRIPT_NAME
    } else {
        FINAL_LINKER_SCRIPT_NAME
    };
    let linker_path = out_dir.join(linker_script_name);

    fs::write(&linker_path, &ld_content)?;
    fs::write(out_dir.join(BUILD_INFO_NAME), build_info_source(config)?)?;
    println!("cargo:rustc-link-search={}", out_dir.display());

    Ok(())
}

fn axtest_coverage_enabled() -> bool {
    env::var("AXTEST_COVERAGE").ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "y" | "yes" | "1" | "true" | "on"
        )
    })
}

fn axtest_coverage_runtime_sections() -> &'static str {
    if axtest_coverage_enabled() {
        r#"
        . = ALIGN(0x10);
        __start___llvm_prf_data = .;
        KEEP(*(__llvm_prf_data))
        __stop___llvm_prf_data = .;

        . = ALIGN(0x10);
        __start___llvm_prf_cnts = .;
        KEEP(*(__llvm_prf_cnts))
        __stop___llvm_prf_cnts = .;

        . = ALIGN(0x10);
        __start___llvm_prf_ucnts = .;
        KEEP(*(__llvm_prf_ucnts))
        __stop___llvm_prf_ucnts = .;

        . = ALIGN(0x10);
        __start___llvm_prf_bits = .;
        KEEP(*(__llvm_prf_bits))
        __stop___llvm_prf_bits = .;

        . = ALIGN(0x10);
        __start___llvm_prf_vnds = .;
        KEEP(*(__llvm_prf_vnds))
        __stop___llvm_prf_vnds = .;"#
    } else {
        ""
    }
}

fn axtest_coverage_output_sections() -> &'static str {
    if axtest_coverage_enabled() {
        r#"    __llvm_prf_names : AT(ADDR(__llvm_prf_names) - AX_LINKER_LOAD_OFFSET) ALIGN(0x10) {
        __start___llvm_prf_names = .;
        KEEP(*(__llvm_prf_names))
        __stop___llvm_prf_names = .;
    }
"#
    } else {
        ""
    }
}

fn build_info_source(config: ax_config::RuntimeValues) -> Result<String> {
    let arch = env::var("CARGO_CFG_TARGET_ARCH")
        .map_err(|err| std::io::Error::other(format!("CARGO_CFG_TARGET_ARCH is not set: {err}")))?;
    let target = env::var("AX_TARGET").unwrap_or_default();
    let mode = env::var("AX_MODE").unwrap_or_default();
    build_info_source_from(&arch, &target, &mode, config)
}

fn build_info_source_from(
    arch: &str,
    target: &str,
    mode: &str,
    config: ax_config::RuntimeValues,
) -> Result<String> {
    let cpu_capacity = config.cpu_capacity;
    let task_stack_size = config.task_stack_size;
    let scheduler_tick_interval_nanos = config
        .scheduler_tick_interval_nanos()
        .map_err(|error| Error::new(ErrorKind::InvalidInput, error))?;

    Ok(quote! {
        pub const ARCH: &str = #arch;
        pub const TARGET: &str = #target;
        pub const MODE: &str = #mode;

        #[cfg(feature = "smp")]
        pub const CPU_CAPACITY: usize = #cpu_capacity;

        pub const TASK_STACK_SIZE: usize = #task_stack_size;
        pub const SCHEDULER_TICK_INTERVAL_NANOS: u64 = #scheduler_tick_interval_nanos;
    }
    .to_string())
}
