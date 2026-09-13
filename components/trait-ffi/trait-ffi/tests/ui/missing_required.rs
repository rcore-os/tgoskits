#[trait_ffi::def_extern_trait]
pub trait Interface { fn read() -> u8; }
struct Provider;
interface::impl_trait! { impl Interface for Provider {} }
fn main() {}
