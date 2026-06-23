# Linux Thread Benchmark Service

This directory contains the Linux-side pieces for an automated board flow that runs the Linux thread benchmark first, reboots, and then lets U-Boot start the ArceOS `thread_perf` image.

The flow is:

```text
U-Boot boot.cmd
  /boot/bench/skip-bench exists -> boot normal Linux without bench_thread=1
  no /boot/bench/boot.flag -> boot Linux with bench_thread=1
Linux systemd service
  runs /boot/bench/thread_overhead_bench
  creates /boot/bench/boot.flag
  reboots
U-Boot boot.cmd
  sees /boot/bench/boot.flag
  removes it
  boots arceos-thread_perf.bin once
```

## Boot Partition Layout

The boot partition should contain the normal Linux boot files plus the ArceOS image and benchmark directory:

```text
/boot
  Image
  uInitrd
  dtb/...
  arceos-thread_perf.bin
  boot.cmd
  boot.scr
  bench/
    thread_overhead_bench
    skip-bench              # optional escape hatch
```

`boot.flag` is created at runtime under `/boot/bench`.

## Install The Linux Service

Copy the service and script into the Linux rootfs:

```bash
install -m 0755 run-thread-bench.sh /usr/local/bin/run-thread-bench.sh
install -m 0644 run-thread-bench.service /etc/systemd/system/run-thread-bench.service
systemctl daemon-reload
systemctl enable run-thread-bench.service
```

Place the Linux benchmark binary at:

```text
/boot/bench/thread_overhead_bench
```

Make sure it is executable:

```bash
chmod 0755 /boot/bench/thread_overhead_bench
sync
```

## Trigger Condition

The script redirects its output to `/dev/console`, so the Linux benchmark result should appear on the board serial console before rebooting.

The service only runs when the Linux kernel command line contains:

```text
bench_thread=1
```

The benchmark `boot.cmd` appends this flag to the Linux `bootargs`. If Linux is booted without `bench_thread=1`, the service exits immediately and leaves Linux running normally.

## Marker Files

Files under `/boot/bench` control and record the flow:

- `skip-bench`: force U-Boot to boot normal Linux and make the service skip the benchmark.
- `boot.flag`: created after Linux benchmark completion; U-Boot uses it to boot ArceOS once.

U-Boot removes `boot.flag` before starting ArceOS, so a later reset returns to the Linux path.
If the board U-Boot cannot remove files, create `skip-bench` to recover normal Linux boot even while `boot.flag` still exists.

## Re-run The Full Flow

From Linux, remove the runtime marker files and reboot:

```bash
rm -f /boot/bench/boot.flag /boot/bench/skip-bench
sync
reboot
```

To force normal Linux boot without running the benchmark:

```bash
touch /boot/bench/skip-bench
sync
reboot
```

## Build boot.scr

After editing `boot.cmd`, rebuild `boot.scr` on a Linux host with `mkimage`:

```bash
mkimage -C none -A arm -T script -d boot.cmd boot.scr
```

Some AArch64 U-Boot builds also accept `-A arm64`; use the same architecture option your board image normally uses.
