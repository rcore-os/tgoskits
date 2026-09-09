struct Provider;
#[trait_ffi::impl_extern_trait(namespace = "Wrong")]
impl Interface for Provider {}
fn main() {}
