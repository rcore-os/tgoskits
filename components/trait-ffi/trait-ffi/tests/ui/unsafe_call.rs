#[trait_ffi::def_extern_trait]
pub trait Interface {
    /// # Safety
    /// `value` must point to a valid byte.
    unsafe fn read(value: *const u8) -> u8;
}
fn call() { interface::read(&1); }
fn main() {}
