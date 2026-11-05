fn main() {
    clang_sys::load().expect("musl host build scripts must be able to load libclang");
    println!("cargo:rerun-if-changed=build.rs");
}
