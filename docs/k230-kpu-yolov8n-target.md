# K230 KPU YOLOv8n 对标目标和验证记录

本文记录 K230 KPU/NPU 适配中与 kunOS 对标目标相关的事实、验证命令、当前 StarryOS 差距和下一步计划。默认实验环境为 Docker/Linux。

zevorn 对 kunOS 状态的最新确认是：

```text
kunOS 目前是跑过 K230 SDK，K230 SDK 大核心 RT-Smart 的 yolov8n 跑过了。
```

这句话把我们的对标目标从“确认 QEMU K230/KPU 路线是否可行”收敛为更具体的目标：对齐 K230 SDK 在 big-core RT-Smart 侧运行 `object_detect_yolov8n` 的方式，然后把这条路线产生的真实 KPU runtime 材料转成 StarryOS 可复放的验证输入。

## 1. 当前结论

截至本轮验证，kunOS 对标目标已经在本地 QEMU K230 环境中复现到 KPU trace：

| 项目 | 结论 |
| --- | --- |
| kunOS 预构建 big-core 固件 | `target/upstreams/kunos/prebuilt/k230-sdk/riscv-nomtee/rtt_system.bin` 存在且可解析 |
| RT-Smart 启动入口 | ROMFS `init.sh` 进入 `/bin/object_detect_yolov8n` 并运行 `./ob_det.elf yolov8n_320.kmodel 0.15 0.2 bus.jpg 0` |
| QEMU 启动 | 使用 `-machine k230,boot-both-cores=on -smp 2` 后，big-core RT-Smart 能进入目标应用 |
| 应用入口 | 串口输出 `case ./ob_det.elf built at May 23 2026 00:33:22` |
| KPU trace | 已出现 `k230_kpu_start`、`k230_kpu_runtime_arg_table`、`k230_kpu_l2_load`、`k230_kpu_l2_store`、`k230_kpu_gnne_summary` |
| trace 文件 | `/Users/joshua/tmp/tgoskits/target/official-k230/kunos-yolov8n-kpu-trace.log` |
| trace 规模 | 2270 行、238846 字节 |
| KPU submit 次数 | `k230_kpu_start` 54 次，`k230_kpu_gnne_summary` 54 次 |
| QEMU KPU 指令未知项 | `gnne_summary` 里当前观察到的 `unknown=0`，未发现未知非零项 |
| debug=1 推理日志 | `/Users/joshua/tmp/tgoskits/target/official-k230/kunos-yolov8n-debug1-uart3.log` |
| 推理阶段结果 | `OBDet run`、`OBDet get_output`、`OBDet post_process` 均已完成 |
| official snapshot/capture | 官方 kunOS/RT-Smart YOLOv8n 已 capture 到 54 条 KPU command；当前稳定素材来自 `K230_KPU_CAPTURE_DIR` pre-start low16m/L2/DDR snapshot |
| StarryOS 复放 | 已用 `yolov8n-full-sequence-delta.krun` 在 StarryOS 中复放 54 条真实 KPU command，`kpu-smoke` 通过 |
| 当前边界 | 这是“官方 runtime 展开后的完整 KPU command 序列可复放”，还不是 StarryOS 原生 `.kmodel` runtime |

因此，主线 blocker 已经改变。此前手工注入或自编 runner 的 `Illegal Instruction` 只能说明那条旁路实验的 ELF/RT-Smart loader 组合有兼容问题，不能再作为“官方/kunOS YOLOv8n 路线没有跑到 KPU”的结论。

## 2. kunOS 对标目标的准确含义

这个对标目标不是 small-core Linux 用户态直接运行 RT-Smart ELF，也不是 PC simulator。准确层次如下：

| 维度 | 对标含义 |
| --- | --- |
| SDK | 使用 K230 SDK 官方 RT-Smart 应用、NNCase runtime、模型和输入组织方式 |
| CPU/OS | 运行在 K230 big-core RT-Smart，不是 Linux 小核用户态 |
| 模型 | `yolov8n_320.kmodel` |
| 应用 | `/bin/object_detect_yolov8n/ob_det.elf` |
| 输入 | `/bin/object_detect_yolov8n/bus.jpg` |
| KPU 侧 | 通过 QEMU `k230` machine 的 KPU 设备模型执行 command stream |
| 证据 | RT-Smart 应用启动日志 + QEMU KPU trace + 后续可提取的 output/reference |

这说明 StarryOS 当前不需要先原生解析 `.kmodel`。更合理的阶段性目标是：先把 kunOS/K230 SDK 路线已经能产生的 KPU runtime 行为捕获下来，再让 StarryOS 复放同一份 command/buffer/IRQ 材料。

## 3. kunOS `rtt_system.bin` 解析结果

解析 `target/upstreams/kunos/prebuilt/k230-sdk/riscv-nomtee/rtt_system.bin` 后得到：

| 文件/字段 | 结果 |
| --- | --- |
| K230 header | `K230`，payload hash 校验通过 |
| payload size | `0xbb06e3` |
| gzip ROMFS 解压后大小 | `0x16f2d68` |
| `init.sh` | 存在 |
| `ob_det.elf` | 存在于 `/bin/object_detect_yolov8n` |
| `yolov8n_320.kmodel` | 存在于 `/bin/object_detect_yolov8n` |
| `bus.jpg` | 存在于 `/bin/object_detect_yolov8n` |

`init.sh` 内容为：

```sh
#!/bin/sh
cd /bin/object_detect_yolov8n
./ob_det.elf yolov8n_320.kmodel 0.15 0.2 bus.jpg 0
```

这与 zevorn 描述的“跑过 K230 SDK 大核心 RT-Smart yolov8n”完全对齐：运行入口、模型和图片输入都在 RT-Smart ROMFS 中，不依赖小核 Linux 用户态启动该 ELF。

换成绝对路径表述，本轮 reference 的实际启动目标是：

```text
/bin/object_detect_yolov8n/ob_det.elf yolov8n_320.kmodel 0.15 0.2 bus.jpg
```

其中 `init.sh` 当前使用的最后一个参数是 `0`，debug 复验时仅把该参数 patch 为 `1`，用于让应用打印 `OBDet run/get_output/post_process` timing。

## 4. QEMU 复现步骤

将 kunOS 的完整 `rtt_system.bin` 写入官方 no-initapp SD 镜像的 `rtt` 分区：

| 产物 | 路径 |
| --- | --- |
| kunOS prebuilt RTT | `/Users/joshua/tmp/tgoskits/target/upstreams/kunos/prebuilt/k230-sdk/riscv-nomtee/rtt_system.bin` |
| 官方 no-initapp SD 镜像 | `/Users/joshua/tmp/tgoskits/target/official-k230/CanMV-K230_sdcard_v1.7_nncase_v2.9.0.no-initapp.img` |
| 注入 kunOS RTT 后的 SD 镜像 | `/Users/joshua/tmp/tgoskits/target/official-k230/CanMV-K230_sdcard_v1.7_kunos-yolov8n-rtt.img` |
| KPU trace | `/Users/joshua/tmp/tgoskits/target/official-k230/kunos-yolov8n-kpu-trace.log` |

```sh
cp target/official-k230/CanMV-K230_sdcard_v1.7_nncase_v2.9.0.no-initapp.img \
  target/official-k230/CanMV-K230_sdcard_v1.7_kunos-yolov8n-rtt.img

dd if=/dev/zero \
  of=target/official-k230/CanMV-K230_sdcard_v1.7_kunos-yolov8n-rtt.img \
  bs=1m seek=10 count=20 conv=notrunc

dd if=target/upstreams/kunos/prebuilt/k230-sdk/riscv-nomtee/rtt_system.bin \
  of=target/official-k230/CanMV-K230_sdcard_v1.7_kunos-yolov8n-rtt.img \
  bs=1m seek=10 conv=notrunc
```

运行 QEMU：

```sh
docker exec -i k230-official-runtime bash -lc '
cd /mnt/tgoskits &&
timeout 180s target/qemu-k230-docker-build/qemu-system-riscv64 \
  -machine k230,boot-both-cores=on \
  -smp 2 \
  -m 2G \
  -bios target/upstreams/kunos/prebuilt/k230-sdk/riscv-nomtee/u-boot \
  -drive if=sd,file=target/official-k230/CanMV-K230_sdcard_v1.7_kunos-yolov8n-rtt.img,format=raw,snapshot=on \
  -nic none \
  -display none \
  -trace enable=k230_kpu_start \
  -trace enable=k230_kpu_gnne_summary \
  -trace enable=k230_kpu_gnne_compute_summary \
  -trace enable=k230_kpu_runtime_arg_table \
  -trace enable=k230_kpu_l2_load \
  -trace enable=k230_kpu_l2_load_w \
  -trace enable=k230_kpu_l2_store \
  -trace enable=k230_kpu_l2_store_detail \
  -trace enable=k230_kpu_l2_store_hash \
  -trace file=target/official-k230/kunos-yolov8n-kpu-trace.log \
  -serial file:target/official-k230/kunos-yolov8n-uart0.log \
  -serial file:target/official-k230/kunos-yolov8n-uart1.log \
  -serial file:target/official-k230/kunos-yolov8n-uart2.log \
  -serial mon:stdio \
  -serial file:target/official-k230/kunos-yolov8n-uart4.log
'
```

关键串口输出：

```text
OpenSBI v0.9
...
RT-SMART Hello RISC-V.
msh /bin/object_detect_yolov8n>case ./ob_det.elf built at May 23 2026 00:33:22
```

关键 KPU trace 片段：

```text
k230_kpu_start k230-kpu start 0x10348b48 end 0x103548fc hi 0x0
k230_kpu_runtime_arg_table k230-kpu base 0x80000000 addr_words 2 words 0x10373020 0x1074f000 0x10000020 0x0
k230_kpu_l2_load k230-kpu source 0x10373020 logical 0x200 bytes 307200 head 0x0
k230_kpu_l2_store k230-kpu logical 0x1074f000 physical 0x1074f000 bytes 819200
k230_kpu_gnne_summary k230-kpu instructions 12825 l2_load 1 l2_load_w 19 l2_store 1 bytes 20385242 unknown 0
k230_kpu_gnne_compute_summary k230-kpu mfu_act1 5 mfu_pdp1 0 mfu_transpose 0 ai2d_compute 0 pu_compute 1256 pdp0_compute 0
```

本次运行使用 180 秒 timeout 结束。由于 `init.sh` 的 debug 参数为 `0`，应用不会打印完整 timing 或检测结果；但 KPU trace 已经证明 official/kunOS RT-Smart YOLOv8n 路线进入了 QEMU KPU command 执行路径。

## 5. debug=1 推理完成日志

为了确认不只是“进入 KPU trace”，还补做了一次 debug=1 运行。方法是只修改 kunOS `rtt_system.bin` 中的 `init.sh` 命令，把最后一个参数从 `0` 改为 `1`：

```sh
#!/bin/sh
cd /bin/object_detect_yolov8n
./ob_det.elf yolov8n_320.kmodel 0.15 0.2 bus.jpg 1
```

由此生成的实验产物：

| 产物 | 路径 |
| --- | --- |
| patched RTT | `/Users/joshua/tmp/tgoskits/target/official-k230/rtt_system_kunos_yolov8n_debug1.bin` |
| patched SD image | `/Users/joshua/tmp/tgoskits/target/official-k230/CanMV-K230_sdcard_v1.7_kunos-yolov8n-debug1.img` |
| RT-Smart app log | `/Users/joshua/tmp/tgoskits/target/official-k230/kunos-yolov8n-debug1-uart3.log` |

关键日志如下：

```text
RT-SMART Hello RISC-V.
msh /bin/object_detect_yolov8n>case ./ob_det.elf built at May 23 2026 00:33:22
OBDet set_input init took 2.24511 ms
OBDet set_output_init took 5.07222 ms
OBDet pre_process image took 64.7141 ms
OBDet run took 30442.4 ms
OBDet get_output took 0.517037 ms
OBDet post_process took 155.852 ms
```

这组日志说明 YOLOv8n 的模型输入初始化、输出初始化、图片预处理、KPU/NNCase 推理、输出读取和后处理都已经完成。该结果比 KPU trace 更强，因为它覆盖了官方 `ob_det.elf` 应用自身从启动到后处理结束的完整推理阶段。

debug=1 运行在后处理结束后，RT-Smart 的 `ipcm-discovery` 内核线程出现一次 `Store/AMO Access Fault`。该 fault 出现在 `OBDet post_process` 完成之后，应单独作为 RT-Smart/QEMU 系统侧后续问题记录；它不改变“YOLOv8n 推理和后处理已经跑完”的结论。

## 5.1 official snapshot 与 StarryOS full-sequence-delta replay

为了把 kunOS/RT-Smart reference 转成 StarryOS 可复放材料，本轮最终采用 `K230_KPU_CAPTURE_DIR` pre-start capture：QEMU 每次进入 `k230_kpu_start()` 后、真正执行 KPU command 前，保存 low16m、KPU L2 和 64 MiB DDR snapshot。这样每个 `run-NNNN-*` 都对应官方 RT-Smart runtime 已经为第 N 次 submit 准备好的内存状态。

| 产物 | 路径/结果 |
| --- | --- |
| capture 脚本 | `/Users/joshua/tmp/tgoskits/target/official-k230/capture_kunos_yolov8n_full_series.py` |
| trace events | `k230_kpu_start`、`k230_kpu_l2_store`、`k230_kpu_l2_store_hash`、`k230_kpu_gnne_summary` |
| full-series trace | `/Users/joshua/tmp/tgoskits/target/official-k230/kunos-yolov8n-full-series-kpu-trace.log` |
| pre-start snapshots | `/Users/joshua/tmp/tgoskits/target/official-k230/yolov8n-prestart-snapshots` |
| snapshot 数量 | 54 组，每组包含 `low16m`、`l2`、`ddr64m` |
| low16m capture base | `0x10000020`，不是 `0x10000000` |

转换器使用相邻 pre-start snapshot 做 run-level delta，把官方 runtime 在两次 KPU start 之间由 CPU 写入的内存变化显式写进 `.krun`。这比早期“初始 snapshot + 54 条 command”更接近官方运行状态，也避免了 last-command-only 复放缺少前序状态的问题。

生成的 StarryOS 复放材料为：

| 产物 | 路径 |
| --- | --- |
| `.krun` | `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-full-sequence-delta.krun` |
| capture JSON | `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-full-sequence-delta.capture.json` |
| guest capture dir | `/usr/share/k230-kpu-smoke/captures` |
| run 级命令 | 54 个 `run_file`，每次 run 前应用对应 delta |
| output check | per-run `run_check_hash`，最终 run 对齐官方 store hash |

StarryOS 验证命令：

```sh
docker exec k230-official-runtime bash -lc '
cd /mnt/tgoskits/target/worktrees/tgoskits-k230-upstream-dev &&
export PATH=/mnt/tgoskits/target/qemu-k230-docker-build:/opt/qemu-10.2.1/bin:/opt/riscv64-linux-musl-cross/bin:$PATH
cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-smoke
'
```

关键输出：

```text
KPU_SMOKE: runtime_image_progress name=kunos_yolov8n_full_sequence_delta run=54/54 irq_count=54
KPU_SMOKE: runtime_image kunos_yolov8n_full_sequence_delta runs=54 status=0x0000000400000004 irq_count=0->54
KPU_SMOKE: real_kmodel path=/usr/share/k230-kpu-smoke/models/yolov8n_320.kmodel size=3493048 magic=LDMK version=6 hash=0x0585d1887f7dd46c
KPU_SMOKE_PASS
```

这一步的准确意义是：StarryOS 已经能承载 kunOS/RT-Smart YOLOv8n 运行中真实产生的完整 54 次 KPU submit，并验证每次 command 在 QEMU KPU 模型中完成、触发 IRQ、写出与官方 trace 对齐的 output hash。它已经达到“官方 runtime 展开后的 KPU workload 可复放”的展示目标。

需要继续保留的边界是：这仍不是 StarryOS 原生 `.kmodel` runtime。`.kmodel` 解析、tensor metadata、command 生成和 CPU 侧后处理仍由 kunOS/K230 SDK RT-Smart reference 完成；StarryOS 当前复放的是 reference 展开后的 command、buffer 和 run-level delta。

## 6. 旁路实验与边界

在确认 kunOS 预构建 `rtt_system.bin` 之前，我们做过两类旁路实验：

1. 将本地 official SDK 编译出的 `ob_det.elf + yolov8n_320.kmodel + bus.jpg` 手工注入 no-initapp 镜像。
2. 编写并注入更小的 `kpu_yolov8n_minimal.elf`，只保留 NNCase interpreter、输入 tensor、`interp.run()` 和输出 hash。

这两条旁路都能进入 RT-Smart 用户态，但在应用早期 C/C++ 初始化或装载阶段触发 `Illegal Instruction`，尚未到 KPU trace。后续判断是：这更像本地自编 ELF 与该 RT-Smart loader/运行库组合的兼容问题，不能代表 kunOS 预构建 SDK 路线失败。

当前主线应该以 kunOS 自带 `rtt_system.bin` 为 reference，因为它是作者确认跑过的 SDK big-core RT-Smart yolov8n 固件，并且本地已经观察到 KPU trace。

## 7. StarryOS 当前支持与差距

StarryOS 当前已经完成的是 KPU 设备承载能力，不是完整 SDK runtime：

| 能力 | 当前状态 |
| --- | --- |
| QEMU K230 启动 | 已有 `k230-qemu` test group |
| KPU 设备发现 | 已通过 FDT/rdrive 发现 `canaan,k230-kpu` |
| `/dev/kpu` UAPI | 已支持 CFG/L2 mmap、command range、run、wait done、status、IRQ count |
| fake output smoke | 已验证 QEMU completion side effect |
| runtime scratch mmap | 已暴露 RDATA、command、direct I/O、DDR 等 QEMU-only window |
| runtime 风格 smoke | 已覆盖 runtime arg table direct I/O 和 DDR mirror 路径 |
| `.krun` 装载 | 已有 rootfs `.krun` 加载和 `capture-to-krun.py` 转换入口 |
| 真实模型资产 | 已准备 `yolov8n_320.kmodel` 及 sidecar |
| 真实 KPU command 复放 | 已复放 kunOS/RT-Smart YOLOv8n 完整 54 条 KPU command，并通过 per-run output hash 检查 |
| StarryOS 原生 NNCase runtime | 已新增并跑通 `kpu-nncase-runtime` case，minimal/image demo 可在 StarryOS guest 内加载 `yolov8n_320.kmodel`，由 NNCase `interpreter.run()` 现场生成并提交 54 条 KPU command；QEMU KPU 已能正常读取权重/RDATA 并写回非零 Starry output tensor |

与 kunOS reference 的差距如下：

| 差距 | 当前状态 |
| --- | --- |
| 完整 KPU trace 转 capture | 已完成 compact full-sequence-delta `.krun`，覆盖完整 54 条 command 和 run-level memory delta |
| 输入/输出 reference | 已完成 per-run store hash 验证；原生 runtime 已打印 output tensor paddr 和 output hash，但 final direct-io 区域仍全 0，尚未与 YOLO 检测语义对齐 |
| command stream 材料 | trace、pre-start snapshot、delta capture 已固化；后续重点是复现脚本化和体积管理 |
| StarryOS 复放 | 已复放完整 KPU command 序列；同时已跑通 StarryOS guest 内 `.kmodel` 加载和 NNCase runtime command 生成的 minimal 核心路径 |

因此，当前准确表述是：

```text
StarryOS 已经具备 QEMU K230/KPU 设备承载、runtime capture 复放和原生 NNCase runtime 能力；kunOS/K230 SDK big-core RT-Smart YOLOv8n reference 已经在本地 QEMU 中跑到 KPU trace；StarryOS 已经能复放这次 reference 的完整 54 条 KPU command，并且 `kpu-nncase-runtime` 已能在 StarryOS guest 内加载 `.kmodel`、调用 `interpreter.run()`、现场生成并提交 54 条 KPU command。本轮已经修正 RDATA/权重 mirror，QEMU KPU 能从 Starry runtime alias 正常读取权重并写回非零 output tensor。下一步是输出语义对齐：明确 NNCase output tensor、direct bbox/class buffer 和官方后处理之间的对应关系。
```

反过来说，当前可以写成“StarryOS 已经原生加载 `.kmodel` 并跑通 NNCase runtime 的执行链路”，也可以写成“`kpu-nncase-runtime` 在 QEMU K230 下已通过”；但还不能写成“YOLOv8n 检测语义已经完全对齐官方 RT-Smart”。最新诊断显示：StarryOS demo 已打印 output tensor paddr `0x3c3bf020`、`0x3c46c020`、`0x3c54e020`、`0x3c587020`；已读取 QEMU/kunOS reference final direct-io 区域 bbox `0x1059a900` bytes `33600`、class `0x105b0e20` bytes `672000`；direct output 仍全 0。因此剩余 gap 是 output tensor/direct output 语义定位，不是模型加载、54 command submit、权重读取或简单后处理阈值问题。

## 8. 下一步计划

下一阶段应按下面顺序推进：

1. 固化 kunOS reference 运行脚本、QEMU capture hook 使用方法、转换命令和 StarryOS smoke 命令，避免手工 `dd` 和长 QEMU 参数散落在记录中。
2. 压缩或分层管理 full-sequence-delta capture 资产，保证课程展示和后续复现不依赖不可控的大体积临时目录。
3. 在 debug=1 已确认推理完成的基础上，继续补一个 YOLO 输出 tensor hash、bbox 数量或检测结果摘要，形成更稳定的 RT-Smart reference。
4. 抓 StarryOS QEMU KPU trace，与 kunOS reference trace 逐条 diff，优先定位第一个 `l2_load`、`l2_store` 或 `l2_store_hash` 分叉点。
5. 对比 StarryOS replay output hash 与 kunOS/RT-Smart reference，形成展示版本的核心证据链。

报告中的验收口径建议写成：

```text
第一阶段展示目标不是 StarryOS 原生解析 .kmodel，而是 StarryOS 在 QEMU K230/KPU 下复放 K230 SDK big-core RT-Smart YOLOv8n 的真实 KPU command。当前已经完成完整 54 条 command 的 full-sequence-delta replay；下一步对标 kunOS 的主线是固化复现材料、补充输出语义摘要，并评估 StarryOS 原生 runtime 接 .kmodel 的可行性。
```
