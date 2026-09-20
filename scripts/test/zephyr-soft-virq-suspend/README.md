# 挂起 vCPU 的定向中断投递

该工作流验证 PR #1934 剩余的运行时问题：向 vCPU1 投递中断时，不能通过 VM 广播让闲置的 vCPU0 退出 PSCI 挂起。`run.py` 构建固定版本 Zephyr，通过 `cargo xtask axvisor qemu` 启动双 vCPU guest，并同时核验 300 次投递、guest 应答和非目标 vCPU 隔离。

## 1. 状态与分层

本实现参照本地 Linux 7.1，提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。`virt/kvm/kvm_main.c` 的 `kvm_vcpu_check_block()` 检查可运行状态、timer 和 `KVM_REQ_UNBLOCK`；`kvm_vcpu_block()` 登记目标等待者后重新检查条件；`__kvm_vcpu_kick()` 唤醒目标，仅在需要时发送远端 guest-exit IPI。

### 1.1 唤醒请求

AxVM 的 `VcpuRunState::request_unblock()` 发布每 vCPU 请求位，`VcpuEventWaitSnapshot::take_pending_event()` 消费该请求。`VmRuntimeHandle` 的单目标 kick、设备请求和各架构 interrupt dispatch 都调用绑定到线程的 `VcpuKickHandle::kick_from_task()`；该入口统一负责发布请求、唤醒目标和必要的远端 IPI，调用方不再重复操作线程或处理器。x86 的 hard-IRQ 入口发布相同请求，但保留 IRQ 上下文的延后投递规则。`ax-task` 的 sticky park handshake 保护条件检查到真正阻塞之间的窗口；请求在等待快照建立之前到达也不会丢失。VM 生命周期广播继续使用 `notify_all()`，不会被单目标通知推进。

这个请求位只表示需要离开等待，不表示中断数量或 pending 状态。AArch64、RISC-V、x86 和 LoongArch 的等待策略复用同一机制，同时保留各自的中断与 timer 可运行判定。

共同路径由 `ArchOps` 和 `VmRuntimeHandle` 组合，架构差异只保留在进入等待前必须读取的状态。

| 架构 | 额外可运行条件 | 共享通知入口 |
| --- | --- | --- |
| AArch64 | VGIC pending、已登记 timer wait 的完成状态 | `VcpuKickHandle` |
| x86_64 | 架构后端的 `has_pending_event()` | `VcpuKickHandle`，hard IRQ 保留延后处理 |
| RISC-V | 既有 `ArchOps` 默认等待策略与 runtime interrupt queue | `VcpuKickHandle` |
| LoongArch64 | `has_enabled_pending_interrupt()` 与 runtime interrupt queue | `VcpuKickHandle` |

公开 VM API 不接收架构名称，队列、物理来源中断和 LoongArch 外部中断在发布后都使用同一个目标 kick 操作。

### 1.2 中断状态

Linux 的 `kvm_vgic_inject_irq()` 更新 `pending_latch` 或 `line_level`，`vgic_flush_lr_state()` 按可用 LR 装入中断。当前 `arm_vgic` 的 controller/redistributor 已拥有 pending、active、LR refill 和 maintenance 状态；本 PR 不重新引入 runtime retry slot、固定容量重复边沿 FIFO 或 GICv3 寄存器预检查。当前 AArch64 `policy/exception.rs` 已保留 HVC 返回 PC，也无需恢复被删除的 `arm_vcpu`。

GIC pending 位允许合并重复边沿。因此测试只保留一个未应答的中断，确认 guest 已接收后再投递下一次，不把 300 次快速 enqueue 等同于架构保证的 300 次 ISR。LR 饱和及优先级规则由现有 `arm_vgic` 测试负责；本工作流专门验证真实 SMP、PSCI 和阻塞唤醒。

## 2. 构建环境

`run.py` 从 ELF 读取入口与 `virq_mailbox` 地址，生成输出目录内的 VM/build 配置。guest 二进制通过 `image_location = "memory"` 在构建时嵌入 Axvisor，不需要把 guest 写入 rootfs，也不使用 `debugfs` 或 Linux 宿主机内的文件复制。当前 `xtask` 可能自动准备其默认受管 rootfs，但本场景不挂载磁盘、不从该镜像加载 guest。

### 2.1 工具准备

以下命令使用独立 Python 环境和固定的 GNU Arm 10.3-2021.07 交叉工具链。还需项目固定的 Rust 工具链、`git`、`curl`、`tar`、`xz`、`dtc` 和支持 AArch64 virtualization 的 QEMU；Ubuntu 上系统依赖可由 `sudo apt-get install git curl xz-utils device-tree-compiler qemu-system-arm python3-venv` 安装。

```bash
python3 -m venv /tmp/virq-venv
/tmp/virq-venv/bin/pip install cmake==4.4.3 ninja==1.13.2 pyelftools==0.33 PyYAML==6.0.3 packaging==26.3
export PATH="/tmp/virq-venv/bin:$PATH"
mkdir -p /tmp/virq-tools
curl -fL 'https://developer.arm.com/-/media/Files/downloads/gnu-a/10.3-2021.07/binrel/gcc-arm-10.3-2021.07-x86_64-aarch64-none-elf.tar.xz' -o /tmp/virq-tools/toolchain.tar.xz
tar -xJf /tmp/virq-tools/toolchain.tar.xz -C /tmp/virq-tools
export CROSS_COMPILE=/tmp/virq-tools/gcc-arm-10.3-2021.07-x86_64-aarch64-none-elf/bin/aarch64-none-elf-
"${CROSS_COMPILE}gcc" --version
```

`--cross-compile` 必须使用绝对前缀。通用 Linux GCC 不一定提供 Zephyr 所需的 include-fixed 布局，不能直接替换为 `aarch64-linux-gnu-`。

### 2.2 固定 guest

该 guest 使用 Zephyr 自带的 minimal libc，不依赖 west modules 或额外 SDK。先固定源码再配置；`run.py` 校验实际提交，并通过 `WEST=NOTFOUND` 避免读取本机其他 west workspace 的模块。

```bash
git clone https://github.com/zephyrproject-rtos/zephyr.git /tmp/virq-zephyr
git -C /tmp/virq-zephyr checkout --detach 32229d0b6cc60a1906307311b8e79ec483ad1e88
python3 scripts/test/zephyr-soft-virq-suspend/run.py \
  --zephyr-base /tmp/virq-zephyr \
  --cross-compile "$CROSS_COMPILE"
```

从 TGOSKits 仓库根运行。默认产物目录是 `tmp/axbuild/virq-suspend`；`--output` 可选择独立目录，`--prepare-only` 只构建 guest 并生成配置。普通 Axvisor 构建不启用 `test-virq-delivery`，没有后台注入器。

## 3. 判定与限制

`virq_regression.rs` 使用现有 `AxVM::inject_interrupt_to_vcpu()` 驱动场景。guest 各 CPU 关闭自身 timer IRQ 并通过 HVC 执行 `CPU_SUSPEND(standby)`；在 CPU1 启用 SPI 48 时由 Zephyr 设置路由，ISR 同时检查实际 CPU。mailbox 的每个字段只有一个写入方，位于一致性 guest RAM，由现有 guest-memory API 读取；该配置仅用于 QEMU。

### 3.1 成功与失败

host 在两个 vCPU ready 后，逐次等待 guest 应答，并检查 CPU0 的挂起次数不变。测试入口自动接入并激活 guest 控制台，以读取 guest 完成标记。只有三个成功标记及 `VM[2] state changed to Stopped` 都出现，`run.py` 才以零退出并输出 `AXVISOR_VIRQ_SUSPEND_PASSED`。

```text
VIRQ_INJECT_COMPLETE vm=2 vcpu=1 vector=48 samples=300 errors=0
E1_COUNTERS idle_vcpu_returns=0 acknowledgements=300
SOFTWARE VIRQ COMPLETE streams=1 samples_each=300 total=300
```

这些是判定格式，不是历史运行样本。guest 失败、host 失败、提前退出或缺少任何标记都会使脚本失败。host 应答等待上限为 30 秒，整个 Axvisor 构建与运行上限为 900 秒；成功后脚本停止自己创建的进程组，避免 Axvisor shell 永久等待。超时不算成功，也不会重试投递或放宽断言。

### 3.2 证据与回滚

完整输出保存在产物目录的 `qemu.log`，记录 host 提交、dirty 状态和 Zephyr 提交。它证明本场景的投递与隔离，不代表实体板延迟、吞吐或历史 A/B 性能结果。旧 `results`、个人笔记和原始会话记录不作为验证依据。

回滚代码即可恢复旧行为，无持久状态迁移。合入前仍需虚拟化领域审查人核对请求发布、等待消费及各架构调用链；测试通过不替代设计审查。
