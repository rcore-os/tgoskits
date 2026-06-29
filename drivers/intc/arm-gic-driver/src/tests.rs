extern crate std;

#[cfg(target_arch = "aarch64")]
use crate::version::v3::{LPI, RedistributorV3, RedistributorV4, SGI};
use crate::{CheckedIntIdError, IntId, checked_intid};

#[cfg(target_arch = "aarch64")]
#[test]
fn size_lpi() {
    let size = size_of::<LPI>();
    assert_eq!(size, 0x10000);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn size_sgi() {
    assert_eq!(size_of::<SGI>(), 0x10000);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn test_v3_rd() {
    let size = size_of::<RedistributorV3>();
    assert_eq!(size, 0x20000);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn test_v4_rd() {
    assert_eq!(size_of::<RedistributorV4>(), 0x40000);
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

#[test]
fn checked_intid_rejects_special_and_out_of_range_intids() {
    assert_eq!(checked_intid(1019, 1020).unwrap().to_u32(), 1019);
    assert_eq!(checked_intid(1020, 1024), Err(CheckedIntIdError));
    assert_eq!(checked_intid(4096, 1024), Err(CheckedIntIdError));
}
