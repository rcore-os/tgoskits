# K230 KPU YOLOv8n 展示 Runbook

本文记录给老师现场展示时应使用的最短命令。默认环境是 Docker/Linux，默认容器名为 `k230-official-runtime`。

## 1. 一句话结论

当前现场展示有两条线：优先展示 StarryOS 原生 NNCase runtime，其次用 54 条 full-sequence replay 做稳定兜底。原生 runtime 已经能在 StarryOS QEMU K230 guest 内加载真实 `yolov8n_320.kmodel`，调用官方 NNCase runtime 生成 54 条 KPU command，通过 StarryOS `/dev/kpu` 提交，完成 done/IRQ，并让 QEMU KPU 从 Starry runtime alias 正常读取权重/RDATA、写回非零 Starry output tensor。

54 条 full-sequence replay 仍然重要：它复放 kunOS/K230 SDK RT-Smart YOLOv8n 展开后的完整 KPU workload，能稳定看到 `run=54/54`、IRQ 和 `KPU_SMOKE_PASS`。

现场首选命令：

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
bash test-suit/starryos/k230-qemu/qemu-k230/demo-teacher.sh
```

这条命令默认自动进入 `starryos-dev:ubuntu-qemu10.2.1` Docker 镜像，运行原生 NNCase runtime case，并把完整日志保存到：

```text
target/k230-kpu-demo/teacher-nncase-runtime.log
```

它会在终端只打印适合展示的证据摘要，例如：

```text
NNCASE_MINIMAL: load_model ok
NNCASE_MINIMAL: model io inputs=1 outputs=4
K230_SDK_COMPAT: mirrored runtime rdata 0x3c000020 -> 0x10000020 bytes=5242848
K230_SDK_COMPAT: gnne_enable run=54 ... status=0x0000000400000004
NNCASE_MINIMAL_PASS
YOLOV8N_DEMO: output[0] bytes=705600 fnv1a64=...
YOLOV8N_DEMO_PASS
K230_NNCASE_RUNTIME_PASS
```

如果现场还想展示 kunOS/RT-Smart 54 条 command replay 兜底，使用：

```sh
bash test-suit/starryos/k230-qemu/qemu-k230/demo-teacher.sh --with-replay
```

稳定 replay 命令：

```sh
cd /Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/demo-yolov8n-full-sequence-replay.sh
```

最新一次本地验证结果是：

```text
KPU_SMOKE: runtime_image_progress kunos_yolov8n_full_sequence_delta run=54/54 irq_count=54
KPU_SMOKE: runtime_image kunos_yolov8n_full_sequence_delta runs=54 status=0x0000000400000004 irq_count=0->54
KPU_SMOKE: real_kmodel path=/usr/share/k230-kpu-smoke/models/yolov8n_320.kmodel size=3493048 magic=LDMK version=6 hash=0x0585d1887f7dd46c
KPU_SMOKE_PASS
all starry k230-qemu qemu tests passed
demo replay passed
```

NNCase runtime 调试命令：

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
    export PATH=/workspace/target/qemu-k230-docker-build:/mnt/tgoskits/target/qemu-k230-docker-build:/opt/riscv64-linux-musl-cross/bin:/opt/x86_64-linux-musl-cross/bin:/opt/qemu-10.2.1/bin:$PATH
    cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-nncase-runtime
  '
```

当前 NNCase runtime 最关键证据行是：

```text
NNCASE_MINIMAL: load_model ok
NNCASE_MINIMAL: model io inputs=1 outputs=4
K230_SDK_COMPAT: mirrored runtime rdata 0x3c000020 -> 0x10000020 bytes=5242848
K230_SDK_COMPAT: gnne_enable raw=... mode=runtime-alias submit=...
K230_SDK_COMPAT: arg_table words=... 0x10000020 ...
K230_SDK_COMPAT: stats mmz_alloc=15 kpu_run=54
NNCASE_MINIMAL_PASS
YOLOV8N_DEMO_PASS
K230_NNCASE_RUNTIME_PASS
all starry k230-qemu qemu tests passed
```

## 2. 现场展示命令

### 2.1 原生 NNCase runtime 调试展示

如果现场时间允许，可以使用第 1 节的 Docker 命令展示“真实 `.kmodel -> NNCase -> 54 条 KPU command` 已经发生在 StarryOS guest 内”。展示时重点指出这些输出：

```text
NNCASE_MINIMAL: load_model ok
NNCASE_MINIMAL: input[0] datatype=6 shape=[1,3,320,320]
K230_SDK_COMPAT: identity mmap l2 0x80000000..0x80200000
K230_SDK_COMPAT: mirrored runtime rdata 0x3c000020 -> 0x10000020 bytes=5242848
K230_SDK_COMPAT: gnne_enable raw=... mode=runtime-alias submit=...
K230_SDK_COMPAT: arg_table words=... 0x10000020 ...
K230_SDK_COMPAT: gnne_enable run=54 ...
NNCASE_MINIMAL_PASS
YOLOV8N_DEMO_PASS
K230_NNCASE_RUNTIME_PASS
```

这组日志说明 `.kmodel` 加载、NNCase runtime command 生成、StarryOS `/dev/kpu` 提交、IRQ/done 等待、权重/RDATA 读取和 output tensor 写回已经发生在 StarryOS guest 内。

注意：当前图片 demo 的检测框语义尚未与官方 reference 对齐，本地日志仍是 `detections=0`。但这已经不是权重读取问题：四个 NNCase output tensor 均有非零 hash/stats，trace 中 StarryOS 两段 54-run 与官方 reference summary 匹配，之前 `0x102...`/`0x103...` 权重读取为零的问题已消失。展示时应强调“原生 runtime 核心链路、权重读取、output tensor 写回已跑通”，不要声称检测精度已经完成验证。

### 2.2 full-sequence replay 稳定展示

在宿主机或 Docker 内均可运行：

```sh
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/demo-yolov8n-full-sequence-replay.sh
```

如果从宿主机运行，脚本会优先进入正在运行的 `k230-official-runtime` 容器再执行。也可以显式写成：

```sh
docker exec -it k230-official-runtime bash -lc '
cd /mnt/tgoskits/target/worktrees/tgoskits-k230-upstream-dev &&
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/demo-yolov8n-full-sequence-replay.sh
'
```

展示时重点指出这些输出：

```text
KPU_SMOKE: optional_runtime_image selecting full_sequence_delta ...
KPU_SMOKE: runtime_image_progress ... run=54/54 irq_count=54
KPU_SMOKE: runtime_image kunos_yolov8n_full_sequence_delta runs=54 status=0x0000000400000004 irq_count=0->54
KPU_SMOKE: real_kmodel ... magic=LDMK version=6 ...
KPU_SMOKE_PASS
```

这组日志说明 StarryOS 已在 QEMU K230/KPU 下复放 kunOS/K230 SDK RT-Smart YOLOv8n 展开后的完整 54 条 KPU command，并完成 per-run hash、done status 和 IRQ 计数验证。

## 3. 展示前检查

展示前只需要确认三件事：

```sh
docker ps --format '{{.Names}}' | grep -x k230-official-runtime
test -f test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-full-sequence-delta.krun
test -f test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/kmodels/yolov8n_320.kmodel
```

如果第三项模型文件缺失，运行：

```sh
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/prepare-real-kmodel.sh
```

如果第一项 Docker 容器缺失，先启动既有实验容器或使用第 1 节的显式 `docker run` 命令。默认验收环境是 Docker/Linux。

## 4. 结果日志在哪里

replay 展示脚本会把完整输出保存到：

```text
target/k230-kpu-demo/yolov8n-full-sequence-replay.log
```

如果现场滚屏太快，直接看最后几行：

```sh
tail -n 30 target/k230-kpu-demo/yolov8n-full-sequence-replay.log
```

也可以只提取证据行：

```sh
grep -E 'KPU_SMOKE: (optional_runtime_image selecting full_sequence_delta|runtime_image_progress .*run=54/54|runtime_image .*full_sequence_delta.*runs=54|real_kmodel .*magic=LDMK)|KPU_SMOKE_PASS|all starry k230-qemu qemu tests passed|demo replay passed' \
  target/k230-kpu-demo/yolov8n-full-sequence-replay.log
```

原生 NNCase runtime case 当前直接由 `cargo xtask starry test qemu` 输出到终端。若要保存日志，现场可以包一层 `tee`：

```sh
cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-nncase-runtime | tee target/k230-kpu-demo/nncase-runtime-demo.log
```

关键证据行：

```sh
grep -E 'NNCASE_MINIMAL: (load_model ok|model io|output\\[0\\])|YOLOV8N_DEMO: output\\[0\\]|K230_SDK_COMPAT: (identity mmap l2|mirrored runtime rdata|gnne_enable raw|arg_table words|stats)|NNCASE_MINIMAL_PASS|YOLOV8N_DEMO_PASS|K230_NNCASE_RUNTIME_PASS' \
  target/k230-kpu-demo/nncase-runtime-demo.log
```

## 5. 脚本清单

| 脚本 | 用途 | 现场是否建议运行 |
| --- | --- | --- |
| `test-suit/starryos/k230-qemu/qemu-k230/demo-teacher.sh` | 老师现场展示入口；默认跑原生 NNCase runtime，并只打印关键证据摘要；`--with-replay` 可追加 54 条 full-sequence replay | 当前首选 |
| `cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-nncase-runtime` | StarryOS 原生 NNCase runtime 展示；加载真实 `.kmodel`，现场生成 54 条 command，验证权重读取和 output tensor 写回 | 当前优先 |
| `test-suit/starryos/k230-qemu/qemu-k230/kpu-nncase-runtime/c/tools/build-nncase-runtime-binaries.sh` | 用官方 K230 SDK Docker 镜像重新交叉编译 runtime demo 二进制 | 展示前准备，不建议现场临时运行 |
| `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/demo-yolov8n-full-sequence-replay.sh` | 一键 StarryOS replay 展示，自动提取关键证据 | 当前优先 |
| `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/prepare-real-kmodel.sh` | 下载/安装 `yolov8n_320.kmodel` 到 smoke assets | 只在模型缺失时运行 |
| `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/prepare-yolov8n-full-sequence-delta.sh` | 用已有 official trace/snapshot 重新生成 `.krun` | 不建议现场运行 |
| `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/prepare-yolov8n-full-sequence-delta.sh --capture` | 重新跑官方 kunOS/RT-Smart reference capture，再生成 `.krun` | 不建议现场运行 |
| `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/capture-kunos-yolov8n-full-series.py` | 只做官方 full-series pre-start snapshot capture | 不建议现场运行 |

## 6. 重新生成 full-sequence-delta capture

如果 `assets/captures/yolov8n-full-sequence-delta.krun` 已存在，不需要现场重新生成。重新生成完整素材可运行：

```sh
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/prepare-yolov8n-full-sequence-delta.sh
```

如果要从官方 kunOS/RT-Smart reference 重新 capture，再转换：

```sh
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/prepare-yolov8n-full-sequence-delta.sh --capture
```

`--capture` 会启动官方 K230 QEMU reference，设置 `K230_KPU_CAPTURE_DIR`，保存 54 组 pre-start low16m/L2/DDR snapshot，然后调用转换器生成：

```text
test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-full-sequence-delta.krun
```

注意：capture 生成物约 205 MiB，默认被 `.gitignore` 忽略，只作为本地展示资产保留。

## 7. 关键资产路径

| 资产 | 路径 |
| --- | --- |
| StarryOS worktree | `/Users/joshua/tmp/tgoskits/target/worktrees/tgoskits-k230-upstream-dev` |
| Docker 内 worktree | `/mnt/tgoskits/target/worktrees/tgoskits-k230-upstream-dev` |
| QEMU K230 binary | `/Users/joshua/tmp/tgoskits/target/qemu-k230-docker-build/qemu-system-riscv64` |
| replay `.krun` | `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-full-sequence-delta.krun` |
| real kmodel asset | `test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/kmodels/yolov8n_320.kmodel` |
| official full-series trace | `/Users/joshua/tmp/tgoskits/target/official-k230/kunos-yolov8n-full-series-kpu-trace.log` |
| official pre-start snapshots | `/Users/joshua/tmp/tgoskits/target/official-k230/yolov8n-prestart-snapshots` |
| NNCase runtime case | `test-suit/starryos/k230-qemu/qemu-k230/kpu-nncase-runtime` |
| NNCase runtime demo binaries | `test-suit/starryos/k230-qemu/qemu-k230/kpu-nncase-runtime/c/assets/bin` |

## 8. 常见问题

### Docker socket 不可访问

如果在受限沙箱里直接运行，可能看到 Docker socket permission denied。现场在普通终端里运行，或直接使用显式 `docker exec` 命令。

### 脚本找不到 Git 仓库

这类问题已处理：脚本不再依赖 `git rev-parse` 查找仓库根目录，而是从脚本路径向上查找 `Cargo.toml` 和 `test-suit`。这对 Docker 内 worktree 更稳。

### 没有安装 full-sequence-delta capture

CMake 现在以 `yolov8n-full-sequence-delta.krun` 作为安装 capture 目录的触发条件。若 guest 中没有出现：

```text
KPU_SMOKE: optional_runtime_image selecting full_sequence_delta ...
```

先检查本地是否存在：

```sh
ls test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/assets/captures/yolov8n-full-sequence-delta.krun
```

### 为什么不现场重新 capture

重新 capture 要启动官方 kunOS/RT-Smart reference、保存 54 组 low16m/L2/DDR snapshot，再转换 `.krun`，耗时更长且生成物大。现场展示只需要证明 StarryOS 能复放已经固化的 official runtime workload，因此直接跑 replay 脚本。

## 9. 当前边界

当前已经有两条展示线：

| 展示线 | 证明内容 | 边界 |
| --- | --- | --- |
| `kpu-nncase-runtime` | StarryOS guest 内加载真实 `.kmodel`，NNCase runtime 现场生成 54 条 KPU command 并通过 `/dev/kpu` 执行；QEMU KPU 已能读取权重/RDATA 并写回非零 Starry output tensor | 检测框语义尚未和官方 RT-Smart reference 对齐，当前 `detections=0`，direct output 区域仍全 0 |
| `demo-yolov8n-full-sequence-replay.sh` | StarryOS 复放 kunOS/RT-Smart YOLOv8n 展开后的完整 54 条 KPU command，per-run hash/IRQ 已验证 | command 生成不发生在 StarryOS guest 内 |

因此，报告中的准确表述应是：StarryOS 已经从 replay 阶段推进到原生 NNCase runtime 阶段，能在 StarryOS guest 中加载 `yolov8n_320.kmodel` 并完成 KPU command submit 闭环；后续 remaining gap 是输出语义与官方 reference 对齐，而不是 `.kmodel` 是否能加载或 runtime 是否能调用 KPU。
