#[trait_ffi::def_extern_trait(abi = "C", abi = "Rust")]
pub trait Interface { fn read(); }
fn main() {}
