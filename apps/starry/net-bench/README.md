# apps/starry/net-bench — StarryOS 网络性能基线

当前目录提供 StarryOS `iperf3` 网络 smoke/baseline workflow，
包含 guest 侧多次迭代测量（per-boot mean/stddev）与 host 侧自动汇总（`summarize.py`）。

SLIRP/TAP 场景的绝对值不是完整性能 benchmark 结论；正式性能对比仍需
QEMU+TAP+vhost、同拓扑 Linux 基线、多次重复统计和环境指纹记录。

## 目录结构

```
apps/starry/net-bench/
├── README.md
├── run.sh                            一键启动（host 侧 server + xtask + 汇总）
├── summarize.py                      解析 run log → per-test mean/stddev 报告
├── net-bench-common.sh               guest 侧核心（多迭代 + BEGIN/END 标记）
├── net-bench.sh                      guest 入口：SLIRP，设置 HOST_IP=10.0.2.2
├── net-bench-tap.sh                  guest 入口：TAP，设置 HOST_IP=192.168.100.1
├── qemu-aarch64.toml                 SLIRP smp=1
├── qemu-aarch64-smp4.toml            SLIRP smp=4
├── qemu-aarch64-tap.toml             TAP smp=1
├── prebuild.sh                       构建前置（apk install iperf3 + overlay 安装）
├── build-aarch64-unknown-none-softfloat.toml
└── results/                          测试结果
```

## 测量覆盖

每次 QEMU 启动（guest 侧 `net-bench-common.sh`）跑：

| test-id | 说明 |
|---------|------|
| `tcp1`  | TCP 单流上行（guest → host）|
| `tcp4`  | TCP 4 并发流上行 |
| `tcp1r` | TCP 单流下行（host → guest，`-R`）|
| `udp1g` | UDP 大包，目标 1 Gbit/s |
| `udp64` | UDP 64B 小包 PPS（per-packet overhead）|

默认每 test-id 跑 1 次 warmup + 5 次测量（在 QEMU 内），host `--repeat N` 可
额外多次重启累积跨 boot 方差。

## 快速开始

```sh
# smp=1 SLIRP smoke，单次启动（default）
bash apps/starry/net-bench/run.sh aarch64

# smp=4 SLIRP，3 次重启累积
bash apps/starry/net-bench/run.sh aarch64 slirp-smp4 --repeat 3

# TAP，需提前配置 tap0
sudo ip tuntap add dev tap0 mode tap user "$USER"
sudo ip addr add 192.168.100.1/24 dev tap0
sudo ip link set tap0 up
bash apps/starry/net-bench/run.sh aarch64 tap

# 全量（slirp / slirp-smp4 / tap 依次）
bash apps/starry/net-bench/run.sh aarch64 all
```

run.sh 在每个场景结束后自动调用 `summarize.py`，输出 `results/summary-*.txt`。

## 手动汇总

```sh
# 单个 run log
python3 apps/starry/net-bench/summarize.py results/starry-aarch64-slirp-20260618-*.txt

# 跨多次重启合并（--repeat 3 生成 r1/r2/r3 三个 log）
python3 apps/starry/net-bench/summarize.py results/starry-aarch64-slirp-*-r*.txt

# 机器可读 JSON
python3 apps/starry/net-bench/summarize.py --json results/starry-aarch64-slirp-*.txt
```

相对标准差 > 10% 的指标会标注 `[NOISY >10%]`（methodology §3.4 纪律）。

## 环境指纹

`run.sh` 在每个场景前自动写入 `results/fingerprint-*.txt`，记录：
host uname / nproc、QEMU 版本与 accel、iperf3 版本、kvm/vhost-net 可用性、
Starry commit。

## 前置条件

```sh
sudo apt-get install -y iperf3
```

## 基线结果（2026-06-17，旧格式）

旧格式 log（无 BEGIN/END 标记）无法被 `summarize.py` 解析；`results/baseline-all-2026-06-17.txt`
保留手工整理的 smoke 数字供参考：

| 场景 | TCP 1流 | TCP 4流 | UDP (target 1G) |
|------|---------|---------|-----------------|
| Starry smp=1 (SLIRP) | 93.1 Mbit/s | 93.9 Mbit/s | 70.9 Mbit/s |
| Starry smp=4 (SLIRP) | 70.1 Mbit/s | 80.1 Mbit/s | 62.9 Mbit/s |
| Starry smp=1 (TAP, no vhost) | 96.3 Mbit/s | 91.0 Mbit/s | 62.7 Mbit/s |

## 常见问题

- `NET_BENCH_FAILED: iperf3 server unreachable after 15 retries`
  host 上 iperf3 server 未启动，或绑定到了非 `0.0.0.0` 的地址。
  确认 `ss -tlnp | grep 5201` 显示 `*:5201` 或 `0.0.0.0:5201`。

- `error: TCP port 5201 is already listening on host`
  停止已有 server 后重跑，或手动运行 xtask。

- `error: TAP interface tap0 does not exist`
  按上方 TAP 流程建 tap0 后重跑。

- `no NET_BENCH_BEGIN/END blocks found`
  `summarize.py` 收到的是旧格式 log（无标记）；用 `baseline-all-*.txt` 手工数字代替。
