use std::{env, fs, path::PathBuf, process::Command};

use cc::Build;

fn main() {
    let sysroot = build_lib();
    write_static_bindings(sysroot.as_deref());
}

fn set_arch_flags(builder: &mut Build) {
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    match arch.as_str() {
        "aarch64" => {
            builder.flag("-mgeneral-regs-only");
        }
        "riscv64" => {
            builder.flag_if_supported("-march=rv64gc");
            builder.flag_if_supported("-mabi=lp64d");
            builder.flag_if_supported("-mcmodel=medany");
        }
        "x86_64" => {
            builder.flag_if_supported("-mno-sse");
        }
        "loongarch64" => {
            builder.flag_if_supported("-msoft-float");
        }
        _ => {
            panic!("unsupported architecture: {}", arch);
        }
    }
}

fn build_lib() -> Option<String> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let c_src = manifest_dir.join("./lwprintf/lwprintf/src/lwprintf/lwprintf.c");
    let include_dir = manifest_dir.join("./lwprintf/lwprintf/src/include");
    let opts_file = manifest_dir.join("lwprintf_opts.h");

    println!("cargo:rerun-if-changed={}", c_src.display());
    println!("cargo:rerun-if-changed={}", opts_file.display());
    println!(
        "cargo:rerun-if-changed={}",
        include_dir.join("lwprintf/lwprintf.h").display()
    );

    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let libc_env = env::var("CARGO_CFG_TARGET_ENV").unwrap();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let mut builder = Build::new();
    builder
        .file(&c_src)
        .include(&include_dir)
        .include(&manifest_dir)
        .flags([
            "-std=gnu99",
            "-fdata-sections",
            "-ffunction-sections",
            "-fPIC",
            "-fno-builtin",
            "-ffreestanding",
            "-fno-omit-frame-pointer",
        ])
        .warnings(true);

    let get_sysroot = |cc: &str| {
        let output = Command::new(cc)
            .args(["-print-sysroot"])
            .output()
            .expect("failed to execute gcc -print-sysroot");

        let sysroot = core::str::from_utf8(&output.stdout).unwrap();
        format!("-I{}/include/", sysroot.trim_end())
    };

    let sysroot = if os == "none" {
        let musl_gcc = format!("{}-linux-musl-gcc", arch);
        set_arch_flags(&mut builder);
        builder.compiler(&musl_gcc);
        Some(get_sysroot(&musl_gcc))
    } else if arch == "loongarch64" && libc_env == "musl" {
        let musl_gcc = format!("{}-linux-musl-gcc", arch);
        Some(get_sysroot(&musl_gcc))
    } else {
        None
    };

    builder.compile("lwprintf");
    sysroot
}

fn write_static_bindings(_sysroot: Option<&str>) {
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_path.join("lwprintf.rs"), STATIC_BINDINGS).expect("write lwprintf bindings");
}

const STATIC_BINDINGS: &str = r#"
pub const SIZE_MAX: i32 = -1;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct lwprintf_t {
    pub out_fn: lwprintf_output_fn,
    pub arg: *mut ::core::ffi::c_void,
}

pub type lwprintf_output_fn = ::core::option::Option<
    unsafe extern "C" fn(
        ch: ::core::ffi::c_int,
        lwobj: *mut lwprintf_t,
    ) -> ::core::ffi::c_int,
>;

unsafe extern "C" {
    pub fn lwprintf_init_ex(
        lwobj: *mut lwprintf_t,
        out_fn: lwprintf_output_fn,
    ) -> u8;
}
"#;
