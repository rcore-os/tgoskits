# 添加 StarryOS QEMU 测试用例

本文档说明如何在 `test-suit/starryos/` 下添加新的测试用例。

## 目录结构

```
test-suit/starryos/
  normal/                     # 常规测试（每次 push 均运行）
    smoke/                    # 基础启动测试
      qemu-<arch>.toml        # 每个架构一个 TOML 文件
    usb/                      # USB 设备测试（含 C 源码）
      c/
        CMakeLists.txt
        prebuild.sh           # 可选：在 rootfs 内安装依赖包
        src/
          main.c
      qemu-<arch>.toml
    helloworld/               # 简单 C 程序（无需 prebuild）
      c/
        CMakeLists.txt
        src/
          main.c
      qemu-<arch>.toml
  stress/                     # 压力测试（PR 合入 main 时运行）
    stress-ng-0/
      qemu-<arch>.toml
```

## 快速开始：纯 Shell 测试

如果测试只需要 shell 命令（不需要编译二进制），只需创建：

```
test-suit/starryos/normal/<用例名>/qemu-<arch>.toml
```

示例（`smoke/qemu-riscv64.toml`）：

```toml
args = [
    "-nographic", "-cpu", "rv64",
    "-device", "virtio-blk-pci,drive=disk0",
    "-drive", "id=disk0,if=none,format=raw,file=${workspace}/target/rootfs/rootfs-riscv64-alpine.img",
    "-device", "virtio-net-pci,netdev=net0",
    "-netdev", "user,id=net0",
]
uefi = false
to_bin = true
shell_prefix = "root@starry:"
shell_init_cmd = "pwd && echo 'All tests passed!'"
success_regex = ["(?m)^All tests passed!\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b']
timeout = 15
```

## C 测试用例

### 1. 创建目录结构

```
test-suit/starryos/normal/<用例名>/
  c/
    CMakeLists.txt     # 必需：构建定义
    prebuild.sh         # 可选：向 rootfs 安装依赖包
    src/                # 源码目录
  qemu-<arch>.toml     # 每个支持的架构一个文件
```

### 2. 编写 `CMakeLists.txt`

构建系统使用 clang 交叉编译，以 rootfs 作为 sysroot。编译出的可执行文件会被安装到客户机的 `/usr/bin/` 目录。

```cmake
cmake_minimum_required(VERSION 3.20)
project(mytest C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(mytest src/main.c)
target_compile_options(mytest PRIVATE -Wall -Wextra -Werror)

install(TARGETS mytest RUNTIME DESTINATION usr/bin)
```

### 3. 可选：`prebuild.sh`

如果测试需要安装额外的依赖包（如库文件）：

```sh
#!/bin/sh
set -eu

apk add gcc musl-dev libusb-dev   # 按需添加
```

该脚本通过 qemu-user 在 rootfs 内执行，可以直接使用 `apk add`。

> **注意**：如果使用了 C 标准库头文件（如 `stdio.h`），需要安装 `gcc musl-dev`。

### 4. 编写 `qemu-<arch>.toml`

将 `shell_init_cmd` 设为安装后的二进制路径：

```toml
shell_init_cmd = "/usr/bin/mytest"
```

QEMU 参数可以从已有用例复制（如 `smoke/qemu-<arch>.toml`），按需调整。

如果某个用例需要多核环境，可以直接在 `qemu-<arch>.toml` 的 `args` 中加入
`"-smp", "<N>"`。StarryOS 的测试运行器会自动用同样的 CPU 数重新配置内核构建，
避免出现 QEMU 是多核而内核仍按单核模式编译的问题。

### 5. 支持的架构

| 架构         | Target                              | QEMU CPU  |
|-------------|-------------------------------------|-----------|
| x86_64      | x86_64-unknown-none                 | (默认)     |
| aarch64     | aarch64-unknown-none-softfloat      | cortex-a53 |
| riscv64     | riscv64gc-unknown-none-elf          | rv64      |
| loongarch64 | loongarch64-unknown-none-softfloat  | la464     |

只为**实际验证通过的架构**创建 `qemu-<arch>.toml`。

## TOML 字段说明

| 字段              | 类型            | 说明 |
|------------------|-----------------|------|
| `args`           | `[string]`      | QEMU 命令行参数。`${workspace}` 会被替换为仓库根目录。 |
| `uefi`           | `bool`          | 是否使用 UEFI 启动（大多数用例为 false） |
| `to_bin`         | `bool`          | 是否用 objcopy 将 ELF 转为裸二进制 |
| `shell_prefix`   | `string`        | shell 提示符匹配模式，等待该模式出现后再发送命令 |
| `shell_init_cmd` | `string`        | 发送到客户机 shell 的测试命令 |
| `success_regex`  | `[string]`      | 所有正则均匹配则判定为 PASS（支持多行正则） |
| `fail_regex`     | `[string]`      | 任一正则匹配则立即判定为 FAIL |
| `timeout`        | `integer`       | 超时秒数，超时则判定为失败 |

## 运行测试

```bash
# 运行某架构的所有常规测试
cargo starry test qemu -t riscv64

# 运行指定测试用例
cargo starry test qemu -t riscv64 -c helloworld

# 运行压力测试
cargo starry test qemu --stress -t riscv64
```

## 注意事项

- `fail_regex` 要尽量精准，避免匹配到正常输出如 `failed: 0`。
- `success_regex` 应选择输出中**稳定且唯一**的成功标志行。
- 对于较慢的测试，先确认命令仍在正常执行，再酌情增加 `timeout`。
- 通过 `prebuild.sh` 安装的二进制依赖会在 staging rootfs 中交叉编译，标准 Alpine 包均可使用。
- 不要在同一个工作区中并行运行多个 `cargo starry test qemu` 命令。
