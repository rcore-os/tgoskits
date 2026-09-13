#[trait_ffi::def_extern_trait(abi = "C")]
pub trait Interface { fn read() -> String; }
fn main() {}
