fn main() {
    let target_os = std::env::var_os("CARGO_CFG_TARGET_OS");
    let host_test = std::env::var_os("CARGO_FEATURE_HOST_TEST").is_some();
    if host_test && target_os.is_some_and(|os| os == "linux") {
        let ld_script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("host-test.ld");
        println!("cargo::rerun-if-changed={}", ld_script.display());
        println!("cargo::rustc-link-arg=-T{}", ld_script.display());
    }
}
