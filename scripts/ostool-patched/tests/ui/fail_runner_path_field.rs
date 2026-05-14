use ostool::build::CargoQemuRunnerArgs;

fn main() {
    let _ = CargoQemuRunnerArgs {
        qemu_config: None,
        debug: false,
        dtb_dump: false,
        show_output: true,
    };
}
