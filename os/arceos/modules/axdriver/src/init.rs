use crate::{error::Result, registers, source};

pub fn init_static_drivers() -> Result {
    rdrive::init(rdrive::Platform::Static(source::STATIC_DEVICES))?;

    let linker_registers = registers::linker_registers();
    if !linker_registers.is_empty() {
        rdrive::register_append(linker_registers);
    }

    let builtin_registers = registers::builtin_registers();
    if !builtin_registers.is_empty() {
        rdrive::register_append(builtin_registers);
    }

    rdrive::probe_pre_kernel()?;
    Ok(())
}
