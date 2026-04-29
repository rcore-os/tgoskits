fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() != "windows"
        && std::env::var("CARGO_CFG_TARGET_OS").unwrap() != "linux"
    {
        bare_test_macros::build_test_setup!();
    }
}
