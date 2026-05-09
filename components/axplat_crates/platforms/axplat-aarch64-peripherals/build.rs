// Build-time toggle for the GICv3 backend in `gic.rs`.
//
// Runtime detection between v2 and v3 isn't possible (TCG's v2
// distributor aborts on v3-offset probes, HVF's v3 distributor aborts
// on v2-offset probes), and threading a Cargo feature all the way from
// `starryos` down to this crate would require touching every
// intermediate crate's feature graph. So we use an env var:
//
//   export AX_GIC_V3=1
//   cargo starry build --arch aarch64
//
// …turns on the v3 path. Leaving the env var unset (or 0) keeps the
// default v2 path that QEMU TCG `-cpu cortex-a72` expects.
fn main() {
    println!("cargo:rerun-if-env-changed=AX_GIC_V3");
    println!("cargo::rustc-check-cfg=cfg(gic_v3)");
    if matches!(std::env::var("AX_GIC_V3").as_deref(), Ok("1")) {
        println!("cargo:rustc-cfg=gic_v3");
    }
}
