# RK3588 CPU DVFS (cpufreq)

Feature-gated ondemand CPU frequency/voltage scaling for the Orange Pi 5 Plus
(RK3588). It is off by default in the generic and QEMU builds — the
`ax-driver/rk3588-cpufreq` feature compiles the driver to no-ops there — and is
enabled on the Orange Pi board build.

## What it does

The RK3588 CPU clock is a voltage-coupled PVTPLL. An **ondemand** governor scales
each of the three CPU clusters (A55 little `cpu0-3`, two A76 big pairs `cpu4-5` /
`cpu6-7`) between OPPs by pairing an SCMI PVTPLL ring target with a PMIC rail
voltage:

- **A76** uses the full voltage lever — its RK8602/RK8603 rails read back over I2C,
  so every voltage write is confirmed and the SCMI clock rate is read back before an
  OPP is committed.
- **A55** is **ring-only**: its RK806 rail cannot be read back (a MISO hardware
  limit), so it stays on the boot-confirmed 675 mV rail and scales only its SCMI
  ring. 675 mV over-volts every A55 ring ≤ 1008 MHz, so it can never undervolt.

It is **fail-safe**: if either PMIC bus does not come up at boot, `GOV_READY` stays
`false` and every cluster is left on its boot OPP (no scaling).

## Enable and build

The feature is wired into the Orange Pi 5 Plus board config
(`os/StarryOS/configs/board/orangepi-5-plus.toml`), so the standard board
build/test entry enables it:

```bash
cargo xtask starry test board --board orangepi-5-plus
```

To enable it in a custom build, add `"ax-driver/rk3588-cpufreq"` to that config's
`features` list.

## Observable verification

At boot (early init, before the console handoff) the driver logs the rail bring-up
and governor arming to the serial console:

```
cpufreq: A55 rail boot voltage = <uV> uV
cpufreq: A55 <before>-><after>, A76 <before>-><after> MHz
cpufreq: ondemand governor armed (both PMIC buses up)
```

Under load, per-cluster OPP transitions are logged
(`gov: A76b0 peak=<n>% opp <i>-><j> = <mhz> MHz @ <mv> mV`), and the delivered
frequency can be read exactly with `cpuprobe`'s `mhz_pmc` (the PMU cycle counter is
enabled at boot): a busy cluster climbs toward its top OPP, an idle cluster sheds
back down one step at a time. The companion `apps/starry/sysbench-board` harness
(PR #1658) drives an all-core load and captures these numbers against a Linux
baseline.
