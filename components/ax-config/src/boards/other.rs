use super::{Board, TARGET_AARCH64, TARGET_RISCV64, TARGET_X86_64};

pub static K230_CANMV: Board = Board {
    id: "k230-canmv",
    target: TARGET_RISCV64,
};
pub static VISIONFIVE2: Board = Board {
    id: "visionfive2",
    target: TARGET_RISCV64,
};
pub static ASUS_NUC15CRH_X86_64: Board = Board {
    id: "asus-nuc15crh-x86_64",
    target: TARGET_X86_64,
};
pub static RDK_S100: Board = Board {
    id: "rdk-s100",
    target: TARGET_AARCH64,
};
pub static TAC_E400: Board = Board {
    id: "tac-e400",
    target: TARGET_AARCH64,
};
