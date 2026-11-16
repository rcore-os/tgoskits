use super::{Board, TARGET_LOONGARCH64};

pub static QEMU_LOONGARCH64: Board = Board {
    id: "qemu-loongarch64",
    target: TARGET_LOONGARCH64,
};
