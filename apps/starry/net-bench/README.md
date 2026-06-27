# apps/starry/net-bench — StarryOS 网络性能基线

当前目录提供 StarryOS `iperf3` 网络 smoke/baseline workflow，
包含 guest 侧多次迭代测量（per-boot mean/stddev）与 host 侧自动汇总（`summarize.py`）。

**测试拓扑分级**（对齐 `www/starry-net-qemu-benchmark-plan.md`）：
- **主力拓扑**：QEMU+TAP+**vhost-net** — 用于正式性能对比与优化验证
- **降级拓扑**：QEMU+TAP（无vhost）— 功能/趋势兜底
- **禁用于压测**：SLIRP — 仅功能冒烟，吞吐数据不可信

## 目录结构

```
apps/starry/net-bench/
├── README.md
├── run.sh                            一键启动（host 侧 server + xtask + 汇总）
├── summarize.py                      解析 run log → per-test mean/stddev 报告
├── setup-vhost-tap.sh                配置 vhost-net 环境（br0 + tap0 + 前置检查）
├── wslconfig-example.txt             WSL2 降噪配置样例
├── net-bench-common.sh               guest 侧核心（多迭代 + BEGIN/END 标记）
├── net-bench.sh                      guest 入口：SLIRP，设置 HOST_IP=10.0.2.2
├── net-bench-tap.sh                  guest 入口：TAP/vhost，设置 HOST_IP=192.168.100.1
├── qemu-aarch64.toml                 SLIRP smp=1（仅冒烟）
├── qemu-aarch64-smp4.toml            SLIRP smp=4（仅冒烟）
├── qemu-aarch64-tap.toml             TAP smp=1（无vhost，降级）
├── qemu-aarch64-vhost.toml           TAP+vhost smp=1（主力）
├── qemu-aarch64-vhost-smp4.toml      TAP+vhost smp=4（多核扩展）
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

### 功能冒烟（SLIRP，无需配置）
```sh
# smp=1 SLIRP smoke（仅验证功能，吞吐数据不可信）
bash apps/starry/net-bench/run.sh aarch64 slirp
```

### 主力性能测试（vhost-net，推荐）
```sh
# 1. 一次性环境配置（配置 br0 + tap0 + 检查 /dev/kvm 和 /dev/vhost-net）
sudo bash apps/starry/net-bench/setup-vhost-tap.sh setup

# 2. 运行性能测试
bash apps/starry/net-bench/run.sh aarch64 vhost           # smp=1
bash apps/starry/net-bench/run.sh aarch64 vhost-smp4      # smp=4（多核扩展）

# 3. 多次重启累积方差（推荐 --repeat 3-5）
bash apps/starry/net-bench/run.sh aarch64 vhost --repeat 5

# 4. 全量场景（包含 vhost/vhost-smp4）
bash apps/starry/net-bench/run.sh aarch64 all
```

### 降级场景（TAP 无 vhost）
```sh
# TAP 功能测试（vhost-net 不可用时）
bash apps/starry/net-bench/run.sh aarch64 tap
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

对齐 `www/starry-net-qemu-benchmark-plan.md §6.3` 可复现性要求。

## 前置条件

### 所有场景
```sh
sudo apt-get install -y iperf3
```

### vhost-net 场景（主力测试）
1. **WSL2 + Windows 11**（或 Win10 21H2+ with KB5020030）
2. **嵌套虚拟化**：在 `%USERPROFILE%\.wslconfig` 添加：
   ```ini
   [wsl2]
   nestedVirtualization=true
   ```
   然后重启：`wsl --shutdown`
3. **验证环境**：
   ```sh
   sudo bash apps/starry/net-bench/setup-vhost-tap.sh check
   ```
4. **一次性配置**：
   ```sh
   sudo bash apps/starry/net-bench/setup-vhost-tap.sh setup
   ```

完整 WSL2 降噪配置（固定 CPU/内存、关闭回收、绑核等）参考 `wslconfig-example.txt`。

## 基线结果（2026-06-17，旧格式）

旧格式 log（无 BEGIN/END 标记）无法被 `summarize.py` 解析；`results/baseline-all-2026-06-17.txt`
保留手工整理的 smoke 数字供参考：

| 场景 | TCP 1流 | TCP 4流 | UDP (target 1G) |
|------|---------|---------|-----------------|
| Starry smp=1 (SLIRP) | 93.1 Mbit/s | 93.9 Mbit/s | 70.9 Mbit/s |
| Starry smp=4 (SLIRP) | 70.1 Mbit/s | 80.1 Mbit/s | 62.9 Mbit/s |
| Starry smp=1 (TAP, no vhost) | 96.3 Mbit/s | 91.0 Mbit/s | 62.7 Mbit/s |

## 测试拓扑对比

| 场景 | 拓扑 | 用途 | 吞吐可信度 | 前置条件 |
|------|------|------|-----------|---------|
| **vhost** | QEMU+TAP+vhost-net | 主力性能测试与优化对比 | ✅ 高 | /dev/kvm + /dev/vhost-net + br0 |
| **vhost-smp4** | 同上 smp=4 | 多核扩展曲线 | ✅ 高 | 同上 |
| tap | QEMU+TAP（无vhost） | 功能验证/趋势兜底 | ⚠️ 中 | tap0 + host IP |
| slirp | QEMU usermode | 功能冒烟 | ❌ 低（禁用于压测） | 无 |

**规划纪律**（`www/starry-net-qemu-benchmark-plan.md §0`）：
- 所有性能数据必须来自 vhost 场景
- SLIRP 数据仅用于功能验证，不得用于性能结论
- TAP（无vhost）仅在 vhost-net 不可用时作为降级方案

## 常见问题

### vhost 场景
- `error: /dev/kvm not found`
  WSL2 需启用嵌套虚拟化，见上方前置条件。

- `error: /dev/vhost-net not found`
  运行 `sudo modprobe vhost_net` 或检查内核 `CONFIG_VHOST_NET`。

- `error: TAP interface tap0 does not exist`
  运行 `sudo bash apps/starry/net-bench/setup-vhost-tap.sh setup`。

### 所有场景
- `NET_BENCH_FAILED: iperf3 server unreachable after 15 retries`
  host 上 iperf3 server 未启动，或绑定到了非 `0.0.0.0` 的地址。
  确认 `ss -tlnp | grep 5201` 显示 `*:5201` 或 `0.0.0.0:5201`。

- `error: TCP port 5201 is already listening on host`
  停止已有 server 后重跑，或手动运行 xtask。

- `no NET_BENCH_BEGIN/END blocks found`
  `summarize.py` 收到的是旧格式 log（无标记）；用 `baseline-all-*.txt` 手工数字代替。
