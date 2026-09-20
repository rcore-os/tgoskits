#![feature(linkage)]
#[trait_ffi::def_extern_trait(weak_default)]
pub unsafe trait Interface { fn read() -> u8 { 1 } }
fn main() {}
