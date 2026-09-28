#[cfg(feature = "arceos")]
use ax_std as _;

fn main() {
    #[cfg(feature = "initramfs-smoke")]
    {
        let issue = std::fs::read_to_string("/etc/issue").expect("initramfs file is unreadable");
        assert_eq!(issue, "TGOS host initramfs\n");
        println!("HOST_CMDLINE: {}", ax_hal::boot::bootargs().unwrap_or(""));
        println!("HOST_INITRAMFS_PASSED");
    }
    println!("Hello, world!");
}
