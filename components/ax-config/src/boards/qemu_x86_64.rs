use super::{Board, TARGET_X86_64};

pub static QEMU_X86_64: Board = Board {
    id: "qemu-x86_64",
    target: TARGET_X86_64,
};
