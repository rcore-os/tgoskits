#[trait_ffi::def_extern_trait]
pub trait Interface {
    /// # Safety
    /// The pointer must be readable.
    unsafe fn read(value: *const u8) -> u8;
}
fn call() { trait_ffi::call_interface!(Interface::read(&1)); }
fn main() {}
