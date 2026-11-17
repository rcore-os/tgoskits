# CI 容器环境

`Dockerfile` 提供 ArceOS、StarryOS、AxVisor 及宿主检查使用的构建环境，`Dockerfile.axvisor-lvz` 在其上增加固定提交的 LoongArch LVZ 模拟器。完整发布条件见[容器发布说明](../docs/docs/ci/publishing/containers.md)。

## 1. 镜像准备

固定宿主依赖在 Dockerfile 构建时准备，源码依赖和客户机根文件系统仍由当前提交的构建配置决定。

### 1.1 工具链

基础镜像包含 Ubuntu 24.04、QEMU 11.1.1、四种架构的 musl C/C++ 交叉编译器、用户态静态 QEMU，以及 `rust-toolchain.toml` 指定的 Rust 工具链。额外安装四种架构的 musl Rust targets，供应用辅助程序使用。

Node 24.11.1 自带 npm 11，宿主 JDK 17 编译 Java 测试资产，`container/install-go.sh` 提供经 SHA256 校验的 Go 1.26.3。eBPF 使用 `bpf-linker 0.13.0`；HDL 的 Verilator、Icarus Verilog、Yosys 和 Bluespec 版本由 `apps/starry/hdl-lang/setup-hdl-toolchain.sh` 统一维护。

### 1.2 宿主依赖

文件系统与固件工具包括 `debugfs`、`fakeroot`、`sfdisk`、`mkimage`、`dtc` 和 `mtools`。镜像还提供 LLD、Git LFS、Python pip、压缩工具及音视频工具，应用预构建阶段不需要补装这些宿主软件。

容器任务要求 `TGOSKITS_PREBUILT_CONTAINER=1`。缺少宿主工具、Rust target 或 `bpf-linker` 时，预构建脚本返回错误并要求修复镜像，不尝试调用包管理器。直接运行在裸机上的任务保留原有准备路径；Node 的 `setup-node` 也仅用于裸机任务，容器使用预装版本。

`npm ci`、Nix devShell、客机根文件系统内的 `apk` 准备和客机安装测试继续使用当前源码或目标系统的依赖。它们不属于固定宿主环境，不预制到镜像中。

## 2. 本地使用

Docker build context 为仓库根目录。`BUILD_JOBS` 控制 QEMU 和 HDL 源码构建的并行数，默认 4。

### 2.1 构建

在仓库根目录执行基础镜像构建，再用同一镜像作为 LVZ 的父镜像。

```bash
docker build -t tgoskits-ci:base -f container/Dockerfile .
docker build -t tgoskits-ci:axvisor-lvz -f container/Dockerfile.axvisor-lvz \
  --build-arg BASE_IMAGE=tgoskits-ci:base .
```

### 2.2 检查与运行

`check-env.sh` 在构建末尾执行工具检查和四架构 C/Rust 编译运行探针。可以断网重跑，确认固定环境完整。

```bash
docker run --rm --network none tgoskits-ci:base \
  bash /usr/local/lib/tgoskits/check-env.sh
```

挂载源码后，通过项目任务入口运行对应目标；源码依赖下载仍需要网络。

```bash
docker run --rm -it -v "$PWD:/workspace" -w /workspace tgoskits-ci:base \
  cargo xtask starry app qemu -t ebpf/upb --arch x86_64
```
