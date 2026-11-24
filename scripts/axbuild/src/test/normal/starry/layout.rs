//! Keep public case selectors independent of source directory placement.

pub(super) fn is_gicv2_boot_alias(path: &str) -> bool {
    matches!(
        path,
        "gicv2-boot" | "qemu/gicv2-boot" | "qemu/standard/gicv2-boot"
    )
}

pub(super) fn board_case_id(path: &str) -> String {
    path.strip_prefix("board/")
        .map_or_else(|| path.to_string(), |suffix| format!("board-{suffix}"))
}

pub(super) fn case_id(path: &str) -> String {
    if path == "tty-console-input-burst" {
        return "openrc".to_string();
    }
    if path == "qemu/tty-console-input-burst" || path == "qemu/standard/tty-console-input-burst" {
        return "qemu/openrc".to_string();
    }
    for (directory, selector) in [
        ("qemu/standard", "qemu"),
        ("qemu/x86_64/ahci-single", "ahci-single"),
        ("qemu/x86_64/timer-preemption", "timer-preemption-x86"),
        ("qemu/x86_64/e1000", "qemu-e1000"),
    ] {
        if path == directory {
            return selector.to_string();
        }
        if let Some(suffix) = path.strip_prefix(&format!("{directory}/")) {
            return format!("{selector}/{suffix}");
        }
    }
    path.to_string()
}

pub(super) fn matches_case(path: &str, selected: &str) -> bool {
    let path = case_id(path);
    let selected = case_id(selected);
    path == selected
        || path.starts_with(&format!("{selected}/"))
        || (!selected.contains('/') && path.rsplit('/').next() == Some(selected.as_str()))
}
