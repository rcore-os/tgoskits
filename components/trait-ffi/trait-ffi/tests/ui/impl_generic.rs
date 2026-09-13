#[trait_ffi::def_extern_trait]
pub trait Interface { fn read(); }
struct Provider<T>(T);
#[trait_ffi::impl_extern_trait]
impl<T> Interface for Provider<T> { fn read() {} }
fn main() {}
