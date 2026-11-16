use super::{Board, TARGET_AARCH64};

pub static QEMU_AARCH64: Board = Board {
    id: "qemu-aarch64",
    target: TARGET_AARCH64,
};
