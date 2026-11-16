core::cfg_select! {
    target_arch = "x86_64" => {
        mod x86_64;
        use x86_64::run as run_case;
    }
    target_arch = "aarch64" => {
        mod aarch64;
        use aarch64::run as run_case;
    }
    target_arch = "loongarch64" => {
        mod loongarch64;
        use loongarch64::run as run_case;
    }
}

/// Checks current-architecture virtualization ownership, retirement and resource release.
pub fn run() {
    run_case();
}
