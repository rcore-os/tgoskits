#[cfg(feature = "arceos")]
use ax_std as _;

fn main() {
    #[cfg(any(feature = "cmdline-smoke", feature = "initramfs-smoke"))]
    println!("HOST_CMDLINE: {}", ax_hal::boot::bootargs().unwrap_or(""));
    #[cfg(feature = "initramfs-smoke")]
    {
        match ax_fs_ng::root::root_kind().expect("root filesystem selected") {
            ax_fs_ng::root::RootKind::Memory => {
                let issue =
                    std::fs::read_to_string("/etc/issue").expect("initramfs file is unreadable");
                assert_eq!(issue, "TGOS host initramfs\n");
                println!("HOST_INITRAMFS_PASSED");
            }
            ax_fs_ng::root::RootKind::Block => {
                assert!(
                    !std::fs::read_to_string("/etc/alpine-release")
                        .unwrap()
                        .is_empty()
                );
                assert_ne!(
                    std::fs::read_to_string("/etc/issue").unwrap_or_default(),
                    "TGOS host initramfs\n"
                );
                println!("HOST_DISK_ROOT_PASSED");
            }
        }
    }
    println!("Hello, world!");
}
