fn main() {
    println!("cargo:rustc-check-cfg=cfg(driver_batch, values(\"riscv-aia\", \"riscv-plic\"))");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").expect("target arch");
    let default_batch = (arch == "riscv64").then_some("riscv-aia");
    let batch = arceos_test_build::drivers::generate_bindings(default_batch);
    if arch == "riscv64" {
        println!("cargo:rustc-cfg=driver_batch=\"{batch}\"");
    }
}
