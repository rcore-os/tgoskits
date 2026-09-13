fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("Cargo must provide CARGO_MANIFEST_DIR to ax-tracepoint's build script");
    println!("cargo::rerun-if-changed=my_section.ld");
    println!("cargo::rustc-link-arg=-T{manifest_dir}/my_section.ld");
}
