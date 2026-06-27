# net-bench 快速参考

## 一键测试
```bash
bash apps/starry/net-bench/bin/bench
```

## 常用命令

### 测试
```bash
# 默认配置（vhost，自动检测架构）
bash apps/starry/net-bench/bin/bench

# 指定场景
bash apps/starry/net-bench/bin/bench vhost
bash apps/starry/net-bench/bin/bench vhost-smp4
bash apps/starry/net-bench/bin/bench tap

# 重复测试
bash apps/starry/net-bench/bin/bench vhost --repeat 5

# 强制架构
bash apps/starry/net-bench/bin/bench vhost --arch x86_64
bash apps/starry/net-bench/bin/bench vhost --arch aarch64
```

### 环境管理
```bash
# 检测环境
bash apps/starry/net-bench/env/detect-env.sh

# 仅配置
bash apps/starry/net-bench/bin/setup

# 查看状态
sudo bash apps/starry/net-bench/bin/teardown status

# 清理
sudo bash apps/starry/net-bench/bin/teardown
```

### 高级选项
```bash
# 仅配置环境，不运行测试
bash apps/starry/net-bench/bin/bench --setup-only

# 跳过配置，直接测试
bash apps/starry/net-bench/bin/bench vhost --skip-setup

# 测试后不自动清理
bash apps/starry/net-bench/bin/bench vhost --no-cleanup
```

## 测试场景

| 场景 | 说明 | 性能 |
|------|------|------|
| vhost | TAP+vhost-net | 推荐 |
| vhost-smp4 | TAP+vhost-net+SMP4 | 多核 |
| tap | TAP（无vhost） | 降级 |
| slirp | SLIRP | 仅冒烟 |

## 环境要求

### 必需
- iperf3
- bridge-utils
- jq

### 可选
- dnsmasq（DHCP，否则 guest 需手动配 IP）
- KVM（硬件加速，否则用 TCG）
- vhost-net（高性能，否则降级 TAP）

### 安装依赖
```bash
sudo apt-get install -y iperf3 bridge-utils jq dnsmasq
```

## 架构支持

| Host 架构 | Guest 架构 | 加速 | 配置文件 |
|----------|-----------|------|---------|
| x86_64 | x86_64 | KVM | vhost-x86_64-kvm.toml（推荐）|
| x86_64 | x86_64 | TCG | vhost-x86_64-tcg.toml |
| x86_64 | aarch64 | TCG | vhost-aarch64-tcg.toml |
| aarch64 | aarch64 | KVM | vhost-aarch64-kvm.toml |

## 常见问题

### KVM 不可用
```bash
# WSL2: 启用嵌套虚拟化
# 编辑 %USERPROFILE%\.wslconfig:
[wsl2]
nestedVirtualization=true

# 然后重启
wsl --shutdown

# 裸 Linux: 检查权限
sudo chmod 666 /dev/kvm
```

### vhost-net 不可用
```bash
sudo modprobe vhost_net
```

### DHCP 失败
```bash
# 检查 dnsmasq
ps aux | grep dnsmasq

# 手动启动
sudo dnsmasq --interface=br0 --bind-interfaces \
    --dhcp-range=192.168.100.10,192.168.100.50,12h --port=0
```

### 端口占用
```bash
# 检查端口
ss -tuln | grep 5201

# 停止占用进程
sudo pkill -f "iperf3 -s"
```

## 测试结果

```bash
# 查看摘要
cat apps/starry/net-bench/results/summary-*.txt

# 手动汇总
python3 apps/starry/net-bench/core/summarize.py \
    apps/starry/net-bench/results/starry-*.txt
```

## 目录结构

```
apps/starry/net-bench/
├── bin/bench       # 主入口
├── bin/setup       # 配置
├── bin/teardown    # 清理
├── env/            # 环境脚本
├── core/           # 核心逻辑
├── qemu/           # QEMU 配置
└── results/        # 测试结果
```

## 工作流程

```
bench 命令
    ↓
检测环境 (env/detect-env.sh)
    ↓
配置网络 (env/setup-common.sh)
    ├─ 创建 br0/tap0
    ├─ 启动 iperf3
    └─ 启动 dnsmasq
    ↓
选择 QEMU 配置 (qemu/vhost-*.toml)
    ↓
运行测试 (cargo xtask starry app qemu)
    ↓
自动清理 (env/teardown.sh)
    ├─ 停止进程
    ├─ 删除网络设备
    └─ 清理状态文件
```

## 详细文档

- [README.md](../README.md) - 完整文档
- [STRUCTURE.md](STRUCTURE.md) - 架构设计
- [REFACTOR.md](REFACTOR.md) - 重构说明
- [TEST_REPORT.md](TEST_REPORT.md) - 测试报告
- [TODO.md](TODO.md) - 待办事项

## 获取帮助

```bash
bash apps/starry/net-bench/bin/bench --help
```
