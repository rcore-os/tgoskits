---
sidebar_position: 5
sidebar_label: "StarryOS Test-Suit"
---

# StarryOS test-suit 设计

## 1. StarryOS

StarryOS 测试分为**普通测试**（`normal/`）和**压力测试**（`stress/`）两组，每组下每个子目录代表一个独立的测试用例。用例可以无源码（仅平台配置文件），也可以包含 C 或 Rust 源码（分别放在 `c/` 或 `rust/` 子目录中）。目录名即测试用例名，由 xtask 自动扫描发现。

```text
test-suit/starryos/
├── normal/                               # 普通测试用例
│   ├── smoke/                            # 无源码用例：仅平台配置文件
│   │   ├── qemu-aarch64.toml
│   │   ├── qemu-riscv64.toml
│   │   ├── qemu-loongarch64.toml
│   │   ├── qemu-x86_64.toml
│   │   └── board-orangepi-5-plus.toml
│   ├── my_c_test/                        # 含 C 源码用例
│   │   ├── c/                            # C 源码目录
│   │   │   └── main.c
│   │   ├── qemu-x86_64.toml             # 平台配置与 c/ 同级
│   │   └── qemu-aarch64.toml
│   └── my_rust_test/                     # 含 Rust 源码用例
│       ├── rust/                         # Rust 源码目录
│       │   ├── Cargo.toml
│       │   └── src/
│       │       └── main.rs
│       ├── qemu-x86_64.toml             # 平台配置与 rust/ 同级
│       └── qemu-riscv64.toml
└── stress/                               # 压力测试用例
    └── stress-ng-0/
        ├── qemu-aarch64.toml
        ├── qemu-riscv64.toml
        ├── qemu-loongarch64.toml
        └── qemu-x86_64.toml
```

### 1.1 测试分组

| 分组 | 路径 | 说明 | 运行命令 |
|------|------|------|----------|
| normal | `test-suit/starryos/normal/` | 普通功能测试 | `cargo xtask starry test qemu --target <arch>` |
| stress | `test-suit/starryos/stress/` | 压力/负载测试 | `cargo xtask starry test qemu --target <arch> --stress` |

## 2. C 测试用例

### 2.1 目录结构

```text
test-suit/starryos/normal/
└── my_c_test/                        # 含 C 源码用例
    ├── c/                            # C 源码目录
    │   ├── CMakeLists.txt            # CMake 构建脚本（必需）
    │   ├── main.c                    # C 入口文件
    │   └── ...                       # 其他 C 源文件或头文件
    ├── qemu-x86_64.toml             # QEMU 测试配置与 c/ 同级
    ├── qemu-aarch64.toml
    └── board-orangepi-5-plus.toml   # 板级测试配置（可选）
```

### 2.2 测例源码

| 文件/目录 | 必需 | 说明 |
|-----------|------|------|
| `c/` | 是（C 测试） | C 源码目录，包含所有 `.c`、`.h` 文件和 CMake 脚本 |
| `c/CMakeLists.txt` | 是 | CMake 构建脚本，定义目标架构的交叉编译规则 |
| `c/main.c` | 是 | C 入口文件，包含 `main()` 函数 |
| `c/*.c` | 是 | 其他 C 源文件 |

### 2.3 QEMU 测试配置

`qemu-{arch}.toml` QEMU 测试配置，放在用例根目录下（与 `c/` 同级），定义 QEMU 启动参数、Shell 交互行为以及测试结果判定规则。

**示例** — `normal/smoke/qemu-x86_64.toml`：

```toml
args = [
    "-nographic",
    "-device",
    "virtio-blk-pci,drive=disk0",
    "-drive",
    "id=disk0,if=none,format=raw,file=${workspace}/target/x86_64-unknown-none/rootfs-x86_64.img",
    "-device",
    "virtio-net-pci,netdev=net0",
    "-netdev",
    "user,id=net0",
]
uefi = false
to_bin = false
shell_prefix = "root@starry:"
shell_init_cmd = "pwd && echo 'All tests passed!'"
success_regex = ["(?m)^All tests passed!\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b']
timeout = 15
```

**示例** — `stress/stress-ng-0/qemu-x86_64.toml`：

```toml
args = [
    "-nographic",
    "-device",
    "virtio-blk-pci,drive=disk0",
    "-drive",
    "id=disk0,if=none,format=raw,file=${workspace}/target/x86_64-unknown-none/rootfs-x86_64.img",
    "-device",
    "virtio-net-pci,netdev=net0",
    "-netdev",
    "user,id=net0",
]
uefi = false
to_bin = false
shell_prefix = "starry:~#"
shell_init_cmd = '''
apk update && \
apk add stress-ng && \
stress-ng --cpu 8 --timeout 10s && \
stress-ng --sigsegv 8 --sigsegv-ops 1000    && \
pwd && ls -al && echo 'All tests passed!'
'''
success_regex = ["(?m)^All tests passed!\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b', '(m)^stress-ng: info: .*failed: [1-9]\d*\s*$']
timeout = 50
```

**字段说明：**

| 字段 | 类型 | 必需 | 默认值 | 说明 |
|------|------|------|--------|------|
| `args` | `[String]` | 是 | — | QEMU 命令行参数，支持 `${workspace}` 占位符 |
| `uefi` | `bool` | 否 | `false` | 是否使用 UEFI 启动 |
| `to_bin` | `bool` | 否 | `false` | 是否将 ELF 转换为 raw binary |
| `shell_prefix` | `String` | 否 | — | Shell 提示符前缀，用于检测 shell 就绪 |
| `shell_init_cmd` | `String` | 否 | — | Shell 就绪后执行的命令，支持多行 `'''` |
| `success_regex` | `[String]` | 是 | — | 成功判定正则列表，任一匹配即判定成功 |
| `fail_regex` | `[String]` | 否 | `[]` | 失败判定正则列表，任一匹配即判定失败 |
| `timeout` | `u64` | 否 | — | 超时秒数 |

### 2.4 板级测试配置

`board-{board_name}.toml` 板级测试配置，放在用例根目录下（与 `c/` 同级），用于物理开发板上的测试，通过串口交互判定结果。与 QEMU 配置相比没有 `args`、`uefi`、`to_bin` 字段，但增加了 `board_type` 标识板型。

**示例** — `normal/smoke/board-orangepi-5-plus.toml`：

```toml
board_type = "OrangePi-5-Plus"
shell_prefix = "root@starry:/root #"
shell_init_cmd = "pwd && echo 'test pass'"
success_regex = ["(?m)^test pass\\s*$"]
fail_regex = []
timeout = 300
```

**字段说明：**

| 字段 | 类型 | 必需 | 说明 |
|------|------|------|------|
| `board_type` | `String` | 是 | 板型标识，需对应 `os/StarryOS/configs/board/{board_name}.toml` |
| `shell_prefix` | `String` | 是 | Shell 提示符前缀 |
| `shell_init_cmd` | `String` | 是 | Shell 就绪后执行的命令 |
| `success_regex` | `[String]` | 是 | 成功判定正则列表 |
| `fail_regex` | `[String]` | 否 | 失败判定正则列表 |
| `timeout` | `u64` | 是 | 超时秒数，物理板通常需要更长时间（如 300s） |

### 2.5 执行 QEMU 测试

#### 2.5.1 命令行参数

```text
cargo xtask starry test qemu --target <arch> [--stress] [--test-case <case>]
```

| 参数 | 说明 |
|------|------|
| `--target` / `-t` | 目标架构或完整 target triple（如 `aarch64`、`riscv64`、`x86_64`、`loongarch64`，或 `aarch64-unknown-none-softfloat`、`riscv64gc-unknown-none-elf`） |
| `--stress` | 运行 stress 组测试，缺省运行 normal 组 |
| `--test-case` / `-c` | 仅运行指定用例 |

#### 2.5.2 发现机制

xtask 扫描 `test-suit/starryos/{normal|stress}/` 下所有子目录，检查其中是否存在 `qemu-{arch}.toml` 文件。若存在，则将该子目录名作为用例名，并将该 TOML 文件作为 QEMU 运行配置加载。

```text
发现路径: test-suit/starryos/<group>/<case-name>/qemu-<arch>.toml
```

例如，对于架构 `aarch64`：

- `test-suit/starryos/normal/smoke/qemu-aarch64.toml` → 用例名 `smoke`
- `test-suit/starryos/stress/stress-ng-0/qemu-aarch64.toml` → 用例名 `stress-ng-0`

#### 2.5.3 构建

xtask 定位用例目录中的 `c/CMakeLists.txt`，配置交叉编译工具链（根据目标架构选择对应的 sysroot 和 compiler），然后执行 `cmake --build` 编译 C 程序。

CMake 脚本需要满足以下要求：

- 使用 `cmake_minimum_required()` 指定最低版本
- 通过 `project()` 声明项目名称和语言
- 定义可执行目标，将所有 `.c` 源文件加入编译
- 使用交叉编译工具链（xtask 会通过 `CMAKE_TOOLCHAIN_FILE` 传入）

**示例** — `c/CMakeLists.txt`：

```cmake
cmake_minimum_required(VERSION 3.20)
project(my_c_test C)

add_executable(my_c_test main.c)
```

源码要求：

- 入口函数为标准 `int main(void)` 或 `int main(int argc, char *argv[])`
- 可引用标准 C 库头文件（`<stdio.h>`、`<stdlib.h>`、`<string.h>` 等）
- 可引用 POSIX 头文件（`<pthread.h>`、`<unistd.h>`、`<sys/socket.h>` 等）
- 所有 `.c` 和 `.h` 文件放在 `c/` 目录下

#### 2.5.4 rootfs 准备与注入

rootfs 镜像是 StarryOS 测试的基础运行环境，提供完整的 Linux 用户态文件系统（含 shell、apk 包管理器等）。xtask 在测试运行前自动下载 rootfs，并将编译产物注入其中。

**1. 下载 rootfs**

xtask 根据目标架构选择对应的 rootfs 镜像，检查本地是否已存在。若不存在，自动从远程仓库下载压缩包并解压：

```text
下载地址: https://github.com/Starry-OS/rootfs/releases/download/20260214/rootfs-{arch}.img.xz
存放路径: {workspace}/target/{target}/rootfs-{arch}.img
```

各架构对应的 rootfs 文件：

| 架构 | rootfs 文件 | 存放路径 |
|------|------------|----------|
| `x86_64` | `rootfs-x86_64.img` | `target/x86_64-unknown-none/` |
| `aarch64` | `rootfs-aarch64.img` | `target/aarch64-unknown-none-softfloat/` |
| `riscv64` | `rootfs-riscv64.img` | `target/riscv64gc-unknown-none-elf/` |
| `loongarch64` | `rootfs-loongarch64.img` | `target/loongarch64-unknown-none-softfloat/` |

下载流程：

1. 检查 `{target}/rootfs-{arch}.img` 是否存在
2. 若不存在，下载 `rootfs-{arch}.img.xz` 到 `{target}/` 目录
3. 解压 `.xz` 文件得到 `.img` 镜像
4. 删除 `.xz` 压缩包

也可通过命令手动下载：

```text
cargo xtask starry rootfs --arch <arch>
```

**2. 注入编译产物**

对于含 C/Rust 源码的测试用例，xtask 将编译产物注入到对应架构的 rootfs 镜像中，使其在系统启动后可直接通过 shell 执行。

**3. 配置 QEMU 磁盘参数**

xtask 自动将 rootfs 镜像路径注入到 QEMU 的 `-drive` 参数中，替换 TOML 配置里的 `${workspace}` 占位符。如果配置中没有声明磁盘设备参数，xtask 会自动添加默认的 `virtio-blk-pci` 和 `virtio-net-pci` 设备。

#### 2.5.5 执行测例

1. 加载 `qemu-{arch}.toml` 配置，构造 QEMU 启动命令
2. 启动 QEMU，开始捕获串口输出
3. 若设置了 `shell_prefix`，等待该前缀出现后发送 `shell_init_cmd`
4. 每收到新输出时，先检查 `fail_regex`（任一匹配 → 失败），再检查 `success_regex`（任一匹配 → 成功）
5. 超时未判定 → 失败

### 2.6 执行开发板测试

#### 2.6.1 命令行参数

```text
cargo xtask starry test board [--test-group <group>] [--board-test-config <path>] [--board-type <type>] [--server <addr>] [--port <port>]
```

| 参数 | 说明 |
|------|------|
| `--test-group` / `-t` | 指定测试组名（如 `smoke-orangepi-5-plus`） |
| `--board-test-config` | 指定板级测试配置文件路径；当前要求与 `--test-group` 一起使用 |
| `--board-type` / `-b` | 指定板型（如 `OrangePi-5-Plus`） |
| `--server` | 串口服务器地址 |
| `--port` | 串口服务器端口 |

#### 2.6.2 发现机制

xtask 扫描 `test-suit/starryos/normal/` 下所有子目录，检查其中是否存在 `board-{board_name}.toml` 文件。若存在，进一步验证对应的构建配置 `os/StarryOS/configs/board/{board_name}.toml` 是否存在，从中提取架构和 target 信息。

```text
测试配置:   test-suit/starryos/normal/<case>/board-<board_name>.toml
构建配置:   os/StarryOS/configs/board/<board_name>.toml
```

#### 2.6.3 构建

与 QEMU 测试相同，xtask 使用 CMake 交叉编译 C 程序。

#### 2.6.4 rootfs 准备与注入

与 QEMU 测试相同，详见[第 2.5.4 节 rootfs 准备与注入](#254-rootfs-准备与注入)。

#### 2.6.5 执行测例

1. 加载 `board-{board_name}.toml` 配置，通过串口服务器连接物理板
2. 等待 `shell_prefix` 出现后发送 `shell_init_cmd`
3. 检查 `fail_regex` 和 `success_regex` 判定结果
4. 超时未判定 → 失败

### 2.7 新增测试用例

**新增普通测试：**

1. 在 `test-suit/starryos/normal/` 下创建用例目录（如 `my_c_feature/`）
2. 创建 `c/` 子目录，放入 `CMakeLists.txt` 和 `.c` 源文件
3. 为每个支持的架构创建 `qemu-{arch}.toml`
4. 如需在物理板上测试，创建 `board-{board_name}.toml`

**新增压力测试：**

1. 在 `test-suit/starryos/stress/` 下创建用例目录
2. 创建 `c/` 子目录，放入 `CMakeLists.txt` 和 `.c` 源文件
3. 为每个支持的架构创建 `qemu-{arch}.toml`
4. 压力测试通常使用更长的 `timeout` 和更复杂的 `shell_init_cmd`

## 3. Rust 测试用例

### 3.1 目录结构

```text
test-suit/starryos/normal/
└── my_rust_test/                     # 含 Rust 源码用例
    ├── rust/                         # Rust 源码目录（标准 Cargo 项目）
    │   ├── Cargo.toml                # 包定义
    │   └── src/
    │       └── main.rs               # 入口源码
    ├── qemu-x86_64.toml             # QEMU 测试配置与 rust/ 同级
    └── qemu-riscv64.toml
```

### 3.2 测例源码

| 文件/目录 | 必需 | 说明 |
|-----------|------|------|
| `rust/` | 是（Rust 测试） | Rust 源码目录，标准 Cargo 项目结构 |
| `rust/Cargo.toml` | 是 | 包定义文件 |
| `rust/src/main.rs` | 是 | 入口源码文件 |
| `rust/src/*.rs` | 是 | 其他源码文件 |

源码要求：

- 入口函数为标准 `fn main()`
- 可使用 `#![no_std]` 和 `#![no_main]` 配合自定义入口（视 OS 支持而定）
- `Cargo.toml` 中声明所需的依赖和 features

### 3.3 QEMU 测试配置

配置文件格式与 C 测试用例相同，详见[第 2.3 节 QEMU 测试配置](#23-qemu-测试配置)。

### 3.4 板级测试配置

配置文件格式与 C 测试用例相同，详见[第 2.4 节 板级测试配置](#24-板级测试配置)。

### 3.5 执行 QEMU 测试

#### 3.5.1 命令行参数

与 C 测试用例相同：`cargo xtask starry test qemu --target <arch> [--stress] [--test-case <case>]`

详见[第 2 节 C 测试用例](#2-c-测试用例)。

#### 3.5.2 发现机制

与 C 测试用例相同，xtask 扫描 `test-suit/starryos/{normal|stress}/` 下所有子目录中的 `qemu-{arch}.toml`。

#### 3.5.3 构建

xtask 定位用例目录中的 `rust/Cargo.toml`，根据目标架构配置交叉编译目标，执行 `cargo build` 编译 Rust 程序。

#### 3.5.4 rootfs 准备与注入

与 C 测试用例相同，详见[第 2.5.4 节 rootfs 准备与注入](#254-rootfs-准备与注入)。

#### 3.5.5 执行测例

与 C 测试用例相同，详见[第 2.5.5 节 执行测例](#255-执行测例)。

### 3.6 执行开发板测试

#### 3.6.1 命令行参数

与 C 测试用例相同：`cargo xtask starry test board [--test-group <group>] [--board-type <type>] [--server <addr>] [--port <port>]`

详见[第 2 节 C 测试用例](#2-c-测试用例)。

#### 3.6.2 发现机制

与 C 测试用例相同，xtask 扫描 `test-suit/starryos/normal/` 下所有子目录中的 `board-{board_name}.toml`。

#### 3.6.3 构建

与 QEMU 测试相同，xtask 使用 `cargo build` 交叉编译 Rust 程序。

#### 3.6.4 rootfs 准备与注入

与 QEMU 测试相同，详见[第 2.5.4 节 rootfs 准备与注入](#254-rootfs-准备与注入)。

#### 3.6.5 执行测例

与 C 测试用例相同，详见[第 2.6.5 节 执行测例](#265-执行测例)。

### 3.7 新增测试用例

**新增普通测试：**

1. 在 `test-suit/starryos/normal/` 下创建用例目录
2. 创建 `rust/` 子目录，放入 `Cargo.toml` 和 `src/main.rs`
3. 为每个支持的架构创建 `qemu-{arch}.toml`
4. 如需在物理板上测试，创建 `board-{board_name}.toml`

**新增压力测试：**

1. 在 `test-suit/starryos/stress/` 下创建用例目录
2. 创建 `rust/` 子目录，放入 `Cargo.toml` 和 `src/main.rs`
3. 为每个支持的架构创建 `qemu-{arch}.toml`

## 4. 无源码用例

无源码用例不需要编写 C 或 Rust 代码，而是利用 StarryOS 文件系统中包管理器（如 `apk add`）直接安装已有的可执行程序，然后通过 Shell 交互驱动测试。此类用例只需提供平台配置文件（`qemu-{arch}.toml` 或 `board-{board_name}.toml`），测试逻辑完全由 `shell_init_cmd` 中的命令序列定义。

典型的无源码用例是 `stress-ng-0`：系统启动后，`shell_init_cmd` 中通过 `apk add stress-ng` 安装压力测试工具，再执行对应的测试命令。

### 4.1 目录结构

```text
test-suit/starryos/
├── normal/
│   └── smoke/                            # 无源码用例：仅平台配置文件
│       ├── qemu-aarch64.toml
│       ├── qemu-riscv64.toml
│       ├── qemu-loongarch64.toml
│       ├── qemu-x86_64.toml
│       └── board-orangepi-5-plus.toml
└── stress/
    └── stress-ng-0/                      # 无源码用例：apk 安装后执行
        ├── qemu-aarch64.toml
        ├── qemu-riscv64.toml
        ├── qemu-loongarch64.toml
        └── qemu-x86_64.toml
```

### 4.2 配置文件

配置文件格式与 C/Rust 测试用例相同，详见[第 2.3 节 QEMU 测试配置](#23-qemu-测试配置)和[第 2.4 节 板级测试配置](#24-板级测试配置)。

关键区别在于：

- 目录中不包含 `c/` 或 `rust/` 子目录
- 测试逻辑完全由 `shell_init_cmd` 定义，通常包含安装和执行两个阶段

**示例** — `stress/stress-ng-0/qemu-x86_64.toml`：

```toml
shell_init_cmd = '''
apk update && \
apk add stress-ng && \
stress-ng --cpu 8 --timeout 10s && \
stress-ng --sigsegv 8 --sigsegv-ops 1000 && \
pwd && ls -al && echo 'All tests passed!'
'''
```

### 4.3 执行流程

1. xtask 扫描发现用例目录中的 `qemu-{arch}.toml`
2. 由于没有 `c/` 或 `rust/` 子目录，跳过构建和 rootfs 注入步骤
3. 直接使用 StarryOS 预构建的 rootfs 镜像启动 QEMU
4. 等待 `shell_prefix` 出现后发送 `shell_init_cmd`（安装并运行测试程序）
5. 通过 `success_regex` / `fail_regex` 判定结果

### 4.4 新增无源码用例

**新增普通测试：**

1. 在 `test-suit/starryos/normal/` 下创建用例目录（如 `my_smoke_test/`）
2. 为每个支持的架构创建 `qemu-{arch}.toml`，在 `shell_init_cmd` 中编写安装和测试命令
3. 如需在物理板上测试，创建 `board-{board_name}.toml`

**新增压力测试：**

1. 在 `test-suit/starryos/stress/` 下创建用例目录
2. 为每个支持的架构创建 `qemu-{arch}.toml`
3. 压力测试通常使用更长的 `timeout` 和更复杂的 `shell_init_cmd`
