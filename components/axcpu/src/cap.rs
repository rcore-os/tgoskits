//! CPU capability helpers.

/// Returns the Linux-style `AT_HWCAP` value for the current architecture.
///
/// This reports only capabilities that the CPU/runtime layer can actually make
/// available to user space. Architecture-specific auxiliary-vector layout is
/// still owned by the OS layer.
pub const fn elf_hwcap() -> usize {
    #[cfg(target_arch = "loongarch64")]
    {
        // Linux loongarch HWCAP bits (uapi/asm/hwcap.h).
        const HWCAP_LOONGARCH_CPUCFG: usize = 1 << 0;
        const HWCAP_LOONGARCH_LAM: usize = 1 << 1;
        const HWCAP_LOONGARCH_UAL: usize = 1 << 2;
        const HWCAP_LOONGARCH_FPU: usize = 1 << 3;
        const HWCAP_LOONGARCH_LSX: usize = 1 << 4;
        const HWCAP_LOONGARCH_LASX: usize = 1 << 5;

        HWCAP_LOONGARCH_CPUCFG
            | HWCAP_LOONGARCH_LAM
            | HWCAP_LOONGARCH_UAL
            | HWCAP_LOONGARCH_FPU
            | HWCAP_LOONGARCH_LSX
            | HWCAP_LOONGARCH_LASX
    }
    #[cfg(target_arch = "riscv64")]
    {
        const RISCV_COMPAT_HWCAP_IMAFDC: usize = (1 << (b'I' - b'A'))
            | (1 << (b'M' - b'A'))
            | (1 << 0)
            | (1 << (b'F' - b'A'))
            | (1 << (b'D' - b'A'))
            | (1 << (b'C' - b'A'));

        RISCV_COMPAT_HWCAP_IMAFDC
    }
    #[cfg(not(any(target_arch = "loongarch64", target_arch = "riscv64")))]
    {
        0
    }
}

/// Returns a conservative RISC-V Linux `hwprobe` value for a known key.
///
/// `None` means the key is unknown and the OS should report it as unsupported
/// according to its ABI policy.
#[cfg(target_arch = "riscv64")]
pub const fn riscv_hwprobe(key: i64) -> Option<u64> {
    const RISCV_HWPROBE_KEY_BASE_BEHAVIOR: i64 = 3;
    const RISCV_HWPROBE_BASE_BEHAVIOR_IMA: u64 = 1 << 0;
    const RISCV_HWPROBE_KEY_IMA_EXT_0: i64 = 4;
    const RISCV_HWPROBE_IMA_FD: u64 = 1 << 0;
    const RISCV_HWPROBE_IMA_C: u64 = 1 << 1;
    const RISCV_HWPROBE_KEY_CPUPERF_0: i64 = 5;
    const RISCV_HWPROBE_KEY_MISALIGNED_SCALAR_PERF: i64 = 9;
    const RISCV_HWPROBE_KEY_MISALIGNED_VECTOR_PERF: i64 = 10;

    match key {
        RISCV_HWPROBE_KEY_BASE_BEHAVIOR => Some(RISCV_HWPROBE_BASE_BEHAVIOR_IMA),
        RISCV_HWPROBE_KEY_IMA_EXT_0 => Some(RISCV_HWPROBE_IMA_FD | RISCV_HWPROBE_IMA_C),
        RISCV_HWPROBE_KEY_CPUPERF_0
        | RISCV_HWPROBE_KEY_MISALIGNED_SCALAR_PERF
        | RISCV_HWPROBE_KEY_MISALIGNED_VECTOR_PERF => Some(0),
        _ => None,
    }
}
