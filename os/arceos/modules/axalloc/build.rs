fn main() {
    println!("cargo:rustc-check-cfg=cfg(arceos_std)");
    println!("cargo:rustc-check-cfg=cfg(tlsf)");
    println!("cargo:rustc-check-cfg=cfg(buddy_slab)");

    let tlsf = std::env::var("CARGO_FEATURE_TLSF").is_ok();
    let buddy_slab = std::env::var("CARGO_FEATURE_BUDDY_SLAB").is_ok();
    if tlsf {
        println!("cargo:rustc-cfg=tlsf");
    } else if buddy_slab {
        println!("cargo:rustc-cfg=buddy_slab");
    }
}
