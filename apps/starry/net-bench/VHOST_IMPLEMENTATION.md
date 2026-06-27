# StarryOS 网络性能测试 vhost-net 方案

## 方案概述

本方案实现了 `www/starry-net-qemu-benchmark-plan.md` 要求的**主力性能测试拓扑**：QEMU+TAP+vhost-net，解决当前 `apps/starry/net-bench` 测试基础设施与规划文档的核心差距。

### 设计目标
1. ✅ 提供符合主力测试要求的 vhost-net 拓扑配置
2. ✅ 自动化环境搭建与前置条件检查
3. ✅ 集成 WSL2 降噪配置指导
4. ✅ 保持向后兼容（SLIRP/TAP 场景仍可用）
5. ✅ 对齐文档规划纪律（禁用 SLIRP 于压测、主力数据来自 vhost）

---

## 交付物清单

### 新增文件（4 个）

| 文件 | 行数 | 用途 |
|------|------|------|
| `qemu-aarch64-vhost.toml` | 47 | vhost-net smp=1 QEMU 配置 |
| `qemu-aarch64-vhost-smp4.toml` | 27 | vhost-net smp=4 多核扩展配置 |
| `setup-vhost-tap.sh` | 236 | 环境自动化配置脚本 |
| `wslconfig-example.txt` | 91 | WSL2 降噪配置样例 + 检查清单 |

### 修改文件（2 个）

| 文件 | 变更 | 主要内容 |
|------|------|----------|
| `run.sh` | +69/-17 | 新增 vhost/vhost-smp4 场景、check_vhost 前置检查、更新 usage |
| `README.md` | +86/-43 | 新增拓扑对比表、vhost 快速开始、前置条件说明 |

**总计**：+401 行新增功能代码与文档

---

## 核心实现

### 1. QEMU 配置（vhost-net + 多队列 + offload）

**`qemu-aarch64-vhost.toml` 关键参数**：
```toml
args = [
    "-accel", "kvm",                              # 必须：KVM 加速
    "-cpu", "host",
    "-device", "virtio-net-pci,netdev=net0,
        mac=52:54:00:12:34:56,
        mq=on,vectors=10,                         # 多队列（queues=4 需 10 vectors）
        csum=on,gso=on,
        host_tso4=on,host_tso6=on,
        guest_tso4=on,guest_tso6=on",             # offload 显式控制
    "-netdev", "tap,id=net0,ifname=tap0,
        script=no,downscript=no,
        vhost=on,queues=4",                       # vhost-net + 多队列
]
```

**对齐规划要求**（qemu-plan §2.3）：
- ✅ `vhost=on`：数据面卸载到内核，绕开 QEMU 用户态瓶颈
- ✅ `mq=on,queues=4`：多队列预留（当前 Starry 单队列，驱动改造后生效）
- ✅ `csum/gso/tso` 显式控制：用于验证 offload 打通效果（analysis §2.4）
- ✅ `-accel kvm -cpu host`：必须 KVM 加速，否则数据无意义

**多核扩展配置** (`qemu-aarch64-vhost-smp4.toml`)：
- 基于 vhost.toml + `-smp 4`
- 用于验证多核扩展曲线（methodology §1）
- 绑核建议：`taskset -c 4-7 cargo xtask ...`

### 2. 环境自动化配置脚本

**`setup-vhost-tap.sh` 功能**：
```bash
# 检查前置条件
sudo bash setup-vhost-tap.sh check
  ├─ /dev/kvm 存在且可访问
  ├─ /dev/vhost-net 存在（自动 modprobe vhost_net）
  └─ 必需命令：ip, brctl, iperf3

# 一键配置
sudo bash setup-vhost-tap.sh setup
  ├─ 创建 br0 (192.168.100.1/24)
  ├─ 创建 tap0 并挂到 br0
  └─ 设置权限（user=${SUDO_USER}）

# 清理环境
sudo bash setup-vhost-tap.sh teardown
```

**拓扑 A（默认）**：
```
[Starry guest] --tap0--> br0 (192.168.100.1/24) <-- [WSL2 host iperf3]
                            └─ vhost-net 内核线程
```

**错误处理**：
- /dev/kvm 缺失：提示 WSL2 嵌套虚拟化配置
- /dev/vhost-net 缺失：尝试 modprobe，失败时提示检查 CONFIG_VHOST_NET
- 所有错误带颜色输出 + 操作指导

### 3. WSL2 降噪配置

**`wslconfig-example.txt` 压制噪声源**（plan §1 + §3.2）：

| 噪声源 | 配置项 | 效果 |
|--------|--------|------|
| A 宿主调度抖动 | `processors=8` | 固定 vCPU 数 |
| B 内存回收停顿 | `memory=16GB`<br>`pageReporting=false` | 固定内存、关闭回收上报 |
| D 无 KVM | `nestedVirtualization=true` | 启用 /dev/kvm |
| E vCPU 迁移 | `kernelCommandLine=isolcpus=4-7` | 可选：隔离核 |

**完整检查清单**（包含 Windows 侧、WSL2 侧、QEMU 侧、测量纪律）：
- Windows 宿主层：电源计划、关闭后台、禁用杀毒实时扫描
- WSL2 配置：本文件全部项
- QEMU/guest 绑核：taskset、固定 smp
- 测量纪律：≥5 次迭代、warmup、固定时长/MTU、环境指纹
- 可比性红线：同 .wslconfig、背靠背测试、不重启 Windows

### 4. run.sh 集成

**新增场景**：
```bash
bash apps/starry/net-bench/run.sh aarch64 vhost          # smp=1
bash apps/starry/net-bench/run.sh aarch64 vhost-smp4     # smp=4
bash apps/starry/net-bench/run.sh aarch64 all            # 含 vhost 场景
```

**前置检查** (`check_vhost`)：
1. /dev/kvm 存在且可访问
2. /dev/vhost-net 存在
3. br0 或 tap0 配置正确（192.168.100.1/24）
4. 失败时给出操作指导（指向 setup-vhost-tap.sh）

**环境指纹增强**：
- 自动记录 `kvm=present/absent`
- 自动记录 `vhost_net=present/absent`
- 对齐 plan §6.3 可复现性要求

---

## 使用流程

### 首次配置（一次性）

```bash
# 1. 检查 WSL2 嵌套虚拟化（修改后需重启 WSL）
# 在 Windows: 编辑 %USERPROFILE%\.wslconfig
[wsl2]
nestedVirtualization=true
processors=8
memory=16GB
pageReporting=false

# 重启 WSL
wsl --shutdown

# 2. 验证环境
sudo bash apps/starry/net-bench/setup-vhost-tap.sh check

# 3. 配置 br0 + tap0
sudo bash apps/starry/net-bench/setup-vhost-tap.sh setup
```

### 日常测试

```bash
# 单次 vhost 测试（smp=1）
bash apps/starry/net-bench/run.sh aarch64 vhost

# 多核扩展测试（smp=4）
bash apps/starry/net-bench/run.sh aarch64 vhost-smp4

# 多次重启累积方差（推荐）
bash apps/starry/net-bench/run.sh aarch64 vhost --repeat 5

# 全量场景（slirp/slirp-smp4/tap/vhost/vhost-smp4）
bash apps/starry/net-bench/run.sh aarch64 all
```

### 清理环境

```bash
sudo bash apps/starry/net-bench/setup-vhost-tap.sh teardown
```

---

## 对齐规划文档

### 已解决的阻塞性差距

| 差距 | 规划要求 | 当前实现 | 状态 |
|------|----------|----------|------|
| **主力拓扑缺失** | QEMU+TAP+vhost | ✅ qemu-aarch64-vhost.toml | 已实现 |
| vhost 参数 | vhost=on, mq, offload | ✅ 完整参数 | 已实现 |
| 多核扩展配置 | vhost smp=4 | ✅ vhost-smp4.toml | 已实现 |
| 环境自动化 | br0+tap0 配置 | ✅ setup-vhost-tap.sh | 已实现 |
| 前置条件检查 | /dev/kvm, /dev/vhost-net | ✅ check 命令 | 已实现 |
| WSL2 降噪 | .wslconfig 指导 | ✅ wslconfig-example.txt | 已实现 |
| 绑核纪律 | taskset 指导 | ✅ 文档 + 脚本注释 | 已实现 |

### 对齐的规划纪律（qemu-plan §0）

| 拓扑 | 规划角色 | 实现状态 |
|------|----------|----------|
| QEMU+TAP+vhost | **主力** | ✅ vhost/vhost-smp4 场景 |
| QEMU+TAP | 降级 | ✅ tap 场景（保留） |
| SLIRP | **禁用于压测** | ✅ README 明确标注"仅冒烟" |

### 支持的测试维度（methodology §1）

| 维度 | vhost 支持 | 说明 |
|------|-----------|------|
| 吞吐 | ✅ | iperf3 tcp1/tcp4/tcp1r/udp1g |
| PPS/小包 | ✅ | iperf3 udp64 |
| 延迟 | ⚠️ | 需补充 netperf RR（后续） |
| 连接速率 | ⚠️ | 需补充 netperf TCP_CRR（后续） |
| CPU 效率 | ⚠️ | 需补充 cycles 采样（后续） |
| **多核扩展** | ✅ | vhost-smp4 + 对比 vhost smp=1 |

---

## 技术细节

### KVM 加速验证

```bash
# 检查 /dev/kvm
ls -l /dev/kvm
# 预期输出：crw-rw-rw- 1 root kvm

# 验证 QEMU 支持 KVM
qemu-system-aarch64 -accel help | grep kvm
# 预期输出包含：kvm

# WSL2 内核版本要求
uname -r
# 需 >= 5.10.16（WSL2 Build 21387+）
```

### vhost-net 模块检查

```bash
# 检查 /dev/vhost-net
ls -l /dev/vhost-net
# 预期输出：crw------- 1 root root

# 手动加载模块
sudo modprobe vhost_net

# 验证内核配置
zgrep VHOST_NET /proc/config.gz
# 预期输出：CONFIG_VHOST_NET=y 或 =m
```

### 拓扑调试

```bash
# 查看 bridge 状态
brctl show br0
# 预期输出：br0 下挂 tap0

# 查看 IP 配置
ip addr show br0
# 预期输出包含：192.168.100.1/24

# 测试 host 侧连通性（在 guest 启动后）
ping -c 3 192.168.100.2
```

### vhost-net 线程验证

```bash
# QEMU 启动后，查看 vhost 内核线程
ps aux | grep vhost
# 预期输出：vhost-<pid> 线程

# 可选：绑核优化
# 找到 vhost 线程 PID，绑到独立核
sudo taskset -cp 3 <vhost-pid>
```

---

## 性能预期

### 与 TAP（无 vhost）对比

| 场景 | TAP | vhost | 提升 |
|------|-----|-------|------|
| TCP 单流吞吐 | ~95 Mbps | **预期 >200 Mbps** | >2x |
| UDP 小包 PPS | 低 | **预期显著提升** | 数倍 |
| CPU 占用 | 高（QEMU 用户态转发） | **低（内核卸载）** | 显著降低 |

### 与 SLIRP 对比

| 场景 | SLIRP | vhost | 提升 |
|------|-------|-------|------|
| TCP 单流吞吐 | ~90 Mbps（封顶） | **预期 >200 Mbps** | >2x |
| 方差 | 大（NAT 抖动） | **小（直接转发）** | 显著改善 |

**实测基线**（需实际跑 vhost 后补充）：
```bash
# 建立 vhost baseline
bash apps/starry/net-bench/run.sh aarch64 vhost --repeat 5
bash apps/starry/net-bench/run.sh aarch64 vhost-smp4 --repeat 5

# 查看汇总报告
cat apps/starry/net-bench/results/summary-aarch64-vhost-*.txt
```

---

## 已知限制与未来工作

### 当前限制

1. **Starry 单队列**：`queues=4` 配置已就位，但 Starry 驱动层当前只用 `QUEUE_ID0`（drivers/net e1000/mod.rs:19），需等 analysis §3.1 多队列改造。
2. **offload 未打通**：配置已开启 `csum/gso/tso`，但需 Starry 侧实现（analysis §2.4）才能生效。
3. **仅 aarch64**：x86_64/riscv64 需补充对应的 vhost.toml（`-cpu host` 替换）。
4. **WSL2 特有方差**：即便降噪，P999 延迟仍可能比物理机高（plan §8）。

### 后续工作（按优先级）

#### 短期（补齐测试工具）
1. 添加 netperf TCP_RR/UDP_RR 延迟测试
2. 添加 netperf TCP_CRR 短连接测试
3. 集成 Linux 基线对比（同拓扑、同 vhost 配置）

#### 中期（细粒度观测）
4. 集成 perf stat 采集 cycles/instructions
5. 集成 qperf 火焰图生成
6. Starry 侧 cycles 采样埋点（poll_interfaces/recv/send）

#### 长期（完整度量）
7. 拷贝次数计数（to_vec/copy_from_slice）
8. 锁竞争观测（lockdep SOCKET_SET/SERVICE）
9. 丢包归因（按原因分类计数）
10. 三方对比报告生成器（Linux/Starry 优化前/Starry 优化后）

---

## 文件依赖关系

```
apps/starry/net-bench/
├── run.sh                          ← 入口，调用 check_vhost + xtask
│   └─ qemu-aarch64-vhost.toml      ← QEMU 配置（vhost=on, mq=on, offload）
│   └─ qemu-aarch64-vhost-smp4.toml ← 多核扩展配置
│   └─ check_vhost()                ← 前置检查：/dev/kvm, /dev/vhost-net, br0
│       └─ setup-vhost-tap.sh       ← 一键配置 br0 + tap0（手动前置）
│           └─ wslconfig-example.txt ← WSL2 降噪配置（手动前置）
├── net-bench-tap.sh                ← guest 侧脚本（复用，HOST_IP=192.168.100.1）
├── summarize.py                    ← 解析结果 → mean/stddev
└── results/
    ├── fingerprint-*.txt           ← 自动记录 kvm/vhost-net 状态
    └── summary-*.txt               ← 汇总报告
```

---

## 验收标准

### 环境检查通过
```bash
sudo bash apps/starry/net-bench/setup-vhost-tap.sh check
# 预期输出：
# [INFO] /dev/kvm present and accessible
# [INFO] /dev/vhost-net present
# [INFO] Required commands present: ip, brctl, iperf3
# [INFO] Environment check passed ✓
```

### vhost 场景运行成功
```bash
bash apps/starry/net-bench/run.sh aarch64 vhost
# 预期输出：
# - NET_BENCH_PASSED
# - summary-aarch64-vhost-*.txt 生成
# - TCP 吞吐 > 200 Mbps（需实测确认）
```

### 多核扩展数据
```bash
bash apps/starry/net-bench/run.sh aarch64 vhost
bash apps/starry/net-bench/run.sh aarch64 vhost-smp4
# 对比两份 summary，验证 smp4 吞吐 > smp1
```

### 环境指纹完整
```bash
cat apps/starry/net-bench/results/fingerprint-aarch64-vhost-*.txt
# 必须包含：
# - kvm: present
# - vhost_net: present
# - qemu_accel: ... kvm ...
```

---

## 总结

本方案完整实现了 `www/starry-net-qemu-benchmark-plan.md` 的主力测试拓扑要求，提供：

✅ **生产级配置**：vhost-net + KVM + 多队列 + offload 控制  
✅ **自动化工具**：一键配置 + 前置检查 + 错误指导  
✅ **文档完整**：WSL2 降噪配置 + 使用流程 + 故障排查  
✅ **向后兼容**：保留 SLIRP/TAP 场景，明确拓扑分级  
✅ **对齐规划**：禁用 SLIRP 压测、主力数据来自 vhost  

**核心价值**：解除 35% 完成度分析中的**最大阻塞**（主力拓扑缺失），使 `apps/starry/net-bench` 从"功能冒烟工具"升级为**可用于正式性能对比与优化验证的测试基础设施**。

**下一步**：运行 `bash apps/starry/net-bench/run.sh aarch64 vhost --repeat 5` 建立 vhost baseline，然后按"后续工作"清单补齐延迟测试、Linux 基线对比、细粒度观测等关键维度。
