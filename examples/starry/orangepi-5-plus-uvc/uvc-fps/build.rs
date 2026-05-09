fn main() {
    println!("cargo:rerun-if-env-changed=PKG_CONFIG");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_LIBDIR");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_SYSROOT_DIR");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_ALLOW_CROSS");

    match pkg_config::Config::new().probe("libuvc") {
        Ok(_) => {}
        Err(err) => {
            println!("cargo:warning=failed to query libuvc with pkg-config: {err}");
            println!("cargo:warning=falling back to -luvc -lusb-1.0");
            println!("cargo:rustc-link-lib=uvc");
            println!("cargo:rustc-link-lib=usb-1.0");
        }
    }
}
