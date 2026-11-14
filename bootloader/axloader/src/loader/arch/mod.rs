//! Current target's preparation, image publication, clock, and identity policy.

cfg_select! {
    target_arch = "riscv64" => {
        mod riscv64;
        use riscv64 as current;
        pub(super) use riscv64::{clock_frequency, clock_ticks};
    }
    target_arch = "x86_64" => {
        mod x86_64;
        use x86_64 as current;
    }
    target_arch = "loongarch64" => {
        mod loongarch64;
        use loongarch64 as current;
    }
}

pub(super) use current::{
    ALLOW_IDENTITY_COUNTER_FALLBACK, RETRY_IDENTITY_FAILURE, flush_instruction_cache, run,
};
