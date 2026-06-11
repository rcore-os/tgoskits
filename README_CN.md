<h1 align="center">TGOSKits</h1>

<p align="center">面向操作系统与虚拟化研发的一体化 Rust 工作区</p>

<div align="center">

[![Build & Test](https://github.com/rcore-os/tgoskits/actions/workflows/ci.yml/badge.svg)](https://github.com/rcore-os/tgoskits/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

## 1. 简介

TGOSKits 是一个面向操作系统与虚拟化开发的集成仓库，汇聚 ArceOS、StarryOS、Axvisor 以及共享组件、平台适配和驱动生态。仓库通过统一的 `cargo xtask` 入口组织构建、运行、调试和测试流程，适合进行组件级开发、跨系统联调和系统级验证。

项目网站：[https://rcore-os.cn/tgoskits/](https://rcore-os.cn/tgoskits/)。如果想先理解项目定位和系统关系，请从 [TGOSKits 文档](https://rcore-os.cn/tgoskits/docs/introduction) 开始。

## 2. 仓库

TGOSKits 仓库通过 Git Subtree 汇入多个独立子项目，并在根目录提供统一的构建、运行、测试和文档入口。主要目录如下：

```text
tgoskits/
├── components/                # 可复用组件 crate
├── os/
│   ├── arceos/                # ArceOS 模块化内核
│   ├── StarryOS/              # StarryOS Linux 兼容 OS
│   └── axvisor/               # Axvisor Type-I Hypervisor
├── platform/                  # 平台与板卡适配 crate
├── drivers/                   # 可复用驱动与驱动子系统
├── test-suit/                 # 系统级测试用例
├── xtask/                     # 根目录统一命令入口
├── scripts/                   # 仓库维护、测试和同步脚本
└── docs/                      # Docusaurus 文档站点
```

更多关于 subtree 同步、组件分层和开发约定的说明，请参考 [仓库结构与协作方式](https://rcore-os.cn/tgoskits/docs/contributing/repo) 和 [组件开发指南](https://rcore-os.cn/tgoskits/docs/development/components)。

## 3. 快速体验

### 3.1 环境配置

首次体验推荐使用项目容器镜像。镜像内已经包含 Rust 工具链、QEMU 和常用交叉编译依赖，与 CI 环境保持一致：

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits

docker pull ghcr.io/rcore-os/tgoskits-container:latest
docker run -it --rm \
  -v "$(pwd)":/workspace \
  -w /workspace \
  ghcr.io/rcore-os/tgoskits-container:latest
```

如果不使用容器，请至少准备 Rust、基础构建工具和常用 QEMU。推荐使用与容器和 CI 一致的 QEMU 10.2.1；发行版自带 QEMU 可用于快速体验，但如果遇到架构缺失或运行差异，请优先切换到容器环境：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sudo apt update
sudo apt install -y cmake make ninja-build pkg-config
sudo apt install -y qemu-system-arm qemu-system-riscv64 qemu-system-x86
cargo install cargo-binutils
```

完整环境说明见 [快速开始总览](https://rcore-os.cn/tgoskits/docs/quickstart/overview) 和 [CI 与容器镜像](https://rcore-os.cn/tgoskits/docs/build/ci)。

### 3.2 QEMU 验证

先确认常用 QEMU 命令可用，并尽量与容器和 CI 使用的 QEMU 10.2.1 对齐：

```bash
qemu-system-riscv64 --version
qemu-system-aarch64 --version
qemu-system-x86_64 --version
qemu-system-loongarch64 --version
```

随后可以用统一的 `cargo xtask` 入口快速运行三个系统路径：

```bash
# ArceOS: 运行 Hello World
cargo xtask arceos qemu --package arceos-helloworld --arch aarch64

# StarryOS: 首次运行前准备 rootfs
cargo xtask starry rootfs --arch aarch64
cargo xtask starry qemu --arch aarch64

# Axvisor: 运行 Hypervisor QEMU 场景
cargo xtask axvisor qemu --arch aarch64
```

如果只想先跑通一个最短路径，建议从 ArceOS Hello World 开始。更多系统、架构组合和 QEMU 参数说明见 [快速开始总览](https://rcore-os.cn/tgoskits/docs/quickstart/overview) 和 [运行与 QEMU](https://rcore-os.cn/tgoskits/docs/build/run)。

## 4. 贡献

欢迎通过 Issue 和 Pull Request 参与 TGOSKits。推荐流程：

1. 阅读 [仓库结构与协作方式](https://rcore-os.cn/tgoskits/docs/contributing/repo)。
2. 基于 `dev` 创建功能分支。
3. 完成开发后运行相关 `cargo xtask` 构建、测试或 clippy 检查。
4. 提交 PR，并在描述中说明变更范围、验证方式和影响面。

完整开发示例、文档贡献和 rootfs 维护说明见 [贡献文档](https://rcore-os.cn/tgoskits/docs/contributing/demo)。问题反馈和补丁提交请使用 [GitHub Issues](https://github.com/rcore-os/tgoskits/issues) 与 [GitHub Pull Requests](https://github.com/rcore-os/tgoskits/pulls)。

## 5. 许可证

TGOSKits 仓库整体采用 [Apache-2.0](./LICENSE) 许可证。部分 subtree 组件可能包含自己的许可证文件；如有差异，以组件目录中的许可证文件为准。
