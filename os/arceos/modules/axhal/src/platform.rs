macro_rules! reject_platform_pair {
    ($left:literal, $right:literal) => {
        #[cfg(all(feature = $left, feature = $right))]
        compile_error!("multiple ax-hal platform features are enabled");
    };
}

#[cfg(all(
    feature = "myplat",
    any(
        feature = "plat-dyn",
        feature = "x86-pc",
        feature = "aarch64-qemu-virt",
        feature = "aarch64-raspi",
        feature = "aarch64-bsta1000b",
        feature = "aarch64-phytium-pi",
        feature = "riscv64-qemu-virt",
        feature = "riscv64-sg2002",
        feature = "riscv64-visionfive2",
        feature = "riscv64-qemu-virt-hv",
        feature = "loongarch64-qemu-virt",
        feature = "x86-qemu-q35"
    )
))]
compile_error!("ax-hal/myplat must not be combined with a built-in ax-hal platform feature");

reject_platform_pair!("plat-dyn", "x86-pc");
reject_platform_pair!("plat-dyn", "aarch64-qemu-virt");
reject_platform_pair!("plat-dyn", "aarch64-raspi");
reject_platform_pair!("plat-dyn", "aarch64-bsta1000b");
reject_platform_pair!("plat-dyn", "aarch64-phytium-pi");
reject_platform_pair!("plat-dyn", "riscv64-qemu-virt");
reject_platform_pair!("plat-dyn", "riscv64-sg2002");
reject_platform_pair!("plat-dyn", "riscv64-visionfive2");
reject_platform_pair!("plat-dyn", "riscv64-qemu-virt-hv");
reject_platform_pair!("plat-dyn", "loongarch64-qemu-virt");
reject_platform_pair!("plat-dyn", "x86-qemu-q35");

reject_platform_pair!("x86-pc", "x86-qemu-q35");
reject_platform_pair!("aarch64-qemu-virt", "aarch64-raspi");
reject_platform_pair!("aarch64-qemu-virt", "aarch64-bsta1000b");
reject_platform_pair!("aarch64-qemu-virt", "aarch64-phytium-pi");
reject_platform_pair!("aarch64-raspi", "aarch64-bsta1000b");
reject_platform_pair!("aarch64-raspi", "aarch64-phytium-pi");
reject_platform_pair!("aarch64-bsta1000b", "aarch64-phytium-pi");
reject_platform_pair!("riscv64-qemu-virt", "riscv64-sg2002");
reject_platform_pair!("riscv64-qemu-virt", "riscv64-visionfive2");
reject_platform_pair!("riscv64-qemu-virt", "riscv64-qemu-virt-hv");
reject_platform_pair!("riscv64-sg2002", "riscv64-visionfive2");
reject_platform_pair!("riscv64-sg2002", "riscv64-qemu-virt-hv");
reject_platform_pair!("riscv64-visionfive2", "riscv64-qemu-virt-hv");

#[cfg(all(target_os = "none", feature = "x86-pc", not(target_arch = "x86_64")))]
compile_error!("ax-hal/x86-pc requires target_arch = \"x86_64\"");
#[cfg(all(
    target_os = "none",
    feature = "x86-qemu-q35",
    not(target_arch = "x86_64")
))]
compile_error!("ax-hal/x86-qemu-q35 requires target_arch = \"x86_64\"");
#[cfg(all(
    target_os = "none",
    feature = "aarch64-qemu-virt",
    not(target_arch = "aarch64")
))]
compile_error!("ax-hal/aarch64-qemu-virt requires target_arch = \"aarch64\"");
#[cfg(all(
    target_os = "none",
    feature = "aarch64-raspi",
    not(target_arch = "aarch64")
))]
compile_error!("ax-hal/aarch64-raspi requires target_arch = \"aarch64\"");
#[cfg(all(
    target_os = "none",
    feature = "aarch64-bsta1000b",
    not(target_arch = "aarch64")
))]
compile_error!("ax-hal/aarch64-bsta1000b requires target_arch = \"aarch64\"");
#[cfg(all(
    target_os = "none",
    feature = "aarch64-phytium-pi",
    not(target_arch = "aarch64")
))]
compile_error!("ax-hal/aarch64-phytium-pi requires target_arch = \"aarch64\"");
#[cfg(all(
    target_os = "none",
    feature = "riscv64-qemu-virt",
    not(target_arch = "riscv64")
))]
compile_error!("ax-hal/riscv64-qemu-virt requires target_arch = \"riscv64\"");
#[cfg(all(
    target_os = "none",
    feature = "riscv64-sg2002",
    not(target_arch = "riscv64")
))]
compile_error!("ax-hal/riscv64-sg2002 requires target_arch = \"riscv64\"");
#[cfg(all(
    target_os = "none",
    feature = "riscv64-visionfive2",
    not(target_arch = "riscv64")
))]
compile_error!("ax-hal/riscv64-visionfive2 requires target_arch = \"riscv64\"");
#[cfg(all(
    target_os = "none",
    feature = "riscv64-qemu-virt-hv",
    not(target_arch = "riscv64")
))]
compile_error!("ax-hal/riscv64-qemu-virt-hv requires target_arch = \"riscv64\"");
#[cfg(all(
    target_os = "none",
    feature = "loongarch64-qemu-virt",
    not(target_arch = "loongarch64")
))]
compile_error!("ax-hal/loongarch64-qemu-virt requires target_arch = \"loongarch64\"");

#[cfg(all(feature = "aarch64-bsta1000b", target_arch = "aarch64"))]
extern crate ax_plat_aarch64_bsta1000b as _;
#[cfg(all(feature = "aarch64-phytium-pi", target_arch = "aarch64"))]
extern crate ax_plat_aarch64_phytium_pi as _;
#[cfg(all(feature = "aarch64-qemu-virt", target_arch = "aarch64"))]
extern crate ax_plat_aarch64_qemu_virt as _;
#[cfg(all(feature = "aarch64-raspi", target_arch = "aarch64"))]
extern crate ax_plat_aarch64_raspi as _;
#[cfg(all(feature = "loongarch64-qemu-virt", target_arch = "loongarch64"))]
extern crate ax_plat_loongarch64_qemu_virt as _;
#[cfg(all(feature = "riscv64-qemu-virt", target_arch = "riscv64"))]
extern crate ax_plat_riscv64_qemu_virt as _;
#[cfg(all(feature = "riscv64-sg2002", target_arch = "riscv64"))]
extern crate ax_plat_riscv64_sg2002 as _;
#[cfg(all(feature = "x86-pc", target_arch = "x86_64"))]
extern crate ax_plat_x86_pc as _;
#[cfg(plat_dyn)]
extern crate axplat_dyn as _;
#[cfg(all(feature = "riscv64-qemu-virt-hv", target_arch = "riscv64"))]
extern crate axplat_riscv64_qemu_virt_hv as _;
#[cfg(all(feature = "riscv64-visionfive2", target_arch = "riscv64"))]
extern crate axplat_riscv64_visionfive2 as _;
#[cfg(all(feature = "x86-qemu-q35", target_arch = "x86_64"))]
extern crate axplat_x86_qemu_q35 as _;

#[cfg(all(
    target_os = "none",
    not(feature = "myplat"),
    not(feature = "plat-dyn"),
    not(feature = "x86-pc"),
    not(feature = "x86-qemu-q35"),
    not(feature = "aarch64-qemu-virt"),
    not(feature = "aarch64-raspi"),
    not(feature = "aarch64-bsta1000b"),
    not(feature = "aarch64-phytium-pi"),
    not(feature = "riscv64-qemu-virt"),
    not(feature = "riscv64-sg2002"),
    not(feature = "riscv64-visionfive2"),
    not(feature = "riscv64-qemu-virt-hv"),
    not(feature = "loongarch64-qemu-virt"),
    feature = "defplat",
    target_arch = "aarch64"
))]
extern crate ax_plat_aarch64_qemu_virt as _;
#[cfg(all(
    target_os = "none",
    not(feature = "myplat"),
    not(feature = "plat-dyn"),
    not(feature = "x86-pc"),
    not(feature = "x86-qemu-q35"),
    not(feature = "aarch64-qemu-virt"),
    not(feature = "aarch64-raspi"),
    not(feature = "aarch64-bsta1000b"),
    not(feature = "aarch64-phytium-pi"),
    not(feature = "riscv64-qemu-virt"),
    not(feature = "riscv64-sg2002"),
    not(feature = "riscv64-visionfive2"),
    not(feature = "riscv64-qemu-virt-hv"),
    not(feature = "loongarch64-qemu-virt"),
    feature = "defplat",
    target_arch = "loongarch64"
))]
extern crate ax_plat_loongarch64_qemu_virt as _;
#[cfg(all(
    target_os = "none",
    not(feature = "myplat"),
    not(feature = "plat-dyn"),
    not(feature = "x86-pc"),
    not(feature = "x86-qemu-q35"),
    not(feature = "aarch64-qemu-virt"),
    not(feature = "aarch64-raspi"),
    not(feature = "aarch64-bsta1000b"),
    not(feature = "aarch64-phytium-pi"),
    not(feature = "riscv64-qemu-virt"),
    not(feature = "riscv64-sg2002"),
    not(feature = "riscv64-visionfive2"),
    not(feature = "riscv64-qemu-virt-hv"),
    not(feature = "loongarch64-qemu-virt"),
    feature = "defplat",
    target_arch = "riscv64"
))]
extern crate ax_plat_riscv64_qemu_virt as _;
#[cfg(all(
    target_os = "none",
    not(feature = "myplat"),
    not(feature = "plat-dyn"),
    not(feature = "x86-pc"),
    not(feature = "x86-qemu-q35"),
    not(feature = "aarch64-qemu-virt"),
    not(feature = "aarch64-raspi"),
    not(feature = "aarch64-bsta1000b"),
    not(feature = "aarch64-phytium-pi"),
    not(feature = "riscv64-qemu-virt"),
    not(feature = "riscv64-sg2002"),
    not(feature = "riscv64-visionfive2"),
    not(feature = "riscv64-qemu-virt-hv"),
    not(feature = "loongarch64-qemu-virt"),
    feature = "defplat",
    target_arch = "x86_64"
))]
extern crate ax_plat_x86_pc as _;

#[cfg(all(
    target_os = "none",
    not(test),
    not(feature = "myplat"),
    not(feature = "plat-dyn"),
    not(feature = "defplat"),
    not(feature = "x86-pc"),
    not(feature = "x86-qemu-q35"),
    not(feature = "aarch64-qemu-virt"),
    not(feature = "aarch64-raspi"),
    not(feature = "aarch64-bsta1000b"),
    not(feature = "aarch64-phytium-pi"),
    not(feature = "riscv64-qemu-virt"),
    not(feature = "riscv64-sg2002"),
    not(feature = "riscv64-visionfive2"),
    not(feature = "riscv64-qemu-virt-hv"),
    not(feature = "loongarch64-qemu-virt")
))]
compile_error!("select an ax-hal platform feature or enable ax-hal/myplat");

#[cfg(any(
    test,
    all(
        not(target_os = "none"),
        not(feature = "myplat"),
        not(feature = "plat-dyn"),
        not(feature = "defplat"),
        not(feature = "x86-pc"),
        not(feature = "x86-qemu-q35"),
        not(feature = "aarch64-qemu-virt"),
        not(feature = "aarch64-raspi"),
        not(feature = "aarch64-bsta1000b"),
        not(feature = "aarch64-phytium-pi"),
        not(feature = "riscv64-qemu-virt"),
        not(feature = "riscv64-sg2002"),
        not(feature = "riscv64-visionfive2"),
        not(feature = "riscv64-qemu-virt-hv"),
        not(feature = "loongarch64-qemu-virt")
    )
))]
#[path = "dummy.rs"]
mod dummy;

pub mod selected {
    #[cfg(all(feature = "riscv64-qemu-virt-hv", target_arch = "riscv64"))]
    pub use axplat_riscv64_qemu_virt_hv::plic_base;

    #[cfg(all(feature = "riscv64-qemu-virt-hv", target_arch = "riscv64"))]
    pub mod irq {
        pub use axplat_riscv64_qemu_virt_hv::irq::InjectIrqIf;
    }
}
