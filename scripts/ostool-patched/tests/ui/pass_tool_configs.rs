use ostool::{
    Tool, ToolConfig, board,
    build::{self, config::{BuildConfig, Cargo}},
    run::{
        qemu::{QemuConfig, RunQemuOptions},
        uboot::{RunUbootOptions, UbootConfig},
    },
};

fn main() {
    let tool = Tool::new(ToolConfig::default()).unwrap();
    let _: BuildConfig = tool.default_build_config();
    let cargo = Cargo::default();
    let qemu: QemuConfig = tool.default_qemu_config_for_cargo(&cargo);
    let _ = tool.default_qemu_config();
    let uboot: UbootConfig = tool.default_uboot_config();
    let _ = tool.default_board_run_config();
    let _ = RunQemuOptions::default();
    let _ = RunUbootOptions::default();
    let _ = board::RunBoardOptions::default();
    let _ = build::CargoRunnerKind::new_qemu(build::CargoQemuRunnerArgs {
        qemu: Some(qemu),
        debug: false,
        dtb_dump: false,
        show_output: true,
    });
    let _ = build::CargoRunnerKind::new_uboot(build::CargoUbootRunnerArgs {
        uboot: Some(uboot),
        show_output: true,
    });
}
