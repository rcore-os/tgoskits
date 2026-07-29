const ORANGEPI_PROFILE: &str = include_str!("../../board-orangepi-5-plus.toml");
const JL_LSGD2K10_PROFILE: &str = include_str!("../../board-jl-lsgd2k10.toml");

#[test]
fn networked_profiles_prefer_session_http_with_inline_fallback() {
    for (board, profile) in [
        ("OrangePi", ORANGEPI_PROFILE),
        ("JL-LSGD2K10", JL_LSGD2K10_PROFILE),
    ] {
        assert!(
            profile.contains("export BLOCK_RW_BENCH_INLINE_FALLBACK='1'"),
            "{board} must retain the serial inline workload if session HTTP is unavailable"
        );
        assert!(
            profile.contains("export BLOCK_RW_BENCH_NETWORK_WAIT_SECONDS='30'"),
            "{board} must wait for its StarryOS network device before falling back"
        );
    }
}
