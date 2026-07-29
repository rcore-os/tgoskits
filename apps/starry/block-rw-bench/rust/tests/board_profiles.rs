const ORANGEPI_PROFILE: &str = include_str!("../../board-orangepi-5-plus.toml");

#[test]
fn orangepi_profile_runs_without_a_starry_network_device() {
    assert!(
        ORANGEPI_PROFILE.contains("export BLOCK_RW_BENCH_INLINE_FALLBACK='1'"),
        "OrangePi StarryOS has no network device, so its bench profile must enable the serial inline workload"
    );
    assert!(
        ORANGEPI_PROFILE.contains("export BLOCK_RW_BENCH_NETWORK_WAIT_SECONDS='0'"),
        "OrangePi must not spend the board-test timeout waiting for an unavailable network device"
    );
}
