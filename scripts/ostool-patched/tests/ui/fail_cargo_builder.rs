use ostool::build::cargo_builder::CargoBuilder;

fn main() {
    let _ = core::mem::size_of::<CargoBuilder<'static>>();
}
