---
sidebar_position: 10
sidebar_label: "K230 NNCase Runtime"
---

# StarryOS K230 NNCase Runtime 运行链路

StarryOS K230 NNCase Runtime 路径用于在 K230 QEMU guest 内加载真实 `yolov8n_320.kmodel`，由 K230 SDK NNCase runtime 生成 KPU command stream，并通过 StarryOS `/dev/kpu` 提交给 QEMU K230 KPU model 执行。该路径验证的是模型加载、runtime command 生成、KPU 设备提交、done/IRQ 和 output tensor 读取的端到端行为。

应用入口位于 `apps/starry/k230-kpu-nncase`。K230 QEMU case 位于 `apps/starry/k230-qemu/qemu-k230/kpu-nncase-runtime`，复用同一份 app 源码、安装同一组 guest 文件，并运行同一个 `/usr/bin/k230-nncase-runtime-demo` wrapper。

## 1. 总体目标

runtime 主路径把模型加载、runtime command 生成、Starry 设备 ABI、QEMU KPU 模型和输出证据串成一条端到端链路。`kpu-smoke` 主要验证 `/dev/kpu` 的底层 ABI，`kpu-nncase-runtime` 验证官方 NNCase runtime 在 guest 内真实参与 command 生成。

```text
yolov8n_320.kmodel
  -> StarryOS 用户态 K230 SDK NNCase interpreter
  -> K230 SDK runtime 生成 KPU command stream
  -> compat shim 把 SDK 设备/MMZ 调用转接到 /dev/kpu
  -> StarryOS KpuDevice ioctl/mmap
  -> QEMU K230 KPU model
  -> done/IRQ、output tensor hash 和 stats
```

三条 K230 KPU 验证路径共享同一个底层设备模型，但它们给出的工程证据不同。路径差异决定失败排查应优先落在设备 ABI、捕获复放资产，还是 NNCase runtime 与 compat shim。

| 路径 | 主要证明对象 | command 来源 | 典型入口 |
| --- | --- | --- | --- |
| `kpu-smoke` | `/dev/kpu` ABI、寄存器、mmap、IRQ、简单 runtime window | C smoke case 或捕获的 `.krun` | `k230-qemu/qemu-k230/kpu-smoke` |
| `.krun` 复放 | 54 条已捕获 command 的稳定回放 | host 侧预生成资产 | `demo-teacher.sh --with-replay` 可选 |
| `kpu-nncase-runtime` | 官方 NNCase runtime 在 guest 内加载真实 `.kmodel` 并生成 command | guest 内 runtime 动态生成 | `k230-qemu/qemu-k230/kpu-nncase-runtime` 或 `k230-kpu-nncase` |

`K230_NNCASE_RUNTIME_PASS` 的含义是 runtime/device 路径已经执行到 KPU done 并产生可检查 output。它不表示 YOLO 后处理语义已经完全等同官方 RT-Smart demo；如果输出里出现 `detections=0`，应优先查看 tensor hash、stats 和 `gnne_enable run=54` 等 runtime 证据。

## 2. 代码与资源组成

K230 NNCase runtime 路径横跨 app、QEMU case、Starry devfs、portable KPU driver crate 和 K230 DTB。用户态 compat 层和 `/dev/kpu` ABI 必须和 `drivers/npu/k230-kpu`、`os/StarryOS/kernel/src/pseudofs/dev/kpu.rs` 保持一致。

### 2.1 源码入口

源码入口按功能分布，而不是按一个 crate 收敛。运行链路中的维护锚点覆盖 ABI、rootfs 安装、runtime 兼容层和 QEMU case 启动配置。

| 路径 | 职责 |
| --- | --- |
| `apps/starry/k230-kpu-nncase/README.md` | app 使用入口和简短运行说明 |
| `apps/starry/k230-kpu-nncase/prebuild.sh` | Starry app runner 的 rootfs overlay 安装脚本 |
| `apps/starry/k230-kpu-nncase/qemu-riscv64.toml` | app 直接运行时的 K230 QEMU 配置 |
| `apps/starry/k230-kpu-nncase/c/CMakeLists.txt` | SDK 资产检查、源码构建或预构建二进制安装 |
| `apps/starry/k230-kpu-nncase/c/src/k230_sdk_compat.*` | K230 SDK runtime 到 Starry `/dev/kpu` 的兼容层 |
| `apps/starry/k230-kpu-nncase/c/src/kpu_nncase_minimal.cc` | 最小 runtime 证明：load model、设置 tensor、run、hash output |
| `apps/starry/k230-kpu-nncase/c/src/k230_yolov8n_demo.cc` | 图像输入、YOLO 输出统计、简单后处理和 PPM 输出 |
| `apps/starry/k230-kpu-nncase/c/src/run-nncase-runtime-demo.sh` | guest 内顺序运行两个 demo 并打印总 PASS |
| `apps/starry/k230-qemu/qemu-k230/kpu-nncase-runtime/` | 迁移后的 K230 QEMU case wrapper |
| `drivers/npu/k230-kpu/src/lib.rs` | KPU 寄存器、ioctl、mmap offset 和 UAPI 常量 |
| `os/StarryOS/kernel/src/pseudofs/dev/kpu.rs` | Starry devfs `/dev/kpu` 和 `/dev/kpu0` 实现 |
| `os/StarryOS/configs/board/k230-canmv.dts` | K230 KPU FDT 节点和 runtime scratch reserved-memory |

这些文件共同定义 guest 设备可见性、用户态 command 提交流程、rootfs 安装内容和 K230 QEMU 启动入口。任何一处改变，都应同步检查 compat shim 常量、`KPU_MMAP_*` offset 和对应 QEMU 配置。

### 2.2 本地资产

K230 NNCase runtime 路径不把官方 SDK 大文件和预构建二进制提交进 Git。`CMakeLists.txt` 和 `prebuild.sh` 都按本地路径发现资产，缺少文件时会明确失败；这避免运行环境退化为没有真实 `.kmodel` 的空跑。

| 资产 | 默认来源 | 是否纳入 Git |
| --- | --- | --- |
| `yolov8n_320.kmodel` | `target/official-k230/k230-sdk-src/src/big/kmodel/ai_poc/kmodel/yolov8n_320.kmodel` | 否 |
| `bus.jpg` | `target/official-k230/k230-sdk-src/src/big/kmodel/ai_poc/images/bus.jpg` | 否 |
| NNCase 静态库和头文件 | `target/official-k230/k230-sdk-src/src/big/nncase/riscv64/` | 否 |
| K230 SDK C++ toolchain | `target/official-k230/k230-sdk-src/toolchain/` | 否 |
| guest demo 二进制 | `apps/starry/k230-kpu-nncase/c/assets/bin/` | 否 |

两个 guest demo 二进制固定放在 app-local ignored 目录下。这个目录是运行前缓存，不属于源码提交内容；本地环境可以用脚本重新生成，也可以放入可信来源的等价二进制。

```text
apps/starry/k230-kpu-nncase/c/assets/bin/
  kpu-nncase-minimal
  k230-yolov8n-demo
```

## 3. Starry 设备语义

K230 KPU 用户态接口由 `KpuDevice` 注册到 Starry devfs。启用 `k230-kpu` feature 时，StarryOS 在 `/dev` 下注册 `/dev/kpu` 和 `/dev/kpu0` 两个名字，它们指向同一个 `KpuDevice` 实例和同一个 character device id `240:1`。

### 3.1 设备探测

`KpuDevice::probe()` 从 FDT 查找 `compatible = "canaan,k230-kpu"` 的节点，映射 CFG MMIO，解析 L2 和 runtime scratch 区域，并尝试注册 IRQ。QEMU K230 DTB 把 CFG、L2、fake-output 和 runtime scratch 都结构化描述出来，避免用户态 compat 层通过任意 `/dev/mem` 访问物理地址。

```dts
kpu: kpu@80400000 {
    compatible = "canaan,k230-kpu";
    reg = <0x0 0x80400000 0x0 0x800>,
          <0x0 0x80000000 0x0 0x200000>;
    reg-names = "cfg", "l2";
    memory-region = <&kpu_fake_output>;
    canaan,qemu-runtime-rdata = <&kpu_runtime_rdata>;
    canaan,qemu-runtime-command = <&kpu_runtime_command>;
    canaan,qemu-runtime-direct-io = <&kpu_runtime_direct_io>;
    canaan,qemu-runtime-ddr = <&kpu_runtime_ddr>;
    interrupts = <189>;
    interrupt-parent = <&plic>;
    status = "okay";
};
```

这个 FDT 节点还决定 `KPU_INFO_F_FDT`、`KPU_INFO_F_IRQ_WAIT`、`KPU_INFO_F_FAKE_OUTPUT` 和 `KPU_INFO_F_RUNTIME_SCRATCH` 这些 `KpuInfo::flags` 位。`kpu-smoke` 和 runtime demo 都依赖这些 flags 判断 QEMU K230 设备能力是否完整。

### 3.2 ioctl 接口

UAPI 常量定义在 `drivers/npu/k230-kpu/src/lib.rs`，当前值是手写稳定值，不是 Linux `_IOC()` 宏编码。`os/StarryOS/kernel/src/pseudofs/dev/kpu.rs` 在 `DeviceOps::ioctl()` 中按这些值分派到 `Kpu::status()`、`Kpu::run_command()`、`Kpu::wait_done()` 和 `copy_to_user()` 等实现。

| ioctl | 值 | 参数 | 作用 |
| --- | ---: | --- | --- |
| `KPU_IOC_GET_STATUS` | `0x4b00` | `u64 *` | 读取 `STATUS_HI/STATUS_LO` 组合后的 64-bit 状态 |
| `KPU_IOC_CLEAR` | `0x4b01` | ignored | 写 `CONTROL_CLEAR` 清 done |
| `KPU_IOC_PROGRAM_COMMAND` | `0x4b02` | `CommandRange *` | 只写 command start/end/high 寄存器 |
| `KPU_IOC_START` | `0x4b03` | ignored | 写 `CONTROL_START` 启动 |
| `KPU_IOC_RUN` | `0x4b04` | `CommandRange *` | clear、program、start 三步组合 |
| `KPU_IOC_WAIT_DONE` | `0x4b05` | poll limit | 优先 IRQ wait，超时后轮询 done |
| `KPU_IOC_GET_INFO` | `0x4b06` | `KpuInfo *` | 返回 cfg/l2/irq/flags |
| `KPU_IOC_GET_IRQ_COUNT` | `0x4b07` | `u64 *` | 返回 IRQ handler 累计计数 |

`CommandRange` 是 16 字节 C ABI struct，compat shim 和 smoke case 都按这个布局传参。driver core 要求 command range 非空，且 start/end 必须落在同一个 4 GiB 窗口内；否则 `command_words()` 返回错误，Starry devfs 转成 invalid input。

```c
struct k230_kpu_command_range {
    uint64_t start_paddr;
    uint64_t end_paddr;
};
```

### 3.3 映射窗口

`/dev/kpu` 的 `mmap` 不允许任意物理地址。用户态只能通过固定 offset 映射 KPU CFG、L2 和 QEMU runtime scratch 区域；这些 offset 必须和 `k230_sdk_compat.h`、`drivers/npu/k230-kpu/src/lib.rs` 以及 `KpuDevice::mmap()` 同步维护。

| mmap offset | 物理起点 | 大小 | 用途 |
| ---: | ---: | ---: | --- |
| `0x0000` | `0x80400000` | `0x800` | CFG MMIO 寄存器 |
| `0x1000` | `0x80000000` | `0x200000` | KPU L2 / arg table |
| `0x2000` | `0x10090000` | `0x100000` | smoke fake-output |
| `0x3000` | `0x10000000` | `0x90000` | runtime rdata mirror |
| `0x4000` | `0x10190000` | `0x370000` | runtime copied command |
| `0x5000` | `0x10500000` | `0xb00000` | runtime direct input/output |
| `0x6000` | `0x3c000000` | `0x4000000` | runtime MMZ DDR bump allocator |

这些物理区间在 `k230-canmv.dts` 的 `reserved-memory` 中声明为 `no-map`。StarryOS 内核不会把它们当普通 RAM 交给页分配器；用户态通过 `/dev/kpu` 显式映射后，才把它们交给 SDK runtime 或 demo 程序使用。

## 4. SDK 兼容层

官方 K230 SDK NNCase runtime 假设运行环境里有一组 Kendryte/RT-Smart 风格接口，例如 `/dev/gnne_device`、`/dev/ai_2d_device`、`/dev/mem` 和 MMZ 分配 API。当前 Starry demo 没有完整实现 K230 MPP/RT-Smart 系统层，而是在 demo 二进制中链接 `k230_sdk_compat.cc` 作为窄兼容层。

### 4.1 链接拦截

`apps/starry/k230-kpu-nncase/c/CMakeLists.txt` 使用 linker wrap 把官方 SDK runtime 的系统调用入口导入 compat shim。这个做法让 demo 二进制仍然链接官方 NNCase runtime 静态库，同时把它对 K230 SDK 系统层的依赖收束到当前 demo 需要的最小集合。

```text
-Wl,--wrap=gnne_enable
-Wl,--wrap=open
-Wl,--wrap=openat
-Wl,--wrap=close
-Wl,--wrap=mmap
-Wl,--wrap=munmap
-Wl,--wrap=ioctl
```

这些 wrapper 只存在于 demo 二进制内部，不会改变 StarryOS 内核 ABI。内核侧仍然只暴露 `/dev/kpu`，不会伪造 `/dev/gnne_device`、`/dev/ai_2d_device` 或通用 `/dev/mem`。

### 4.2 兼容动作

compat 层的核心状态包括 fake fd、runtime window 表、MMZ bump allocation 表、mapping 表和 KPU run 计数。`translate_vaddr()`、`map_window()`、`kd_mpi_sys_get_virmem_info()` 与 `__wrap_gnne_enable()` 共同完成“SDK runtime 看到的地址”到“`/dev/kpu` 可提交物理窗口”的转换。

| SDK 行为 | compat 处理 |
| --- | --- |
| 打开 `/dev/gnne_device`、`/dev/ai_2d_device`、`/dev/mem` | 返回进程内 fake fd，不要求 StarryOS 真有这些节点 |
| `mmap` fake GNNE/AI2D fd | 映射 `/dev/kpu` CFG window |
| `mmap` fake `/dev/mem` | 按传入物理 offset 找 runtime window，再转成 `/dev/kpu` mmap |
| `kd_mpi_sys_mmz_alloc_cached()` | 在 `KPU_RUNTIME_DDR_PADDR` 对应 64 MiB window 内 bump 分配 |
| `kd_mpi_sys_get_virmem_info()` | 查询 compat 记录的 allocation/mapping，返回模拟物理地址 |
| `gnne_enable(pc_start, pc_end, ...)` | 解析或复制 command buffer，必要时 patch arg table，然后 `KPU_IOC_RUN` + `KPU_IOC_WAIT_DONE` |
| `kd_mpi_sys_mmz_flush_cache()` | 当前是 no-op，因为 QEMU/Starry demo 走共享映射和显式窗口 |

`k230_compat_init()` 当前只为 L2 做 identity mapping。runtime 执行时如果 command 位于已知 runtime window，compat 直接提交；如果 command 是普通用户虚拟地址，compat 会复制到 `KPU_RUNTIME_COMMAND_PADDR` window 后提交；如果 command 或 rdata 落在 runtime DDR 别名场景，则镜像到 KPU 可见的 runtime rdata 区域，并把 L2 arg table 中指向 `0x3c000020` 的 word patch 为 `0x10000020`。

### 4.3 运行证据

compat shim 在 stderr 打印的日志是判断“是否真的走到 KPU runtime”的关键证据。相比只看到 `load_model ok`，`gnne_enable run=54` 更能证明官方 NNCase runtime 已经生成并提交 command stream。

```text
K230_SDK_COMPAT: identity mmap l2 0x80000000..0x80200000
K230_SDK_COMPAT: mirrored runtime rdata 0x3c000020 -> 0x10000020
K230_SDK_COMPAT: gnne_enable run=54 ...
K230_SDK_COMPAT: stats mmz_alloc=... kpu_run=54
```

这组日志说明 NNCase runtime 不是只读取了模型文件，而是实际调用了 K230 SDK KPU 执行入口，并由 Starry `/dev/kpu` 完成了 54 次 command run。

## 5. Guest 程序

guest 内实际运行两个 RISC-V Linux musl ELF，再由一个 shell wrapper 汇总 PASS。二者都静态链接 K230 SDK NNCase runtime 和 `k230_sdk_compat.cc`，区别在于 minimal 只验证 runtime 输入输出，YOLO demo 还处理真实图片和输出统计。

### 5.1 最小程序

`kpu_nncase_minimal.cc` 是最小 runtime 证明程序。它通过 `k230_compat_init()` 初始化 `/dev/kpu` compat，读取 `yolov8n_320.kmodel`，创建 NNCase `interpreter`，设置 input/output tensor，然后执行 `interp.run()` 并对 output tensor 计算 FNV-1a 64-bit hash。

最小程序的执行序列固定，适合用来判断模型加载、tensor 设置和 KPU run 是否已经跨过 runtime 层。如果这个程序失败，通常先查 SDK 资产、compat shim 或 `/dev/kpu` 基础 ABI，而不是查 YOLO 后处理。

1. 调用 `k230_compat_init()` 初始化 `/dev/kpu` compat。
2. 从参数读取 `yolov8n_320.kmodel`。
3. 创建 NNCase `interpreter` 并 `load_model(model_span, true)`。
4. 根据模型 input/output 数量和 shape 创建 shared host runtime tensor。
5. 用确定性字节模式填充 input tensor，并执行 `hrt::sync(..., sync_write_back)`。
6. 调用 `interp.run()`。
7. 对每个 output tensor 计算 FNV-1a 64-bit hash。
8. 打印 `NNCASE_MINIMAL_PASS` 后用 `_Exit(0)` 退出。

使用 `_Exit(0)` 是当前 demo 的有意选择：源码注释说明官方 K230 SDK MMZ allocator 在 Starry/Linux ABI 的进程全局析构阶段可能 assert；runtime 已经在 PASS 打印前完成，因此 demo 直接终止进程，避免 teardown 噪声掩盖运行结果。

### 5.2 YOLO 程序

`k230_yolov8n_demo.cc` 在 minimal 之上增加 `bus.jpg` 解码、输入预处理、direct I/O window 检查、输出统计和简化 YOLO 后处理。它的核心维护锚点是 `decode_jpeg()`、`fill_input_from_image()`、`mirror_tensor_to_direct()`、`inspect_direct_yolo_output()` 和 `postprocess_yolov8()`。

YOLO 程序的后处理只是当前 demo 的可视化和统计入口。它用于证明 output tensor 可读取、可统计、可进入应用层解释；它不是官方 RT-Smart YOLO demo 的逐 bit 等价后处理。

1. 用 `libjpeg` 解码 `bus.jpg` 为 RGB。
2. 加载同一个 `yolov8n_320.kmodel`。
3. 根据 input tensor layout 判断 NCHW/NHWC，并做 bilinear resize。
4. 支持 `uint8`/`int8` 输入，或 `float32` 输入归一化到 `[0, 1]`。
5. 把 input tensor 复制到 `KPU_RUNTIME_DIRECT_SOURCE_PADDR` 方便 runtime/direct window 检查。
6. 运行 NNCase interpreter。
7. 打印 output tensor 的物理地址、字节数、FNV-1a hash 和 float stats。
8. 额外检查 direct I/O window 中 YOLO bbox/class 区域，打印 top score。
9. 对第一个 `float32` output 做当前简化后处理、NMS，并写 `/tmp/k230-yolov8n-demo.ppm`。
10. 打印 `YOLOV8N_DEMO_PASS` 后 `_Exit(0)`。

当前后处理阈值和输出 shape 假设写在源码常量里。调整模型或换官方后处理时，需要同步检查这些常量和 `inspect_direct_yolo_output()` 中的 direct window 地址。

| 常量 | 当前值 | 作用 |
| --- | ---: | --- |
| `kScoreThreshold` | `0.15` | 候选检测框分数阈值 |
| `kNmsThreshold` | `0.20` | 同类框 NMS IOU 阈值 |
| `kYoloRows` | `2100` | direct 输出中每类/box 的 row 数 |
| `kYoloClasses` | `80` | COCO 类别数 |

### 5.3 Guest Wrapper

安装到 guest 的 `/usr/bin/k230-nncase-runtime-demo` 是 shell wrapper。它固定从 `/usr/share/k230-nncase-runtime` 读取模型和图片，先运行 minimal，再运行 YOLO demo，最后打印总成功标记。

```sh
MODEL=/usr/share/k230-nncase-runtime/models/yolov8n_320.kmodel
IMAGE=/usr/share/k230-nncase-runtime/images/bus.jpg

/usr/bin/kpu-nncase-minimal "$MODEL"
/usr/bin/k230-yolov8n-demo "$MODEL" "$IMAGE"

echo "K230_NNCASE_RUNTIME_PASS"
```

wrapper 的执行顺序固定：两个 guest 程序都正常结束后才打印 `K230_NNCASE_RUNTIME_PASS`。K230 QEMU case 使用这个最终标记作为 shell 初始化命令完成信号。

## 6. SDK 资产准备

官方 K230 SDK 资产准备在仓库外的 `target/official-k230` 下完成。`apps/starry/k230-kpu-nncase/c/CMakeLists.txt` 会从当前 worktree 向上探测 `target/official-k230/k230-sdk-src`，也允许用 `K230_SDK_ROOT`、`K230_KMODEL` 和 `K230_BUS_JPG` 显式覆盖。

从 tgoskits 仓库根目录准备 SDK 源码。release tarball 也可以使用，但最终目录必须保持同样的 `k230-sdk-src` 布局。

```sh
mkdir -p target/official-k230
git clone https://github.com/kendryte/k230_sdk \
  target/official-k230/k230-sdk-src
```

SDK 自带的 `make prepare_sourcecode` 会下载 toolchain、NNCase 包、utils 包和 kmodel 包。当前 demo 使用 SDK 中面向 x86_64 host 的 RISC-V Linux musl toolchain，所以 Docker 命令中的 `--platform linux/amd64` 是有意保留的。

```sh
docker run --rm --platform linux/amd64 -u root \
  -v "$PWD/target/official-k230/k230-sdk-src":/k230_sdk \
  -v "$PWD/target/official-k230/k230-sdk-src/toolchain":/opt/toolchain \
  -w /k230_sdk \
  ghcr.io/kendryte/k230_sdk:latest \
  bash -lc 'make prepare_sourcecode'
```

准备完成后，下列文件或目录必须存在。缺少任意一项时，后续 CMake 或 `prebuild.sh` 会失败，而不是静默跳过 runtime demo。

```text
target/official-k230/k230-sdk-src/
  toolchain/riscv64-linux-musleabi_for_x86_64-pc-linux-gnu/bin/riscv64-unknown-linux-musl-g++
  src/big/nncase/riscv64/nncase/lib/libNncase.Runtime.Native.a
  src/big/nncase/riscv64/rvvlib/
  src/big/utils/lib/hhb-prebuilt-decode/
  src/big/kmodel/ai_poc/kmodel/yolov8n_320.kmodel
  src/big/kmodel/ai_poc/images/bus.jpg
```

本地构建环境可以通过 CMake 参数显式指定资产和 toolchain。这样可以复用已有 SDK 缓存，而不要求所有环境都把资产放在仓库默认 `target/official-k230` 下。

```text
-DK230_SDK_ROOT=/path/to/k230-sdk-src
-DK230_KMODEL=/path/to/yolov8n_320.kmodel
-DK230_BUS_JPG=/path/to/bus.jpg
-DK230_CXX=/path/to/riscv64-unknown-linux-musl-g++
-DK230_LINUX_MUSL_PREFIX=/path/to/riscv64-linux-musl-cross
```

## 7. 二进制构建

guest demo 二进制推荐由仓库内脚本生成。脚本会把 StarryOS dev image 中的 RISC-V Linux musl sysroot 复制到 Docker volume，再进入 Kendryte SDK container 构建两个静态链接的 guest ELF。

```sh
bash apps/starry/k230-kpu-nncase/c/tools/build-nncase-runtime-binaries.sh
```

该脚本的构建步骤和输出位置由 `apps/starry/k230-kpu-nncase/c/tools/build-nncase-runtime-binaries.sh` 固定，主要用于保证 CMake、SDK toolchain 和 Linux musl sysroot 在不同机器上能找到同一套输入。

1. 从当前 worktree 向上查找 `target/official-k230/k230-sdk-src`。
2. 使用 `starryos-dev:ubuntu-qemu11.1.1` 把 `/opt/riscv64-linux-musl-cross` 复制到 Docker volume `tgoskits-riscv64-linux-musl-cross`。
3. 进入 `ghcr.io/kendryte/k230_sdk:latest` amd64 容器。
4. 调用 CMake，并显式设置 SDK C++ compiler 与 Linux musl sysroot。
5. 静态链接 K230 SDK NNCase runtime、`rvv`、JPEG decode、K230 SDK C++ runtime、Linux musl libc、libgcc、libatomic 等库。
6. 把生成的 RISC-V Linux musl guest ELF 安装到 app-local ignored 目录。

构建结果固定写入 app-local ignored 目录。Starry app runner 默认使用这些预构建文件，不会在每次运行 QEMU 前重新从源码构建。

```text
apps/starry/k230-kpu-nncase/c/assets/bin/
  kpu-nncase-minimal
  k230-yolov8n-demo
```

如果本地环境已经有可信来源构建出的等价二进制，也可以直接把这两个文件放到同一 ignored 目录。这样仍然会走同一个 rootfs overlay 安装和 QEMU runtime 运行路径。

## 8. Rootfs 安装

`apps/starry/k230-kpu-nncase/prebuild.sh` 是 app runner 进入 QEMU 前调用的安装脚本。它要求 `STARRY_OVERLAY_DIR` 已设置，并把两个 guest ELF、一个 shell wrapper、模型和图片复制到 rootfs overlay。

```text
/usr/bin/kpu-nncase-minimal
/usr/bin/k230-yolov8n-demo
/usr/bin/k230-nncase-runtime-demo
/usr/share/k230-nncase-runtime/models/yolov8n_320.kmodel
/usr/share/k230-nncase-runtime/images/bus.jpg
```

`prebuild.sh` 默认从 SDK 和 app-local cache 寻找资产，也支持环境变量覆盖。这个表格用于排查“构建了二进制但 QEMU 内找不到文件”这类问题。

| 资产 | 默认来源 | 可覆盖环境变量 |
| --- | --- | --- |
| SDK root | `target/official-k230/k230-sdk-src` | `K230_SDK_ROOT` |
| kmodel | `${K230_SDK_ROOT}/src/big/kmodel/ai_poc/kmodel/yolov8n_320.kmodel` | `K230_KMODEL` |
| image | `${K230_SDK_ROOT}/src/big/kmodel/ai_poc/images/bus.jpg` | `K230_BUS_JPG` |
| guest binaries | `apps/starry/k230-kpu-nncase/c/assets/bin` | `K230_PREBUILT_DIR` |

缺少任意文件时，脚本会失败并给出对应 hint。这个失败是有意的，因为缺少模型、图片或 guest ELF 时继续启动 QEMU 只会得到误导性的 runtime 失败。

## 9. K230 QEMU

K230 machine 和 KPU model 不在常规 upstream QEMU 10.1、10.2 或 11.0 中。普通系统包里的 `qemu-system-riscv64` 通常会报 `unsupported machine type: "k230"`，所以 K230 runtime demo 必须先准备 `apps/starry/k230-qemu/README.md` 中记录的 QEMU fork。

```sh
bash apps/starry/k230-qemu/prepare-k230-qemu.sh
```

脚本会构建 riscv64-softmmu，并验证 `qemu-system-riscv64 -machine help` 中存在 `k230`。构建产物留在 `target/qemu-k230-docker-build`，QEMU case 还会使用其中的 `pc-bios` 目录。

```text
target/qemu-k230-docker-build/
  qemu-system-riscv64
  pc-bios/
```

运行 K230 case 时，需要把该目录放在默认 QEMU 路径之前。否则 `cargo xtask` 可能拾取系统 QEMU，最终在 machine 解析阶段失败。

```sh
PATH="$PWD/target/qemu-k230-docker-build:$PATH" \
  cargo xtask starry app qemu -t k230-qemu/qemu-k230/kpu-nncase-runtime --arch riscv64
```

## 10. 运行入口

当前有三个入口会落到同一个 guest wrapper：K230 QEMU case、app 入口和日志演示脚本。它们共享相同的 `/usr/bin/k230-nncase-runtime-demo`，区别主要在 rootfs overlay 准备和日志展示方式。

### 10.1 迁移后 Case

迁移后的 case 位于 `apps/starry/k230-qemu/qemu-k230/kpu-nncase-runtime`，适合和 K230 boot、`kpu-smoke` 放在同一组里做 QEMU 回归。它的 `qemu-riscv64.toml` 明确使用 K230 machine、K230 DTB、SD rootfs 和 300 秒 timeout。

```sh
PATH="$PWD/target/qemu-k230-docker-build:$PATH" \
  cargo xtask starry app qemu -t k230-qemu/qemu-k230/kpu-nncase-runtime --arch riscv64
```

该 case 的关键参数集中在 `apps/starry/k230-qemu/qemu-k230/kpu-nncase-runtime/qemu-riscv64.toml`。改 QEMU 参数时应同步检查 app 直接入口，因为两者当前表达同一条 runtime 路径。

```text
-machine k230
-smp 2
-m 2G
-dtb os/StarryOS/configs/board/k230-canmv.dtb
-drive if=sd,format=raw,file=tmp/axbuild/rootfs/rootfs-riscv64-alpine.img
shell_init_cmd = "/usr/bin/k230-nncase-runtime-demo"
timeout = 300
```

### 10.2 App 入口

`apps/starry/k230-kpu-nncase` 是直接 app 入口。资产准备、prebuild 安装和 QEMU 配置都跟 app 目录放在一起，适合本地运行同一条 NNCase runtime 路径。

```sh
PATH="$PWD/target/qemu-k230-docker-build:$PATH" \
  cargo xtask starry app qemu -t k230-kpu-nncase --arch riscv64
```

`apps/starry/k230-kpu-nncase/qemu-riscv64.toml` 当前与迁移后的 K230 case 使用同一类 QEMU 参数和同一个 guest wrapper。两边如果出现 success/fail regex 或 timeout 差异，应明确说明原因。

### 10.3 演示脚本

`apps/starry/k230-kpu-nncase/demo-teacher.sh` 提供流式日志演示入口。脚本默认会在需要时进入 `starryos-dev:ubuntu-qemu11.1.1` Docker 环境，流式打印完整 QEMU/Cargo 输出，并保存 `target/k230-kpu-demo/teacher-nncase-runtime.log`。

```sh
bash apps/starry/k230-kpu-nncase/demo-teacher.sh
```

迁移后的 K230 app-QEMU wrapper 路径只是转发到同一个日志演示脚本。该路径为 K230 QEMU case group 保留稳定的演示入口。

```sh
bash apps/starry/k230-qemu/qemu-k230/demo-teacher.sh
```

演示脚本还提供两个运行模式参数。`--with-replay` 会在 runtime 路径后额外运行本地 `.krun` full-sequence replay fallback，`--no-docker` 则要求当前环境自己满足 QEMU、toolchain 和运行依赖。

| 参数 | 行为 |
| --- | --- |
| `--with-replay` | runtime demo 后额外运行本地 `.krun` full-sequence replay fallback |
| `--no-docker` | 不自动进入 Docker，直接使用当前环境 |

## 11. 故障定位

故障定位应先区分 host QEMU、资产准备、rootfs 安装、Starry `/dev/kpu` 和 NNCase runtime 这几层。常见失败现象可以映射到固定的第一排查点，避免直接从长日志里猜测。

| 现象 | 常见原因 | 处理 |
| --- | --- | --- |
| `unsupported machine type: "k230"` | PATH 中使用了系统 QEMU | 先运行 `prepare-k230-qemu.sh`，并把 `target/qemu-k230-docker-build` 放到 PATH 前面 |
| `missing ... kpu-nncase-minimal` | guest demo 二进制未构建 | 运行 `apps/starry/k230-kpu-nncase/c/tools/build-nncase-runtime-binaries.sh` |
| `missing yolov8n_320.kmodel` 或 `missing bus.jpg` | SDK 资产未准备，或路径不在默认位置 | 运行 SDK `make prepare_sourcecode`，或设置 `K230_KMODEL` / `K230_BUS_JPG` |
| `cannot initialize /dev/kpu compat` | Starry guest 未注册 `/dev/kpu`，或 QEMU/DTB/feature 不匹配 | 检查是否使用 K230 QEMU case、`k230-canmv.dtb` 和启用 `k230-kpu` feature |
| `KPU_IOC_WAIT_DONE failed` | KPU command 未产生 done、IRQ/poll 路径异常或 QEMU model 不匹配 | 先跑 `kpu-smoke`，确认 `/dev/kpu` 基础 ABI 和 IRQ |
| 只看到 `detections=0` | 后处理语义未对齐 | 看 output hash/stats 和 `K230_NNCASE_RUNTIME_PASS` 判断 runtime/device 路径 |

如果 `kpu-nncase-runtime` 失败但 `kpu-smoke` 通过，优先查看 `K230_SDK_COMPAT:` 日志和 SDK 资产路径；如果 `kpu-smoke` 也失败，先回到底层 `/dev/kpu`、FDT runtime scratch 和 K230 QEMU fork。
