use super::{Board, TARGET_RISCV64};

pub static QEMU_RISCV64: Board = Board {
    id: "qemu-riscv64",
    target: TARGET_RISCV64,
};
