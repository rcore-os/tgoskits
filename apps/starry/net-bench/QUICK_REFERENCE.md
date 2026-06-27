# net-bench 测试快速参考卡

## 一键命令

### 环境配置（首次）
```bash
cd /home/asta/tgoskits/wt-feat-net-enhance/apps/starry/net-bench
sudo bash setup-vhost-tap.sh check   # 检查环境
sudo bash setup-vhost-tap.sh setup   # 配置 br0 + tap0
```

### Starry 性能测试
```bash
# 单次测试
bash run.sh aarch64 vhost

# 推荐：多次重启累积方差
bash run.sh aarch64 vhost --repeat 5

# 多核扩展
bash run.sh aarch64 vhost-smp4 --repeat 5
```

### Linux 基线测试
```bash
bash run-linux-baseline.sh aarch64 vhost --repeat 5
bash run-linux-baseline.sh aarch64 vhost-smp4 --repeat 5
```

### 性能对比
```bash
python3 compare-baseline.py \
    results/summary-aarch64-vhost-20260627-*.txt \
    results/summary-linux-baseline-aarch64-vhost-20260627-*.txt
```

### CPU 效率测试
```bash
bash run-with-perf.sh aarch64 vhost
cat results/perf-stat-aarch64-vhost-*.txt
```

---

## 测试前降噪（可选）
```bash
# CPU 频率固定
echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor

# 关闭后台服务
sudo systemctl stop snapd docker

# 释放缓存
sync && echo 3 | sudo tee /proc/sys/vm/drop_caches

# 检查负载
uptime
```

---

## 故障排查

### KVM 不可用
```bash
ls -l /dev/kvm
egrep -c '(vmx|svm)' /proc/cpuinfo
sudo modprobe kvm_intel  # 或 kvm_amd
```

### vhost-net 不可用
```bash
ls -l /dev/vhost-net
sudo modprobe vhost_net
```

### 网络连通性
```bash
ip addr show br0
brctl show br0
ss -tlnp | grep 5201  # iperf3 server
```

### 编译问题
```bash
# 检查 qemu-user-binfmt
ls -l /proc/sys/fs/binfmt_misc/qemu-aarch64

# 重新安装
sudo apt-get install -y qemu-user-binfmt binfmt-support
```

---

## 文件位置

### 脚本
- `apps/starry/net-bench/run.sh` — Starry 测试入口
- `apps/starry/net-bench/run-linux-baseline.sh` — Linux 基线
- `apps/starry/net-bench/compare-baseline.py` — 对比报告
- `apps/starry/net-bench/run-with-perf.sh` — perf stat
- `apps/starry/net-bench/setup-vhost-tap.sh` — 环境配置

### 配置
- `apps/starry/net-bench/qemu-aarch64-vhost.toml` — vhost smp=1
- `apps/starry/net-bench/qemu-aarch64-vhost-smp4.toml` — vhost smp=4

### 结果
- `apps/starry/net-bench/results/` — 所有测试结果
- `results/summary-*.txt` — 汇总报告
- `results/fingerprint-*.txt` — 环境指纹
- `results/perf-stat-*.txt` — perf 数据

### 文档
- `README.md` — 主文档
- `docs/QUICK_START.md` — 快速开始
- `docs/STRUCTURE.md` — 架构设计
- `docs/TODO.md` — 待办事项
- `docs/MULTIQUEUE_ISSUE.md` — 多队列问题说明

---

## 测试覆盖

### iperf3（当前）
- ✅ TCP 单流上行（tcp1）
- ✅ TCP 4流上行（tcp4）
- ✅ TCP 单流下行（tcp1r）
- ✅ UDP 大包吞吐（udp1g）
- ✅ UDP 64B 小包 PPS（udp64）

### netperf（已集成）
- ✅ TCP_RR（TCP 请求-响应延迟）
- ✅ UDP_RR（UDP 请求-响应延迟）
- ✅ TCP_CRR（TCP 短连接速率）

### perf stat（已集成）
- ✅ cycles / instructions / IPC
- ✅ cache-references / cache-misses
- ✅ LLC-load-misses

---

## 预期性能（参考）

### Starry vs Linux（vhost）
| 测试 | Starry 预期 | Linux 基线 | 比例 |
|------|------------|-----------|------|
| TCP 单流 | 200-300 Mbit/s | 800-1000 Mbit/s | 25-35% |
| TCP 4流 | 250-350 Mbit/s | 1000-1500 Mbit/s | 20-30% |
| UDP 64B PPS | 10K-20K pkt/s | 50K-100K pkt/s | 20-40% |

*注：实际数值取决于硬件配置和系统负载*

---

## 关键指标

### 测量纪律
- ✅ ≥5 次迭代
- ✅ warmup 过滤
- ✅ 标准差 > 10% 标注为 NOISY
- ✅ 环境指纹记录

### 核心 KPI
- **吞吐**: Mbit/s（TCP/UDP）
- **PPS**: pkt/s（UDP 64B）
- **延迟**: P50/P99 μs（netperf RR）
- **连接速率**: conn/s（netperf CRR）
- **CPU 效率**: cycles/byte, IPC
- **多核扩展**: 吞吐(smp4) / 吞吐(smp1)

---

## 常用 Git 操作

### 查看当前分支
```bash
cd /home/asta/tgoskits/wt-feat-net-enhance
git branch
git status
```

### 提交更改
```bash
git add apps/starry/net-bench/
git commit -m "refactor(net-bench): restructure with unified test entry

- Reorganize directory structure (bin/, env/, core/, qemu/, docs/)
- Add environment auto-detection and setup scripts
- Create unified test entry points (bin/bench, bin/bench-wsl)
- Update documentation with objective technical style

Progress: Enhanced automation and maintainability"
```

---

## 下一步任务

### 立即执行
1. ⏳ 等待 Starry 编译完成
2. 📊 运行 Starry vhost 基线测试（--repeat 5）
3. 📊 运行 Linux vhost 基线测试（--repeat 5）
4. 📈 生成对比报告

### 后续优化
5. 分析性能差距（预期 25-35%）
6. 定位瓶颈（多拷贝/锁竞争/offload）
7. 针对性优化并验证

---

## 联系信息

- **项目**: tgoskits wt-feat-net-enhance
- **主机**: server01 (Linux 7.0.0-22-generic)
- **用户**: asta
- **sudo 密码**: devscke
- **工作目录**: /home/asta/tgoskits/wt-feat-net-enhance

---

