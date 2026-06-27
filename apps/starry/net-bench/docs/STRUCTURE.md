# net-bench 目录结构设计

## 目录组织

```
apps/starry/net-bench/
├── README.md                    # 项目文档
├── docs/                        # 详细文档
│   ├── TODO.md                  # 待办事项
│   ├── STRUCTURE.md             # 架构设计（本文档）
│   ├── REFACTOR.md              # 重构说明
│   ├── TEST_REPORT.md           # 测试报告
│   ├── MULTIQUEUE_ISSUE.md      # 多队列问题说明
│   └── QUICK_START.md           # 快速入门
│
├── core/                        # 核心测试逻辑（环境无关）
│   ├── net-bench-common.sh      # guest 侧核心逻辑
│   ├── net-bench-slirp.sh       # SLIRP 模式入口
│   ├── net-bench-tap.sh         # TAP/vhost 模式入口
│   ├── net-bench-netperf.sh     # netperf 延迟测试
│   ├── summarize.py             # 结果汇总
│   ├── compare-baseline.py      # 基线对比
│   └── prebuild.sh              # 构建前置（apk install）
│
├── env/                         # 环境配置脚本
│   ├── detect-env.sh            # 自动检测环境类型（WSL/裸Linux/架构/KVM）
│   ├── setup-common.sh          # 通用网络配置（br0/tap0/iperf3/dhcp）
│   └── teardown.sh              # 自动回退清理
│
├── qemu/                        # QEMU 配置文件
│   ├── vhost-x86_64-kvm.toml    # x86_64 + KVM
│   ├── vhost-x86_64-tcg.toml    # x86_64 + TCG（无KVM）
│   ├── vhost-aarch64-kvm.toml   # aarch64 + KVM
│   └── vhost-aarch64-tcg.toml   # aarch64 + TCG
│
├── build-configs/               # 构建配置
│   └── build-aarch64.toml
│
├── bin/                         # 统一入口
│   ├── bench                    # 主入口：自动检测环境并测试
│   ├── bench-wsl                # WSL2 快捷入口
│   ├── setup                    # 配置入口
│   └── teardown                 # 清理入口
│
└── results/                     # 测试结果
    └── README.md
```

## 设计原则

### 1. 环境自动检测
`env/detect-env.sh` 负责检测：
- 平台类型（WSL2 / 裸 Linux）
- CPU 架构（x86_64 / aarch64）
- KVM 可用性
- vhost-net 可用性

输出推荐配置供其他脚本使用。

### 2. 配置隔离
- WSL 和裸 Linux 配置逻辑分离
- 通用部分抽取到 `env/setup-common.sh`
- 特定平台差异由检测脚本处理

### 3. 自动回退
所有配置操作记录到 `.bench-state.json`：
```json
{
  "timestamp": "2026-06-27T14:00:00Z",
  "created_resources": [
    {"type": "bridge", "name": "br0"},
    {"type": "tap", "name": "tap0"}
  ],
  "processes": [
    {"pid": 12345, "cmd": "iperf3 -s"}
  ]
}
```
清理时读取并恢复原始状态。

### 4. 资源避让
配置脚本检测已有资源：
- 端口占用检测
- 网络设备存在性检查
- 进程重复启动避免

### 5. 默认策略
- 优先使用 x86_64 架构（最常见）
- KVM 可用时优先使用 KVM
- KVM 不可用时自动降级 TCG

### 6. 统一入口
`bin/bench` 自动完成：检测 → 配置 → 测试 → 清理

## 环境检测逻辑

`detect-env.sh` 输出 JSON 格式：
```json
{
  "platform": "wsl2",
  "arch": "x86_64",
  "kvm_available": true,
  "vhost_available": true,
  "recommended_arch": "x86_64",
  "recommended_accel": "kvm",
  "recommended_config": "qemu/vhost-x86_64-kvm.toml"
}
```

## 使用流程

### 基本流程
```bash
# 一键测试（自动检测环境）
bash apps/starry/net-bench/bin/bench
```

### 手动流程
```bash
# 1. 配置环境（首次运行）
bash apps/starry/net-bench/bin/setup

# 2. 运行测试
bash apps/starry/net-bench/bin/bench vhost --skip-setup

# 3. 清理环境
bash apps/starry/net-bench/bin/teardown
```

## 架构选择

### 默认行为
1. 检测 host 架构
2. 检测 KVM 可用性
3. 如果同架构且 KVM 可用 → 推荐 KVM 加速
4. 如果跨架构或 KVM 不可用 → 使用 TCG

### 用户指定
```bash
# 强制使用特定架构
bash bin/bench vhost --arch aarch64
```

## 配置文件命名规范

QEMU 配置文件：`<scenario>-<arch>-<accel>.toml`
- scenario: vhost, tap, slirp
- arch: x86_64, aarch64
- accel: kvm, tcg

示例：
- `vhost-x86_64-kvm.toml`
- `vhost-aarch64-tcg.toml`

## 兼容性

### 向后兼容
- 旧的 QEMU 配置文件保留（如 `qemu-aarch64-vhost.toml`）
- 旧的脚本（`run.sh`, `setup-vhost-tap.sh`）仍可用
- 新工具会尝试查找旧配置作为后备

### 迁移路径
用户可以逐步迁移到新工具，两套工具可以共存。
