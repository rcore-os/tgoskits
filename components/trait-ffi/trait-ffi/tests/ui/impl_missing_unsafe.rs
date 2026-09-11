#[trait_ffi::def_extern_trait]
pub unsafe trait Interface { fn read(); }
struct Provider;
#[trait_ffi::impl_extern_trait]
impl Interface for Provider { fn read() {} }
fn main() {}
