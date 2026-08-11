# Task-2 网络功能迁移清单

## 迁移上下文

| 项目 | 值 |
|---|---|
| 源工作区 | `/home/huhu/tgoskits-net-dev` |
| 源分支 | `openrace/task2-net-dev` |
| 源基线 | `e04a3ca28076b87c2f872b0aab252376db5e270b` |
| 目标工作区 | `/home/huhu/tgoskits-net-task2-clean` |
| 目标分支 | `openrace/task2-net-clean` |
| 目标基线 | `upstream/dev` / `fd63e75259b9393d5e6f26917df9ccd94913441d` |
| 迁移原则 | 以已验证功能为来源，只迁移任务二所需的最小内容 |

当前文档只记录迁移范围和验收方法。代码迁移完成前，不应把源工作区的整份
`git diff` 或历史实验副本直接复制到目标分支。

## A. 必须迁移

这些内容直接构成 Linux + RTOS 的 IP 网络链路、T2N1 应用协议和验收证据。

### 协议与 Guest 应用

- [x] `Cargo.toml`：注册 `components/task2-net-protocol` workspace member 和依赖。
- [x] `Cargo.lock`：仅保留由新 workspace package 产生的锁文件变化。
- [x] `components/task2-net-protocol/`：T2N1 帧、typed payload、ACK/重传、去重、
      乱序、错误通知、heartbeat 和 Safe/Active 状态机。
- [x] `apps/arceos/task2-net/Cargo.toml`：保留 Linux userspace 构建所需 package 边界。
- [x] `apps/arceos/task2-net/src/main.rs`：迁移 `no-default-features` 的 Linux 端点路径。
      ArceOS 专用路径只在后续确认 P1 仍需时迁移。
- [x] `scripts/test/net-dual-guest/zephyr-task2/CMakeLists.txt`。
- [x] `scripts/test/net-dual-guest/zephyr-task2/app.overlay`。
- [x] `scripts/test/net-dual-guest/zephyr-task2/prj.conf`，特别是
      `CONFIG_ARMV8_A_NS=y`。
- [x] `scripts/test/net-dual-guest/zephyr-task2/src/main.c`。

### 最终 P2/P3 拓扑

- [x] `scripts/test/net-dual-guest/qemu-aarch64-p2.toml`。
- [x] `scripts/test/net-dual-guest/qemu-aarch64-p2-stability-1h.toml`。
- [x] `scripts/test/net-dual-guest/qemu-aarch64-p3-proxy-final.toml`。
- [x] `scripts/test/net-dual-guest/vm-aarch64-p2-linux.toml`。
- [x] `scripts/test/net-dual-guest/vm-aarch64-p2-rtos.toml`。
- [x] `scripts/test/net-dual-guest/manifest.toml`。
- [x] `scripts/test/net-dual-guest/prepare-p2-host-dtb.sh`。
- [x] `scripts/test/net-dual-guest/carveout_host_dtb.py`。

P1 预检和 initramfs 工具仍是 canonical 验收链的一部分（README、P1/P3 记录会直接引用）：

- [x] `scripts/test/net-dual-guest/qemu-aarch64-p1.toml` 和 `vm-aarch64-p1.toml`。
- [x] `scripts/test/net-dual-guest/build-tools.sh`、`udp_probe.c`、`task2-init.c`。
- [x] `scripts/test/net-dual-guest/p1-run-2026-08-10.md`、`p3-run-2026-08-10.md`。

### 构建、故障注入和验证

- [x] `scripts/test/net-dual-guest/build-linux-task2.sh`。
- [x] `scripts/test/net-dual-guest/build-linux-initramfs.sh`。
- [x] `scripts/test/net-dual-guest/build-zephyr-task2.sh`。
- [x] `scripts/test/net-dual-guest/linux-init.sh`。
- [x] `scripts/test/net-dual-guest/qmp_link.py`。
- [x] `scripts/test/net-dual-guest/ack_drop_proxy.py`。
- [x] `scripts/test/net-dual-guest/task2_responder.py`。
- [x] `scripts/test/net-dual-guest/validate_manifest.py`。
- [x] `scripts/test/net-dual-guest/verify_fdt_devices.py`。
- [x] `scripts/test/net-dual-guest/verify_isolation.py`。
- [x] `scripts/test/net-dual-guest/verify_pcap.py`。
- [x] `scripts/test/net-dual-guest/verify_fault_pcap.py`。
- [x] `scripts/test/net-dual-guest/verify_protocol_injection.py`。
- [x] `scripts/test/net-dual-guest/test_*.py`。

### 设计与测试文档

- [x] `scripts/test/net-dual-guest/README.md`。
- [x] `book/design/task2-dual-guest-network.md`。
- [x] `scripts/test/net-dual-guest/task2-run-2026-08-11.md`。
- [x] 根据目标仓库的实际配置修正 README 中过时的 P1 build config 引用。

## B. 迁移前必须重新判断

目标分支已经包含官方双 Guest virtio-net 支持（`547f3266a` 引入的
`virtualization/axvirtio-common` 和 `virtualization/axvirtio-net`）。以下内容不能
直接覆盖，必须先比较现有实现和新架构边界：

- [x] `drivers/ax-driver/src/virtio/net.rs` 的 FDT virtio-MMIO probe：目标基线已包含官方双 Guest
      `axvirtio-common`/`axvirtio-net` 支持，无需重复迁移旧 probe 实验。
- [x] `net/ax-net/src/lib.rs` 的公开 `flush_egress`：任务端点关闭前发送最终报文需要该最小 API。
- [x] `net/ax-net/src/udp.rs` 的 egress 回归测试：不迁移；upstream/dev 已包含等价 UDP close 前
      egress 修复，旧测试模型也不能代表互联链路。
- [x] `os/arceos/ulib/axstd/src/net/udp.rs` 的 `set_nonblocking`：协议重传和 heartbeat 定时器需要。
- [x] `os/axvisor/build.rs` 的镜像 `rerun-if-changed` 修复：不迁移；当前任务二流程未复现镜像变更后
      stale build，避免带入无独立证据的通用修复。
- [x] `virtualization/axvm/src/config.rs` 的 kernel/DTB/ramdisk relocation 修复：不迁移；任务二 VM
      使用 `KeepConfigured`，clean AxVisor 运行已确认 kernel、DTB 和 initramfs 均按配置加载。

判断规则：如果目标分支已有等价能力，只保留目标实现；只有原始回归测试在目标
代码上仍然失败时，才迁移对应修复，并为该修复单独记录原因和测试证据。

## C. 默认不迁移

以下内容属于探索副本、临时证据或与最终 Linux + Zephyr 验收无关的修改：

- [ ] `.gitignore` 中 `/docs/my/task2-net-dev-reference.md`。
- [ ] `net/ax-net/src/router.rs` 中把无路由日志从 `debug!` 改为 `warn!`。
- [ ] `apps/arceos/task2-net/build-aarch64-dhcp.toml`。
- [ ] `apps/arceos/task2-net/build-aarch64-unknown-none-softfloat.toml`。
- [ ] `apps/arceos/task2-net/build-riscv64gc-unknown-none-elf.toml`。
- [ ] `apps/arceos/task2-net/qemu-aarch64-p1-direct.toml`。
- [ ] `scripts/test/net-dual-guest/qemu-aarch64-p2-link-2325.toml`。
- [ ] `scripts/test/net-dual-guest/qemu-aarch64-p2-stability-2330.toml`。
- [ ] `scripts/test/net-dual-guest/qemu-aarch64-p3-proxy.toml`。
- [ ] `scripts/test/net-dual-guest/qemu-aarch64-p3-proxy-final-2315.toml`。
- [ ] `scripts/test/net-dual-guest/zephyr-task2/app-single.overlay`。

如果需要保留历史运行输入，应将其放到明确的 evidence/archive 区域，并在文档中
注明“不可作为 canonical 验收配置”，而不是与最终配置并列。

## D. 证据 instrumentation 的边界

最终隔离验证当前只需要可解析的 assigned SPI route、stage-2 MMIO、GPA/HPA 和
manifest/pcap 证据。以下日志修改不承载网络数据：

- `virtualization/arm_vgic/src/controller/physical.rs`；
- `virtualization/arm_vgic/src/controller/state.rs`；
- `virtualization/axvm/src/irq/deferred.rs`；
- `virtualization/axvm/src/arch/aarch64/gic/physical.rs` 中除 route registration 之外的 IRQ 计数；
- `drivers/ax-driver/src/virtio/mod.rs` 和 `virtio/net.rs` 中的 DMA/IRQ 计数。

clean 分支仅保留 `axvm::arch::aarch64::gic::physical` 在成功安装 route 后输出的一条
`registered assigned AArch64 SPI route host_intid=... guest_intid=...` 记录；它是
`verify_isolation.py` 所需的最小真实证据。其余日志不迁移，避免普通运行默认输出大量诊断信息。

## E. 分阶段验收矩阵

- [x] `cargo test -p task2-net-protocol`（19 passed）。
- [x] `cargo clippy -p task2-net-protocol --all-targets -- -D warnings`。
- [x] Python 验证器单元测试（10 passed）。
- [x] `cargo fmt --all -- --check` 和 `git diff --check`。
- [x] Linux userspace 和 Zephyr 镜像可复算构建。
- [x] `validate_manifest.py` 和 `verify_fdt_devices.py` 通过。
- [x] P2 clean 短跑：双侧 UDP/T2N1 序号账本一致，pcap 各 400 个 Task-2 UDP 帧。
- [x] clean AxVisor `verify_isolation.py`：identity GPA/HPA、stage-2 MMIO、SPI route、DMA
      evidence 全部通过；route log hash=`7a11e1fa955f99c89ca66923c2fe0107150263bce3035024053f274cc995c7f7`。
- [x] P3：ACK 丢失、重传去重、乱序、非法参数、QMP link down/up（记录于
      `p3-run-2026-08-10.md` 和 `task2-run-2026-08-11.md`）。
- [x] P2 clean 长稳：已运行 3689.839 秒（约 61.5 分钟），统计成功率、应用错误、超时、恢复、
      延迟和有效吞吐量；结果详见 `task2-run-2026-08-11.md` 的 clean-worktree 小节。
- [x] 提交前复核目标分支 diff：仅包含任务二资产、两个必要网络 API、DTB 可复算修复和最小
      AArch64 SPI route 证据日志。

## F. 提交拆分建议

1. 任务二核心协议、Linux/Zephyr 端点和 canonical 测试资产。
2. 必要的通用 AxVM/AxVisor 修复，每个修复单独提交并带回归证据。
3. 可选的 P1/ArceOS 适配或 evidence instrumentation。
4. 最终文档和验收记录（若不与核心提交同提交）。
