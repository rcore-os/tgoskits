#[test]
fn arithmetic_smoke() {
    assert_eq!(2 + 2, 4);
}

#[test]
fn explicit_result_smoke() -> Result<(), &'static str> {
    assert!(core::mem::size_of::<usize>() > 0);
    Ok(())
}
