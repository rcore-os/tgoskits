const ORANGEPI_PROFILE: &str = include_str!("../../board-orangepi-5-plus.toml");
const LICHEERV_NANO_PROFILE: &str = include_str!("../../board-licheerv-nano-sg2002.toml");
const AKA_00_PROFILE: &str = include_str!("../../board-aka-00-sg2002.toml");
const VISIONFIVE2_PROFILE: &str = include_str!("../../board-visionfive2.toml");
const PHYTIUMPI_PROFILE: &str = include_str!("../../board-phytiumpi.toml");
const RK3568_PROFILE: &str = include_str!("../../board-roc-rk3568-pc.toml");
const JL_LSGD2K10_PROFILE: &str = include_str!("../../board-jl-lsgd2k10.toml");
const INIT_SCRIPT: &str = include_str!("../../init.sh");

#[test]
fn board_profiles_require_the_uploaded_session_helper() {
    for (board, profile) in [
        ("OrangePi", ORANGEPI_PROFILE),
        ("LicheeRV Nano", LICHEERV_NANO_PROFILE),
        ("AKA-00", AKA_00_PROFILE),
        ("VisionFive2", VISIONFIVE2_PROFILE),
        ("PhytiumPi", PHYTIUMPI_PROFILE),
        ("ROC-RK3568-PC", RK3568_PROFILE),
        ("JL-LSGD2K10", JL_LSGD2K10_PROFILE),
    ] {
        assert!(
            !profile.contains("BLOCK_RW_BENCH_INLINE_FALLBACK"),
            "{board} must not turn a missing session helper into another successful workload"
        );
        assert!(
            profile.contains("BLOCK_RW_BENCH_NETWORK_WAIT_SECONDS='30'"),
            "{board} must give the session HTTP path a bounded startup window"
        );
        assert!(
            profile.contains("_SESSION_FAILED"),
            "{board} must classify an unavailable session helper as failure"
        );
    }

    assert!(
        !INIT_SCRIPT.contains("run_inline_fallback"),
        "the board test must have one success-producing workload"
    );
}
