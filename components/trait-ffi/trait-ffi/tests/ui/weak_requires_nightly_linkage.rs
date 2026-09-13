#[trait_ffi::def_extern_trait(weak_default)]
pub trait Interface { fn read() -> u8 { 1 } }
fn main() {}
