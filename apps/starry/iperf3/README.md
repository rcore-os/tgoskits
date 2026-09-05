# StarryOS iperf3 benchmark

This board app runs a fixed TCP benchmark matrix on Orange Pi 5 Plus or AKA-00-SG2002. It covers
single-stream TX/RX, bidirectional traffic, 2/4/8-stream TX, and 4-stream RX.
Every scenario runs three times. The native iperf3 text is shown as the test
runs, followed by the parsed median and a final summary table. Per-run text and
the machine-readable summary remain under `${TMPDIR:-/tmp}/starry-iperf3-bench/` for later
inspection. Set `TMPDIR` before running the script to keep results in a project directory.

Run the complete benchmark from the repository root:

```bash
cargo xtask starry app board -t iperf3 -b OrangePi-5-Plus

STARRY_WIFI_SSID='<ssid>' STARRY_WIFI_PASSWORD='<password>' \
  cargo xtask starry app board -t iperf3 -b AKA-00-SG2002 \
  --board-config board-aka-00-sg2002.toml
```

The board session provides both the address of the persistent iperf3 server and
the script URL. The app's `init.sh` is merged into `shell_init_cmd`, where it
downloads and starts the benchmark script. The xtask command therefore needs
neither a fixed IP address nor a separate board launcher.

The AKA build reads the two Wi-Fi environment variables at compile time. The
`ax-driver` AIC glue validates them, derives the WPA2 PMK, and publishes a
station startup transaction. The runner changes no repository DTB; it creates
only a session-scoped DTB copy with a fresh `/chosen/rng-seed`, which is deleted
when the board run ends. After startup WPA2 and DHCP complete, the benchmark
script uses the ordinary ostool HTTP session-file path.

The benchmark profile is intentionally fixed: 10 seconds, a 2-second omit,
128K application blocks, three rounds, and a 15-second cooldown after each
connection so TCP teardown from one round cannot affect the next.
`native-network-smoke` remains the short CI-oriented connectivity check.
