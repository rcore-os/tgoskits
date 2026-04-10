use tgmath::{add, clamp, gcd, sub};

#[test]
fn integration_add_sub() {
    assert_eq!(add(100, 200), 300);
    assert_eq!(sub(300, 200), 100);
}

#[test]
fn integration_clamp_boundary() {
    assert_eq!(clamp(0, 0, 100), 0);
    assert_eq!(clamp(100, 0, 100), 100);
}

#[test]
fn integration_gcd_coprime() {
    assert_eq!(gcd(13, 7), 1);
}
