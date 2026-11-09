use super::{Board, TARGET_AARCH64, TARGET_LOONGARCH64, TARGET_RISCV64, TARGET_X86_64};

pub static QEMU_AARCH64: Board = Board {
    id: "qemu-aarch64",
    target: TARGET_AARCH64,
};
pub static QEMU_RISCV64: Board = Board {
    id: "qemu-riscv64",
    target: TARGET_RISCV64,
};
pub static QEMU_LOONGARCH64: Board = Board {
    id: "qemu-loongarch64",
    target: TARGET_LOONGARCH64,
};
pub static QEMU_X86_64: Board = Board {
    id: "qemu-x86_64",
    target: TARGET_X86_64,
};
