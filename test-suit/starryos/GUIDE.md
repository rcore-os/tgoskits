# 添加 StarryOS QEMU 测试用例

本文档说明如何在 `test-suit/starryos/` 下添加新的测试用例。

## 目录结构

```
test-suit/starryos/
  normal/                     # 常规测试（每次 push 均运行）
    board-orangepi-5-plus/    # build group：OrangePi 5 Plus 物理板测
      build-aarch64-unknown-none-softfloat.toml
      board-orangepi-5-plus/
        board-orangepi-5-plus.toml
      pcie-enumerate/
        board-orangepi-5-plus.toml
    qemu-smp1/                # build group：默认单核 QEMU 构建
      build-<target>.toml     # 每个目标一个构建配置
      smoke/                  # case：基础启动测试
        qemu-<arch>.toml      # 每个架构一个运行配置
      usb/                    # case：USB 设备测试（含 C 源码）
        qemu-<arch>.toml
        c/
          CMakeLists.txt
          prebuild.sh         # 可选：在 rootfs 内安装依赖包
          src/
            main.c
      helloworld/             # case：简单 C 程序（无需 prebuild）
        qemu-<arch>.toml
        c/
          CMakeLists.txt
          src/
            main.c
      bugfix/                 # case：一次 Starry/QEMU 运行多个 guest 程序
        qemu-<arch>.toml
        bug-a/
          c/
            CMakeLists.txt
            src/
              main.c
        bug-b/
          c/
            CMakeLists.txt
            src/
              main.c
    qemu-smp4/                # build group：显式 `-smp 4` 的 QEMU 测试
      build-<target>.toml
      affinity/
        qemu-x86_64.toml
      test-shm-deadlock/
        qemu-<arch>.toml
        c/
          CMakeLists.txt
          src/
            main.c
  stress/                     # 压力测试（PR 合入 main 时运行）
    stress-ng-0/
      build-<target>.toml
      stress-ng-0/
        qemu-<arch>.toml
```

## 快速开始：纯 Shell 测试

如果测试只需要 shell 命令（不需要编译二进制），只需创建：

```
test-suit/starryos/normal/qemu-smp1/build-<target>.toml
test-suit/starryos/normal/qemu-smp1/<case>/qemu-<arch>.toml
```

示例（`qemu-smp1/smoke/qemu-riscv64.toml`）：

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
test-suit/starryos/normal/qemu-smp1/
  build-<target>.toml
  <case>/
    qemu-<arch>.toml   # 每个支持的架构一个运行配置
    c/
      CMakeLists.txt   # 必需：构建定义
      prebuild.sh       # 可选：向 rootfs 安装依赖包
      src/              # 源码目录
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

### 3. 分组 C 测试

如果多个 C 测试可以共用同一份 QEMU 配置，可以放在同一个 case 目录下：

```
test-suit/starryos/normal/qemu-smp1/
  build-<target>.toml
  <case>/
    qemu-<arch>.toml
    <子用例-a>/c/CMakeLists.txt
    <子用例-a>/c/src/main.c
    <子用例-b>/c/CMakeLists.txt
    <子用例-b>/c/src/main.c
```

在 `qemu-<arch>.toml` 中使用 `test_commands`，不要同时写 `shell_init_cmd`：

```toml
shell_prefix = "root@starry:"
test_commands = [
    "/usr/bin/test-a",
    "/usr/bin/test-b",
]
success_regex = ["(?m)^STARRY_GROUPED_TESTS_PASSED\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b', '(?m)^STARRY_GROUPED_TEST_FAILED:']
```

运行器会按子目录名稳定构建所有 C 子用例，生成 `/usr/bin/starry-run-case-tests`，
并在 guest 中顺序执行 `test_commands`。任一命令返回非 0 时，该 grouped case 失败。

### 4. 可选：`prebuild.sh`

如果测试需要安装额外的依赖包（rootfs 未默认提供的包）：

```sh
#!/bin/sh
set -eu

apk add zlib-dev   # 按需添加
```

该脚本通过 qemu-user 在 rootfs 内执行，可以直接使用 `apk add`。

> **注意**：rootfs 已默认包含基础构建工具链与常用开发包，不要重复安装默认包。

### 5. 编写 `qemu-<arch>.toml`

将 `shell_init_cmd` 设为安装后的二进制路径：

```toml
shell_init_cmd = "/usr/bin/mytest"
```

QEMU 参数可以从已有用例复制（如 `qemu-smp1/smoke/qemu-<arch>.toml`），按需调整。

如果某个用例需要多核环境，可以直接在 `qemu-<arch>.toml` 的 `args` 中加入
`"-smp", "<N>"`。StarryOS 的测试运行器会自动用同样的 CPU 数重新配置内核构建，
避免出现 QEMU 是多核而内核仍按单核模式编译的问题。

### 6. 支持的架构

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
| `test_commands`  | `[string]`      | 分组测试的 guest 命令列表；不能与 `shell_init_cmd` 同时使用 |
| `success_regex`  | `[string]`      | 所有正则均匹配则判定为 PASS（支持多行正则） |
| `fail_regex`     | `[string]`      | 任一正则匹配则立即判定为 FAIL |
| `timeout`        | `integer`       | 超时秒数，超时则判定为失败 |

## 运行测试

```bash
# 运行某架构的所有常规测试
cargo xtask starry test qemu --arch riscv64

# 运行指定测试用例
cargo xtask starry test qemu --arch riscv64 -c helloworld

# 运行压力测试
cargo xtask starry test qemu --stress --arch riscv64
```

## 注意事项

- `fail_regex` 要尽量精准，避免匹配到正常输出如 `failed: 0`。
- `success_regex` 应选择输出中**稳定且唯一**的成功标志行。
- 对于较慢的测试，先确认命令仍在正常执行，再酌情增加 `timeout`。
- 通过 `prebuild.sh` 安装的二进制依赖会在 staging rootfs 中交叉编译，标准 Alpine 包均可使用。
- 不要在同一个工作区中并行运行多个 `cargo xtask starry test qemu` 命令。
