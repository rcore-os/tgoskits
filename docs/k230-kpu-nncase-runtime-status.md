# StarryOS K230 NNCase Runtime 状态记录

本文记录当前 StarryOS K230 KPU/NPU 适配中“原生 NNCase runtime”路线的目标、边界、已知问题和后续验证方法。它和现有 `docs/k230-kpu-demo-runbook.md` 的定位不同：runbook 面向老师现场展示命令，本文面向课程报告、后续调试和任务交接。

当前工作区：

```text
/Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
```

当前主线：

```text
StarryOS K230 NNCase runtime
```

## 1. 目标

最终目标是让 StarryOS 在 QEMU K230 上运行真实 YOLOv8n 模型，而不是只验证寄存器、IRQ 或手写 command。完整链路应拆成三层：

| 层级 | 目标 | 当前状态 |
| --- | --- | --- |
| KPU 驱动层 | StarryOS 发现 KPU、暴露 `/dev/kpu`、提交 command、等待 done/IRQ | 已完成，`kpu-smoke` 已验证 |
| Runtime 层 | 用户态加载 `.kmodel`，调用 NNCase runtime，由 runtime 生成 KPU command stream | 已通过 `kpu-nncase-runtime`：minimal 和 image demo 都在 StarryOS guest 内完成 54 次 `gnne_enable`，`_Exit` 只用于跳过官方 SDK 退出清理，KPU run、done/IRQ 和 output tensor hash 已产生 |
| Demo 层 | 图片输入、预处理、推理、后处理，输出 YOLOv8n 检测结果 | decode/preprocess/run/postprocess 流程可执行，四个 NNCase output tensor 已有非零结果；当前 `detections=0`，检测框语义和官方 reference 仍待对齐 |

当前 NNCase runtime 路线的最小验收目标是：

1. 用官方 K230 SDK 的 NNCase runtime 构建 riscv64 用户态 demo。
2. 在 StarryOS QEMU K230 guest 内运行该 demo。
3. demo 加载 `yolov8n_320.kmodel`。
4. demo 调用 `interpreter.load_model()` 和 `interpreter.run()`。
5. runtime 通过 compat shim 把 KPU command 提交到 StarryOS `/dev/kpu`。
6. StarryOS 侧观察到 KPU start/done/IRQ。
7. demo 打印 output hash 或后处理结果，作为可检查输出。

这条路线的关键价值是：`.kmodel -> NNCase runtime -> KPU command -> /dev/kpu -> QEMU KPU` 这段路径发生在 StarryOS guest 内，而不是预先把 command stream 展开好再复放。

## 2. 2026-05-29 最新结果

本轮已经完成“正常读取权重/RDATA”这一步：`kpu-nncase-minimal` 和 `k230-yolov8n-demo` 可以在 StarryOS QEMU K230 guest 内加载真实 `yolov8n_320.kmodel`，调用官方 NNCase interpreter，现场生成并提交 54 条 KPU command，等待 QEMU KPU done/IRQ，并读取非零 output tensor hash。`kpu-nncase-runtime` case 已完整通过。

此前的关键问题不是 `.kmodel` 不能加载，也不是 command 未生成，而是官方 runtime 的 DDR/RDATA 地址语义和 QEMU KPU 低位 runtime window 没完全对齐。早期只镜像 `0x10000020..0x10190000`，停止在 command window 前，导致后续 `0x102...`/`0x103...` 高地址权重读取仍然是 `head 0x0`。最新修正把 mirror 扩展到 direct-io 前：

```text
0x3c000020 -> 0x10000020
bytes = KPU_RUNTIME_DIRECT_IO_PADDR - KPU_RUNTIME_RDATA_BASE = 5242848
range = 0x10000020..0x10500000
```

因此当前结论是：

1. `.kmodel -> NNCase runtime -> 54 条 KPU command -> /dev/kpu -> done/IRQ -> output tensor hash` 已经做到。
2. QEMU KPU 现在能从 Starry runtime alias 中读取完整 RDATA/command/runtime image 范围，之前缺失的 `0x102...`/`0x103...` 权重段已变成非零读取。
3. StarryOS 两个 NNCase demo 各跑 54 次 KPU submit；trace 中前 54 次和后 54 次都与官方 reference 的 54-run `gnne_summary` 匹配。
4. 当前剩余问题不再是“权重是否能被 QEMU 读取”，而是 YOLO output tensor 的语义解释、预处理/后处理和官方检测框 reference 对齐。

当前调试命令仍是：

```sh
cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-nncase-runtime
```

核心运行日志如下：

```text
NNCASE_MINIMAL: loading kmodel path=/usr/share/k230-nncase-runtime/models/yolov8n_320.kmodel bytes=3647752
NNCASE_MINIMAL: load_model ok
NNCASE_MINIMAL: model io inputs=1 outputs=4
NNCASE_MINIMAL: input[0] datatype=6 shape=[1,3,320,320] bytes=307200
NNCASE_MINIMAL: output[0] datatype=11 shape=[1,84,2100] elements=176400
NNCASE_MINIMAL: running nncase interpreter
K230_SDK_COMPAT: mirrored runtime rdata 0x3c000020 -> 0x10000020 bytes=5242848
K230_SDK_COMPAT: gnne_enable run=54 ... status=0x0000000400000004
NNCASE_MINIMAL: interp.run done
NNCASE_MINIMAL: output[0] bytes=705600 fnv1a64=0x31f54b838e803a0e
NNCASE_MINIMAL: output[1] bytes=921600 fnv1a64=0x7f6b778e4c78e285
NNCASE_MINIMAL: output[2] bytes=230400 fnv1a64=0x39a7c87f84766372
NNCASE_MINIMAL: output[3] bytes=57600 fnv1a64=0x907ad03f8695f75e
K230_SDK_COMPAT: stats mmz_alloc=15 kpu_run=54
NNCASE_MINIMAL_PASS
YOLOV8N_DEMO: output[0] bytes=705600 fnv1a64=0x79742dfead3bd654 min=-0.075500 max=527.000000 mean=2.884613
YOLOV8N_DEMO: output[1] bytes=921600 fnv1a64=0x84dfc61bc9818fa3 min=-62.593750 max=45.375000 mean=-2.924197
YOLOV8N_DEMO: output[2] bytes=230400 fnv1a64=0x690a770e12ee45a3 min=-145.250000 max=56.093750 mean=-18.802158
YOLOV8N_DEMO: output[3] bytes=57600 fnv1a64=0x566fe3584ecf9b25 min=-135.875000 max=33.781250 mean=-22.656527
YOLOV8N_DEMO_PASS
K230_NNCASE_RUNTIME_PASS
all starry k230-qemu qemu tests passed
```

这说明 StarryOS 现在已经不是只复放 `.krun`。在 `kpu-nncase-minimal` 中，`.kmodel` 由 StarryOS guest 内的 NNCase interpreter 加载，KPU command 由 runtime 在 guest 内生成，再由 compat shim 转交给 `/dev/kpu`。这已经达成“StarryOS 原生运行官方 NNCase runtime 产物并加载真实 `.kmodel`”的核心阶段目标。

### 2.1 L2 identity mapping 的原因和证据

最新调试确认，官方 K230 SDK runtime 不只是把 command buffer 传给 `gnne_enable()`。它还会通过 L2 上的 runtime arg table/RDATA 上下文组织每次 submit 的参数。关键证据是：

1. 反查官方库可见 `gnne_get_l2()` 返回固定物理地址 `0x80000000`。
2. QEMU K230/KPU 的 runtime arg table 也位于 L2 base：`K230_GNNE_RUNTIME_ARG_TABLE = 0x80000000`。
3. Starry/Linux ABI 下，普通用户态地址空间不会天然把 `0x80000000..0x80200000` identity-map 给 runtime；如果不处理，runtime 对 L2 arg table 的写入不能按官方 RT-Smart 语义落到 QEMU KPU 可见的物理 L2。
4. 只 identity-map L2 后，compat shim 能看到非零且随 run 变化的 arg table words，例如：

```text
K230_SDK_COMPAT: identity mmap l2 0x80000000..0x80200000
K230_SDK_COMPAT: arg_table words=0x3c373020 0x3c596020 0x3c000020 0x00000000
```

这组 words 落在 runtime DDR window `0x3c000000..0x40000000`，说明 NNCase/K230 runtime 已经按模型运行时状态写入真实参数地址，而不是只提交一段固定空 command。第三个 word 原本是 `0x3c000020`，compat shim 会先把官方 DDR 中从 `0x3c000020` 开始的 runtime image 镜像到 QEMU KPU 低位 runtime alias，再把 arg table patch 成 `0x10000020`，让 QEMU KPU 从可见窗口读 RDATA/权重。

另一个关键修正是 command 提交方式。早期 shim 把 command 复制到固定 command window `0x10190000` 再提交，QEMU 侧看不到官方 runtime 原始地址上下文。最新策略是：如果 `gnne_enable()` 传入的 `pc_start` 属于官方 runtime DDR，就把 command bytes 复制到低位 alias 后提交，例如 `0x3c372a88` 会以 `0x10372a88` 的 runtime alias 提交；只有无法翻译时才复制到兜底 command window。调试日志中已经出现 runtime alias 提交：

```text
K230_SDK_COMPAT: gnne_enable raw=0x3c372a88..0x3c372bc2 len=314 mode=runtime-alias submit=0x10372a88 arg_patch=1
K230_SDK_COMPAT: arg_table words=0x3c9bc020 0x3c7b1f00 0x10000020 0x00000000
```

trace 验证进一步说明当前不是“只是地址不同”。StarryOS trace 中 minimal 和 image demo 一共 108 次 run，按 54 次拆成两段后都与官方 reference 的 54-run summary 匹配：

```text
reference_runs=54 candidate_runs=108
first54_match=True
last54_match=True
reference_l2_load_w_sum=1783 candidate_first54_sum=1783 candidate_last54_sum=1783
starry detail total=3566 nonzero=3502 zero=64
```

剩余 64 条 `head 0x0` 的 `l2_load_w` 来自已经完整镜像的低地址区，例如 `0x100c...`，更像真实权重/填充中的零值；此前异常的 `source 0x102... head 0x0` 和 `source 0x103... head 0x0` 已经消失。

### 2.2 为什么不能 identity-map 低位 runtime windows

曾经尝试把低位 runtime windows 也 identity-map 到用户态，例如 `0x10000000..0x11000000` 一类 QEMU runtime/fake output/direct I/O 区间。这个方向会更早破坏官方 runtime 的 allocator 或 Starry/Linux 用户地址空间布局，表现为 SDK/MMZ allocator 更早进入异常清理路径。

当前结论是：

1. 必须 identity-map L2，因为官方 `gnne_get_l2()` 固定返回 `0x80000000`，runtime 会直接访问这个地址作为 arg table/L2 上下文。
2. 不应 identity-map 低位 runtime windows，因为这些地址不是官方 SDK 用户态 ABI 下必须固定裸访问的全部范围，且容易和 Starry/Linux 用户地址空间、mmap 返回地址或 allocator 管理区发生冲突。
3. 低位 runtime/direct I/O/DDR window 应继续通过 `/dev/kpu` 受限 mmap、MMZ bump allocator、虚拟 `/dev/mem` 定向映射和 `translate_vaddr()` 处理，而不是一股脑固定映射。

### 2.3 `_Exit` workaround 的边界

`_Exit(0)` workaround 已落地，并且只放在 demo 已经打印 PASS、flush 日志之后，用于绕过 C++ 静态析构和官方 SDK/MMZ allocator 清理路径。它的边界必须写清：

| 项目 | 说明 |
| --- | --- |
| 解决什么 | 避免 `interpreter.run()` 已完成后，进程退出时官方 SDK/MMZ allocator 在 `insert_free_node` 里 abort |
| 不解决什么 | 不修复 YOLO 输出语义、不改变 command stream、不改变 KPU 执行结果 |
| 是否影响 KPU 证据 | 不影响。`kpu_run=54`、done status、IRQ 和 output hash 已在 `_Exit` 之前产生 |
| 是否应长期保留 | 短期展示可以接受；长期应继续查官方 MMZ allocator 在 Starry/Linux ABI shim 下的 free/list 管理差异 |

仍需谨慎表述的是：当前 QEMU KPU 已经把结果写回 Starry tensor，四个 NNCase output tensor 都是非零且有可检查 hash；但 YOLO 语义输出尚未和官方 RT-Smart reference 的检测框逐项对齐。本次图片 demo 的后处理结果仍是 `detections=0`，它证明 decode/preprocess/run/postprocess 管线可执行，不等价于真实检测精度已验证。`direct[yolo_bbox]` 和 `direct[yolo_class]` 读取仍全 0，因此 direct-output 路线还不能作为主证据；本阶段主证据是 Starry tensor output hash/stats、54-run summary match、以及高地址权重读取不再缺失。

## 3. 与 54 条 `.krun` 复放的区别

当前已经完成的 54 条 `.krun` 复放，是从 kunOS/K230 SDK RT-Smart YOLOv8n reference 中抓取完整 KPU workload，再转换成 StarryOS 可复放的 `.krun`：

```text
kunOS / RT-Smart / K230 SDK
  -> 运行 YOLOv8n
  -> 捕获 54 条 KPU command 及 pre-start snapshot/delta
  -> 生成 yolov8n-full-sequence-delta.krun
  -> StarryOS kpu-smoke 逐条复放
```

NNCase runtime 路线则应变成：

```text
StarryOS 用户态 demo
  -> 读取 yolov8n_320.kmodel
  -> NNCase interpreter.load_model()
  -> NNCase interpreter.run()
  -> K230 runtime/HAL 生成 KPU command
  -> compat shim 提交到 /dev/kpu
  -> StarryOS KPU 驱动等待 IRQ
  -> demo 读取输出 tensor/hash/检测框
```

两者的差异如下：

| 维度 | 54 条 `.krun` 复放 | NNCase runtime 路线 |
| --- | --- | --- |
| `.kmodel` 解析 | 不在 StarryOS 中发生；只安装模型用于资产校验 | 在 StarryOS guest 内发生 |
| command 生成 | 已由 kunOS/RT-Smart reference 提前生成 | 由 StarryOS guest 内的 NNCase runtime 生成 |
| StarryOS 主要证明 | 能承载完整真实 KPU command 序列，IRQ/hash 对齐 | 能运行 K230 NNCase runtime，并把 runtime 生成的 command 接到 `/dev/kpu` |
| 展示稳定性 | 高，适合现场展示 | runtime case 已通过；检测框语义仍在调试 |
| 技术含量边界 | 类似“播放已展开 workload” | 更接近“StarryOS 原生运行真实模型” |

因此，`.krun` 复放不是最终 runtime 目标，但它不是无意义工作。它证明了 StarryOS 的 KPU 驱动、FDT、MMIO、mmap、IRQ、QEMU KPU 模型和完整 YOLOv8n command 序列之间已经能闭环。NNCase runtime 路线是在这个基础上继续补齐 `.kmodel` 加载和 command 生成。

## 4. 两段式构建是否算“作弊”

当前计划采用两段式构建：

```text
官方 K230 SDK amd64 Docker 镜像
  -> 交叉编译 riscv64 NNCase demo 二进制
StarryOS test-suit
  -> 安装预构建 demo 二进制、模型、图片
  -> 在 StarryOS QEMU K230 guest 内运行
```

准确边界如下：

| 问题 | 结论 |
| --- | --- |
| 这是不是 `.krun` 复放？ | 不是。`.krun` 复放直接提交已展开 command；NNCase demo 在 StarryOS guest 内加载 `.kmodel` 并调用 runtime。 |
| 这是不是 StarryOS dev 镜像内源码构建？ | 不是。源码构建阶段放在官方 K230 SDK amd64 镜像中完成。 |
| 这是不是运行层面的作弊？ | 不是。只要二进制实际在 StarryOS guest 内运行，并且 runtime 现场生成/提交 command，运行链路仍然是 StarryOS 承载。 |
| 这是不是构建层面的妥协？ | 是。它绕过了 StarryOS arm64 开发镜像无法直接使用官方 amd64 SDK toolchain、旧 linker 不兼容官方库属性等问题。 |
| 展示时应该怎么表述？ | 应表述为“官方 SDK 镜像交叉构建，StarryOS 原生运行”，不能表述为“StarryOS 开发镜像内完整源码构建官方 runtime”。 |

这不是为了降低运行目标，而是为了把问题拆开：

1. 先验证 StarryOS 能不能运行官方 runtime 产物，并提供足够的 `/dev/kpu`/MMIO/IRQ/内存兼容层。
2. 再决定是否需要把构建也纳入 StarryOS 默认开发镜像。

短期课程展示更关心运行链路是否真实。构建链路可以通过脚本和 Docker 镜像保证可复现，不必要求现场在 arm64 Starry dev 镜像内重新编译官方 SDK runtime。

## 5. 当前启动桩和 CRT 问题

本节记录曾经遇到的启动问题和最终修正。最开始的判断是官方 SDK toolchain 生成的 riscv64 ELF 与 Starry/Linux ABI 的入口假设不一致。

已观察到的现象：

```text
readelf: Entry point address: 0x0
StarryOS guest: segmentation fault at VA:0x0 EXECUTE | USER
```

初步原因：

1. 官方 SDK toolchain 生成的 `_start` 位于 `.start` section。
2. 该 `.start` section 不是普通 Linux 用户态 loader 期望的 executable/alloc text section。
3. RT-Smart loader 可能对该 section 或入口有特殊处理。
4. StarryOS 当前用户态 loader 按 Linux ABI/ELF 入口执行，看到 `e_entry=0x0` 后会从 0 地址取指，导致用户态 `pc=0` 崩溃。

进一步验证后发现，问题不止是 ELF entry。官方 SDK libc/syscall 路径更偏 RT-Smart ABI。例如 SDK libc 中 `syscall_write` 使用的 syscall number 不是 RISC-V Linux ABI 的 `write=64`。因此，如果继续使用官方 SDK libc/CRT，即便修正 entry，后面仍会进入错误 syscall 流。

最终采用的修复不是保留官方 SDK CRT 再加 `_starry_start`，而是：

```text
K230 SDK g++/binutils
  -> 负责编译/链接官方 NNCase、RVV、K230 runtime 静态库
Linux riscv64-musl crt1.o/crti.o/crtn.o/libc/libgcc
  -> 提供 Starry/Linux ABI 入口、libc 初始化和 Linux syscall ABI
SDK libstdc++/libsupc++/libatomic
  -> 保留官方 runtime 需要的 C++ 支持库
```

这样生成的 demo ELF 入口不再是 `0x0`，并且在 `qemu-riscv64-static -strace` 中能看到 Linux ABI syscall，如 `set_tid_address`、`ioctl`、`writev`、`exit_group`。本地检查示例：

```text
Entry point address: 0x1269e
```

这个结论对报告很重要：我们不是要求 StarryOS 兼容 RT-Smart libc，而是把官方 NNCase/K230 runtime 静态库重新链接到 Starry/Linux ABI 上，再在 StarryOS guest 内运行。

已知后续风险：

| 风险 | 说明 |
| --- | --- |
| 官方 SDK libc 不能直接用于 Starry/Linux ABI | 已确认 syscall number 不兼容；当前方案避开 SDK libc/CRT |
| 官方 NNCase/RVV 库需要 C908V | `-smp 1` 只启动 C908，会在 `vsetvli` 上 SIGILL；当前 QEMU case 使用 `-smp 2` 启动 C908V |
| RISC-V vector 上下文 | 当前为展示路径启用了用户态 VS 状态；完整多任务 vector save/restore 仍需后续工程化 |
| YOLO 语义输出 | 当前图片 demo 完成 output hash 和后处理流程，但检测框语义尚未与官方 reference 对齐 |

## 6. 当前 compat shim 设计边界

当前方向不是把完整 K230 Linux/RT-Smart 设备栈移植进 StarryOS，而是在 demo 内做最小 compat shim：

| 接口 | 处理方式 |
| --- | --- |
| `/dev/gnne_device` | 应用内 wrap 成虚拟 fd，最终转发到 `/dev/kpu` |
| `/dev/ai_2d_device` | 短期先最小兼容或绕过，图片模式优先 CPU 预处理 |
| `/dev/mem` | 只允许映射 KPU runtime 已声明窗口，不做通用物理内存后门 |
| `kd_mpi_sys_mmz_alloc_cached/free` | 用 `/dev/kpu` 暴露的 runtime DDR window 做 4KiB 对齐 bump allocator |
| `kd_mpi_sys_mmz_flush_cache` | 初期可作为 no-op 或同步边界，后续按 QEMU/缓存模型补语义 |
| `gnne_get_l2` / L2 访问 | 官方 runtime 固定使用 `0x80000000` 作为 L2/arg table，Starry/Linux ABI 下只对 L2 做 identity mapping |
| runtime RDATA mirror | 当 arg table 指向 `0x3c000020` 时，把官方 DDR 中 `0x3c000020..0x3c500000` 镜像到 QEMU KPU 可见的 `0x10000020..0x10500000` |
| `gnne_enable` | wrap 官方 runtime 的 command buffer；优先把 runtime DDR command 复制到低位 runtime alias 后提交，无法翻译时才复制到兜底 command window；通过 `KPU_IOC_RUN` 和 `KPU_IOC_WAIT_DONE` 提交 |

本轮修正了一个关键细节：虚拟 `/dev/gnne_device`、`/dev/ai_2d_device`、`/dev/mem` fd 必须返回正数。早期 shim 返回负数 fake fd，官方 runtime 按 POSIX 规则把它当作 `open` 失败，表现为：

```text
open /dev/gnne_device failed: No error information
```

现在 fake fd 改为稳定的正数区间，并同时 wrap `open` 与 `openat`。之后 runtime 能成功进入 `gnne_enable`，并在每次 `interpreter.run()` 中提交 54 次 KPU command。

本轮又修正了两个关键细节：官方 `gnne_get_l2()` 返回 `0x80000000`，所以 L2 必须按官方语义对用户态 runtime 可见；但低位 runtime windows 不应全部 identity-map，否则容易破坏 Starry/Linux 用户地址空间或官方 MMZ allocator 管理区。低位窗口现在通过受限 mmap、allocator、地址翻译和 runtime alias mirror 接住：arg table 第三个 word 从 `0x3c000020` patch 到 `0x10000020`，并把 RDATA/command/runtime image 扩展镜像到 direct-io 前。

这个边界很重要：StarryOS kernel UAPI 暂时保持稳定，优先在用户态 demo/shim 中吸收官方 SDK runtime 的兼容需求。只有当 runtime 必须依赖无法在用户态 shim 解决的能力时，再考虑新增内核 ioctl 或设备节点。

## 7. 验证命令

以下命令用于记录当前主线，不要求每次文档修改后都运行。

### 7.1 构建 NNCase runtime demo 二进制

在宿主机的 K230 worktree 中运行：

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-nncase-runtime/c/tools/build-nncase-runtime-binaries.sh
```

预期结果：

```text
assets/bin/kpu-nncase-minimal
assets/bin/k230-yolov8n-demo
```

这一步使用官方 K230 SDK amd64 Docker 镜像交叉编译 riscv64 二进制。

### 7.2 检查 ELF entry

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
docker run --rm \
  -v /Users/joshua/tmp/tgoskits:/workspace \
  -w /workspace/target/worktrees/tgoskits-k230-upstream-dev \
  starryos-dev:ubuntu-qemu10.2.1 \
  bash -lc '
    riscv64-linux-musl-readelf -h \
      test-suit/starryos/k230-qemu/qemu-k230/kpu-nncase-runtime/c/assets/bin/kpu-nncase-minimal |
      grep "Entry point"
  '
```

修复前的错误特征：

```text
Entry point address: 0x0
```

修复后的目标特征：

```text
Entry point address: 非 0
```

### 7.3 qemu-user 最小启动验证

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
docker run --rm \
  -v /Users/joshua/tmp/tgoskits:/workspace \
  -w /workspace/target/worktrees/tgoskits-k230-upstream-dev \
  starryos-dev:ubuntu-qemu10.2.1 \
  bash -lc '
    qemu-riscv64-static -strace \
      test-suit/starryos/k230-qemu/qemu-k230/kpu-nncase-runtime/c/assets/bin/kpu-nncase-minimal \
      /no/such/model 2>&1 | sed -n "1,120p"
  '
```

最低预期：

```text
进程不再 pc=0 立即崩溃
应用能进入 main 或至少打印模型路径错误
```

如果仍然只看到异常 syscall、`EFAULT` 或没有应用日志，需要继续确认官方 SDK CRT/libc 是否是 RT-Smart 专用 ABI。

### 7.4 StarryOS K230 guest 运行验证

运行新增 NNCase runtime case 时需要确保使用 K230 QEMU，而不是普通 `/opt/qemu-10.2.1`：

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
docker run --rm \
  -v /Users/joshua/tmp/tgoskits:/workspace \
  -v /Users/joshua/tmp/tgoskits:/mnt/tgoskits \
  -w /workspace/target/worktrees/tgoskits-k230-upstream-dev \
  starryos-dev:ubuntu-qemu10.2.1 \
  bash -lc '
    set -eu
    ldconfig -p | grep -q libfdt.so.1 || (apt-get update && apt-get install -y libfdt1)
    export PATH=$PWD/target/qemu-k230/bin:/opt/riscv64-linux-musl-cross/bin:/opt/x86_64-linux-musl-cross/bin:/opt/qemu-10.2.1/bin:$PATH
    qemu-system-riscv64 -machine help | grep k230
    cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-nncase-runtime
  '
```

目标日志：

```text
K230_NNCASE_RUNTIME: minimal
load_model ok
K230_SDK_COMPAT: mirrored runtime rdata 0x3c000020 -> 0x10000020 bytes=5242848
interp.run done
NNCASE_MINIMAL_PASS
YOLOV8N_DEMO_PASS
K230_NNCASE_RUNTIME_PASS
all starry k230-qemu qemu tests passed
K230_SDK_COMPAT: stats mmz_alloc=... kpu_run=54
```

如果要检查权重读取是否覆盖到位，继续运行 trace 脚本：

```sh
STARRY_TRACE_RUN_WAIT=120 STARRY_TRACE_TIMEOUT=180 \
  test-suit/starryos/k230-qemu/qemu-k230/kpu-nncase-runtime/c/tools/trace-nncase-runtime.sh
```

当前 trace 预期结果是 `trace-nncase-runtime: pass`，并且前 54 次、后 54 次 run 都匹配官方 54-run reference summary。

若出现：

```text
segmentation fault at VA:0x0 EXECUTE | USER
pc(sepc)=0x0000000000000000
```

优先回到 ELF entry、Linux musl CRT 链接和 QEMU C908V/RVV 配置，而不是先怀疑 KPU 驱动。

### 7.5 现有 54 条 `.krun` 展示回归

这条命令用于确认当前稳定展示路径没有被破坏：

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/demo-yolov8n-full-sequence-replay.sh
```

预期关键日志：

```text
KPU_SMOKE: runtime_image_progress ... run=54/54 irq_count=54
KPU_SMOKE: runtime_image kunos_yolov8n_full_sequence_delta runs=54
KPU_SMOKE: real_kmodel ... magic=LDMK version=6
KPU_SMOKE_PASS
all starry k230-qemu qemu tests passed
demo replay passed
```

这条回归和 NNCase runtime case 互补：前者保证已有可展示成果稳定，后者验证下一阶段真实 `.kmodel` runtime 路线。

## 8. 下一步建议

当前 C/D 阶段已经从“能否加载 `.kmodel` 并提交真实 runtime command”推进到“权重读取已对齐，继续做输出语义对齐”。下一步建议按以下顺序推进：

1. 保存当前稳定验收日志和 trace 摘要，作为报告证据：`load_model ok`、`kpu_run=54`、done status、output hash、`K230_NNCASE_RUNTIME_PASS`、`first54_match=True`、`last54_match=True`。
2. 对齐官方 RT-Smart/kunOS reference 的图片预处理和后处理参数，解释当前 StarryOS 图片 demo 为什么 `detections=0`。
3. 明确 NNCase 四个 output tensor 和 K230 SDK demo 最终 bbox/class buffer 的对应关系；当前 direct bbox/class 仍为 0，不能作为主输出证据。
4. 如果 QEMU KPU 模型本身只提供 deterministic/fake-ish output，而不计算真实 YOLO 语义，需要把 tensor hash/stats 对齐作为展示验收，不把 bbox 精度作为当前阶段承诺。
5. 工程化 RISC-V vector 上下文：当前展示路径已能执行 RVV 指令，但通用多任务场景仍需要完整 vector save/restore。

## 9. 报告表述建议

报告中建议使用以下表述：

```text
我们已经完成 StarryOS K230 KPU 驱动与完整 YOLOv8n KPU workload 复放验证。
第二阶段的核心 runtime 路径已经通过：复用官方 K230 SDK/NNCase runtime 静态库，在官方 SDK amd64 镜像中交叉构建 riscv64 demo，并由 StarryOS QEMU K230 guest 原生运行。minimal demo 在 StarryOS 内加载 yolov8n_320.kmodel，通过 NNCase interpreter 现场生成 54 条 KPU command，再经 /dev/kpu 提交给 QEMU KPU 模型，完成 done/IRQ 和 output hash。图片 demo 也能完成 decode/preprocess/run/postprocess 流程，四个 NNCase output tensor 已有非零 hash/stats。
本轮解决了地址窗口和权重读取的关键问题：把官方 runtime DDR 中 `0x3c000020..0x3c500000` 镜像到 QEMU KPU 可见的 `0x10000020..0x10500000`，并将 L2 arg table 中的 RDATA 指针 patch 到低位 runtime alias。trace 验证中 StarryOS 两段 54-run 序列都和官方 reference summary 匹配，之前 `0x102...`/`0x103...` 权重段读零的问题已经消失。
这不是 54 条 .krun 复放，因为 command 生成发生在 StarryOS guest 运行时；但它也不是 StarryOS dev 镜像内源码构建官方 runtime，而是交叉构建加 StarryOS 原生运行的两段式方案。
当前 remaining gap 是 YOLO 输出语义对齐：检测框结果尚未与官方 reference 做逐项对齐，direct bbox/class buffer 仍为 0；但 KPU 权重读取、54 次 command 执行、done/IRQ 和 Starry tensor 写回已经完成。
```

不要把两段式方案描述成：

```text
StarryOS 开发镜像已经能完整源码构建官方 K230 SDK runtime。
```

更准确的描述是：

```text
官方 SDK 镜像负责可复现交叉构建，StarryOS 负责运行时承载、设备兼容和 KPU 提交。
```
