extern crate std;
use crate::{
    IntId,
    version::v3::{LPI, RedistributorV3, RedistributorV4, SGI},
};

#[test]
fn size_lpi() {
    let size = size_of::<LPI>();
    assert_eq!(size, 0x10000);
}

#[test]
fn size_sgi() {
    assert_eq!(size_of::<SGI>(), 0x10000);
}

#[test]
fn test_v3_rd() {
    let size = size_of::<RedistributorV3>();
    assert_eq!(size, 0x20000);
}

#[test]
fn test_v4_rd() {
    let size = size_of::<RedistributorV4>();
    assert_eq!(size, 0x40000);
}

#[test]
#[should_panic]
fn test_sgi() {
    let id = IntId::sgi(40);
    assert_eq!(id.is_sgi(), true);
}

#[test]
#[should_panic]
fn test_ppi() {
    let id = IntId::ppi(17);
    assert_eq!(id.is_private(), true);
}
