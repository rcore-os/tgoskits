# K230 KPU RDIF Delta Capture 阶段报告

本文记录 2026-05-29 当前阶段的 StarryOS K230 KPU/NPU 适配路线更新。当前主线已经从“只复放一条或 54 条 KPU command”修正为：

```text
在官方 kunOS/K230 SDK RT-Smart YOLOv8n 运行中，捕获每次 KPU start 前的 low16m、L2、DDR 快照；
再把相邻 run 之间的内存变化转换成 StarryOS .krun 的 run-level delta；
最后由 StarryOS kpu-smoke 按 run 顺序自动应用 delta、提交 command、检查 hash。
```

这个阶段的目标仍然不是在 StarryOS 内原生解析 `.kmodel`，而是把官方 RT-Smart runtime 已经展开好的 KPU command 和 memory state 变成 StarryOS 可复放、可验证、可逐步收敛的素材。

最新结论已经从“run1/run2 发散，需要继续猜测 delta 缺口”推进为：

```text
已定位 QEMU pre-start low16m dump 的真实基址是 0x10000020，不是 0x10000000；
旧生成器按 0x10000000 解释 snapshot，导致 command stream 和各 low window 均错位 32 字节；
修复 LOW_CAPTURE_BASE 与 low_window_segments 映射后，compact full-sequence-delta .krun 已在 StarryOS kpu-smoke 中完整复放 54 条 KPU command 并通过 per-run hash 验证。
```

## 1. 为什么纯 54 条 command replay 不够

上一阶段已经证明两件事：

| 项目 | 结论 |
| --- | --- |
| 官方 reference | kunOS/K230 SDK big-core RT-Smart YOLOv8n 在 QEMU K230/KPU 中执行到 54 条 KPU command |
| StarryOS smoke | StarryOS 能复放最后一条真实 KPU command，`kunos_yolov8n_last_command` 走到 done/IRQ 并产生稳定 output hash |

但“按同一份初始 snapshot 依次提交 54 条 command”仍然不够。原因是 KPU command 不是独立纯函数：

1. 每次 KPU start 前，RT-Smart/NNCase runtime 可能由 CPU 侧改写 command buffer、arg table、RDATA、direct I/O buffer、DDR 权重/中间区。
2. 前一次 KPU completion 之后，CPU 可能读取 output、做后处理或为下一次 submit 准备新的参数区。
3. QEMU KPU/frontend 可能保留跨 submit 的 runtime shadow、RDATA alias、DDR mirror 或内部状态。
4. 同一条 command 的字节范围可以复用，但 command 周围的 memory context 可能已经被 runtime 改写。

因此，纯 54 command replay 只保证“命令顺序相同”，不能保证“每条命令启动前的世界状态相同”。如果只用一份 before-run 全量 snapshot，然后直接连续提交 54 条 command，StarryOS 侧会缺失 RT-Smart CPU 在两次 KPU start 之间写入的内存 delta。

## 2. 问题定位：32-byte low16m 基址错误

本阶段曾经把失败现象理解为：

```text
run1/run2 发散，可能说明 run 之间 CPU-side memory delta 仍未捕获完整。
```

后续复查 QEMU hook 和生成器后确认，这个判断需要修正。真正的首要问题不是 run2 才开始的 runtime delta 缺失，而是生成器对 low16m dump 的物理基址理解错了 32 字节。

| 项目 | 修正前 | 实际情况 |
| --- | --- |
| QEMU hook low16m dump 基址 | `0x10000000` | `0x10000020` |
| 生成器解释方式 | 按 `0x10000000` 计算 file offset 和 window segment | 应按 `0x10000020` 作为 dump 起点 |
| 直接后果 | command stream、RDATA、fake output、command window、direct I/O window 都可能错位 32 字节 | run1 即可失败或 hash 不一致 |
| 修复点 | 无 | 修复 `LOW_CAPTURE_BASE` 和 `low_window_segments` 映射 |

这次错误的关键经验是：capture 文件名叫 `low16m` 不等于它一定从 `0x10000000` 开始。QEMU hook 实际 dump 的范围以 `K230_GNNE_RUNTIME_RDATA_BASE` 为起点，而当前模型里该常量是：

```text
0x10000020
```

因此，snapshot 中的 file offset 和 guest physical address 的换算应该是：

```text
file_offset = guest_paddr - 0x10000020
```

而不是：

```text
file_offset = guest_paddr - 0x10000000
```

这 32 字节错位足以让 command stream 读取到错误的 command bytes，也会让 RDATA/direct I/O 等 window 的初始化偏离官方 RT-Smart pre-start 状态。

阶段判断：

```text
run1/run2 发散是有效的调试线索，但最终根因是 low16m capture base 错误；
修复 32-byte base 偏移后，54 条 command 的 per-run hash 已全部验证通过。
```

## 3. 新 `.krun` run-level delta 设计

为了解决 run 之间状态不一致的问题，`.krun` 从单 command/单 section 模式扩展为 run-level 模式。核心思想是：

1. 文件开头仍然有全局 `copy_file`/`fill`，用于建立 run1 前的初始状态。
2. 每个 `run_file` 表示一次 KPU submit。
3. 每个 `run_file` 后面可以跟随 `run_copy`、`run_copy_file`、`run_fill`，这些只在该 run 启动前应用。
4. 每个 run 可选 `run_check_hash`，用于在该次 KPU completion 后立即检查输出。
5. 全局 `check_hash` 仍可用于最终 output/hash。

### 3.1 旧模式边界

旧 `.krun` 适合这些场景：

```text
copy_file ...
command_file ...
check_hash ...
```

或：

```text
copy_file ...
run_file ...
run_file ...
check_hash ...
```

但它只能表达“初始状态 + 多个 command”。它不能表达 run2 前、run3 前、...、run54 前由 CPU runtime 写入的差分。

### 3.2 新增 run-level 指令

新增的 run-level 指令语义如下：

| 指令 | 作用 |
| --- | --- |
| `run_file <paddr> <path> <file_offset> <len>` | 定义一次 KPU submit 的 command stream |
| `run_copy <window> <offset> <bytes...>` | 在当前 run 启动前向指定 window 写入 inline bytes |
| `run_copy_file <window> <offset> <path> <file_offset> <len>` | 在当前 run 启动前从 snapshot/blob 拷贝一段 bytes |
| `run_fill <window> <offset> <len> <byte>` | 在当前 run 启动前填充指定内存范围 |
| `run_check_hash <window> <offset> <len> <hash>` | 当前 run completion 后立即检查 hash |

这些指令把“CPU 在两次 KPU start 之间做了什么”显式化。转换器可以用相邻 pre-start snapshot 做 diff，只把变化的块写成 `run_copy_file`；StarryOS smoke 不需要理解 `.kmodel` 或 runtime，只按 `.krun` 描述重建每次 submit 前的内存状态。

### 3.3 full-sequence-delta capture

`full-sequence-delta` capture 的生成逻辑是：

1. 解析官方 trace，得到 54 条 `k230_kpu_start` 的 command range。
2. 使用 `K230_KPU_CAPTURE_DIR` 生成的 `run-0001-*`、`run-0002-*` ... pre-start snapshot。
3. 以 `0x10000020` 作为 low16m capture base，正确计算 command/RDATA/fake output/command/direct I/O 的 file offset。
4. run1 使用全量初始 snapshot 初始化 low16m、L2、DDR。
5. 从 run2 开始，对相邻 snapshot 做块级 diff，生成每个 run 自己的 `sections`。
6. converter 输出 `.krun`：
   - 全局 `copy_file` 建立 run1 初始状态；
   - 每个 `run_file` 提交对应 command；
   - 每个 run 的 `run_copy_file` 应用启动前 delta；
   - `run_check_hash` 或最终 `check_hash` 验证结果。

这种设计比“全量 snapshot + 54 command”更接近官方 runtime，因为它让 StarryOS 在每次 KPU start 前看到与 RT-Smart reference 更接近的 memory view。修复 32-byte base 后，compact full-sequence-delta `.krun` 已经可以完整复放 54 次 KPU submit。

## 4. QEMU `K230_KPU_CAPTURE_DIR` pre-start snapshot hook

QEMU KPU 模型中增加了一个实验性 capture hook。它读取环境变量：

```text
K230_KPU_CAPTURE_DIR
```

当该变量非空时，QEMU 每次 `k230_kpu_start()` 记录 trace 后、真正执行 KPU command 前，保存三类 guest physical memory：

| 文件 | 范围 | 用途 |
| --- | --- | --- |
| `run-NNNN-low16m.bin` | `0x10000020..0x11000020` | RDATA、fake output、command window、direct I/O 等 runtime low window |
| `run-NNNN-l2.bin` | `0x80000000..0x80200000` | KPU L2 / arg table / GLB 相关状态 |
| `run-NNNN-ddr64m.bin` | `0x3c000000..0x40000000` | runtime DDR window、权重/中间 buffer/mirror 区 |
| `starts.jsonl` | 每行一个 start metadata | 记录 index、device、start、end、hi |

这个 hook 的位置很重要：它在 KPU start 前保存 snapshot，而不是在 completion 后保存。这样每个 `run-NNNN-*` 都代表官方 RT-Smart runtime 已经为第 N 次 submit 准备好的输入状态。

同样重要的是 hook 的 dump 基址：`run-NNNN-low16m.bin` 的第 0 字节对应 guest physical `0x10000020`。所有从 low16m snapshot 中截取 command bytes 或 runtime low window bytes 的工具，都必须用 `0x10000020` 做基准。

如果只保存 completion 后状态，转换器仍然需要推导 CPU 在 completion 到下一次 start 之间写了什么；而 pre-start snapshot 直接把推导问题变成相邻 snapshot diff：

```text
delta(run N) = pre_start_snapshot(run N) - pre_start_snapshot(run N - 1)
```

这也是 run2 发散后路线修正的核心：不再假设 CPU-side memory 不变，而是把 CPU-side memory delta 纳入 capture。

本阶段进一步补充的经验是：在讨论 delta 完整性之前，必须先保证每个 snapshot 的 guest physical base 与转换器一致。否则即使 delta 设计正确，也会因为 file offset 错位而在 run1 失败。

## 5. 当前验证结果

修复 `LOW_CAPTURE_BASE` 和 `low_window_segments` 映射后，重新生成 compact full-sequence-delta `.krun`，StarryOS `kpu-smoke` 已完成 54 条真实 KPU command 的完整复放。

关键结果：

| 项目 | 结果 |
| --- | --- |
| replay 形态 | compact full-sequence-delta `.krun` |
| KPU command 数量 | 54 条 |
| per-run 进度 | `run=54/54` |
| IRQ 计数 | `irq_count=54` |
| 最终 status | `0x0000000400000004` |
| smoke 结果 | `KPU_SMOKE_PASS` |

真实 `.kmodel` 文件也在 smoke 中被识别：

| 字段 | 值 |
| --- | --- |
| size | `3493048` |
| magic | `LDMK` |
| version | `6` |
| hash | `0x0585d1887f7dd46c` |

推荐在报告中使用的结论口径：

```text
StarryOS 已能加载官方 RT-Smart YOLOv8n 的 compact full-sequence-delta capture，
逐次复放 54 条真实 KPU command，并完成 per-run hash、done status 和 IRQ 计数验证。
这证明 StarryOS K230 KPU RDIF/UAPI 已经具备承载官方 runtime 展开后 KPU 工作负载的能力。
```

## 6. 当前工件与路径

已知相关工件如下：

| 类别 | 路径 |
| --- | --- |
| official full trace | `/Users/joshua/tmp/tgoskits/target/official-k230/kunos-yolov8n-full-series-kpu-trace.log` |
| prestart snapshots | `/Users/joshua/tmp/tgoskits/target/official-k230/yolov8n-prestart-snapshots` |
| old last-command snapshot low16m | `/Users/joshua/tmp/tgoskits/target/official-k230/kunos-yolov8n-post-low16m.bin` |
| old last-command snapshot L2 | `/Users/joshua/tmp/tgoskits/target/official-k230/kunos-yolov8n-post-l2.bin` |
| old last-command `.krun` | `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-last-command.krun` |
| old full-sequence `.krun` | `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-full-sequence.krun` |
| old full-sequence capture JSON | `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-full-sequence.capture.json` |
| new compact delta `.krun` | `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-full-sequence-delta.krun`，由修复后的 generator 重新生成 |
| new compact delta capture JSON | `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-full-sequence-delta.capture.json` |

注意：`yolov8n-full-sequence.krun` 属于“初始 snapshot + 54 command”的旧形态；真正通过完整 per-run hash 验证的是修复 low16m base 后生成的 compact `yolov8n-full-sequence-delta.krun` 形态。

## 7. 下一步验证路径

当前 54 条 command per-run hash 已经通过，下一步建议从“能跑通”推进到“可报告、可复现、可维护”：

1. 固化 QEMU hook、capture 命令、转换命令和 StarryOS smoke 命令，保证助教可以复现同一条链路。
2. 在文档和工具 metadata 中明确 `low16m` capture base 是 `0x10000020`，避免后续再按 `0x10000000` 解释。
3. 保存 compact full-sequence-delta capture 的 metadata：
   - command_count；
   - total_command_bytes；
   - store_count；
   - delta_block_size；
   - delta_section_count；
   - ddr_delta。
4. 保留 per-run hash 验证作为默认展示证据；如根文件系统体积过大，可在提交物中保留生成脚本和摘要，实际二进制 capture 作为本地实验产物。
5. StarryOS 原生 NNCase runtime 已经通过：minimal/image demo 都能在 guest 内加载 `.kmodel`，现场生成并提交 54 条 KPU command，等到 done/IRQ 并输出 hash。
6. 最新 RDATA mirror 修正确认 QEMU KPU 能从 Starry runtime alias 正常读取权重/RDATA，并写回非零 Starry output tensor；剩余 direct-io 诊断显示 reference bbox `0x1059a900` bytes `33600`、class `0x105b0e20` bytes `672000`，StarryOS 当前读取 stats 仍全 0，后续重点是 output tensor/direct output 语义对齐。

验收口径建议：

```text
StarryOS 在 QEMU K230/KPU 下加载官方 RT-Smart YOLOv8n 的 run-level delta capture，
逐次复放 54 次 KPU submit，并完成 per-run hash、IRQ 和 done status 验证。
```

## 8. 风险与边界

当前路线已经跑通，但仍有以下风险和边界需要在报告里说明：

| 风险 | 说明 | 应对 |
| --- | --- | --- |
| capture base 再次误解 | `low16m` 文件名容易让人误以为基址是 `0x10000000` | 文档、metadata、转换器常量统一写明 `0x10000020` |
| CPU-side delta 不完整 | 当前 low16m/L2/DDR 已足够通过 54 条 command；其他模型可能访问更多 window | 根据 trace 中 source/output 地址扩展 capture window |
| KPU/frontend 隐状态 | 当前 per-run hash 通过，说明 YOLOv8n 路径未遇到无法恢复的隐藏状态；其他模型仍可能触发 | 保留 per-run hash，失败时再补 QEMU state capture |
| delta 粒度过粗 | block diff 会生成较大 `.krun` 和 rootfs asset | 当前 compact delta 已可用；后续可引入二进制 delta index |
| DDR 体积大 | 54 组 64 MiB DDR snapshot 体积很大 | 本地验证保留全量；提交/展示时只保留可重生成脚本和必要小样本 |
| Delta replay 本身仍不是原生 `.kmodel` runtime | Delta replay 复放的是 runtime 展开后的执行材料；原生 runtime 路径已由 `kpu-nncase-runtime` 单独跑通 | 报告中区分 replay 与 runtime 两条线；输出语义对齐以后者的 tensor/direct-output 分析为主 |

阶段性结论：

```text
32-byte low16m base 错误已经定位并修复；
compact full-sequence-delta .krun 已在 StarryOS 中完整复放 54 条官方 KPU command；
per-run hash、IRQ count 和 done status 均通过，真实 kmodel asset 也已被 smoke 识别；
下一步重点是固化复现流程和报告证据，而不是继续证明 KPU command replay 是否可行。
```
