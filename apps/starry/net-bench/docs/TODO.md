# net-bench 待办事项

## 已完成（PR 反馈修复）

- Linux 基线 initramfs 改为按需构建：从受管 Alpine rootfs（含 busybox/iperf3/
  ip/nc）提取并打包，不再提交生成物；`run-linux-baseline.sh` 在缺失或校验失败
  （gzip/cpio 完整性 + init 入口）时重建，并支持 `--rebuild-rootfs` 强制重建
- 删除并 gitignore 失效的占位 `linux-baseline/initramfs.cpio.gz`（20 字节坏档）
  与 `rootfs-build/` 中间产物
- Linux 基线内核解析显式化：优先 `linux-baseline/vmlinuz`，否则仅在 host 架构
  与目标一致时回退 host 内核，跨架构明确报错（避免静默用错内核）
- eBPF net_stats：四个架构 QEMU 配置统一 `--test`，保证各架构都产生自测流量
- eBPF net_stats：recv 改用与 send 相同的 `AxResult<usize>` sret 指针解析，
  移除依赖 rdx 寄存器约定的脆弱实现

## 已完成（本次重构）

- 统一为单一严肃入口 `run.sh`（显式 arch/scenario/accel/repeat），智能入口
  `bin/bench` 降级为实验性便捷壳并委托 `run.sh`
- 抽取主机侧公共流程到 `core/lib.sh`（常量、配置矩阵、iperf3 生命周期、前置检查、
  指纹、汇总），消除散落硬编码
- 修复 `prebuild.sh` 引用 `core/` 下 guest 脚本的路径（原断裂）
- 修复结果汇总路径（统一指向 `core/summarize.py`）
- 移除 QEMU 配置中被 ostool 忽略的死字段 `prebuild = "core/prebuild.sh"`
- 补齐配置矩阵：新增 `vhost-smp4`/`tap-smp4` 各 arch/accel 变体；修正
  `vhost-aarch64-tcg` 缺 `vhost=on` 的问题
- 新增 `build-x86_64-unknown-none.toml`，使 x86_64 配置可正确启用 virtio-net
- 统一 netperf 标记格式与 `net-bench-common.sh` 对齐
- 删除重复/失效文件：`core/prebuild.sh`、根目录 `net-bench-netperf.sh`、
  根目录 `compare-baseline.py`、`setup-vhost-tap.sh`、`build-configs/`、旧根目录
  `qemu-aarch64-*.toml`

## 高优先级

### 多队列支持
- [ ] 在 `env/setup-common.sh` 中按需创建多队列 TAP（`ip tuntap add ... multi_queue`）
- [ ] 提供多队列专用 QEMU 配置（`mq=on,queues=N`），与单队列配置共存
- [ ] 待 `drivers/net` 多队列改造后再启用 mq 参数（当前单队列，见 MULTIQUEUE_ISSUE.md）

### x86_64 + KVM 完整验证
- [ ] 在裸 Linux x86_64 + KVM 环境端到端跑通 vhost 场景
- [ ] 验证 x86_64 rootfs 中 guest 脚本注入与运行

## 中优先级

### eBPF net_stats 集成修正
- [ ] 将 net_stats 采样从 host 侧改为 guest 侧执行，输出纳入 QEMU guest log
- [ ] 由 host 侧 summarize.py 解析 NET_STATS 块（已支持解析，待接入 guest 采样）

### CI 接入（可选，保持非默认）
- [ ] 提供显式触发的 CI 示例（至少 SLIRP + TCG 冒烟），不纳入默认全量 app 测试

## 低优先级

- [ ] 支持 riscv64 / loongarch64 配置矩阵
- [ ] 拓扑 B（guest ↔ guest 双 VM）支持
- [ ] 结果自动可视化（多核扩展曲线、三方对比图）

## 备注

- net-bench 列于 `apps/.ignore`，默认不参与 app 发现/CI，仅通过
  `--test-case net-bench` 显式触发——符合"严肃测试显式入口、非默认"的设计。
- 多队列为性能优化项；当前单队列配置满足基线测试需求。
