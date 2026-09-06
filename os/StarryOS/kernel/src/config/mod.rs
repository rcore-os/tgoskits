//! Architecture-specific configurations.

cfg_if::cfg_if! {
    if #[cfg(target_arch = "riscv64")] {
    #[rustfmt::skip]
        mod riscv64;
        pub use riscv64::*;
    } else if #[cfg(target_arch = "loongarch64")] {
        #[rustfmt::skip]
        mod loongarch64;
        pub use loongarch64::*;
    } else if #[cfg(target_arch = "x86_64")] {
        #[rustfmt::skip]
        mod x86_64;
        pub use x86_64::*;
    } else if #[cfg(target_arch = "aarch64")] {
        #[rustfmt::skip]
        mod aarch64;
        pub use aarch64::*;
    } else {
        compile_error!("Unsupported architecture");
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn user_stack_layout_is_inside_user_space() {
        const {
            assert!(USER_SPACE_BASE < USER_STACK_TOP_MAX);
            assert!(USER_STACK_SIZE > 0);
            assert!(USER_STACK_TOP_MAX <= USER_SPACE_BASE + USER_SPACE_MAX_SIZE);
        }
    }

    #[test]
    fn signal_trampoline_is_page_aligned() {
        assert_eq!(SIGNAL_TRAMPOLINE & 0xfff, 0);
    }
}
