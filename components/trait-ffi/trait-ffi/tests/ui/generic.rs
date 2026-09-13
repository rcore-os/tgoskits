#[trait_ffi::def_extern_trait]
pub trait Interface { fn read<T>(value: T); }
fn main() {}
