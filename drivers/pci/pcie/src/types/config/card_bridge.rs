use core::ops::Deref;

use super::PciHeaderBase;

#[derive(Debug)]
pub struct CardBusBridge {
    base: PciHeaderBase,
}

impl CardBusBridge {
    fn header(&self) -> &PciHeaderBase {
        &self.base
    }
}

impl Deref for CardBusBridge {
    type Target = PciHeaderBase;

    fn deref(&self) -> &Self::Target {
        self.header()
    }
}
