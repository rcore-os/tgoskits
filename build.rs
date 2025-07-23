fn main() {
    println!("cargo:rerun-if-env-changed=TEST");
    let test = std::env::var("TEST").unwrap_or("oscomp_pre".to_string());
    println!("cargo:rustc-cfg=test=\"{test}\"");
}
