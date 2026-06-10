struct Board {
    feature: &'static str,
    arch_feature: &'static str,
    arch_cfg: &'static str,
    uefi_target: &'static str,
}

const BOARDS: &[Board] = &[Board {
    feature: "CARGO_FEATURE_BOARD_ASUS_NUC15CRH",
    arch_feature: "CARGO_FEATURE_ARCH_X86_64",
    arch_cfg: "x86_64",
    uefi_target: "x86_64-unknown-uefi",
}];

fn main() {
    let selected_boards = BOARDS
        .iter()
        .filter(|board| std::env::var_os(board.feature).is_some())
        .collect::<Vec<_>>();

    if selected_boards.is_empty() {
        return;
    }
    if selected_boards.len() > 1 {
        panic!("axloader supports exactly one board-* feature per build");
    }

    let board = selected_boards[0];
    if std::env::var_os(board.arch_feature).is_none() {
        panic!("selected axloader board feature did not enable its arch-* feature");
    }
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some(std::ffi::OsStr::new("uefi")) {
        return;
    }
    if std::env::var("TARGET").as_deref() != Ok(board.uefi_target) {
        panic!(
            "selected axloader board requires target `{}`, got `{}`",
            board.uefi_target,
            std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string())
        );
    }
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok(board.arch_cfg) {
        panic!(
            "selected axloader board requires target_arch `{}`, got `{}`",
            board.arch_cfg,
            std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "unknown".to_string())
        );
    }
}
