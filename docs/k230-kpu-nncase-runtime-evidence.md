# StarryOS K230 NNCase Runtime 阶段证据

本文记录 2026-05-29 阶段性验收结果：StarryOS 已经在 QEMU K230/C908V 环境中跑通原生 NNCase runtime，完成真实 `.kmodel` 加载、54 条 KPU command 现场生成、`/dev/kpu` 提交、done/IRQ、权重/RDATA 读取和非零 output tensor 写回。它面向课程报告和现场展示复盘，重点保存命令、关键日志、已经达到的水平、和 kunOS/RT-Smart 54 条 command replay 的区别，以及剩余 YOLO 语义对齐缺口。

## 1. 阶段结论

当前已经达到的核心结果：

```text
kpu-nncase-runtime 在 StarryOS guest 内加载 yolov8n_320.kmodel
NNCase interpreter.run() 在 guest 内生成并提交 54 条 KPU command
minimal demo 打印 NNCASE_MINIMAL_PASS
image demo 打印 YOLOV8N_DEMO_PASS
kpu-nncase-runtime case 打印 K230_NNCASE_RUNTIME_PASS
QEMU KPU 从 Starry runtime alias 正常读取权重/RDATA，并写回非零 output tensor
kpu-smoke 在 -smp 2 下通过 54/54 replay
```

这说明 StarryOS 当前已经不只是“复放 kunOS/RT-Smart 展开后的 KPU command”。在 `kpu-nncase-runtime` case 中，`.kmodel` 的加载和 NNCase runtime 的 `interpreter.run()` 都发生在 StarryOS guest 内，runtime 现场生成 KPU command，再通过 StarryOS `/dev/kpu` 提交到 QEMU K230 KPU 模型。

需要严格保留的边界：

```text
当前最强证据是 kpu-nncase-runtime 已完成 load_model、interpreter.run、两段 54 次 gnne_enable、done/IRQ、权重/RDATA 读取、trace summary match 和 output tensor hash/stats。
_Exit workaround 已落地，只跳过清理析构，不改变 KPU run 或 output hash。
当前 image demo 的后处理结果是 candidates=0 / detections=0。
最新诊断显示这不是权重读取失败导致：完整 RDATA mirror 后，四个 NNCase output tensor 均为非零，StarryOS 两段 54-run trace 都与官方 reference summary 匹配。
NNCase output tensor 物理地址已经打印，QEMU/kunOS reference 的 final direct-io 区域也已读取；StarryOS 当前在这些 direct output 区域读到的 stats 仍全 0。
此前 image 路线已证明 decode、preprocess、run、get_output、postprocess 管线可执行；当前文档先以 minimal 的 54 条 command 为最新硬证据。
它还不证明 YOLOv8n 检测语义已经与官方 RT-Smart reference 对齐。
```

## 2. 验证环境

工作区：

```text
/Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
```

默认实验环境：

```text
Docker/Linux
QEMU K230 machine
C908V vector path
```

`kpu-nncase-runtime` 的 QEMU 配置关键点：

```text
-machine k230
-smp 2
-m 2G
-dtb ${workspace}/os/StarryOS/configs/board/k230-canmv.dtb
shell_init_cmd = /usr/bin/k230-nncase-runtime-demo
success_regex = ^K230_NNCASE_RUNTIME_PASS$
```

`success_regex` 已是当前验收形式；`kpu-nncase-runtime` 和 `kpu-smoke` 都使用 `-smp 2` K230/C908V 配置，用于保证 NNCase/RVV runtime 与 54 条 replay 在当前 QEMU/CPU 配置下稳定。

## 3. 验证命令

构建 NNCase runtime demo 二进制：

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-nncase-runtime/c/tools/build-nncase-runtime-binaries.sh
```

运行 StarryOS 原生 NNCase runtime case：

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-nncase-runtime
```

运行 54 条 `.krun` replay 回归：

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-smoke
```

现场展示也可以使用一键 replay 脚本：

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/demo-yolov8n-full-sequence-replay.sh
```

## 4. `kpu-nncase-runtime` 关键日志

### 4.1 Minimal Demo

minimal demo 证明 StarryOS guest 内可以加载真实 `.kmodel`，创建输入输出 tensor，调用 NNCase interpreter，并由 runtime 生成和提交 54 条 KPU command。

```text
NNCASE_MINIMAL: loading kmodel path=/usr/share/k230-nncase-runtime/models/yolov8n_320.kmodel bytes=3647752
NNCASE_MINIMAL: load_model ok
NNCASE_MINIMAL: model io inputs=1 outputs=4
NNCASE_MINIMAL: input[0] datatype=6 shape=[1,3,320,320] bytes=307200
NNCASE_MINIMAL: output[0] datatype=11 shape=[1,84,2100] elements=176400
NNCASE_MINIMAL: running nncase interpreter
K230_SDK_COMPAT: identity mmap l2 0x80000000..0x80200000
K230_SDK_COMPAT: mirrored runtime rdata 0x3c000020 -> 0x10000020 bytes=5242848
K230_SDK_COMPAT: gnne_enable raw=... mode=runtime-alias submit=... arg_patch=1
K230_SDK_COMPAT: arg_table words=... 0x10000020 ...
K230_SDK_COMPAT: gnne_enable run=54 ... status=0x0000000400000004
NNCASE_MINIMAL: interp.run done
NNCASE_MINIMAL: output[0] bytes=705600 fnv1a64=0x31f54b838e803a0e
NNCASE_MINIMAL: output[1] bytes=921600 fnv1a64=0x7f6b778e4c78e285
NNCASE_MINIMAL: output[2] bytes=230400 fnv1a64=0x39a7c87f84766372
NNCASE_MINIMAL: output[3] bytes=57600 fnv1a64=0x907ad03f8695f75e
K230_SDK_COMPAT: stats mmz_alloc=15 kpu_run=54
NNCASE_MINIMAL_PASS
```

关键证据点：

| 日志 | 含义 |
| --- | --- |
| `load_model ok` | `.kmodel` 已在 StarryOS guest 内由 NNCase runtime 加载 |
| `model io inputs=1 outputs=4` | runtime 成功解析模型输入输出 |
| `identity mmap l2 0x80000000..0x80200000` | 官方 `gnne_get_l2()` 固定返回 L2 base，Starry/Linux ABI 下必须只 identity-map L2 给 runtime |
| `mirrored runtime rdata ... bytes=5242848` | 官方 DDR runtime image 已镜像到 QEMU KPU 可见的低位 runtime alias |
| `mode=runtime-alias submit=0x103...` | command 按官方 runtime DDR 偏移复制到低位 alias 后提交 |
| `arg_table words=... 0x10000020 ...` | L2 runtime arg table 的 RDATA 指针已 patch 到 Starry/QEMU runtime alias |
| `gnne_enable run=54` | 本次 `interpreter.run()` 生成并提交 54 条 KPU command |
| `status=0x0000000400000004` | 每次 command run 等到 QEMU KPU done status |
| `output[*] fnv1a64=...` | 输出 tensor 可读取并可做 hash 记录 |
| `NNCASE_MINIMAL_PASS` | minimal 的核心 runtime run 已通过 |
| `K230_NNCASE_RUNTIME_PASS` | 当前 runtime case 已通过 |

### 4.2 L2 identity mapping 结论

L2 identity mapping 是当前 runtime 路线的关键修正。原因不是 StarryOS 想暴露任意物理内存，而是官方 K230 SDK runtime 的 `gnne_get_l2()` 直接返回 `0x80000000`，并把该地址作为 runtime arg table/L2 上下文使用。Starry/Linux ABI 下如果没有这段固定映射，runtime 写入的 arg table 就不会按 QEMU KPU 模型期望落到物理 L2。

同时，不能把低位 runtime windows 也整体 identity-map。实测把 `0x10000000..0x11000000` 一类低位窗口固定映射后，会更早破坏官方 allocator 或用户地址空间布局。当前安全边界是：

```text
只 identity-map L2: 0x80000000..0x80200000
低位 runtime/direct I/O/command/DDR window: 继续走受限 mmap、MMZ bump allocator 和地址翻译
```

### 4.3 Image Demo

image demo 在 minimal 的基础上增加图片输入、CPU 预处理、输出读取、后处理和 annotated PPM 生成。完整 RDATA mirror 修正后，四个 NNCase output tensor 已不再是全 0。

```text
YOLOV8N_DEMO: decode image=/usr/share/k230-nncase-runtime/images/bus.jpg width=810 height=1080
YOLOV8N_DEMO: load_model ok inputs=1 outputs=4
YOLOV8N_DEMO: preprocess layout=NCHW input=320x320
K230_SDK_COMPAT: mirrored runtime rdata 0x3c000020 -> 0x10000020 bytes=5242848
K230_SDK_COMPAT: gnne_enable run=54 ... status=0x0000000400000004
YOLOV8N_DEMO: run done
YOLOV8N_DEMO: output[0] bytes=705600 fnv1a64=0x79742dfead3bd654
YOLOV8N_DEMO: output[0] stats finite=176400 nan=0 min=-0.075500 max=527.000000 mean=2.884613
YOLOV8N_DEMO: output[1] bytes=921600 fnv1a64=0x84dfc61bc9818fa3
YOLOV8N_DEMO: output[1] stats finite=230400 nan=0 min=-62.593750 max=45.375000 mean=-2.924197
YOLOV8N_DEMO: output[2] bytes=230400 fnv1a64=0x690a770e12ee45a3
YOLOV8N_DEMO: output[2] stats finite=57600 nan=0 min=-145.250000 max=56.093750 mean=-18.802158
YOLOV8N_DEMO: output[3] bytes=57600 fnv1a64=0x566fe3584ecf9b25
YOLOV8N_DEMO: output[3] stats finite=14400 nan=0 min=-135.875000 max=33.781250 mean=-22.656527
YOLOV8N_DEMO: postprocess threshold score=0.15 nms=0.20
YOLOV8N_DEMO: postprocess rows=2100 channels=84 candidates=0
YOLOV8N_DEMO: detections=0
YOLOV8N_DEMO: annotated=/tmp/k230-yolov8n-demo.ppm
K230_SDK_COMPAT: stats mmz_alloc=15 kpu_run=54
YOLOV8N_DEMO_PASS
K230_NNCASE_RUNTIME_PASS
all starry k230-qemu qemu tests passed
```

关键证据点：

| 日志 | 含义 |
| --- | --- |
| `decode image=... bus.jpg width=810 height=1080` | 图片文件模式已跑通 |
| `preprocess layout=NCHW input=320x320` | 当前 demo 已完成 CPU resize 和 layout 转换 |
| `run done` | NNCase `interpreter.run()` 返回 |
| `mirrored runtime rdata ... bytes=5242848` | StarryOS 已把官方 DDR runtime image 镜像到 QEMU KPU 可见的低位 runtime alias |
| `output[0..3] ... fnv1a64=...` | 图片输入对应四个 NNCase output tensor 均可读取且非零 |
| `score_top[0] ... box_raw=(0,0,0,0)` | 后处理能解析 top score，但 bbox 原始输出仍无有效框 |
| `postprocess rows=2100 channels=84` | 后处理已按 YOLOv8n 输出维度运行 |
| `candidates=0` / `detections=0` | 当前语义输出尚未与官方 reference 对齐 |
| `YOLOV8N_DEMO_PASS` | 图片 demo 执行链路已通过 |
| `K230_NNCASE_RUNTIME_PASS` | 当前 case 验收行 |

## 5. `kpu-smoke` 54/54 Replay 回归

`kpu-smoke` 仍保留作为稳定展示和回归路径。它不在 StarryOS guest 内生成 command，而是复放 kunOS/RT-Smart reference 已展开的 54 条 command。

代表性日志：

```text
KPU_SMOKE: runtime_image_progress kunos_yolov8n_full_sequence_delta run=1/54 irq_count=1
KPU_SMOKE: runtime_image_progress kunos_yolov8n_full_sequence_delta run=16/54 irq_count=16
KPU_SMOKE: runtime_image_progress kunos_yolov8n_full_sequence_delta run=32/54 irq_count=32
KPU_SMOKE: runtime_image_progress kunos_yolov8n_full_sequence_delta run=48/54 irq_count=48
KPU_SMOKE: runtime_image_progress kunos_yolov8n_full_sequence_delta run=54/54 irq_count=54
KPU_SMOKE: runtime_image kunos_yolov8n_full_sequence_delta runs=54 status=0x0000000400000004 irq_count=0->54
KPU_SMOKE: real_kmodel path=/usr/share/k230-kpu-smoke/models/yolov8n_320.kmodel magic=LDMK version=6
KPU_SMOKE_PASS
all starry k230-qemu qemu tests passed
```

这条回归的作用：

1. 证明 StarryOS KPU driver、FDT、MMIO、mmap、IRQ、runtime scratch 和 full-sequence `.krun` loader 仍然稳定。
2. 证明从 kunOS/RT-Smart reference 抓取的完整 54 条 KPU workload 仍能在 StarryOS 上复放。
3. 作为现场展示兜底路径，即使 NNCase runtime 语义对齐还在推进，也能展示完整 YOLOv8n KPU workload 的 StarryOS 承载能力。

## 6. 与 kunOS/RT-Smart 54 Command Replay 的区别

两条路径的定位不同：

| 路线 | command 来自哪里 | StarryOS 内发生了什么 | 证明什么 |
| --- | --- | --- | --- |
| kunOS/RT-Smart 54 command replay | kunOS/RT-Smart/K230 SDK reference 预先运行并 capture | StarryOS 逐条复放 `.krun` 中的 command 和 memory delta | StarryOS 能承载完整真实 KPU workload |
| StarryOS NNCase runtime | StarryOS guest 内 NNCase runtime 现场生成 | StarryOS 用户程序加载 `.kmodel`，执行 `interpreter.run()`，compat shim 提交 command 到 `/dev/kpu` | StarryOS 能运行官方 runtime 产物，并让 `.kmodel -> command` 路径在 guest 内发生 |

因此，当前 `kpu-nncase-runtime` 的意义高于单纯 `.krun` replay：

```text
.krun replay 证明 StarryOS 能跑已经展开好的 workload。
kpu-nncase-runtime 证明 StarryOS 能在 guest 内加载模型并触发 runtime 自己生成 workload。
```

但是，两者仍然互补：

1. `.krun` replay 是稳定展示和低变量回归。
2. NNCase runtime 是通向“StarryOS 原生跑真实模型”的主线。
3. 当前二者都提交 54 条 KPU command，说明 command 数量和 QEMU KPU 执行路径已经一致。

## 7. 已达到的水平

当前可以对外准确表述为：

```text
StarryOS 已经在 QEMU K230/C908V 下跑通官方 K230 SDK/NNCase runtime 产物。
minimal demo 在 StarryOS guest 内加载 yolov8n_320.kmodel，调用 NNCase interpreter.run()，
由 runtime 现场生成并提交 54 条 KPU command，通过 /dev/kpu 等到 done/IRQ，
并输出可记录 hash。image demo 也完成 run 和 output tensor 读取，四个 output tensor 均为非零。
```

也可以说：

```text
StarryOS 已经从“复放 kunOS/RT-Smart YOLOv8n 展开后的 54 条 command”
推进到“StarryOS guest 内原生加载 .kmodel 并运行 NNCase runtime”。
```

不应说：

```text
YOLOv8n 检测框语义已经完全对齐官方 RT-Smart。
StarryOS 已经验证了最终检测精度。
StarryOS dev 镜像已经能完整源码构建官方 K230 SDK runtime。
```

可以声称 `kpu-nncase-runtime` 在 QEMU K230 环境下已经通过；但不要声称 YOLOv8n 检测框语义已经完全对齐官方 RT-Smart。

## 8. 剩余 Gap

当前剩余缺口已经从“模型能不能加载、command 能不能生成、KPU 能不能 submit、权重能不能读取”收敛到输出语义对齐。

| Gap | 当前现象 | 下一步 |
| --- | --- | --- |
| SDK/MMZ allocator 退出清理 | 已用 PASS 后 `_Exit(0)` 避开，不影响 KPU run/output 证据 | 长期再分析官方 allocator free/list 管理差异 |
| L2 identity mapping | 官方 `gnne_get_l2()` 返回 `0x80000000`，runtime 需要直接访问 L2 arg table；低位 runtime windows identity-map 会破坏 allocator/地址空间 | 只 identity-map L2，低位 windows 继续走受限 mmap、MMZ bump allocator 和地址翻译 |
| Output semantic alignment | image demo 输出 `candidates=0` / `detections=0`；`max_score=0.0625`，bbox raw 为 0 | 优先对齐 QEMU trace 中最终 `l2_store` 写回地址与 NNCase output tensor 物理地址，再判断是 QEMU 算子覆盖不足还是 runtime buffer 映射问题 |
| Reference hash | minimal/image demo 已输出 FNV hash，但还没有和官方 reference 固化比较 | 从 kunOS/RT-Smart 或官方 SDK reference 记录同输入、同模型、同 runtime 的 output hash |
| Postprocess 参数 | 已改为官方 README 示例参数 `score=0.15, nms=0.20`，仍无候选框 | 保留参数一致性，后续重点不再放在阈值 |
| 图片预处理语义 | 当前 demo 走 CPU resize/NCHW 预处理，语义已接近官方 image path 的 RGB/NCHW/tf_bilinear half_pixel | 后续只需做 reference hash 比对，避免在无证据情况下继续猜预处理 |
| 完整展示资产 | runtime case 已可跑，capture/replay 大资产不应直接进 PR | 保存完整 `kpu-nncase-runtime` 日志、output hash、annotated PPM、必要时保存 reference 对比表 |

## 9. 最新 Direct-IO 与 Output Paddr 诊断

最新一轮诊断进一步把问题从“后处理或 copy-back 可疑”收敛到“QEMU KPU trace 逐条分叉定位”。

### 9.1 NNCase Output Tensor 物理地址

StarryOS image demo 已打印 4 个 output tensor 的物理地址：

| Output | Paddr | 当前观察 |
| --- | --- | --- |
| `output0` | `0x3c3bf020` | 可读取，hash/stats 非全 0，但 top score 低，bbox raw 仍为 0 |
| `output1` | `0x3c46c020` | 当前 stats 全 0 |
| `output2` | `0x3c54e020` | 当前 stats 全 0 |
| `output3` | `0x3c587020` | 当前 stats 全 0 |

这说明 StarryOS 已经能拿到 NNCase runtime 分配的 output buffer 物理地址，问题不再是“用户态完全不知道输出 tensor 在哪里”。同时，`output0` 能读到非全 0 内容，也说明不是简单的 output tensor copy-back 完全缺失。

### 9.2 QEMU/kunOS Reference Final Direct-IO 区域

已经直接读取 QEMU/kunOS reference 中的 final direct-io 区域：

| 区域 | Paddr | Bytes | 含义 |
| --- | --- | ---: | --- |
| bbox | `0x1059a900` | `33600` | reference final bbox/direct output 区域 |
| class | `0x105b0e20` | `672000` | reference final class/direct output 区域 |

当前 StarryOS 读取这些 direct-io 区域的 stats 都是全 0。这个结果很关键：它说明剩余问题不只是 NNCase output tensor 的 host-side copy-back，也不只是后处理代码读错 tensor；更可能是 StarryOS 这一路 runtime command 执行中，某个 direct output 区域没有被 QEMU KPU 模型写出，或者某次 load/store 的输入、别名、RDATA、DDR window 与 kunOS reference 分叉。

### 9.3 Direct Source 输入同步

为了排除“runtime direct source 没有被填充”的可能，已经把预处理后的 input tensor 拷贝到：

```text
KPU_RUNTIME_DIRECT_SOURCE_PADDR = 0x10500020
```

对应输入 hash：

```text
0xfe3bfacc028c5231
```

但 direct output 仍然全 0。因此，当前问题也不能简单归因为“输入 tensor 没有进入 QEMU KPU runtime direct source window”。

### 9.4 当前排除项

基于上述证据，目前可以排除或降级以下方向：

| 方向 | 当前判断 |
| --- | --- |
| 模型加载失败 | 已排除，`load_model ok` 且 model IO 正常 |
| 54 条 command 没有提交 | 已排除，`gnne_enable run=54` 且每次 done status 正常 |
| 简单阈值/后处理问题 | 已降级，官方阈值下 raw output 本身缺少有效 bbox/class 语义 |
| 单纯 output tensor copy-back 问题 | 已降级，已打印 output tensor paddr，且 final direct-io 区域也全 0 |
| direct source 完全没填 | 已降级，预处理 input 已拷贝到 `0x10500020` 且 hash 已记录 |

当前最可能的下一步是抓取 StarryOS 运行 `kpu-nncase-runtime` 时的 QEMU KPU trace，并与 kunOS/RT-Smart reference trace 逐条 diff：

```text
kunOS trace:   start/l2_load/l2_store/l2_store_hash/gnne_summary ...
Starry trace:  start/l2_load/l2_store/l2_store_hash/gnne_summary ...
```

目标不是先猜后处理，而是定位第一个分叉点：

1. 第几个 command run 开始不同。
2. 第一次不同发生在 `l2_load`、`l2_store`、`l2_store_hash` 还是 arg table/RDATA 地址解析。
3. 分叉地址是否落在 direct source、direct output、NNCase output tensor、RDATA alias、DDR window 或 L2。
4. 分叉 hash 是否能解释最终 bbox/class direct output 全 0。

## 10. 下一步优先级

建议下一步按以下顺序做：

1. 抓取 StarryOS `kpu-nncase-runtime` 的 QEMU KPU trace，启用与 kunOS reference 相同的 `k230_kpu_start`、`k230_kpu_l2_load`、`k230_kpu_l2_store`、`k230_kpu_l2_store_hash`、`k230_kpu_gnne_summary` 事件。
2. 将 Starry trace 与 kunOS/RT-Smart trace 逐条 diff，优先定位第一个 `l2_load`/`l2_store`/hash 分叉点。
3. 对照分叉点的 guest physical address，判断问题落在 direct source、direct output、NNCase output tensor、RDATA alias、DDR window 还是 L2。
4. 固化 reference final direct-io 区域的 hash/stats：bbox `0x1059a900` bytes `33600`，class `0x105b0e20` bytes `672000`。
5. 在 trace 分叉点明确前，暂不继续扩大后处理参数搜索范围，避免把 QEMU KPU 写回问题误判为 YOLO 后处理问题。

完成这些后，StarryOS K230 NNCase runtime 路线就可以从“执行链路跑通”升级为“输出语义与官方 reference 对齐”。
