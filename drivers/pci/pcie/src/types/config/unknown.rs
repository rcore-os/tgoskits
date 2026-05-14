use core::ops::Deref;

use super::PciHeaderBase;

#[derive(Debug)]
pub struct Unknown {
    base: PciHeaderBase,
}

impl Unknown {
    fn header(&self) -> &PciHeaderBase {
        &self.base
    }
}

impl Deref for Unknown {
    type Target = PciHeaderBase;

    fn deref(&self) -> &Self::Target {
        self.header()
    }
}
