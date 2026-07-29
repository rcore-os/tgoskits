# AxVisor Zephyr e1000 Validation

This note records the current e1000 network evidence for the Quancheng Lab 2026 AxVisor contest work.

## Scope

The experiment runs Zephyr RTOS as an AxVisor guest and validates IPv4/UDP communication through a QEMU e1000 NIC.

- tgoskits branch: `contest/axvisor-2026`
- tgoskits baseline: `dbc942796507d9eb20828e086bf1901584444931`
- Zephyr build: `echo_server_e1000_fixed_0x80000000_el1ns_bam_only`
- QEMU NIC: `-nic user,model=e1000`
- guest IPv4: `192.0.2.1`
- host IPv4: `192.0.2.2`
- guest UDP port: `4242`
- host forward: `127.0.0.1:14243 -> 192.0.2.1:4242`

## Result

The strict probe waits for Zephyr boot, IPv4 configuration, network-connected marker and UDP-ready marker before sending test traffic.

```text
marker_vm_created=PASS
marker_vm_booted=PASS
marker_zephyr_boot=PASS
marker_ipv4=PASS
marker_network_connected=PASS
marker_udp_ready=PASS
udp_attempt_count=20
udp_success_count=20
udp_success_rate=1.000000
udp_payload_validation=PASS
udp_rtt_min_ms=0.605
udp_rtt_mean_ms=1.070
udp_rtt_p95_ms=1.569
udp_rtt_max_ms=5.073
monitor_capture=PASS
```

QEMU monitor evidence confirms the device is an e1000 NIC:

```text
Ethernet controller: PCI device 8086:100e
e1000.0: index=0,type=nic,model=e1000,macaddr=52:54:00:12:34:56
```

## Fixes Captured In Evidence

The evidence bundle records two important fixes that made the e1000 path reliable under AxVisor:

- AxVisor GICv3 combined EOI mode: `cpu.set_eoi_mode(false)`.
- Zephyr e1000 broadcast receive enable: set `RCTL_BAM` so ARP/broadcast traffic is accepted.

Earlier runs reached Zephyr boot and IPv4 configuration but timed out on UDP. The strict passing run shows that interrupt EOI behavior and e1000 broadcast receive behavior both matter for the AxVisor/e1000 route.

## Evidence Bundle

Windows evidence directory:

```text
results/network/2026-07-25-axvisor-zephyr-e1000-strict20-pass
```

Archive:

```text
2026-07-24_axvisor-zephyr-e1000-el1ns-bam-only-host-eoi0-strict20-evidence.tar.gz
```

Archive SHA256:

```text
4fc592a221049bcf6e3a6148e65291bf777801cfa54b48665a2ac6ef02bec99a
```

Important files inside the evidence bundle:

- `report.txt`: strict 20-packet UDP result.
- `selected-console-evidence.txt`: QEMU command, AxVisor VM creation, Zephyr network markers and e1000 monitor line.
- `qemu-monitor.txt`: PCI/e1000 device evidence.
- `axvisor-gic-combined-eoi.patch`: host-side GIC EOI fix.
- `zephyr-e1000-bam.patch`: guest-side e1000 broadcast receive fix.
- `axvisor-runtime.toml`, `axvisor-guest.toml`, `axvisor-board.toml`: run configuration.
- `build-zephyr.sh`, `run-strict-diag.sh`: build and validation scripts.
- `zephyr.config`, `zephyr.dts`, `zephyr.elf`, `zephyr.bin`: Zephyr build evidence.

## Integrated Dual-Guest Use

This single-guest e1000 probe is now the RTOS-side network baseline for the integrated contest run. The current dual-guest evidence uses Linux guest `192.0.2.10` and Zephyr/e1000 RTOS guest `192.0.2.20` on an isolated per-run IPv4 bridge recorded in `bridge.txt`, then runs plain UDP echo, QCZ1 reliable UDP control, Linux-side AI inference, RTOS-side control output, Linux and RTOS periodic probes, and tcpdump validation in one script.

See `README.md`, `docs/reproduce.md` and `docs/realtime-evaluation.md` for the passing dual-guest results and long-sample realtime comparison.
