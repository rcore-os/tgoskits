# net-bench 快速参考

## 使用模式

### 严肃测试（推荐，参数明确）

显式指定架构 / 场景 / 加速器，保证可复现：

```bash
# 主力性能拓扑（需先配置网络）
sudo bash apps/starry/net-bench/bin/setup
bash apps/starry/net-bench/run.sh --scenario vhost --arch x86_64 --repeat 5
sudo bash apps/starry/net-bench/bin/teardown
```

也可直接用 xtask 跑单个配置（run.sh 内部即调用它）：

```bash
cargo xtask starry app qemu \
    --test-case net-bench \
    --arch x86_64 \
    --qemu-config apps/starry/net-bench/qemu/vhost-x86_64-kvm.toml
```

### 智能入口（实验性，开发期便捷）

自动检测环境并委托 run.sh：

```bash
bash apps/starry/net-bench/bin/bench vhost
bash apps/starry/net-bench/bin/bench-wsl
```

## run.sh 选项

```
--scenario S   slirp|tap|vhost|vhost-smp4|tap-smp4（默认 vhost）
--arch A       aarch64|x86_64（默认 aarch64）
--accel A      kvm|tcg（默认：同架构且 KVM 可用时 kvm）
--repeat N     每场景重启 QEMU 跑 N 次并汇总（默认 1）
--no-summary   跳过自动汇总
```

## 常用命令

```bash
# 单次 vhost
bash apps/starry/net-bench/run.sh --scenario vhost --arch x86_64

# 多次重启累积跨启动方差（推荐 >=5）
bash apps/starry/net-bench/run.sh --scenario vhost --arch x86_64 --repeat 5

# 多核扩展
bash apps/starry/net-bench/run.sh --scenario vhost-smp4 --arch x86_64 --repeat 5

# 功能冒烟（SLIRP，无需 sudo/网络配置）
bash apps/starry/net-bench/run.sh --scenario slirp --arch x86_64

# CPU 效率（perf stat）
bash apps/starry/net-bench/run-with-perf.sh --arch x86_64 --scenario vhost

# Linux 同拓扑基线（首次运行会从受管 Alpine rootfs 自动构建 initramfs）
bash apps/starry/net-bench/run-linux-baseline.sh aarch64 vhost --repeat 5
# 强制重建 initramfs（升级 rootfs 或缓存损坏时）
bash apps/starry/net-bench/run-linux-baseline.sh aarch64 vhost --rebuild-rootfs
```

## 环境管理

```bash
# 检测环境（无需 sudo）
bash apps/starry/net-bench/env/detect-env.sh

# 配置 TAP 网络（需 sudo，仅 vhost/tap 场景）
sudo bash apps/starry/net-bench/bin/setup

# 查看状态 / 清理
bash apps/starry/net-bench/bin/teardown status
sudo bash apps/starry/net-bench/bin/teardown
```

注意：`slirp` 场景用 QEMU usermode 网络，无需 sudo 配置；`vhost`/`tap` 需要 TAP 网络。

## 测试场景

| 场景 | 拓扑 | 用途 |
|------|------|------|
| vhost | TAP+vhost-net | 主力性能测试 |
| vhost-smp4 | TAP+vhost-net+SMP4 | 多核扩展 |
| tap | TAP（无vhost）| 功能/趋势兜底 |
| tap-smp4 | TAP+SMP4 | vhost 不可用时多核兜底 |
| slirp | SLIRP | 仅功能冒烟 |

## 环境要求

```bash
sudo apt-get install -y iperf3 bridge-utils jq dnsmasq
```

- KVM：`/dev/kvm` 可用（WSL2 需嵌套虚拟化）
- vhost-net：`sudo modprobe vhost_net`

## 测试结果

```bash
# 查看汇总
cat apps/starry/net-bench/results/summary-*.txt

# 手动汇总
python3 apps/starry/net-bench/core/summarize.py \
    apps/starry/net-bench/results/starry-*.txt

# 基线对比
python3 apps/starry/net-bench/core/compare-baseline.py \
    results/summary-aarch64-vhost-*.txt \
    results/summary-linux-baseline-aarch64-vhost-*.txt
```

## 故障排查

### KVM 不可用
```bash
ls -l /dev/kvm
# WSL2: 在 %USERPROFILE%\.wslconfig 加 [wsl2] nestedVirtualization=true，然后 wsl --shutdown
# 裸 Linux: sudo modprobe kvm_intel  # 或 kvm_amd
```

### vhost-net 不可用
```bash
ls -l /dev/vhost-net
sudo modprobe vhost_net
```

### 端口占用
```bash
ss -tlnp | grep 5201
sudo pkill -f "iperf3 -s"
```

## 关键指标与纪律

- 每数据点 ≥5 次迭代，取 mean + stddev；stddev >10% 标注 NOISY
- 每次测试前 warmup（丢弃首次）
- 记录环境指纹（`fingerprint-*.txt`）
- 核心 KPI：吞吐 Mbit/s、PPS、延迟 P50/P99、cycles/byte、多核扩展比

## 获取帮助

```bash
bash apps/starry/net-bench/run.sh --help
bash apps/starry/net-bench/bin/bench --help
```
