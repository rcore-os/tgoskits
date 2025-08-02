#![allow(unexpected_cfgs)]

cfg_if::cfg_if! {
    if #[cfg(test = "pre")] {
        pub const CMDLINE: &[&str] = &["/musl/busybox", "sh", "-c", include_str!("pre.sh")];
    } else if #[cfg(test = "final")] {
        pub const CMDLINE: &[&str] = &["/musl/busybox", "sh", "-c", include_str!("final.sh")];
    } else if #[cfg(test = "alpine")] {
        pub const CMDLINE: &[&str] = &["/bin/busybox", "sh"];
    } else {
        pub const CMDLINE: &[&str] = &[];
        compile_error!("unknown test");
    }
}
