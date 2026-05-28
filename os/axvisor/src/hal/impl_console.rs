use axvisor_api::console::ConsoleIf;
use std::os::arceos::modules::ax_hal;

pub struct ConsoleImpl;

#[axvisor_api::api_impl]
impl ConsoleIf for ConsoleImpl {
    fn write_bytes(bytes: &[u8]) {
        ax_hal::console::write_bytes(bytes);
    }

    fn read_bytes(bytes: &mut [u8]) -> usize {
        ax_hal::console::read_bytes(bytes)
    }
}
