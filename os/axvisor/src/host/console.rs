use ax_std::os::arceos::modules;

pub fn write_bytes(bytes: &[u8]) {
    modules::ax_hal::console::write_bytes(bytes);
}

pub fn read_bytes(bytes: &mut [u8]) -> usize {
    modules::ax_hal::console::read_bytes(bytes)
}
