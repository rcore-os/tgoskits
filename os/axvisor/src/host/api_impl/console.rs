use axvisor_api::console::ConsoleIf;

pub struct ConsoleImpl;

#[axvisor_api::api_impl]
impl ConsoleIf for ConsoleImpl {
    fn write_bytes(bytes: &[u8]) {
        crate::host::console::write_bytes(bytes);
    }

    fn read_bytes(bytes: &mut [u8]) -> usize {
        crate::host::console::read_bytes(bytes)
    }
}
