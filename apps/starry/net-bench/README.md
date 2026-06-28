# StarryOS 网络性能测试套件（net-bench）

面向 StarryOS 网络栈优化的性能测试套件，提供"入口 + 参数明确"的严肃测试流程，
覆盖吞吐 / PPS / 延迟 / CPU 效率 / 多核扩展等维度，并支持与 Linux 基线同拓扑对照。

> 方法论与拓扑纪律见仓库 `www/starry-net-benchmark-methodology.md` 与
> `www/starry-net-qemu-benchmark-plan.md`。

## 设计理念

- **唯一严肃入口 `run.sh`**：架构 / 场景 / 加速器都显式指定（或用明确默认值），
  保证可复现、可控、可对照。这是日常 KPI 与优化前后对比的标准入口。
- **智能入口 `bin/bench`（实验性，非默认）**：自动检测本机环境（WSL/裸 Linux/
  KVM/vhost）并推断参数，便于开发期快速验证。它刻意保持实验性、不作为默认严肃
  测试入口；内部最终委托给 `run.sh` 执行，保证行为一致。
- **公共流程封装 `core/lib.sh`**：常量（网段/端口/拓扑）、配置矩阵解析、iperf3
  服务端生命周期、前置检查、环境指纹、结果汇总集中封装，避免散落硬编码。

## 快速开始

### 严肃测试（推荐，参数明确）

```bash
# 主力性能拓扑：TAP + vhost-net（需 sudo 预先配置网络）
sudo bash apps/starry/net-bench/bin/setup
bash apps/starry/net-bench/run.sh --scenario vhost --arch x86_64

# 多核扩展
bash apps/starry/net-bench/run.sh --scenario vhost-smp4 --arch x86_64 --repeat 5

# 功能冒烟（SLIRP，无需 sudo / 无需网络配置）
bash apps/starry/net-bench/run.sh --scenario slirp --arch x86_64

# 清理
sudo bash apps/starry/net-bench/bin/teardown
```

也可直接用 xtask 运行单个 QEMU 配置（run.sh 内部即调用它）：

```bash
cargo xtask starry app qemu \
    --test-case net-bench \
    --arch x86_64 \
    --qemu-config apps/starry/net-bench/qemu/vhost-x86_64-kvm.toml
```

> 注意：直接用 xtask 时，TAP/vhost 场景需自行用 `bin/setup` 配好网络并启动
> iperf3 服务端；`run.sh` 会自动管理 iperf3 服务端生命周期。

### 智能入口（实验性，开发期便捷）

```bash
# 自动检测环境、配置网络、跑测试、自动清理；内部委托 run.sh
bash apps/starry/net-bench/bin/bench vhost
bash apps/starry/net-bench/bin/bench-wsl        # WSL2 快捷壳
```

## 测试场景与配置矩阵

配置文件命名规范：`qemu/<scenario>-<arch>-<accel>.toml`

| 场景 | 拓扑 | 用途 |
|------|------|------|
| slirp | QEMU usermode | 功能冒烟（性能数据无意义）|
| tap | TAP（无 vhost）| 功能 / 趋势兜底 |
| vhost | TAP + vhost-net | 主力性能测试 |
| vhost-smp4 | TAP + vhost-net, smp=4 | 多核扩展 |
| tap-smp4 | TAP, smp=4 | vhost 不可用时的多核兜底 |

- arch：`aarch64` / `x86_64`
- accel：`kvm`（同架构 + KVM 可用）/ `tcg`（跨架构或无 KVM，仅功能验证）

## 测试覆盖（guest 侧自动运行）

| test-id | 说明 |
|---------|------|
| tcp1 | TCP 单流上行（guest → host）|
| tcp4 | TCP 4 并发流上行 |
| tcp1r | TCP 单流下行（host → guest）|
| udp1g | UDP 大包，目标 1 Gbit/s |
| udp64 | UDP 64B 小包 PPS |

默认每个 test-id：1 次 warmup + 5 次测量（见 `core/net-bench-common.sh`）。
延迟 / 短连接维度由 `core/net-bench-netperf.sh`（TCP_RR/UDP_RR/TCP_CRR）补充。

## 环境要求

TAP/vhost 场景的宿主配置由 `bin/setup` 一键接管（裸机可直接运行）：

```bash
sudo bash apps/starry/net-bench/bin/setup
```

`setup` 会自动：补齐依赖（`iperf3`/`iproute2`/`bridge-utils`/`jq`/`dnsmasq`，
apt 系统自动安装）、加载 `vhost_net` 模块、放开 `/dev/kvm` 与 `/dev/vhost-net`
权限、创建 `br0`/`tap0` 并配地址、启动 `dnsmasq` DHCP。iperf3 服务端由
`run.sh` 自管，不在此启动。清理用 `sudo bash apps/starry/net-bench/bin/teardown`。

```bash
# 如需手动安装依赖（非 apt 系统）：
sudo apt-get install -y iperf3 bridge-utils jq dnsmasq   # Debian/Ubuntu
```

唯一无法由脚本完成的是 **WSL2 启用嵌套虚拟化**（属于 Windows 侧配置）。
若 `/dev/kvm` 不存在，编辑 `%USERPROFILE%\.wslconfig`：

```ini
[wsl2]
nestedVirtualization=true
```

然后 `wsl --shutdown` 重启。

### guest 如何获取 IP（重要）

当前 StarryOS/ArceOS 在 `cargo xtask starry app qemu` 路径下的 guest 内核**只支持
DHCP** 获取地址——没有任何 crate 读取 `AX_IP`/`AX_GW`/`AX_PREFIX_LEN`
（`axruntime` 的 `parse_network_config` 恒为 `NetworkConfig::default()`）。因此：

- **SLIRP**：QEMU usermode 内建 DHCP 自动应答，guest 直接联网，无需额外配置。
- **TAP / vhost**：host 侧 bridge 上必须有 DHCP 服务（`bin/setup` 会启动
  `dnsmasq`）。缺 DHCP 时 guest 会 `DHCP bootstrap timed out` 并取不到地址，
  net-bench 报 `iperf3 server unreachable`。`run.sh` 的前置检查（`nb_check_tap`）
  会校验 `:67` 上有 DHCP 服务，缺失时直接报错并提示运行 `bin/setup`。

> iperf3 服务端由测试入口（`run.sh` / `run-with-perf.sh`）自管生命周期，
> `bin/setup` 不再启动 iperf3，避免与入口争用端口 5201。

## 结果分析

```bash
# 汇总 mean/stddev（NOISY 标记 >10% stddev）
python3 apps/starry/net-bench/core/summarize.py \
    apps/starry/net-bench/results/starry-*.txt

# 与 Linux 基线对比
bash apps/starry/net-bench/run-linux-baseline.sh aarch64 vhost --repeat 5
python3 apps/starry/net-bench/core/compare-baseline.py \
    results/summary-aarch64-vhost-*.txt \
    results/summary-linux-baseline-aarch64-vhost-*.txt

# CPU 效率（cycles/byte, IPC）
bash apps/starry/net-bench/run-with-perf.sh --arch aarch64 --scenario vhost
```

结果保存在 `results/`：`starry-*`（原始日志）、`summary-*`（汇总）、
`fingerprint-*`（环境指纹）、`perf-stat-*`（perf 数据）。

## 目录结构

```
apps/starry/net-bench/
├── README.md                 # 本文档
├── run.sh                    # 唯一严肃入口（显式参数）
├── run-with-perf.sh          # perf stat 包裹变体
├── run-linux-baseline.sh     # Linux 同拓扑基线
├── prebuild.sh               # rootfs 预构建（apk + guest 脚本安装）
├── bin/                      # 入口壳
│   ├── bench                 # 智能入口（实验性，委托 run.sh）
│   ├── bench-wsl             # WSL2 快捷壳
│   ├── setup                 # 配置网络（委托 env/setup-common.sh）
│   └── teardown              # 清理（委托 env/teardown.sh）
├── core/                     # 核心逻辑
│   ├── lib.sh                # 主机侧公共流程封装
│   ├── net-bench-common.sh   # guest 侧基准核心
│   ├── net-bench.sh          # SLIRP guest 入口
│   ├── net-bench-tap.sh      # TAP/vhost guest 入口
│   ├── net-bench-netperf.sh  # netperf 延迟测试
│   ├── summarize.py          # 结果汇总
│   └── compare-baseline.py   # 基线对比
├── env/                      # 环境管理
│   ├── detect-env.sh         # 环境自动检测（JSON/human）
│   ├── setup-common.sh       # 通用网络配置（br0/tap0/dnsmasq DHCP，状态化）
│   └── teardown.sh           # 状态化回滚清理
├── qemu/                     # QEMU 配置矩阵 <scenario>-<arch>-<accel>.toml
├── build-*.toml              # 构建配置（启用 virtio-net/virtio-blk）
├── docs/                     # 详细文档
└── results/                  # 测试结果
```

## 详细文档

- [快速参考](docs/QUICK_START.md)
- [架构设计](docs/STRUCTURE.md)
- [多队列问题](docs/MULTIQUEUE_ISSUE.md)
- [待办事项](docs/TODO.md)
