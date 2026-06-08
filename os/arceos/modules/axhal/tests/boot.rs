#[test]
fn bootargs_facade_is_available() {
    ax_hal::dtb::init(0);

    assert_eq!(ax_hal::boot::bootargs(), None);
}
