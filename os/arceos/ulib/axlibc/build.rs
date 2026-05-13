fn main() {
    fn gen_c_to_rust_bindings(in_file: &str, out_file: &str) {
        println!("cargo:rerun-if-changed={in_file}");
        println!("cargo:rerun-if-changed=include/ax_pthread_mutex.h");

        let target = std::env::var("TARGET").unwrap();
        let allow_types = ["tm", "jmp_buf"];
        let mut builder = bindgen::Builder::default()
            .header(in_file)
            .clang_arg("-I./include")
            .derive_default(true)
            .size_t_is_usize(false)
            .use_core();
        for feature in ["MULTITASK", "SMP", "LOCKDEP"] {
            println!("cargo:rerun-if-env-changed=CARGO_FEATURE_{feature}");
            if std::env::var_os(format!("CARGO_FEATURE_{feature}")).is_some() {
                builder = builder.clang_arg(format!("-DAX_CONFIG_{feature}"));
            }
        }
        if let Some(llvm_target) = target.strip_suffix("-softfloat") {
            // remove "-softfloat" suffix for some targets
            builder = builder.clang_arg(format!("--target={llvm_target}"));
        }
        for ty in allow_types {
            builder = builder.allowlist_type(ty);
        }

        builder
            .generate()
            .expect("Unable to generate c->rust bindings")
            .write_to_file(out_file)
            .expect("Couldn't write bindings!");
    }

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_path = format!("{out_dir}/libctypes_gen.rs");
    gen_c_to_rust_bindings("ctypes.h", &out_path);
}
