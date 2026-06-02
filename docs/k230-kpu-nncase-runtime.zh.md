# StarryOS K230 NNCase Runtime Demo

本文记录 StarryOS K230 QEMU 路径上的 KPU/NPU runtime demo。该 demo
有意和底层 `/dev/kpu` smoke case 分开：`kpu-smoke` 证明设备接口可用，
`kpu-nncase-runtime` 证明真实 `.kmodel` 可以在 StarryOS guest 内由官方
NNCase runtime 加载并执行。

## 目标

runtime 路径如下：

```text
yolov8n_320.kmodel
  -> StarryOS 用户态内的 K230 NNCase interpreter
  -> runtime 在 guest 内生成 KPU command stream
  -> StarryOS /dev/kpu ioctl/mmap
  -> QEMU K230 KPU model
  -> done/IRQ 和 output tensor hash
```

这里和 `.krun` 复放的关键区别是：KPU command 不是预先展开后写死的，
而是由 NNCase runtime 在 StarryOS guest 内根据真实 `.kmodel` 生成。
`.krun` 复放仍然保留为稳定 fallback 和 54 条 command workload 的对照，
但本 demo 的主要证据是 runtime 原生路径。

## 本地资产

本 PR 不提交大型或第三方资产：

- `yolov8n_320.kmodel`
- `bus.jpg`
- K230 SDK NNCase 静态库和头文件
- K230 SDK C++ toolchain
- StarryOS guest 内运行的预构建 demo 二进制

这些资产来自官方 Kendryte K230 SDK，并准备在未纳入 git 跟踪的
`target/official-k230` 目录下。

### 1. 准备官方 K230 SDK

在 tgoskits 仓库根目录执行：

```sh
mkdir -p target/official-k230
git clone https://github.com/kendryte/k230_sdk \
  target/official-k230/k230-sdk-src
```

官方 SDK README 也提供 release tarball mirror。如果 GitHub clone 速度较慢，
可以下载 release tarball，并解压到同一个目录：

```text
target/official-k230/k230-sdk-src/
```

然后使用 SDK 自带的 `make prepare_sourcecode` 下载 toolchain、NNCase 包、
utils 包和 kmodel 包：

```sh
docker run --rm --platform linux/amd64 -u root \
  -v "$PWD/target/official-k230/k230-sdk-src":/k230_sdk \
  -v "$PWD/target/official-k230/k230-sdk-src/toolchain":/opt/toolchain \
  -w /k230_sdk \
  ghcr.io/kendryte/k230_sdk:latest \
  bash -lc 'make prepare_sourcecode'
```

这里的 `--platform linux/amd64` 是有意保留的：本 demo 使用的是 K230 SDK
中面向 x86_64 host 的 RISC-V Linux musl toolchain。

`make prepare_sourcecode` 完成后，下面这些文件必须存在：

```text
target/official-k230/k230-sdk-src/
  toolchain/riscv64-linux-musleabi_for_x86_64-pc-linux-gnu/bin/riscv64-unknown-linux-musl-g++
  src/big/nncase/riscv64/nncase/lib/libNncase.Runtime.Native.a
  src/big/nncase/riscv64/rvvlib/
  src/big/utils/lib/hhb-prebuilt-decode/
  src/big/kmodel/ai_poc/kmodel/yolov8n_320.kmodel
  src/big/kmodel/ai_poc/images/bus.jpg
```

demo CMake 也支持从 `kpu-smoke/c/assets/kmodels/yolov8n_320.kmodel`
读取模型作为 fallback，但可复现的推荐来源仍然是上面的官方 SDK 路径。

### 2. 构建 StarryOS guest demo 二进制

在 tgoskits 仓库根目录执行：

```sh
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-nncase-runtime/c/tools/build-nncase-runtime-binaries.sh
```

该脚本会：

- 从当前 worktree 向上查找 `target/official-k230/k230-sdk-src`；
- 把 StarryOS dev image 内的 `/opt/riscv64-linux-musl-cross` sysroot
  复制到名为 `tgoskits-riscv64-linux-musl-cross` 的 Docker volume；
- 进入 `ghcr.io/kendryte/k230_sdk:latest` amd64 容器；
- 链接 K230 SDK NNCase runtime、RVV library、JPEG decode library、
  K230 SDK C++ runtime 和 Linux musl sysroot；
- 在下面的 ignored 目录中生成两个 RISC-V demo 二进制：

```text
test-suit/starryos/k230-qemu/qemu-k230/kpu-nncase-runtime/c/assets/bin/
  kpu-nncase-minimal
  k230-yolov8n-demo
```

如果 reviewer 已经有可信来源构建出的等价二进制，也可以手动把这两个文件放到
同一个 ignored 目录。test case 会直接使用它们，而不重新从源码构建。

### 3. `cargo xtask` 会安装什么到 guest

运行 `kpu-nncase-runtime` case 时，CMake 会把下面的文件安装到 guest rootfs
overlay：

```text
/usr/bin/kpu-nncase-minimal
/usr/bin/k230-yolov8n-demo
/usr/bin/k230-nncase-runtime-demo
/usr/share/k230-nncase-runtime/models/yolov8n_320.kmodel
/usr/share/k230-nncase-runtime/images/bus.jpg
```

模型和图片来自第 1 步列出的 SDK 路径。两个 demo 二进制来自
`c/assets/bin/`，除非当前环境本身就是 amd64 K230 SDK 构建环境并显式传入
`-DK230_CXX=...`。

### 4. 准备 K230 QEMU

runtime case 还需要带 K230 machine/KPU model 的 QEMU fork。该 QEMU 的详细
说明在 `test-suit/starryos/k230-qemu/README.md` 中。准备命令是：

```sh
bash test-suit/starryos/k230-qemu/prepare-k230-qemu.sh
```

运行 case 时，需要把该 QEMU build 目录放在默认 QEMU 路径之前：

```sh
PATH="$PWD/target/qemu-k230-docker-build:$PATH" \
  cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-nncase-runtime
```

## 验证

运行 QEMU case：

```sh
cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-nncase-runtime
```

预期关键证据包括：

```text
NNCASE_MINIMAL: load_model ok
NNCASE_MINIMAL: model io inputs=1 outputs=4
K230_SDK_COMPAT: identity mmap l2 0x80000000..0x80200000
K230_SDK_COMPAT: mirrored runtime rdata 0x3c000020 -> 0x10000020
K230_SDK_COMPAT: gnne_enable run=54
NNCASE_MINIMAL: interp.run done
NNCASE_MINIMAL_PASS
YOLOV8N_DEMO_PASS
K230_NNCASE_RUNTIME_PASS
all starry k230-qemu qemu tests passed
```

面向课堂展示的终端 demo 可以使用：

```sh
bash test-suit/starryos/k230-qemu/qemu-k230/demo-teacher.sh
```

该脚本会把完整 QEMU/Cargo 输出实时打印到终端，同时保存到
`target/k230-kpu-demo/teacher-nncase-runtime.log`。case 结束后，它还会打印
一段简短证据摘要，方便现场讲解。

## 当前边界

已完成：

- K230 runtime demo 二进制可以在 StarryOS guest 内运行。
- 真实 `yolov8n_320.kmodel` 可以由 NNCase runtime 加载。
- runtime 会生成 KPU command，并通过 `/dev/kpu` 提交。
- StarryOS 可以观察到 KPU done/IRQ，以及非零 output tensor hash/stats。

本 PR 未完成：

- YOLO 检测框语义与官方 RT-Smart reference 的完整后处理对齐。
- Camera/VICAP/VO 或完整 K230 MPP 集成。
- 提交真实 `.kmodel`、SDK 静态库或预构建 demo 二进制。

如果 demo 输出 `detections=0`，这并不表示 KPU 没有运行。它表示剩余工作在
应用层 YOLO output interpretation 和 postprocess 对齐；runtime/device 路径已经
完成执行。
